//! Pluggable notification system.
//!
//! Channels are selected via the `NOTIFY_CHANNELS` env var (comma-separated).
//! Each channel implements [`NotifyChannel`] and is dispatched concurrently.

#[cfg(feature = "telegram")]
pub mod telegram;

#[cfg(feature = "slack")]
pub mod slack;

#[cfg(feature = "teams")]
pub mod teams;

#[cfg(feature = "pagerduty")]
pub mod pagerduty;

#[cfg(feature = "webhook")]
pub mod webhook;

#[cfg(feature = "ses")]
pub mod ses;

#[cfg(feature = "sns")]
pub mod sns;

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::Result;
use reqwest::Client;

use crate::anomaly::Spike;
use crate::config::Config;
use crate::cost_explorer::SpendHistory;

// ── Shared formatting helpers ────────────────────────────────────────────────

pub(crate) fn fmt_pct(pct: f64) -> String {
    if pct.is_infinite() {
        "NEW".to_string()
    } else {
        format!("+{:.0}%", pct)
    }
}

pub(crate) fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Escape Slack mrkdwn special characters so user-supplied text
/// (e.g. `setup_name`) cannot break formatting.
#[cfg(feature = "slack")]
pub(crate) fn escape_mrkdwn(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '*' | '_' | '~' | '`' | '>' => {
                // Zero-width space breaks mrkdwn parsing
                out.push('\u{200B}');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Compute sorted (descending by avg) services above the noise floor, plus grand total.
pub(crate) fn ranked_services(
    spend_data: &SpendHistory,
    min_avg_daily: f64,
) -> (Vec<(String, f64)>, f64) {
    let mut services: Vec<(String, f64)> = spend_data
        .iter()
        .map(|(service, costs)| {
            let total: f64 = costs.iter().sum();
            let avg = if !costs.is_empty() {
                total / costs.len() as f64
            } else {
                0.0
            };
            (service.clone(), avg)
        })
        .filter(|(_, avg)| *avg >= min_avg_daily)
        .collect();
    services.sort_by(|a, b| b.1.total_cmp(&a.1));
    let grand_total = services.iter().map(|(_, avg)| avg).sum();
    (services, grand_total)
}

// ── Trait ────────────────────────────────────────────────────────────────────

/// A notification channel that can send spike alerts and daily digests.
pub trait NotifyChannel: Send + Sync {
    /// Human-readable channel name for logging.
    fn name(&self) -> &'static str;

    /// Send a cost-spike alert. Returns `Ok(true)` if the remote API accepted it.
    fn send_spike_alert<'a>(
        &'a self,
        cfg: &'a Config,
        spikes: &'a [Spike],
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>>;

    /// Send a daily cost digest. Returns `Ok(true)` if accepted.
    fn send_daily_digest<'a>(
        &'a self,
        cfg: &'a Config,
        spend_data: &'a SpendHistory,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>>;
}

// ── Channel result ──────────────────────────────────────────────────────────

/// Outcome of a single channel's send attempt.
#[derive(Debug)]
pub struct ChannelResult {
    pub channel: &'static str,
    pub sent: bool,
    pub error: Option<anyhow::Error>,
}

// ── Shared HTTP client ──────────────────────────────────────────────────────

/// Shared HTTP client for all webhook-based channels.
/// Initialised once on first use, reused across warm invocations.
/// NOTE: timeouts are set once from env vars, so they're identical across invocations.
static SHARED_HTTP_CLIENT: OnceLock<Client> = OnceLock::new();

pub fn shared_http_client(cfg: &Config) -> &'static Client {
    SHARED_HTTP_CLIENT.get_or_init(|| {
        Client::builder()
            .timeout(Duration::from_secs(cfg.http_timeout_secs))
            .connect_timeout(Duration::from_secs(cfg.http_connect_timeout_secs))
            .build()
            .expect("Failed to build shared HTTP client")
    })
}

// ── Channel construction ────────────────────────────────────────────────────

/// Build the set of active notification channels from config.
#[allow(unused_variables)] // base_cfg used only when ses/sns features are enabled
pub fn build_channels(
    cfg: &Config,
    base_cfg: &aws_config::SdkConfig,
) -> Vec<Box<dyn NotifyChannel>> {
    let mut channels: Vec<Box<dyn NotifyChannel>> = Vec::new();

    for name in &cfg.notify_channels {
        match name.as_str() {
            #[cfg(feature = "telegram")]
            "telegram" => channels.push(Box::new(telegram::Telegram)),
            #[cfg(not(feature = "telegram"))]
            "telegram" => tracing::error!("'telegram' requested but compiled without the feature"),

            #[cfg(feature = "slack")]
            "slack" => channels.push(Box::new(slack::Slack)),
            #[cfg(not(feature = "slack"))]
            "slack" => tracing::error!("'slack' requested but compiled without the feature"),

            #[cfg(feature = "teams")]
            "teams" => channels.push(Box::new(teams::Teams)),
            #[cfg(not(feature = "teams"))]
            "teams" => tracing::error!("'teams' requested but compiled without the feature"),

            #[cfg(feature = "pagerduty")]
            "pagerduty" => channels.push(Box::new(pagerduty::PagerDuty)),
            #[cfg(not(feature = "pagerduty"))]
            "pagerduty" => tracing::error!("'pagerduty' requested but compiled without the feature"),

            #[cfg(feature = "webhook")]
            "webhook" => channels.push(Box::new(webhook::Webhook)),
            #[cfg(not(feature = "webhook"))]
            "webhook" => tracing::error!("'webhook' requested but compiled without the feature"),

            #[cfg(feature = "ses")]
            "ses" => channels.push(Box::new(ses::Ses::new(base_cfg))),
            #[cfg(not(feature = "ses"))]
            "ses" => tracing::error!("'ses' requested but compiled without the feature"),

            #[cfg(feature = "sns")]
            "sns" => channels.push(Box::new(sns::Sns::new(base_cfg))),
            #[cfg(not(feature = "sns"))]
            "sns" => tracing::error!("'sns' requested but compiled without the feature"),

            other => tracing::warn!(channel = other, "Unknown notification channel — skipping"),
        }
    }

    if channels.is_empty() {
        tracing::warn!("No notification channels configured");
    }

    channels
}

// ── Fan-out dispatcher ──────────────────────────────────────────────────────

/// Send a spike alert to all channels concurrently. Never short-circuits on failure.
pub async fn fan_out_spike_alert(
    channels: &[Box<dyn NotifyChannel>],
    cfg: &Config,
    spikes: &[Spike],
) -> Vec<ChannelResult> {
    let futures: Vec<_> = channels
        .iter()
        .map(|ch| {
            let name = ch.name();
            async move {
                match ch.send_spike_alert(cfg, spikes).await {
                    Ok(sent) => ChannelResult {
                        channel: name,
                        sent,
                        error: None,
                    },
                    Err(e) => {
                        tracing::warn!(channel = name, error = %e, "Channel send failed");
                        ChannelResult {
                            channel: name,
                            sent: false,
                            error: Some(e),
                        }
                    }
                }
            }
        })
        .collect();

    futures_util::future::join_all(futures).await
}

/// Send a daily digest to all channels concurrently.
pub async fn fan_out_daily_digest(
    channels: &[Box<dyn NotifyChannel>],
    cfg: &Config,
    spend_data: &SpendHistory,
) -> Vec<ChannelResult> {
    let futures: Vec<_> = channels
        .iter()
        .map(|ch| {
            let name = ch.name();
            async move {
                match ch.send_daily_digest(cfg, spend_data).await {
                    Ok(sent) => ChannelResult {
                        channel: name,
                        sent,
                        error: None,
                    },
                    Err(e) => {
                        tracing::warn!(channel = name, error = %e, "Channel digest failed");
                        ChannelResult {
                            channel: name,
                            sent: false,
                            error: Some(e),
                        }
                    }
                }
            }
        })
        .collect();

    futures_util::future::join_all(futures).await
}
