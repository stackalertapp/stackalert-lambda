use std::future::Future;
use std::pin::Pin;

use anyhow::{Context, Result};
use tracing::{info, warn};

use super::{NotifyChannel, escape_html, fmt_pct, ranked_services, shared_http_client};
use crate::anomaly::Spike;
use crate::config::{Config, TelegramConfig};
use crate::cost_explorer::SpendHistory;

const TELEGRAM_API: &str = "https://api.telegram.org";

pub struct Telegram;

impl NotifyChannel for Telegram {
    fn name(&self) -> &'static str {
        "telegram"
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
                .telegram
                .as_ref()
                .context("Telegram channel active but config missing")?;
            let text = build_spike_message(spikes, cfg.dedup_cooldown_hours, cfg.max_spike_display);
            send_telegram(cfg, tcfg, &text).await
        })
    }

    fn send_daily_digest<'a>(
        &'a self,
        cfg: &'a Config,
        spend_data: &'a SpendHistory,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(async move {
            let tcfg = cfg
                .telegram
                .as_ref()
                .context("Telegram channel active but config missing")?;
            let text =
                build_digest_message(spend_data, cfg.min_avg_daily_usd, cfg.max_digest_display);
            send_telegram(cfg, tcfg, &text).await
        })
    }
}

// ── HTTP send ───────────────────────────────────────────────────────────────

async fn send_telegram(cfg: &Config, tcfg: &TelegramConfig, text: &str) -> Result<bool> {
    // NOTE: Telegram requires the bot token in the URL path — there is no header-based
    // alternative. Never log `url` or the request object; doing so would leak the token
    // into CloudWatch. All tracing statements in this function intentionally omit it.
    let url = format!("{TELEGRAM_API}/bot{}/sendMessage", tcfg.bot_token);

    let resp = shared_http_client(cfg)
        .post(&url)
        .json(&serde_json::json!({
            "chat_id": tcfg.chat_id,
            "text": text,
            "parse_mode": "HTML",
            "disable_web_page_preview": true,
        }))
        .send()
        .await
        .context("Failed to send Telegram message")?;

    if resp.status().is_success() {
        info!("Telegram message sent");
        Ok(true)
    } else {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        warn!(%status, "Telegram API returned non-2xx");
        // Don't log body — may contain sensitive token info in error messages
        let _ = body;
        Ok(false)
    }
}

// ── Message formatting ──────────────────────────────────────────────────────

fn build_spike_message(spikes: &[Spike], check_interval_hours: u32, max_display: usize) -> String {
    assert!(!spikes.is_empty(), "build_spike_message called with empty spikes");
    let total_extra: f64 = spikes.iter().map(|s| s.extra_usd).sum();
    let top_spike = &spikes[0];

    let mut msg = String::from("⚠️ <b>AWS Cost Spike Detected</b>\n\n");
    msg.push_str(&format!(
        "🔝 <b>{}</b> spiked {} (${:.2} today vs ${:.2}/day avg)\n",
        escape_html(&top_spike.service),
        fmt_pct(top_spike.pct_increase),
        top_spike.today,
        top_spike.avg_daily,
    ));
    msg.push_str(&format!("💰 Total extra spend: ~${:.2}\n\n", total_extra));

    msg.push_str("<b>Affected services:</b>\n");
    for spike in spikes.iter().take(max_display) {
        msg.push_str(&format!(
            "• <b>{}</b>  {} (${:.2} today vs ${:.2} avg)\n",
            escape_html(&spike.service),
            fmt_pct(spike.pct_increase),
            spike.today,
            spike.avg_daily,
        ));
    }

    if spikes.len() > max_display {
        msg.push_str(&format!(
            "<i>...and {} more services</i>\n",
            spikes.len() - max_display
        ));
    }

    msg.push_str(&format!(
        "\n🔔 StackAlert · Checks every {check_interval_hours}h"
    ));
    msg
}

