use std::future::Future;
use std::pin::Pin;

use anyhow::{Context, Result};
use tracing::{info, warn};

use super::{NotifyChannel, fmt_pct, ranked_services, shared_http_client};
use crate::anomaly::Spike;
use crate::config::Config;
use crate::cost_explorer::SpendHistory;

pub struct Teams;

impl NotifyChannel for Teams {
    fn name(&self) -> &'static str {
        "teams"
    }

    fn send_spike_alert<'a>(
        &'a self,
        cfg: &'a Config,
        spikes: &'a [Spike],
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(async move {
            if spikes.is_empty() {
                return Ok(false);
            }
            let tcfg = cfg
                .teams
                .as_ref()
                .context("Teams channel active but config missing")?;
            let card = build_spike_card(
                spikes,
                &cfg.setup_name,
                cfg.dedup_cooldown_hours,
                cfg.max_spike_display,
            );
            send_teams(cfg, &tcfg.webhook_url, &card).await
        })
    }

    fn send_daily_digest<'a>(
        &'a self,
        cfg: &'a Config,
        spend_data: &'a SpendHistory,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(async move {
            let tcfg = cfg
                .teams
                .as_ref()
                .context("Teams channel active but config missing")?;
            let card = build_digest_card(
                spend_data,
                &cfg.setup_name,
                cfg.min_avg_daily_usd,
                cfg.max_digest_display,
            );
            send_teams(cfg, &tcfg.webhook_url, &card).await
        })
    }
}

async fn send_teams(cfg: &Config, webhook_url: &str, card: &serde_json::Value) -> Result<bool> {
    // NOTE: webhook_url is a secret (acts as bearer token) — do not log it.
    let resp = shared_http_client(cfg)
        .post(webhook_url)
        .json(card)
        .send()
        .await
        .context("Failed to send Teams webhook")?;

    if resp.status().is_success() {
        info!("Teams message sent");
        Ok(true)
    } else {
        let status = resp.status();
        warn!(%status, "Teams webhook returned non-2xx");
        Ok(false)
    }
}

// ── Adaptive Card builders ──────────────────────────────────────────────────

fn build_spike_card(
    spikes: &[Spike],
    setup_name: &str,
    check_interval_hours: u32,
    max_display: usize,
) -> serde_json::Value {
    assert!(
        !spikes.is_empty(),
        "build_spike_card called with empty spikes"
    );
    let total_extra: f64 = spikes.iter().map(|s| s.extra_usd).sum();
    let top = &spikes[0];

    let mut lines = vec![
        format!(
            "**{}** spiked {} (${:.2} today vs ${:.2}/day avg)",
            top.service,
            fmt_pct(top.pct_increase),
            top.today,
            top.avg_daily,
        ),
        format!("Total extra spend: ~${:.2}", total_extra),
        String::new(),
        "**Affected services:**".to_string(),
    ];

    for spike in spikes.iter().take(max_display) {
        lines.push(format!(
            "- **{}** {} (${:.2} today vs ${:.2} avg)",
            spike.service,
            fmt_pct(spike.pct_increase),
            spike.today,
            spike.avg_daily,
        ));
    }
    if spikes.len() > max_display {
        lines.push(format!(
            "_...and {} more services_",
            spikes.len() - max_display
        ));
    }
    lines.push(String::new());
    lines.push(format!(
        "{setup_name} · Checks every {check_interval_hours}h"
    ));

    wrap_adaptive_card(
        &format!("{setup_name}: Cost Spike Detected"),
        &lines.join("\n\n"),
    )
}

fn build_digest_card(
    spend_data: &SpendHistory,
    setup_name: &str,
    min_avg_daily: f64,
    max_display: usize,
) -> serde_json::Value {
    let (services, grand_total) = ranked_services(spend_data, min_avg_daily);

    let mut lines = vec![
        format!("Avg daily spend: **${:.2}**", grand_total),
        String::new(),
        "**Top services (avg/day):**".to_string(),
    ];
    for (service, avg) in services.iter().take(max_display) {
        lines.push(format!("- {} — ${:.2}/day", service, avg));
    }
    if services.len() > max_display {
        lines.push(format!(
            "_...and {} more services_",
            services.len() - max_display
        ));
    }
    lines.push(String::new());
    lines.push(format!("{setup_name} Daily Digest"));

    wrap_adaptive_card(&format!("{setup_name}: Daily Digest"), &lines.join("\n\n"))
}

fn wrap_adaptive_card(title: &str, body: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "message",
        "attachments": [{
            "contentType": "application/vnd.microsoft.card.adaptive",
            "content": {
                "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                "type": "AdaptiveCard",
                "version": "1.4",
                "body": [
                    {
                        "type": "TextBlock",
                        "text": title,
                        "size": "large",
                        "weight": "bolder"
                    },
                    {
                        "type": "TextBlock",
                        "text": body,
                        "wrap": true
                    }
                ]
            }
        }]
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn test_spike_card_structure() {
        let spikes = vec![Spike {
            service: "Amazon EC2".to_string(),
            avg_daily: 18.0,
            today: 75.0,
            pct_increase: 316.67,
            extra_usd: 57.0,
        }];
        let card = build_spike_card(&spikes, "StackAlert", 6, 5);
        let json = serde_json::to_string(&card).unwrap();
        assert!(json.contains("AdaptiveCard"));
        assert!(json.contains("StackAlert: Cost Spike Detected"));
        assert!(json.contains("Amazon EC2"));
    }

    #[test]
    fn test_digest_card_structure() {
        let mut data = HashMap::new();
        data.insert("Amazon EC2".to_string(), vec![18.0, 19.0]);
        let card = build_digest_card(&data, "StackAlert", 0.10, 10);
        let json = serde_json::to_string(&card).unwrap();
        assert!(json.contains("AdaptiveCard"));
        assert!(json.contains("StackAlert: Daily Digest"));
    }
}
