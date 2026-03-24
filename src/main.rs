mod accounts;
mod anomaly;
mod config;
mod cost_explorer;
mod telegram;

use anyhow::Result;
use lambda_runtime::{run, service_fn, tracing, Error, LambdaEvent};
use serde::{Deserialize, Serialize};
use tracing::info;

use accounts::AccountContext;

/// Event payload from EventBridge (single-account) or Step Functions (per-account).
#[derive(Deserialize, Default)]
struct SchedulerEvent {
    /// "spike" (default) or "digest"
    #[serde(default)]
    mode: Option<String>,

    /// Populated by the dashboard's Step Functions Map state for multi-account mode.
    /// Absent in single-account / open-source mode — config is read from env vars instead.
    account: Option<AccountContext>,
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
    let mode = payload.mode.as_deref().unwrap_or("spike");
    info!(request_id = %ctx.request_id, %mode, "StackAlert invocation started");

    let cfg = match &payload.account {
        // Multi-account: context injected by the dashboard's Step Functions
        Some(account_ctx) => config::Config::from_account_context(account_ctx).await?,
        // Single-account: read from env vars (open-source / self-hosted)
        None => config::Config::load().await?,
    };

    let result = match mode {
        "digest" => run_digest(&cfg).await?,
        "spike" | _ => run_spike_check(&cfg).await?,
    };

    info!(
        mode = %result.mode,
        spikes = result.spikes_found,
        alerts = result.alerts_sent,
        "StackAlert check complete"
    );

    Ok(result)
}

async fn run_spike_check(cfg: &config::Config) -> Result<CheckResponse> {
    let aws_cfg = cost_explorer::build_aws_config(cfg).await?;
    let spend_data = cost_explorer::fetch_spend(&aws_cfg, 8).await?;
    let spikes = anomaly::detect_spikes(&spend_data, cfg.spike_threshold_pct);

    let mut alerts_sent = 0;
    if !spikes.is_empty() {
        match telegram::send_spike_alert(&cfg.telegram_bot_token, &cfg.telegram_chat_id, &spikes)
            .await
        {
            Ok(true) => alerts_sent = 1,
            Ok(false) => tracing::warn!("Telegram message not sent (API returned non-2xx)"),
            Err(e) => tracing::warn!(error = %e, "Telegram send failed — continuing"),
        }
    } else {
        info!("No spikes detected");
    }

    Ok(CheckResponse {
        mode: "spike".to_string(),
        spikes_found: spikes.len(),
        alerts_sent,
    })
}

async fn run_digest(cfg: &config::Config) -> Result<CheckResponse> {
    let aws_cfg = cost_explorer::build_aws_config(cfg).await?;
    let spend_data = cost_explorer::fetch_spend(&aws_cfg, 7).await?;

    let alerts_sent = match telegram::send_daily_digest(
        &cfg.telegram_bot_token,
        &cfg.telegram_chat_id,
        &spend_data,
    )
    .await
    {
        Ok(sent) => sent as usize,
        Err(e) => {
            tracing::warn!(error = %e, "Telegram digest send failed — continuing");
            0
        }
    };

    Ok(CheckResponse {
        mode: "digest".to_string(),
        spikes_found: 0,
        alerts_sent,
    })
}
