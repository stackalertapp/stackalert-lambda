use anyhow::{Context, Result};
use tracing::info;

use crate::accounts::AccountContext;

/// Runtime configuration for one cost-check invocation.
pub struct Config {
    /// Telegram bot token (from SSM Parameter Store — shared across all accounts)
    pub telegram_bot_token: String,

    /// Telegram chat ID to send alerts to (per-account in multi-account mode)
    pub telegram_chat_id: String,

    /// Spike threshold: alert if today > avg * (1 + threshold/100)
    pub spike_threshold_pct: f64,

    /// IAM role ARN to assume for cross-account Cost Explorer access.
    /// None = single-account mode (Lambda's own credentials).
    pub cross_account_role_arn: Option<String>,

    /// ExternalId for confused-deputy protection when assuming the cross-account role.
    pub external_id: Option<String>,

    /// Namespace for SSM-based alert deduplication keys.
    ///
    /// - Single-account (open-source / self-hosted): `"self"`
    /// - Multi-account (SaaS): the customer's AWS account ID (e.g. `"700483457242"`)
    ///
    /// This isolates dedup state so the same service spiking in two different
    /// customer accounts results in two independent alerts.
    pub account_namespace: String,
}

impl Config {
    // ── Single-account mode (open-source / self-hosted) ─────────────────────

    pub async fn load() -> Result<Self> {
        let telegram_chat_id =
            std::env::var("TELEGRAM_CHAT_ID").context("TELEGRAM_CHAT_ID env var not set")?;

        let spike_threshold_pct = std::env::var("SPIKE_THRESHOLD_PCT")
            .unwrap_or_else(|_| "50".to_string())
            .parse::<f64>()
            .context("SPIKE_THRESHOLD_PCT must be a number")?;

        let cross_account_role_arn = std::env::var("CROSS_ACCOUNT_ROLE_ARN").ok();
        let external_id = std::env::var("EXTERNAL_ID").ok();

        let telegram_bot_token = Self::load_bot_token().await?;

        info!(
            cross_account = cross_account_role_arn.is_some(),
            threshold = spike_threshold_pct,
            "Config loaded (single-account mode)"
        );

        Ok(Config {
            telegram_bot_token,
            telegram_chat_id,
            spike_threshold_pct,
            cross_account_role_arn,
            external_id,
            account_namespace: "self".to_string(),
        })
    }

    // ── Multi-account mode (StackAlert SaaS / Step Functions) ───────────────

    pub async fn from_account_context(ctx: &AccountContext) -> Result<Self> {
        let telegram_bot_token = Self::load_bot_token().await?;

        let telegram_chat_id = ctx
            .telegram_chat_id
            .clone()
            .or_else(|| std::env::var("TELEGRAM_CHAT_ID").ok())
            .unwrap_or_default();

        info!(
            account_id = %ctx.aws_account_id,
            role_arn   = %ctx.role_arn,
            threshold  = ctx.spike_threshold,
            "Config loaded (multi-account context)"
        );

        Ok(Config {
            telegram_bot_token,
            telegram_chat_id,
            spike_threshold_pct: ctx.spike_threshold,
            cross_account_role_arn: Some(ctx.role_arn.clone()),
            external_id: Some(ctx.external_id.clone()),
            account_namespace: ctx.aws_account_id.clone(),
        })
    }

    // ── Shared helper ────────────────────────────────────────────────────────

    async fn load_bot_token() -> Result<String> {
        let ssm_param = std::env::var("TELEGRAM_BOT_TOKEN_SSM_PARAM")
            .context("TELEGRAM_BOT_TOKEN_SSM_PARAM env var not set")?;

        let aws_cfg = aws_config::load_from_env().await;
        let ssm = aws_sdk_ssm::Client::new(&aws_cfg);

        let resp = ssm
            .get_parameter()
            .name(&ssm_param)
            .with_decryption(true)
            .send()
            .await
            .with_context(|| format!("Failed to fetch SSM parameter: {ssm_param}"))?;

        resp.parameter
            .and_then(|p| p.value)
            .context("SSM parameter has no value")
    }
}