fn build_digest_message(
    spend_data: &SpendHistory,
    min_avg_daily: f64,
    max_display: usize,
) -> String {
    let (services, grand_total) = ranked_services(spend_data, min_avg_daily);

    let mut msg = String::from("📊 <b>Daily AWS Cost Digest</b>\n\n");
    msg.push_str(&format!(
        "💰 Avg daily spend: <b>${:.2}</b>\n\n",
        grand_total
    ));

    msg.push_str("<b>Top services (avg/day):</b>\n");
    for (service, avg) in services.iter().take(max_display) {
        msg.push_str(&format!("• {} — ${:.2}/day\n", escape_html(service), avg));
    }

    if services.len() > max_display {
        msg.push_str(&format!(
            "<i>...and {} more services</i>\n",
            services.len() - max_display
        ));
    }

    msg.push_str("\n📅 StackAlert Daily Digest");
    msg
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn test_spike_message_format() {
        let spikes = vec![
            Spike {
                service: "Amazon EC2".to_string(),
                avg_daily: 18.0,
                today: 75.0,
                pct_increase: 316.67,
                extra_usd: 57.0,
            },
            Spike {
                service: "Amazon RDS".to_string(),
                avg_daily: 5.0,
                today: 20.0,
                pct_increase: 300.0,
                extra_usd: 15.0,
            },
        ];
        let msg = build_spike_message(&spikes, 6, 5);
        assert!(msg.contains("AWS Cost Spike Detected"));
        assert!(msg.contains("Amazon EC2"));
        assert!(msg.contains("Amazon RDS"));
        assert!(msg.contains("+317%"));
    }

    #[test]
    fn test_spike_message_new_service() {
        let spikes = vec![Spike {
            service: "Amazon SageMaker".to_string(),
            avg_daily: 0.0,
            today: 45.0,
            pct_increase: f64::INFINITY,
            extra_usd: 45.0,
        }];
        let msg = build_spike_message(&spikes, 6, 5);
        assert!(msg.contains("NEW"));
        assert!(msg.contains("SageMaker"));
    }

    #[test]
    fn test_digest_message_format() {
        let mut spend_data = HashMap::new();
        spend_data.insert(
            "Amazon EC2".to_string(),
            vec![18.0, 19.0, 17.5, 18.5, 18.0, 19.5, 17.0],
        );
        spend_data.insert(
            "Amazon S3".to_string(),
            vec![0.50, 0.45, 0.55, 0.48, 0.52, 0.47, 0.53],
        );
        let msg = build_digest_message(&spend_data, 0.10, 10);
        assert!(msg.contains("Daily AWS Cost Digest"));
        assert!(msg.contains("Amazon EC2"));
        assert!(msg.contains("Avg daily spend"));
    }

    #[test]
    fn test_digest_filters_noise() {
        let mut spend_data = HashMap::new();
        spend_data.insert("Amazon EC2".to_string(), vec![18.0, 19.0, 17.5]);
        spend_data.insert("AWS Tax".to_string(), vec![0.01, 0.02, 0.01]);
        let msg = build_digest_message(&spend_data, 0.10, 10);
        assert!(msg.contains("Amazon EC2"));
        assert!(!msg.contains("AWS Tax"));
    }

    #[test]
    fn test_escape_html() {
        assert_eq!(escape_html("a < b & c > d"), "a &lt; b &amp; c &gt; d");
    }

    #[test]
    fn test_spike_message_truncates_at_5() {
        let spikes: Vec<Spike> = (0..7)
            .map(|i| Spike {
                service: format!("Service {}", i),
                avg_daily: 10.0,
                today: 20.0,
                pct_increase: 100.0,
                extra_usd: 10.0 - i as f64,
            })
            .collect();
        let msg = build_spike_message(&spikes, 6, 5);
        assert!(msg.contains("...and 2 more services"));
    }
}
