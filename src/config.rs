use anyhow::{Context, Result};
use tracing::info;

// ── Per-channel config structs ──────────────────────────────────────────────

#[cfg(feature = "telegram")]
pub struct TelegramConfig {
    pub bot_token: String,
    pub chat_id: String,
}

#[cfg(feature = "slack")]
pub struct SlackConfig {
    pub webhook_url: String,
}

#[cfg(feature = "teams")]
pub struct TeamsConfig {
    pub webhook_url: String,
}

#[cfg(feature = "pagerduty")]
pub struct PagerDutyConfig {
    pub routing_key: String,
    pub severity: String,
}

#[cfg(feature = "ses")]
pub struct SesConfig {
    pub from_address: String,
    pub to_addresses: Vec<String>,
}

#[cfg(feature = "webhook")]
pub struct WebhookConfig {
    pub url: String,
    pub auth_header: Option<String>,
}

#[cfg(feature = "sns")]
pub struct SnsConfig {
    pub topic_arn: String,
}

// ── Main config ─────────────────────────────────────────────────────────────

/// Runtime configuration for one cost-check invocation.
pub struct Config {
    /// Spike threshold: alert if today > avg * (1 + threshold/100)
    pub spike_threshold_pct: f64,

    /// IAM role ARN to assume for cross-account Cost Explorer access.
    /// None = use Lambda's own credentials.
    /// Event field `role_arn` → env `CROSS_ACCOUNT_ROLE_ARN`.
    pub cross_account_role_arn: Option<String>,

    /// ExternalId for confused-deputy protection when assuming the cross-account role.
    /// Event field `external_id` → env `EXTERNAL_ID`.
    #[cfg_attr(not(feature = "cross-account"), allow(dead_code))]
    pub external_id: Option<String>,

    /// Namespace for SSM-based alert deduplication keys.
    /// Event field `account_id` → default `"self"`.
    pub account_namespace: String,

    /// Human-readable name for this setup, shown in alert messages.
    /// Event field `setup_name` → env `SETUP_NAME` → default `"StackAlert"`.
    pub setup_name: String,

    /// Number of historical days used to compute the baseline average.
    /// Env: `HISTORY_DAYS` (default: 7)
    pub history_days: u32,

    /// Services whose average daily cost is below this value are ignored (noise filter).
    /// Env: `MIN_AVG_DAILY_USD` (default: 0.10)
    pub min_avg_daily_usd: f64,

    /// How many hours to suppress repeat alerts for the same service after one was sent.
    /// Env: `DEDUP_COOLDOWN_HOURS` (default: 6)
    pub dedup_cooldown_hours: u32,

    /// Max services shown in spike alert messages.
    /// Env: `MAX_SPIKE_DISPLAY` (default: 5)
    pub max_spike_display: usize,

    /// Max services shown in daily digest messages.
    /// Env: `MAX_DIGEST_DISPLAY` (default: 10)
    pub max_digest_display: usize,

    /// HTTP request timeout for all webhook-based channels (seconds).
    /// Env: `HTTP_TIMEOUT_SECS` (fallback: `TELEGRAM_TIMEOUT_SECS`, default: 10)
    pub http_timeout_secs: u64,

    /// HTTP connect timeout for all webhook-based channels (seconds).
    /// Env: `HTTP_CONNECT_TIMEOUT_SECS` (fallback: `TELEGRAM_CONNECT_TIMEOUT_SECS`, default: 5)
    pub http_connect_timeout_secs: u64,

    /// Active notification channels.
    /// Env: `NOTIFY_CHANNELS` (comma-separated, default: "telegram")
    pub notify_channels: Vec<String>,

    // ── Per-channel config (loaded only for active channels) ────────────
    #[cfg(feature = "telegram")]
    pub telegram: Option<TelegramConfig>,

    #[cfg(feature = "slack")]
    pub slack: Option<SlackConfig>,

    #[cfg(feature = "teams")]
    pub teams: Option<TeamsConfig>,

    #[cfg(feature = "pagerduty")]
    pub pagerduty: Option<PagerDutyConfig>,

    #[cfg(feature = "ses")]
    pub ses: Option<SesConfig>,

    #[cfg(feature = "webhook")]
    pub webhook: Option<WebhookConfig>,

    #[cfg(feature = "sns")]
    pub sns: Option<SnsConfig>,
}

