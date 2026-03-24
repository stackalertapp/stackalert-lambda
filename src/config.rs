use anyhow::{Context, Result};
use tracing::info;

/// Runtime configuration loaded from environment variables + SSM
pub struct Config {
    /// Telegram bot token (from SSM Parameter Store)
    pub telegram_bot_token: String,

    /// Telegram chat ID to send alerts to
    pub telegram_chat_id: String,

    /// Spike threshold: alert if today > avg * (1 + threshold/100)
    /// Default: 50 (= 50% above 7-day average triggers alert)
    pub spike_threshold_pct: f64,

    /// Optional: cross-account IAM role ARN to assume before querying Cost Explorer.
    /// If not set, uses the Lambda's own credentials (single-account mode).
    pub cross_account_role_arn: Option<String>,
}

impl Config {
    /// Load config from environment variables and SSM Parameter Store.
    ///
    /// Required env vars:
    /// - `TELEGRAM_BOT_TOKEN_SSM_PARAM`: SSM parameter name for the Telegram bot token
    /// - `TELEGRAM_CHAT_ID`: Telegram chat ID to send alerts to
    ///
    /// Optional env vars:
    /// - `SPIKE_THRESHOLD_PCT`: Spike threshold percentage (default: 50)
    /// - `CROSS_ACCOUNT_ROLE_ARN`: IAM role ARN for cross-account access
    pub async fn load() -> Result<Self> {
        let ssm_param = std::env::var("TELEGRAM_BOT_TOKEN_SSM_PARAM")
            .context("TELEGRAM_BOT_TOKEN_SSM_PARAM env var not set")?;

        let telegram_chat_id =
            std::env::var("TELEGRAM_CHAT_ID").context("TELEGRAM_CHAT_ID env var not set")?;

        let spike_threshold_pct = std::env::var("SPIKE_THRESHOLD_PCT")
            .unwrap_or_else(|_| "50".to_string())
            .parse::<f64>()
            .context("SPIKE_THRESHOLD_PCT must be a number")?;

        let cross_account_role_arn = std::env::var("CROSS_ACCOUNT_ROLE_ARN").ok();

        // Fetch Telegram bot token from SSM Parameter Store
        let aws_cfg = aws_config::load_from_env().await;
        let ssm = aws_sdk_ssm::Client::new(&aws_cfg);

        let resp = ssm
            .get_parameter()
            .name(&ssm_param)
            .with_decryption(true)
            .send()
            .await
            .with_context(|| format!("Failed to fetch SSM parameter: {ssm_param}"))?;

        let telegram_bot_token = resp
            .parameter
            .and_then(|p| p.value)
            .context("SSM parameter has no value")?;

        info!(
            ssm_param = %ssm_param,
            cross_account = cross_account_role_arn.is_some(),
            threshold = spike_threshold_pct,
            "Config loaded"
        );

        Ok(Config {
            telegram_bot_token,
            telegram_chat_id,
            spike_threshold_pct,
            cross_account_role_arn,
        })
    }
}
