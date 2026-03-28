use std::future::Future;
use std::pin::Pin;

use anyhow::{Context, Result};
use tracing::{info, warn};

use super::{NotifyChannel, escape_mrkdwn, fmt_pct, ranked_services, shared_http_client};
use crate::anomaly::Spike;
use crate::config::Config;
use crate::cost_explorer::SpendHistory;

pub struct Slack;

impl NotifyChannel for Slack {
    fn name(&self) -> &'static str {
        "slack"
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
            let scfg = cfg
                .slack
                .as_ref()
                .context("Slack channel active but config missing")?;
            let text = build_spike_message(
                spikes,
                &cfg.setup_name,
                cfg.dedup_cooldown_hours,
                cfg.max_spike_display,
            );
            send_slack(cfg, &scfg.webhook_url, &text).await
        })
    }

    fn send_daily_digest<'a>(
        &'a self,
        cfg: &'a Config,
        spend_data: &'a SpendHistory,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(async move {
            let scfg = cfg
                .slack
                .as_ref()
                .context("Slack channel active but config missing")?;
            let text = build_digest_message(
                spend_data,
                &cfg.setup_name,
                cfg.min_avg_daily_usd,
                cfg.max_digest_display,
            );
            send_slack(cfg, &scfg.webhook_url, &text).await
        })
    }
}

async fn send_slack(cfg: &Config, webhook_url: &str, text: &str) -> Result<bool> {
    // NOTE: webhook_url is a secret (acts as bearer token) — do not log it.
    let resp = shared_http_client(cfg)
        .post(webhook_url)
        .json(&serde_json::json!({ "text": text }))
        .send()
        .await
        .context("Failed to send Slack webhook")?;

    if resp.status().is_success() {
        info!("Slack message sent");
        Ok(true)
    } else {
        let status = resp.status();
        warn!(%status, "Slack webhook returned non-2xx");
        Ok(false)
    }
}

// ── Message formatting (Slack mrkdwn) ───────────────────────────────────────

fn build_spike_message(
    spikes: &[Spike],
    setup_name: &str,
    check_interval_hours: u32,
    max_display: usize,
) -> String {
    assert!(!spikes.is_empty(), "build_spike_message called with empty spikes");
    let total_extra: f64 = spikes.iter().map(|s| s.extra_usd).sum();
    let top = &spikes[0];

    let name = escape_mrkdwn(setup_name);
    let mut msg = format!(":warning: *{name}: Cost Spike Detected*\n\n");
    msg.push_str(&format!(
        ":chart_with_upwards_trend: *{}* spiked {} (${:.2} today vs ${:.2}/day avg)\n",
        top.service,
        fmt_pct(top.pct_increase),
        top.today,
        top.avg_daily,
    ));
    msg.push_str(&format!(
        ":moneybag: Total extra spend: ~${:.2}\n\n",
        total_extra
    ));

    msg.push_str("*Affected services:*\n");
    for spike in spikes.iter().take(max_display) {
        msg.push_str(&format!(
            "• *{}*  {} (${:.2} today vs ${:.2} avg)\n",
            spike.service,
            fmt_pct(spike.pct_increase),
            spike.today,
            spike.avg_daily,
        ));
    }

    if spikes.len() > max_display {
        msg.push_str(&format!(
            "_...and {} more services_\n",
            spikes.len() - max_display
        ));
    }

    msg.push_str(&format!(
        "\n:bell: {name} · Checks every {check_interval_hours}h"
    ));
    msg
}

fn build_digest_message(
    spend_data: &SpendHistory,
    setup_name: &str,
    min_avg_daily: f64,
    max_display: usize,
) -> String {
    let (services, grand_total) = ranked_services(spend_data, min_avg_daily);

    let name = escape_mrkdwn(setup_name);
    let mut msg = format!(":bar_chart: *{name}: Daily Digest*\n\n");
    msg.push_str(&format!(
        ":moneybag: Avg daily spend: *${:.2}*\n\n",
        grand_total
    ));
    msg.push_str("*Top services (avg/day):*\n");
    for (service, avg) in services.iter().take(max_display) {
        msg.push_str(&format!("• {} — ${:.2}/day\n", service, avg));
    }
    if services.len() > max_display {
        msg.push_str(&format!(
            "_...and {} more services_\n",
            services.len() - max_display
        ));
    }
    msg.push_str(&format!("\n:calendar: {name} Daily Digest"));
    msg
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn test_spike_message_uses_mrkdwn() {
        let spikes = vec![Spike {
            service: "Amazon EC2".to_string(),
            avg_daily: 18.0,
            today: 75.0,
            pct_increase: 316.67,
            extra_usd: 57.0,
        }];
        let msg = build_spike_message(&spikes, "StackAlert", 6, 5);
        assert!(msg.contains("*StackAlert: Cost Spike Detected*"));
        assert!(msg.contains("*Amazon EC2*"));
        assert!(msg.contains("+317%"));
        assert!(!msg.contains("<b>"));
    }

    #[test]
    fn test_digest_message_uses_mrkdwn() {
        let mut data = HashMap::new();
        data.insert("Amazon EC2".to_string(), vec![18.0, 19.0, 17.5]);
        let msg = build_digest_message(&data, "StackAlert", 0.10, 10);
        assert!(msg.contains("*StackAlert: Daily Digest*"));
        assert!(!msg.contains("<b>"));
    }

    #[test]
    fn test_new_service_shows_new() {
        let spikes = vec![Spike {
            service: "SageMaker".to_string(),
            avg_daily: 0.0,
            today: 45.0,
            pct_increase: f64::INFINITY,
            extra_usd: 45.0,
        }];
        let msg = build_spike_message(&spikes, "StackAlert", 6, 5);
        assert!(msg.contains("NEW"));
    }
}