/// Optional per-invocation overrides from the event payload.
/// Event values take precedence over env vars.
#[derive(Default)]
pub struct EventOverrides {
    pub role_arn: Option<String>,
    pub external_id: Option<String>,
    pub account_id: Option<String>,
    pub spike_threshold_pct: Option<f64>,
    pub notify_channels: Option<String>,
    pub telegram_chat_id: Option<String>,
    pub setup_name: Option<String>,
}

impl Config {
    /// Load configuration from env vars, with optional event-level overrides.
    ///
    /// **Self-hosted**: call with `EventOverrides::default()` — all config from env vars.
    /// **Dashboard SaaS**: Step Functions passes per-account overrides in the event payload.
    pub async fn load(
        base_cfg: &aws_config::SdkConfig,
        overrides: &EventOverrides,
    ) -> Result<Self> {
        let spike_threshold_pct = overrides.spike_threshold_pct.map_or_else(
            || {
                std::env::var("SPIKE_THRESHOLD_PCT")
                    .unwrap_or_else(|_| "50".to_string())
                    .parse::<f64>()
                    .context("SPIKE_THRESHOLD_PCT must be a number")
            },
            Ok,
        )?;

        let cross_account_role_arn = overrides
            .role_arn
            .clone()
            .or_else(|| std::env::var("CROSS_ACCOUNT_ROLE_ARN").ok());
        let external_id = overrides
            .external_id
            .clone()
            .or_else(|| std::env::var("EXTERNAL_ID").ok());
        let account_namespace = overrides
            .account_id
            .clone()
            .unwrap_or_else(|| "self".to_string());
        let setup_name = overrides
            .setup_name
            .clone()
            .or_else(|| std::env::var("SETUP_NAME").ok())
            .unwrap_or_else(|| "StackAlert".to_string());

        let notify_channels = overrides
            .notify_channels
            .as_deref()
            .map(|s| {
                s.split(',')
                    .map(|c| c.trim().to_string())
                    .filter(|c| !c.is_empty())
                    .collect()
            })
            .unwrap_or_else(Self::parse_notify_channels);

        let telegram_chat_id_override = overrides.telegram_chat_id.as_deref();

        info!(
            cross_account = cross_account_role_arn.is_some(),
            %account_namespace,
            %setup_name,
            threshold = spike_threshold_pct,
            channels = ?notify_channels,
            "Config loaded"
        );

        let ssm = aws_sdk_ssm::Client::new(base_cfg);

        let cfg = Config {
            spike_threshold_pct,
            cross_account_role_arn,
            external_id,
            account_namespace,
            setup_name,
            history_days: Self::parse_env_u32("HISTORY_DAYS", 7)?,
            min_avg_daily_usd: Self::parse_env_f64("MIN_AVG_DAILY_USD", 0.10)?,
            dedup_cooldown_hours: Self::parse_env_u32("DEDUP_COOLDOWN_HOURS", 6)?,
            max_spike_display: Self::parse_env_usize("MAX_SPIKE_DISPLAY", 5)?,
            max_digest_display: Self::parse_env_usize("MAX_DIGEST_DISPLAY", 10)?,
            http_timeout_secs: Self::parse_http_timeout()?,
            http_connect_timeout_secs: Self::parse_http_connect_timeout()?,

            #[cfg(feature = "telegram")]
            telegram: Self::load_telegram_config(&ssm, &notify_channels, telegram_chat_id_override)
                .await?,

            #[cfg(feature = "slack")]
            slack: Self::load_slack_config(&ssm, &notify_channels).await?,

            #[cfg(feature = "teams")]
            teams: Self::load_teams_config(&ssm, &notify_channels).await?,

            #[cfg(feature = "pagerduty")]
            pagerduty: Self::load_pagerduty_config(&ssm, &notify_channels).await?,

            #[cfg(feature = "ses")]
            ses: Self::load_ses_config(&notify_channels)?,

            #[cfg(feature = "webhook")]
            webhook: Self::load_webhook_config(&ssm, &notify_channels).await?,

            #[cfg(feature = "sns")]
            sns: Self::load_sns_config(&notify_channels)?,

            notify_channels,
        };
        cfg.validate()?;
        info!(
            history_days = cfg.history_days,
            min_avg_daily_usd = cfg.min_avg_daily_usd,
            dedup_cooldown_hours = cfg.dedup_cooldown_hours,
            http_timeout_secs = cfg.http_timeout_secs,
            max_spike_display = cfg.max_spike_display,
            max_digest_display = cfg.max_digest_display,
            "Config validated"
        );
        Ok(cfg)
    }

