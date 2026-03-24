mod anomaly;
mod config;
mod cost_explorer;
mod telegram;

use anyhow::Result;
use lambda_runtime::{run, service_fn, tracing, Error, LambdaEvent};
use serde::{Deserialize, Serialize};
use tracing::info;

/// EventBridge sends a payload with the mode field, or empty for spike check
#[derive(Deserialize, Default)]
struct SchedulerEvent {
    /// "spike" (default) or "digest"
    #[serde(default)]
    mode: Option<String>,
}

/// Lambda response
#[derive(Serialize)]
struct Response {
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

async fn handler(event: LambdaEvent<SchedulerEvent>) -> Result<Response, Error> {
    let (payload, ctx) = event.into_parts();
    let mode = payload.mode.as_deref().unwrap_or("spike");
    info!(request_id = %ctx.request_id, %mode, "StackAlert check started");

    // Strict mode validation — reject unknown modes immediately
    match mode {
        "spike" | "digest" => {}
        unknown => {
            return Err(anyhow::anyhow!(
                "Unknown mode: '{}'. Expected 'spike' or 'digest'.",
                unknown
            )
            .into());
        }
    }

    let cfg = config::Config::load().await?;

    let result = match mode {
        "digest" => run_digest(&cfg).await?,
        _ => run_spike_check(&cfg).await?,
    };

    info!(
        mode = %result.mode,
        spikes = result.spikes_found,
        alerts = result.alerts_sent,
        "StackAlert check complete"
    );

    Ok(result)
}

async fn run_spike_check(cfg: &config::Config) -> Result<Response> {
    let aws_cfg = cost_explorer::build_aws_config(cfg).await?;
    let spend_data = cost_explorer::fetch_spend(&aws_cfg, 8).await?;
    let spikes = anomaly::detect_spikes(&spend_data, cfg.spike_threshold_pct);

    let mut alerts_sent = 0;
    if !spikes.is_empty() {
        // Graceful degradation: Telegram failure doesn't abort the cost check
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

    Ok(Response {
        mode: "spike".to_string(),
        spikes_found: spikes.len(),
        alerts_sent,
    })
}

async fn run_digest(cfg: &config::Config) -> Result<Response> {
    let aws_cfg = cost_explorer::build_aws_config(cfg).await?;
    let spend_data = cost_explorer::fetch_spend(&aws_cfg, 7).await?;

    // Graceful degradation: log Telegram failures but return success
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

    Ok(Response {
        mode: "digest".to_string(),
        spikes_found: 0,
        alerts_sent,
    })
}
