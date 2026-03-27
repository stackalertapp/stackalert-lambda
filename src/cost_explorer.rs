use anyhow::{Context, Result};
use aws_config::{Region, SdkConfig};
use aws_credential_types::Credentials as AwsCredentials;
use aws_sdk_costexplorer::types::{
    DateInterval, Granularity, GroupDefinition, GroupDefinitionType,
};
use aws_sdk_sts::types::Credentials as StsCredentials;
use chrono::{Duration, NaiveDate, Utc};
use std::collections::HashMap;
use tracing::{debug, info};

use crate::config::Config;

/// Sub-cent cost amounts are noise from AWS rounding — filtered out of spend data
/// and anomaly detection. Shared with `anomaly::detect_spikes`.
pub const MIN_COST_THRESHOLD: f64 = 0.01;

/// STS AssumeRole session duration in seconds (minimum allowed by AWS).
const STS_SESSION_DURATION_SECS: i32 = 900;

/// Daily spend per service: { "Amazon EC2": [18.40, 19.20, ...] }
/// Index 0 = oldest day, last index = today (partial)
pub type SpendHistory = HashMap<String, Vec<f64>>;

/// Build AWS config for Cost Explorer queries.
/// If cross-account role is configured, assumes that role first.
/// Otherwise uses the Lambda's own credentials.
///
/// `base_cfg` is the Lambda's own AWS config (already loaded by the caller),
/// which is also used by SSM and other services — passing it in avoids a
/// redundant `load_from_env()` call per invocation.
pub async fn build_aws_config(cfg: &Config, base_cfg: &SdkConfig) -> Result<SdkConfig> {
    match &cfg.cross_account_role_arn {
        Some(role_arn) => {
            info!(%role_arn, "Using cross-account mode");
            assume_role(
                base_cfg,
                role_arn,
                "stackalert-check",
                cfg.external_id.as_deref(),
            )
            .await
        }
        None => {
            info!("Using single-account mode (Lambda's own credentials)");
            // Ensure us-east-1 for Cost Explorer
            let creds = base_cfg
                .credentials_provider()
                .context("No credentials provider in base AWS config — check Lambda execution role")?
                .clone();
            let ce_cfg = aws_config::defaults(aws_config::BehaviorVersion::latest())
                .credentials_provider(creds)
                .region(Region::new("us-east-1"))
                .load()
                .await;
            Ok(ce_cfg)
        }
    }
}

/// Assume a cross-account IAM role and return AWS config for that account
async fn assume_role(
    base_cfg: &SdkConfig,
    role_arn: &str,
    session_name: &str,
    external_id: Option<&str>,
) -> Result<SdkConfig> {
    let sts = aws_sdk_sts::Client::new(base_cfg);

    let mut req = sts
        .assume_role()
        .role_arn(role_arn)
        .role_session_name(session_name)
        .duration_seconds(STS_SESSION_DURATION_SECS);

    if let Some(eid) = external_id {
        req = req.external_id(eid);
    }

    let resp = req
        .send()
        .await
        .with_context(|| format!("Failed to assume role: {role_arn}"))?;

    let creds: StsCredentials = resp
        .credentials
        .context("No credentials returned from STS")?;

    let expires_at = std::time::SystemTime::try_from(creds.expiration)
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

    let assumed_creds = AwsCredentials::new(
        creds.access_key_id,
        creds.secret_access_key,
        Some(creds.session_token),
        Some(expires_at),
        "sts-assume-role",
    );

    let cfg = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .credentials_provider(assumed_creds)
        .region(Region::new("us-east-1"))
        .load()
        .await;

    info!(role_arn, "Successfully assumed role");
    Ok(cfg)
}

/// Fetch daily spend grouped by SERVICE for the last `days` days.
/// Returns a map of service name → vec of daily costs (oldest → newest).
pub async fn fetch_spend(cfg: &SdkConfig, days: i64) -> Result<SpendHistory> {
    let ce = aws_sdk_costexplorer::Client::new(cfg);

    let today = Utc::now().date_naive();
    let start = today - Duration::days(days);

    let start_str = start.format("%Y-%m-%d").to_string();
    let end_str = today.format("%Y-%m-%d").to_string();

    debug!(start = %start_str, end = %end_str, "Querying Cost Explorer");

    let resp = ce
        .get_cost_and_usage()
        .time_period(
            DateInterval::builder()
                .start(&start_str)
                .end(&end_str)
                .build()
                .context("Failed to build DateInterval")?,
        )
        .granularity(Granularity::Daily)
        .metrics("UnblendedCost")
        .group_by(
            GroupDefinition::builder()
                .r#type(GroupDefinitionType::Dimension)
                .key("SERVICE")
                .build(),
        )
        .send()
        .await
        .context("Cost Explorer GetCostAndUsage API call failed")?;

    let results = resp.results_by_time();
    let mut history: SpendHistory = HashMap::new();

    let mut date_order: Vec<NaiveDate> = Vec::new();

    for result in results {
        if let Some(period) = result.time_period()
            && let Ok(date) = NaiveDate::parse_from_str(period.start(), "%Y-%m-%d")
            && !date_order.contains(&date)
        {
            date_order.push(date);
        }
    }
    date_order.sort();

    let date_index: HashMap<NaiveDate, usize> = date_order
        .iter()
        .enumerate()
        .map(|(i, d)| (*d, i))
        .collect();

    let num_days = date_order.len();

    for result in results {
        let period_start = result
            .time_period()
            .and_then(|p| NaiveDate::parse_from_str(p.start(), "%Y-%m-%d").ok());

        let day_idx = match period_start.and_then(|d| date_index.get(&d)) {
            Some(&idx) => idx,
            None => continue,
        };

        for group in result.groups() {
            let service_name = group
                .keys()
                .first()
                .cloned()
                .unwrap_or_else(|| "Unknown".to_string());

            let amount = group
                .metrics()
                .and_then(|m| m.get("UnblendedCost"))
                .and_then(|mv| mv.amount())
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0);

            if amount < MIN_COST_THRESHOLD {
                continue;
            }

            let vec = history
                .entry(service_name)
                .or_insert_with(|| vec![0.0; num_days]);

            if day_idx < vec.len() {
                vec[day_idx] = amount;
            }
        }
    }

    debug!(
        services = history.len(),
        days = num_days,
        "Spend data fetched"
    );
    Ok(history)
}
