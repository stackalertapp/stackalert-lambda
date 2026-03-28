use std::future::Future;
use std::pin::Pin;

use anyhow::{Context, Result};
use tracing::info;

use super::{NotifyChannel, fmt_pct, ranked_services};
use crate::anomaly::Spike;
use crate::config::Config;
use crate::cost_explorer::SpendHistory;

pub struct Sns {
    client: aws_sdk_sns::Client,
}

impl Sns {
    pub fn new(base_cfg: &aws_config::SdkConfig) -> Self {
        Self {
            client: aws_sdk_sns::Client::new(base_cfg),
        }
    }
}

impl NotifyChannel for Sns {
    fn name(&self) -> &'static str {
        "sns"
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
                .sns
                .as_ref()
                .context("SNS channel active but config missing")?;
            let text = build_spike_text(
                spikes,
                &cfg.setup_name,
                cfg.dedup_cooldown_hours,
                cfg.max_spike_display,
            );
            publish(
                &self.client,
                &scfg.topic_arn,
                &format!("{}: Cost Spike Detected", cfg.setup_name),
                &text,
            )
            .await
        })
    }

    fn send_daily_digest<'a>(
        &'a self,
        cfg: &'a Config,
        spend_data: &'a SpendHistory,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(async move {
            let scfg = cfg
                .sns
                .as_ref()
                .context("SNS channel active but config missing")?;
            let text = build_digest_text(
                spend_data,
                &cfg.setup_name,
                cfg.min_avg_daily_usd,
                cfg.max_digest_display,
            );
            publish(
                &self.client,
                &scfg.topic_arn,
                &format!("{} Daily Digest", cfg.setup_name),
                &text,
            )
            .await
        })
    }
}

async fn publish(
    client: &aws_sdk_sns::Client,
    topic_arn: &str,
    subject: &str,
    message: &str,
) -> Result<bool> {
    let result = client
        .publish()
        .topic_arn(topic_arn)
        .subject(subject)
        .message(message)
        .send()
        .await;

    match result {
        Ok(_) => {
            info!("SNS message published");
            Ok(true)
        }
        Err(e) => Err(anyhow::anyhow!("SNS publish failed: {e}")),
    }
}

// ── Plain text formatting ───────────────────────────────────────────────────

fn build_spike_text(
    spikes: &[Spike],
    setup_name: &str,
    check_interval_hours: u32,
    max_display: usize,
) -> String {
    assert!(!spikes.is_empty(), "build_spike_text called with empty spikes");
    let total_extra: f64 = spikes.iter().map(|s| s.extra_usd).sum();
    let top = &spikes[0];

    let mut msg = format!("{setup_name}: Cost Spike Detected\n\n");
    msg.push_str(&format!(
        "{} spiked {} (${:.2} today vs ${:.2}/day avg)\n",
        top.service,
        fmt_pct(top.pct_increase),
        top.today,
        top.avg_daily,
    ));
    msg.push_str(&format!("Total extra spend: ~${:.2}\n\n", total_extra));

    msg.push_str("Affected services:\n");
    for spike in spikes.iter().take(max_display) {
        msg.push_str(&format!(
            "- {} {} (${:.2} today vs ${:.2} avg)\n",
            spike.service,
            fmt_pct(spike.pct_increase),
            spike.today,
            spike.avg_daily,
        ));
    }
    if spikes.len() > max_display {
        msg.push_str(&format!(
            "...and {} more services\n",
            spikes.len() - max_display
        ));
    }
    msg.push_str(&format!(
        "\n{setup_name} - Checks every {check_interval_hours}h"
    ));
    msg
}

fn build_digest_text(
    spend_data: &SpendHistory,
    setup_name: &str,
    min_avg_daily: f64,
    max_display: usize,
) -> String {
    let (services, grand_total) = ranked_services(spend_data, min_avg_daily);

    let mut msg = format!("{setup_name}: Daily Digest\n\n");
    msg.push_str(&format!("Avg daily spend: ${:.2}\n\n", grand_total));
    msg.push_str("Top services (avg/day):\n");
    for (service, avg) in services.iter().take(max_display) {
        msg.push_str(&format!("- {} -- ${:.2}/day\n", service, avg));
    }
    if services.len() > max_display {
        msg.push_str(&format!(
            "...and {} more services\n",
            services.len() - max_display
        ));
    }
    msg.push_str(&format!("\n{setup_name} Daily Digest"));
    msg
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn test_spike_text() {
        let spikes = vec![Spike {
            service: "Amazon EC2".to_string(),
            avg_daily: 18.0,
            today: 75.0,
            pct_increase: 316.67,
            extra_usd: 57.0,
        }];
        let text = build_spike_text(&spikes, "StackAlert", 6, 5);
        assert!(text.contains("StackAlert: Cost Spike Detected"));
        assert!(text.contains("Amazon EC2"));
        assert!(!text.contains("<b>")); // plain text, no HTML
    }

    #[test]
    fn test_digest_text() {
        let mut data = HashMap::new();
        data.insert("Amazon EC2".to_string(), vec![18.0, 19.0]);
        let text = build_digest_text(&data, "StackAlert", 0.10, 10);
        assert!(text.contains("StackAlert: Daily Digest"));
        assert!(text.contains("Amazon EC2"));
    }
}
