mod anomaly;
mod config;
mod cost_explorer;
mod dedup;
mod notify;

use std::sync::LazyLock;

use anyhow::Result;
use aws_config::SdkConfig;
use lambda_runtime::{Error, LambdaEvent, run, service_fn, tracing};
use serde::{Deserialize, Serialize};
use tracing::info;

use notify::NotifyChannel;

/// Cached AWS SDK config — loaded once on cold start, reused across warm invocations.
/// Avoids redundant IMDS metadata calls (~5-20ms each) on every handler invocation.
static BASE_CONFIG: LazyLock<tokio::sync::OnceCell<SdkConfig>> =
    LazyLock::new(tokio::sync::OnceCell::new);

async fn base_config() -> &'static SdkConfig {
    BASE_CONFIG.get_or_init(aws_config::load_from_env).await
}

/// Event payload from EventBridge or Step Functions.
///
/// All fields except `mode` are optional overrides. When absent, env vars are used.
/// This allows the dashboard to pass per-account config via Step Functions while
/// self-hosted deployments rely entirely on env vars.
#[derive(Deserialize, Default)]
struct SchedulerEvent {
    /// "spike" (default) or "digest"
    #[serde(default)]
    mode: Option<String>,

    // ── Cross-account fields ───────────────────────────────────────────
    /// IAM role ARN to assume for cross-account Cost Explorer access.
    role_arn: Option<String>,
    /// ExternalId for confused-deputy protection.
    external_id: Option<String>,
    /// Target AWS account ID — used as dedup namespace.
    account_id: Option<String>,

    // ── Config overrides (take precedence over env vars) ───────────────
    spike_threshold_pct: Option<f64>,
    notify_channels: Option<String>,
    telegram_chat_id: Option<String>,
    /// Human-readable name shown in alert messages.
    setup_name: Option<String>,
}

impl From<SchedulerEvent> for config::EventOverrides {
    fn from(e: SchedulerEvent) -> Self {
        Self {
            role_arn: e.role_arn,
            external_id: e.external_id,
            account_id: e.account_id,
            spike_threshold_pct: e.spike_threshold_pct,
            notify_channels: e.notify_channels,
            telegram_chat_id: e.telegram_chat_id,
            setup_name: e.setup_name,
        }
    }
}

#[derive(Serialize)]
struct CheckResponse {
    mode: String,
    spikes_found: usize,
    alerts_sent: usize,
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("stackalert_lambda=info".parse().unwrap())
                .add_directive("aws_sdk=warn".parse().unwrap()),
        )
        .json()
        .without_time()
        .init();

    run(service_fn(handler)).await
}

async fn handler(event: LambdaEvent<SchedulerEvent>) -> Result<CheckResponse, Error> {
    let (payload, ctx) = event.into_parts();
    let mode = payload.mode.as_deref().unwrap_or("spike").to_string();
    info!(request_id = %ctx.request_id, %mode, "StackAlert invocation started");

    let base_cfg = base_config().await;
    let overrides: config::EventOverrides = payload.into();
    let cfg = config::Config::load(base_cfg, &overrides).await?;

    let channels = notify::build_channels(&cfg, base_cfg);

    let result = match mode.as_str() {
        "digest" => run_digest(&cfg, base_cfg, &channels).await?,
        _ => run_spike_check(&cfg, base_cfg, &channels).await?,
    };

    info!(
        mode = %result.mode,
        spikes = result.spikes_found,
        alerts = result.alerts_sent,
        "StackAlert check complete"
    );

    Ok(result)
}

async fn run_spike_check(
    cfg: &config::Config,
    base_cfg: &SdkConfig,
    channels: &[Box<dyn NotifyChannel>],
) -> Result<CheckResponse> {
    let aws_cfg = cost_explorer::build_aws_config(cfg, base_cfg).await?;
    let spend_data = cost_explorer::fetch_spend(&aws_cfg, cfg.history_days as i64 + 1).await?;
    let all_spikes = anomaly::detect_spikes(
        &spend_data,
        cfg.spike_threshold_pct,
        cfg.history_days,
        cfg.min_avg_daily_usd,
    );

    let ssm = aws_sdk_ssm::Client::new(base_cfg);

    let spikes_found = all_spikes.len();
    let new_spikes = dedup::filter_new_spikes(
        &ssm,
        all_spikes,
        &cfg.account_namespace,
        cfg.dedup_cooldown_hours,
    )
    .await;

    info!(
        spikes_found,
        new_after_dedup = new_spikes.len(),
        namespace = %cfg.account_namespace,
        "Dedup complete"
    );

    let mut alerts_sent = 0;
    if !new_spikes.is_empty() {
        let results = notify::fan_out_spike_alert(channels, cfg, &new_spikes).await;
        alerts_sent = results.iter().filter(|r| r.sent).count();

        if alerts_sent > 0 {
            dedup::mark_alerted(&ssm, &new_spikes, &cfg.account_namespace).await;
        }

        for r in &results {
            if let Some(ref e) = r.error {
                tracing::warn!(channel = r.channel, error = %e, "Channel failed");
            }
        }
    } else if spikes_found > 0 {
        info!(
            spikes_found,
            "All spikes suppressed by dedup — no alert sent"
        );
    } else {
        info!("No spikes detected");
    }

    Ok(CheckResponse {
        mode: "spike".to_string(),
        spikes_found,
        alerts_sent,
    })
}

async fn run_digest(
    cfg: &config::Config,
    base_cfg: &SdkConfig,
    channels: &[Box<dyn NotifyChannel>],
) -> Result<CheckResponse> {
    let aws_cfg = cost_explorer::build_aws_config(cfg, base_cfg).await?;
    let spend_data = cost_explorer::fetch_spend(&aws_cfg, cfg.history_days as i64).await?;

    let results = notify::fan_out_daily_digest(channels, cfg, &spend_data).await;
    let alerts_sent = results.iter().filter(|r| r.sent).count();

    for r in &results {
        if let Some(ref e) = r.error {
            tracing::warn!(channel = r.channel, error = %e, "Channel digest failed");
        }
    }

    Ok(CheckResponse {
        mode: "digest".to_string(),
        spikes_found: 0,
        alerts_sent,
    })
}
