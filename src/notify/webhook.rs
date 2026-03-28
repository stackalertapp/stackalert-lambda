use std::future::Future;
use std::pin::Pin;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::Serialize;
use tracing::{info, warn};

use super::{NotifyChannel, shared_http_client};
use crate::anomaly::Spike;
use crate::config::Config;
use crate::cost_explorer::SpendHistory;

pub struct Webhook;

impl NotifyChannel for Webhook {
    fn name(&self) -> &'static str {
        "webhook"
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
            let wcfg = cfg
                .webhook
                .as_ref()
                .context("Webhook channel active but config missing")?;
            let payload = SpikePayload {
                event_type: "spike_alert",
                setup_name: &cfg.setup_name,
                timestamp: Utc::now().to_rfc3339(),
                spikes: spikes
                    .iter()
                    .map(|s| SpikeEntry {
                        service: &s.service,
                        avg_daily: s.avg_daily,
                        today: s.today,
                        pct_increase: s.pct_increase,
                        extra_usd: s.extra_usd,
                    })
                    .collect(),
                total_extra_usd: spikes.iter().map(|s| s.extra_usd).sum(),
            };
            send_webhook(cfg, wcfg, &payload).await
        })
    }

    fn send_daily_digest<'a>(
        &'a self,
        cfg: &'a Config,
        spend_data: &'a SpendHistory,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(async move {
            let wcfg = cfg
                .webhook
                .as_ref()
                .context("Webhook channel active but config missing")?;
            let payload = DigestPayload {
                event_type: "daily_digest",
                setup_name: cfg.setup_name.clone(),
                timestamp: Utc::now().to_rfc3339(),
                services: spend_data
                    .iter()
                    .map(|(name, costs)| {
                        let total: f64 = costs.iter().sum();
                        let avg = if !costs.is_empty() {
                            total / costs.len() as f64
                        } else {
                            0.0
                        };
                        DigestEntry {
                            service: name.clone(),
                            avg_daily: avg,
                            total,
                        }
                    })
                    .collect(),
            };
            send_webhook(cfg, wcfg, &payload).await
        })
    }
}

// ── Payload types ───────────────────────────────────────────────────────────

#[derive(Serialize)]
struct SpikePayload<'a> {
    event_type: &'a str,
    setup_name: &'a str,
    timestamp: String,
    spikes: Vec<SpikeEntry<'a>>,
    total_extra_usd: f64,
}

#[derive(Serialize)]
struct SpikeEntry<'a> {
    service: &'a str,
    avg_daily: f64,
    today: f64,
    pct_increase: f64,
    extra_usd: f64,
}

#[derive(Serialize)]
struct DigestPayload {
    event_type: &'static str,
    setup_name: String,
    timestamp: String,
    services: Vec<DigestEntry>,
}

#[derive(Serialize)]
struct DigestEntry {
    service: String,
    avg_daily: f64,
    total: f64,
}

// ── HTTP send ───────────────────────────────────────────────────────────────

use crate::config::WebhookConfig;

async fn send_webhook<T: Serialize>(
    cfg: &Config,
    wcfg: &WebhookConfig,
    payload: &T,
) -> Result<bool> {
    // NOTE: webhook URL and auth_header are secrets — do not log them.
    let mut req = shared_http_client(cfg).post(&wcfg.url).json(payload);

    if let Some(ref auth) = wcfg.auth_header {
        req = req.header("Authorization", auth);
    }

    let resp = req.send().await.context("Failed to send webhook")?;

    if resp.status().is_success() {
        info!("Webhook delivered");
        Ok(true)
    } else {
        let status = resp.status();
        warn!(%status, "Webhook returned non-2xx");
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spike_payload_serializes() {
        let payload = SpikePayload {
            event_type: "spike_alert",
            setup_name: "StackAlert",
            timestamp: "2026-03-27T12:00:00Z".to_string(),
            spikes: vec![SpikeEntry {
                service: "Amazon EC2",
                avg_daily: 18.0,
                today: 75.0,
                pct_increase: 316.67,
                extra_usd: 57.0,
            }],
            total_extra_usd: 57.0,
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("spike_alert"));
        assert!(json.contains("Amazon EC2"));
        assert!(json.contains("316.67"));
    }

    #[test]
    fn test_digest_payload_serializes() {
        let payload = DigestPayload {
            event_type: "daily_digest",
            setup_name: "StackAlert".to_string(),
            timestamp: "2026-03-27T12:00:00Z".to_string(),
            services: vec![DigestEntry {
                service: "Amazon EC2".to_string(),
                avg_daily: 18.14,
                total: 127.0,
            }],
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("daily_digest"));
        assert!(json.contains("18.14"));
    }
}
