mod accounts;
mod anomaly;
mod config;
mod cost_explorer;
mod dedup;
mod telegram;

use anyhow::Result;
use lambda_runtime::{Error, LambdaEvent, run, service_fn, tracing};
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

async fn run_spike_check(cfg: &config::Config) -> Result<CheckResponse> {
    // Load the Lambda's own credentials once — reused by Cost Explorer (single-account
    // mode) and SSM (dedup state always lives in the Lambda's own account).
    let base_cfg = aws_config::load_from_env().await;

    // Cost Explorer may use cross-account credentials; build_aws_config handles that.
    // Fetch history_days + 1 so we have `history_days` full historical days plus today.
    let aws_cfg = cost_explorer::build_aws_config(cfg, &base_cfg).await?;
    let spend_data = cost_explorer::fetch_spend(&aws_cfg, cfg.history_days as i64 + 1).await?;
    let all_spikes = anomaly::detect_spikes(
        &spend_data,
        cfg.spike_threshold_pct,
        cfg.history_days,
        cfg.min_avg_daily_usd,
    );

    // Dedup uses the Lambda's own credentials — SSM state lives in the Lambda's account,
    // not in the customer account, so it works for both single and multi-account modes.
    let ssm = aws_sdk_ssm::Client::new(&base_cfg);

    // Filter out services that were already alerted within the cooldown window.
    // Each service has an independent cooldown — a new spike on Service B is never
    // suppressed by an ongoing alert on Service A.
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
        match telegram::send_spike_alert(
            &cfg.telegram_bot_token,
            &cfg.telegram_chat_id,
            &new_spikes,
        )
        .await
        {
            Ok(true) => {
                alerts_sent = 1;
                // Record the alert time so we don't re-alert within the cooldown window.
                dedup::mark_alerted(&ssm, &new_spikes, &cfg.account_namespace).await;
            }
            Ok(false) => tracing::warn!("Telegram message not sent (API returned non-2xx)"),
            Err(e) => tracing::warn!(error = %e, "Telegram send failed — continuing"),
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

async fn run_digest(cfg: &config::Config) -> Result<CheckResponse> {
    let base_cfg = aws_config::load_from_env().await;
    let aws_cfg = cost_explorer::build_aws_config(cfg, &base_cfg).await?;
    let spend_data = cost_explorer::fetch_spend(&aws_cfg, cfg.history_days as i64).await?;

    let alerts_sent = match telegram::send_daily_digest(
        &cfg.telegram_bot_token,
        &cfg.telegram_chat_id,
        &spend_data,
        cfg.min_avg_daily_usd,
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
