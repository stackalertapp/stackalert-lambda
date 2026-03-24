use crate::anomaly::Spike;
use crate::cost_explorer::SpendHistory;
use anyhow::{Context, Result};
use once_cell::sync::Lazy;
use reqwest::Client;
use std::time::Duration;
use tracing::{info, warn};

const TELEGRAM_API: &str = "https://api.telegram.org";

/// Module-level HTTP client — reused across Lambda warm invocations.
/// Avoids TCP connection setup overhead on every check.
static HTTP_CLIENT: Lazy<Client> = Lazy::new(|| {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(5))
        .build()
        .expect("Failed to build HTTP client")
});

/// Send a Telegram alert when cost spikes are detected.
/// Returns true if the message was sent successfully.
pub async fn send_spike_alert(bot_token: &str, chat_id: &str, spikes: &[Spike]) -> Result<bool> {
    if spikes.is_empty() {
        return Ok(false);
    }

    let text = build_spike_message(spikes);
    send_message(bot_token, chat_id, &text).await
}

/// Send a daily cost digest via Telegram.
pub async fn send_daily_digest(
    bot_token: &str,
    chat_id: &str,
    spend_data: &SpendHistory,
) -> Result<bool> {
    let text = build_digest_message(spend_data);
    send_message(bot_token, chat_id, &text).await
}

async fn send_message(bot_token: &str, chat_id: &str, text: &str) -> Result<bool> {
    let url = format!("{TELEGRAM_API}/bot{bot_token}/sendMessage");

    let resp = HTTP_CLIENT
        .post(&url)
        .json(&serde_json::json!({
            "chat_id": chat_id,
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

fn build_spike_message(spikes: &[Spike]) -> String {
    let total_extra: f64 = spikes.iter().map(|s| s.extra_usd).sum();
    let top_spike = &spikes[0];

    let mut msg = String::from("⚠️ <b>AWS Cost Spike Detected</b>\n\n");

    let pct_str = if top_spike.pct_increase.is_infinite() {
        "NEW".to_string()
    } else {
        format!("+{:.0}%", top_spike.pct_increase)
    };
    msg.push_str(&format!(
        "🔝 <b>{}</b> spiked {} (${:.2} today vs ${:.2}/day avg)\n",
        escape_html(&top_spike.service),
        pct_str,
        top_spike.today,
        top_spike.avg_daily,
    ));
    msg.push_str(&format!("💰 Total extra spend: ~${:.2}\n\n", total_extra));

    msg.push_str("<b>Affected services:</b>\n");
    for spike in spikes.iter().take(5) {
        let pct = if spike.pct_increase.is_infinite() {
            "NEW".to_string()
        } else {
            format!("+{:.0}%", spike.pct_increase)
        };
        msg.push_str(&format!(
            "• <b>{}</b>  {} (${:.2} today vs ${:.2} avg)\n",
            escape_html(&spike.service),
            pct,
            spike.today,
            spike.avg_daily,
        ));
    }

    if spikes.len() > 5 {
        msg.push_str(&format!(
            "<i>...and {} more services</i>\n",
            spikes.len() - 5
        ));
    }

    msg.push_str("\n🔔 StackAlert · Checks every 6h");
    msg
}

fn build_digest_message(spend_data: &SpendHistory) -> String {
    let mut msg = String::from("📊 <b>Daily AWS Cost Digest</b>\n\n");

    let mut services: Vec<(String, f64, f64)> = spend_data
        .iter()
        .map(|(service, costs)| {
            let total: f64 = costs.iter().sum();
            let avg = if !costs.is_empty() {
                total / costs.len() as f64
            } else {
                0.0
            };
            (service.clone(), avg, total)
        })
        .filter(|(_, avg, _)| *avg >= 0.10)
        .collect();

    services.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

    let grand_total_daily: f64 = services.iter().map(|(_, avg, _)| avg).sum();

    msg.push_str(&format!(
        "💰 Avg daily spend: <b>${:.2}</b>\n\n",
        grand_total_daily
    ));

    msg.push_str("<b>Top services (avg/day):</b>\n");
    for (service, avg, _) in services.iter().take(10) {
        msg.push_str(&format!("• {} — ${:.2}/day\n", escape_html(service), avg));
    }

    if services.len() > 10 {
        msg.push_str(&format!(
            "<i>...and {} more services</i>\n",
            services.len() - 10
        ));
    }

    msg.push_str("\n📅 StackAlert Daily Digest");
    msg
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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
        let msg = build_spike_message(&spikes);
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
        let msg = build_spike_message(&spikes);
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
        let msg = build_digest_message(&spend_data);
        assert!(msg.contains("Daily AWS Cost Digest"));
        assert!(msg.contains("Amazon EC2"));
        assert!(msg.contains("Avg daily spend"));
    }

    #[test]
    fn test_digest_filters_noise() {
        let mut spend_data = HashMap::new();
        spend_data.insert("Amazon EC2".to_string(), vec![18.0, 19.0, 17.5]);
        spend_data.insert("AWS Tax".to_string(), vec![0.01, 0.02, 0.01]);
        let msg = build_digest_message(&spend_data);
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
        let msg = build_spike_message(&spikes);
        assert!(msg.contains("...and 2 more services"));
    }
}
