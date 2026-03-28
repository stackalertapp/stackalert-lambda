use std::future::Future;
use std::pin::Pin;

use anyhow::{Context, Result};
use tracing::{info, warn};

use super::{NotifyChannel, shared_http_client};
use crate::anomaly::Spike;
use crate::config::Config;
use crate::cost_explorer::SpendHistory;

const EVENTS_API: &str = "https://events.pagerduty.com/v2/enqueue";

pub struct PagerDuty;

impl NotifyChannel for PagerDuty {
    fn name(&self) -> &'static str {
        "pagerduty"
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
            let pcfg = cfg
                .pagerduty
                .as_ref()
                .context("PagerDuty channel active but config missing")?;
            let summary = build_summary(spikes, cfg.max_spike_display);
            send_event(cfg, pcfg, &summary).await
        })
    }

    fn send_daily_digest<'a>(
        &'a self,
        _cfg: &'a Config,
        _spend_data: &'a SpendHistory,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        // Digests are informational — not incidents. Skip PagerDuty for digests.
        Box::pin(async {
            info!("PagerDuty: skipping daily digest (not an incident)");
            Ok(false)
        })
    }
}

use crate::config::PagerDutyConfig;

async fn send_event(cfg: &Config, pcfg: &PagerDutyConfig, summary: &str) -> Result<bool> {
    // NOTE: routing_key is a secret — do not log the payload.
    let payload = serde_json::json!({
        "routing_key": pcfg.routing_key,
        "event_action": "trigger",
        "payload": {
            "summary": summary,
            "severity": pcfg.severity,
            "source": "stackalert-lambda",
            "component": "aws-cost-monitor",
            "group": "cost-spikes",
        }
    });

    let resp = shared_http_client(cfg)
        .post(EVENTS_API)
        .json(&payload)
        .send()
        .await
        .context("Failed to send PagerDuty event")?;

    if resp.status().is_success() {
        info!("PagerDuty event triggered");
        Ok(true)
    } else {
        let status = resp.status();
        warn!(%status, "PagerDuty API returned non-2xx");
        Ok(false)
    }
}

fn build_summary(spikes: &[Spike], max_display: usize) -> String {
    assert!(!spikes.is_empty(), "build_summary called with empty spikes");
    let total_extra: f64 = spikes.iter().map(|s| s.extra_usd).sum();
    let top = &spikes[0];

    let mut summary = format!(
        "AWS Cost Spike: {} spiked ${:.2} (${:.2} extra). ",
        top.service, top.today, top.extra_usd,
    );

    if spikes.len() > 1 {
        let others: Vec<String> = spikes
            .iter()
            .skip(1)
            .take(max_display - 1)
            .map(|s| s.service.clone())
            .collect();
        summary.push_str(&format!("Also: {}. ", others.join(", ")));
    }

    summary.push_str(&format!("Total extra: ${:.2}", total_extra));
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_summary_single_spike() {
        let spikes = vec![Spike {
            service: "Amazon EC2".to_string(),
            avg_daily: 18.0,
            today: 75.0,
            pct_increase: 316.67,
            extra_usd: 57.0,
        }];
        let summary = build_summary(&spikes, 5);
        assert!(summary.contains("Amazon EC2"));
        assert!(summary.contains("$75.00"));
        assert!(summary.contains("$57.00"));
        assert!(!summary.contains("Also:"));
    }

    #[test]
    fn test_summary_multiple_spikes() {
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
        let summary = build_summary(&spikes, 5);
        assert!(summary.contains("Also: Amazon RDS"));
        assert!(summary.contains("$72.00")); // total extra
    }
}