    // ── Validation ──────────────────────────────────────────────────────

    fn validate(&self) -> Result<()> {
        const KNOWN_CHANNELS: &[&str] = &[
            "telegram",
            "slack",
            "teams",
            "pagerduty",
            "webhook",
            "ses",
            "sns",
        ];
        for ch in &self.notify_channels {
            if !KNOWN_CHANNELS.contains(&ch.as_str()) {
                return Err(anyhow::anyhow!("Unknown notification channel: {ch:?}"));
            }
        }

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

    // ── Channel config loaders ──────────────────────────────────────────

    #[cfg(feature = "telegram")]
    async fn load_telegram_config(
        ssm: &aws_sdk_ssm::Client,
        channels: &[String],
        account_chat_id: Option<&str>,
    ) -> Result<Option<TelegramConfig>> {
        if !channels.iter().any(|c| c == "telegram") {
            return Ok(None);
        }
        let bot_token = Self::load_ssm_param(ssm, "TELEGRAM_BOT_TOKEN_SSM_PARAM").await?;
        let chat_id = account_chat_id
            .map(String::from)
            .or_else(|| std::env::var("TELEGRAM_CHAT_ID").ok())
            .filter(|s| !s.is_empty())
            .context(
                "telegram_chat_id missing from both event payload and TELEGRAM_CHAT_ID env var",
            )?;
        Ok(Some(TelegramConfig { bot_token, chat_id }))
    }

    #[cfg(feature = "slack")]
    async fn load_slack_config(
        ssm: &aws_sdk_ssm::Client,
        channels: &[String],
    ) -> Result<Option<SlackConfig>> {
        if !channels.iter().any(|c| c == "slack") {
            return Ok(None);
        }
        let webhook_url = Self::load_ssm_param(ssm, "SLACK_WEBHOOK_URL_SSM_PARAM").await?;
        Ok(Some(SlackConfig { webhook_url }))
    }

    #[cfg(feature = "teams")]
    async fn load_teams_config(
        ssm: &aws_sdk_ssm::Client,
        channels: &[String],
    ) -> Result<Option<TeamsConfig>> {
        if !channels.iter().any(|c| c == "teams") {
            return Ok(None);
        }
        let webhook_url = Self::load_ssm_param(ssm, "TEAMS_WEBHOOK_URL_SSM_PARAM").await?;
        Ok(Some(TeamsConfig { webhook_url }))
    }

    #[cfg(feature = "pagerduty")]
    async fn load_pagerduty_config(
        ssm: &aws_sdk_ssm::Client,
        channels: &[String],
    ) -> Result<Option<PagerDutyConfig>> {
        if !channels.iter().any(|c| c == "pagerduty") {
            return Ok(None);
        }
        let routing_key = Self::load_ssm_param(ssm, "PAGERDUTY_ROUTING_KEY_SSM_PARAM").await?;
        let severity = std::env::var("PAGERDUTY_SEVERITY").unwrap_or_else(|_| "error".to_string());
        Ok(Some(PagerDutyConfig {
            routing_key,
            severity,
        }))
    }

    #[cfg(feature = "ses")]
    fn load_ses_config(channels: &[String]) -> Result<Option<SesConfig>> {
        if !channels.iter().any(|c| c == "ses") {
            return Ok(None);
        }
        let from_address =
            std::env::var("SES_FROM_ADDRESS").context("SES_FROM_ADDRESS env var not set")?;
        let to_addresses: Vec<String> = std::env::var("SES_TO_ADDRESSES")
            .context("SES_TO_ADDRESSES env var not set")?
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if to_addresses.is_empty() {
            return Err(anyhow::anyhow!(
                "SES_TO_ADDRESSES must contain at least one address"
            ));
        }
        Ok(Some(SesConfig {
            from_address,
            to_addresses,
        }))
    }

    #[cfg(feature = "webhook")]
    async fn load_webhook_config(
        ssm: &aws_sdk_ssm::Client,
        channels: &[String],
    ) -> Result<Option<WebhookConfig>> {
        if !channels.iter().any(|c| c == "webhook") {
            return Ok(None);
        }
        let url = Self::load_ssm_param_optional(ssm, "WEBHOOK_URL_SSM_PARAM")
            .await?
            .or_else(|| std::env::var("WEBHOOK_URL").ok())
            .context("WEBHOOK_URL or WEBHOOK_URL_SSM_PARAM must be set")?;
        let auth_header =
            Self::load_ssm_param_optional(ssm, "WEBHOOK_AUTH_HEADER_SSM_PARAM").await?;
        Ok(Some(WebhookConfig { url, auth_header }))
    }

    #[cfg(feature = "sns")]
    fn load_sns_config(channels: &[String]) -> Result<Option<SnsConfig>> {
        if !channels.iter().any(|c| c == "sns") {
            return Ok(None);
        }
        let topic_arn = std::env::var("SNS_TOPIC_ARN").context("SNS_TOPIC_ARN env var not set")?;
        Ok(Some(SnsConfig { topic_arn }))
    }

    // ── Shared helpers ──────────────────────────────────────────────────

    fn parse_notify_channels() -> Vec<String> {
        std::env::var("NOTIFY_CHANNELS")
            .unwrap_or_else(|_| "telegram".to_string())
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    fn parse_http_timeout() -> Result<u64> {
        Self::parse_env_u64("HTTP_TIMEOUT_SECS", 0).map(|v| {
            if v > 0 {
                v
            } else {
                Self::parse_env_u64("TELEGRAM_TIMEOUT_SECS", 10).unwrap_or(10)
            }
        })
    }

    fn parse_http_connect_timeout() -> Result<u64> {
        Self::parse_env_u64("HTTP_CONNECT_TIMEOUT_SECS", 0).map(|v| {
            if v > 0 {
                v
            } else {
                Self::parse_env_u64("TELEGRAM_CONNECT_TIMEOUT_SECS", 5).unwrap_or(5)
            }
        })
    }

    /// Load a required SSM parameter. The env var names the SSM path.
    async fn load_ssm_param(ssm: &aws_sdk_ssm::Client, env_key: &str) -> Result<String> {
        let ssm_param =
            std::env::var(env_key).with_context(|| format!("{env_key} env var not set"))?;

        info!(env_key, ssm_param = %ssm_param, "Loading SSM parameter");

        let resp = ssm
            .get_parameter()
            .name(&ssm_param)
            .with_decryption(true)
            .send()
            .await
            .with_context(|| format!("Failed to fetch SSM parameter: {ssm_param}"))?;

        let value = resp
            .parameter
            .and_then(|p| p.value)
            .context("SSM parameter has no value")?;

        info!(env_key, ssm_param = %ssm_param, value_len = value.len(), "SSM parameter loaded");
        Ok(value)
    }

    /// Load an optional SSM parameter. Returns Ok(None) if the env var is unset.
    #[allow(dead_code)]
    async fn load_ssm_param_optional(
        ssm: &aws_sdk_ssm::Client,
        env_key: &str,
    ) -> Result<Option<String>> {
        match std::env::var(env_key) {
            Ok(ssm_param) => {
                let resp = ssm
                    .get_parameter()
                    .name(&ssm_param)
                    .with_decryption(true)
                    .send()
                    .await
                    .with_context(|| format!("Failed to fetch SSM parameter: {ssm_param}"))?;
                Ok(resp.parameter.and_then(|p| p.value))
            }
            Err(_) => Ok(None),
        }
    }

    fn parse_env_u32(key: &str, default: u32) -> Result<u32> {
        match std::env::var(key) {
            Ok(val) => val
                .parse::<u32>()
                .with_context(|| format!("{key} must be a positive integer, got {val:?}")),
            Err(_) => Ok(default),
        }
    }

    fn parse_env_usize(key: &str, default: usize) -> Result<usize> {
        match std::env::var(key) {
            Ok(val) => val
                .parse::<usize>()
                .with_context(|| format!("{key} must be a positive integer, got {val:?}")),
            Err(_) => Ok(default),
        }
    }

    fn parse_env_u64(key: &str, default: u64) -> Result<u64> {
        match std::env::var(key) {
            Ok(val) => val
                .parse::<u64>()
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
}
