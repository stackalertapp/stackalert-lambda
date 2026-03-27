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

    /// Number of historical days used to compute the baseline average.
    /// Env: `HISTORY_DAYS` (default: 7)
    pub history_days: u32,

    /// Services whose 7-day average daily cost is below this value are ignored (noise filter).
    /// Env: `MIN_AVG_DAILY_USD` (default: 0.10)
    pub min_avg_daily_usd: f64,

    /// How many hours to suppress repeat alerts for the same service after one was sent.
    /// Should match (or be a multiple of) the EventBridge schedule interval.
    /// Env: `DEDUP_COOLDOWN_HOURS` (default: 6)
    pub dedup_cooldown_hours: u32,
}

impl Config {
    // ── Single-account mode (open-source / self-hosted) ─────────────────────

    pub async fn load(base_cfg: &aws_config::SdkConfig) -> Result<Self> {
        let telegram_chat_id =
            std::env::var("TELEGRAM_CHAT_ID").context("TELEGRAM_CHAT_ID env var not set")?;

        let spike_threshold_pct = std::env::var("SPIKE_THRESHOLD_PCT")
            .unwrap_or_else(|_| "50".to_string())
            .parse::<f64>()
            .context("SPIKE_THRESHOLD_PCT must be a number")?;

        let cross_account_role_arn = std::env::var("CROSS_ACCOUNT_ROLE_ARN").ok();
        let external_id = std::env::var("EXTERNAL_ID").ok();

        let telegram_bot_token = Self::load_bot_token(base_cfg).await?;

        info!(
            cross_account = cross_account_role_arn.is_some(),
            threshold = spike_threshold_pct,
            "Config loaded (single-account mode)"
        );

        let cfg = Config {
            telegram_bot_token,
            telegram_chat_id,
            spike_threshold_pct,
            cross_account_role_arn,
            external_id,
            account_namespace: "self".to_string(),
            history_days: Self::parse_env_u32("HISTORY_DAYS", 7)?,
            min_avg_daily_usd: Self::parse_env_f64("MIN_AVG_DAILY_USD", 0.10)?,
            dedup_cooldown_hours: Self::parse_env_u32("DEDUP_COOLDOWN_HOURS", 6)?,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    // ── Multi-account mode (StackAlert SaaS / Step Functions) ───────────────

    pub async fn from_account_context(ctx: &AccountContext, base_cfg: &aws_config::SdkConfig) -> Result<Self> {
        let telegram_bot_token = Self::load_bot_token(base_cfg).await?;

        let telegram_chat_id = ctx
            .telegram_chat_id
            .clone()
            .or_else(|| std::env::var("TELEGRAM_CHAT_ID").ok())
            .filter(|s| !s.is_empty())
            .context("telegram_chat_id missing from both AccountContext and TELEGRAM_CHAT_ID env var")?;

        info!(
            account_id = %ctx.aws_account_id,
            role_arn   = %ctx.role_arn,
            threshold  = ctx.spike_threshold,
            "Config loaded (multi-account context)"
        );

        let cfg = Config {
            telegram_bot_token,
            telegram_chat_id,
            spike_threshold_pct: ctx.spike_threshold,
            cross_account_role_arn: Some(ctx.role_arn.clone()),
            external_id: Some(ctx.external_id.clone()),
            account_namespace: ctx.aws_account_id.clone(),
            // Multi-account mode reuses the same env var defaults.
            // Individual account overrides can be added to AccountContext later.
            history_days: Self::parse_env_u32("HISTORY_DAYS", 7)?,
            min_avg_daily_usd: Self::parse_env_f64("MIN_AVG_DAILY_USD", 0.10)?,
            dedup_cooldown_hours: Self::parse_env_u32("DEDUP_COOLDOWN_HOURS", 6)?,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    // ── Shared helpers ───────────────────────────────────────────────────────

    fn validate(&self) -> Result<()> {
        if self.history_days == 0 {
            return Err(anyhow::anyhow!("HISTORY_DAYS must be > 0 (got 0)"));
        }
        if self.spike_threshold_pct <= 0.0 {
            return Err(anyhow::anyhow!(
                "SPIKE_THRESHOLD_PCT must be > 0 (got {})",
                self.spike_threshold_pct
            ));
        }
        if self.min_avg_daily_usd < 0.0 {
            return Err(anyhow::anyhow!(
                "MIN_AVG_DAILY_USD must be >= 0 (got {})",
                self.min_avg_daily_usd
            ));
        }
        if self.dedup_cooldown_hours == 0 {
            return Err(anyhow::anyhow!("DEDUP_COOLDOWN_HOURS must be > 0 (got 0)"));
        }
        Ok(())
    }

    fn parse_env_u32(key: &str, default: u32) -> Result<u32> {
        match std::env::var(key) {
            Ok(val) => val
                .parse::<u32>()
                .with_context(|| format!("{key} must be a positive integer, got {val:?}")),
            Err(_) => Ok(default),
        }
    }

    fn parse_env_f64(key: &str, default: f64) -> Result<f64> {
        match std::env::var(key) {
            Ok(val) => val
                .parse::<f64>()
                .with_context(|| format!("{key} must be a number, got {val:?}")),
            Err(_) => Ok(default),
        }
    }

    async fn load_bot_token(base_cfg: &aws_config::SdkConfig) -> Result<String> {
        let ssm_param = std::env::var("TELEGRAM_BOT_TOKEN_SSM_PARAM")
            .context("TELEGRAM_BOT_TOKEN_SSM_PARAM env var not set")?;

        let ssm = aws_sdk_ssm::Client::new(base_cfg);

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
