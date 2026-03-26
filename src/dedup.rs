use aws_sdk_ssm::{types::ParameterType, Client as SsmClient};
use chrono::Utc;
use tracing::{debug, info, warn};

use crate::anomaly::Spike;

/// SSM path prefix for dedup timestamps.
const SSM_PREFIX: &str = "/stackalert/last-alerted/";

/// Filter out spikes that were alerted for the same service within the cooldown window.
///
/// `namespace` isolates different accounts:
///   - self-hosted / single-account mode: `"self"`
///   - multi-account SaaS mode:           the customer's AWS account ID
///
/// `cooldown_hours` — how long to suppress repeat alerts (matches `DEDUP_COOLDOWN_HOURS` config).
///
/// This means:
///   - Service A alerting never blocks Service B (independent SSM keys)
///   - Same service in two different accounts alerts independently
///   - SSM failures degrade gracefully: assume "not yet alerted" and let the alert through
///
/// All SSM GetParameter calls are issued concurrently to minimise Lambda latency.
pub async fn filter_new_spikes(
    ssm: &SsmClient,
    spikes: Vec<Spike>,
    namespace: &str,
    cooldown_hours: u32,
) -> Vec<Spike> {
    if spikes.is_empty() {
        return spikes;
    }

    let cooldown_secs = cooldown_hours as i64 * 3600;
    let now = Utc::now().timestamp();

    // Fan-out: fire all SSM GetParameter calls concurrently.
    // JoinSet preserves the task results so we can correlate them back to spikes by index.
    let mut set = tokio::task::JoinSet::new();
    for (i, spike) in spikes.iter().enumerate() {
        let key = ssm_key(namespace, &spike.service);
        let ssm_clone = ssm.clone();
        set.spawn(async move {
            let ts = fetch_timestamp(&ssm_clone, &key).await;
            (i, ts)
        });
    }

    // Collect results keyed by original index.
    let mut timestamps: Vec<Option<i64>> = vec![None; spikes.len()];
    while let Some(res) = set.join_next().await {
        if let Ok((i, ts)) = res {
            timestamps[i] = ts;
        }
    }

    // Filter in original order so the most-expensive spike stays first.
    spikes
        .into_iter()
        .enumerate()
        .filter_map(|(i, spike)| match timestamps[i] {
            Some(ts) if now - ts < cooldown_secs => {
                let mins_ago = (now - ts) / 60;
                debug!(
                    service = %spike.service,
                    mins_ago,
                    "Dedup: skipping spike — alerted recently"
                );
                None
            }
            Some(_) => {
                debug!(service = %spike.service, "Dedup: cooldown expired — re-alerting");
                Some(spike)
            }
            None => {
                debug!(service = %spike.service, "Dedup: first alert for this service");
                Some(spike)
            }
        })
        .collect()
}

/// Write the current timestamp to SSM for each alerted spike.
/// Called after a Telegram message is successfully sent.
///
/// All SSM PutParameter calls are issued concurrently.
/// Failures are logged as warnings but do NOT abort the invocation.
pub async fn mark_alerted(ssm: &SsmClient, spikes: &[Spike], namespace: &str) {
    let now_str = Utc::now().timestamp().to_string();

    let mut set = tokio::task::JoinSet::new();
    for spike in spikes {
        let key = ssm_key(namespace, &spike.service);
        let ssm_clone = ssm.clone();
        let value = now_str.clone();
        let service = spike.service.clone();
        set.spawn(async move {
            match ssm_clone
                .put_parameter()
                .name(&key)
                .value(&value)
                .r#type(ParameterType::String)
                .overwrite(true)
                .send()
                .await
            {
                Ok(_) => info!(service, "Dedup: marked alerted in SSM"),
                Err(e) => warn!(
                    service,
                    error = %e,
                    "Dedup: failed to write SSM timestamp — will re-alert next cycle"
                ),
            }
        });
    }
    // Drive all tasks to completion (errors already logged inside each task).
    while set.join_next().await.is_some() {}
}

// ── Helpers ─────────────────────────────────────────────────────────────────

async fn fetch_timestamp(ssm: &SsmClient, key: &str) -> Option<i64> {
    let resp = ssm.get_parameter().name(key).send().await.ok()?;
    resp.parameter?.value?.parse::<i64>().ok()
}

/// Build the SSM key for a service in a given namespace.
///
/// SSM path components may only contain letters, numbers, hyphens, underscores,
/// forward slashes, and dots. AWS service names like "Amazon EC2" are sanitized.
///
/// Examples:
///   namespace="self",         service="Amazon EC2"   → /stackalert/last-alerted/self/Amazon_EC2
///   namespace="700483457242", service="Amazon RDS"   → /stackalert/last-alerted/700483457242/Amazon_RDS
fn ssm_key(namespace: &str, service: &str) -> String {
    let safe_service: String = service
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{SSM_PREFIX}{namespace}/{safe_service}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ssm_key_sanitizes_service_name() {
        assert_eq!(
            ssm_key("self", "Amazon EC2"),
            "/stackalert/last-alerted/self/Amazon_EC2"
        );
        assert_eq!(
            ssm_key("700483457242", "AWS Lambda"),
            "/stackalert/last-alerted/700483457242/AWS_Lambda"
        );
        assert_eq!(
            ssm_key("self", "Amazon S3"),
            "/stackalert/last-alerted/self/Amazon_S3"
        );
    }

    #[test]
    fn test_ssm_key_allows_hyphens_and_dots() {
        assert_eq!(
            ssm_key("self", "some-service.v2"),
            "/stackalert/last-alerted/self/some-service.v2"
        );
    }
}
