use aws_sdk_ssm::{types::ParameterType, Client as SsmClient};
use chrono::Utc;
use tracing::{debug, info, warn};

use crate::anomaly::Spike;

/// SSM path prefix for dedup timestamps.
const SSM_PREFIX: &str = "/stackalert/last-alerted/";

/// How long to suppress repeat alerts for the same service (seconds).
/// Matches the 6h EventBridge schedule — so each schedule window alerts at most once per service.
const COOLDOWN_SECS: i64 = 6 * 3600;

/// Filter out spikes that were alerted for the same service within the cooldown window.
///
/// `namespace` isolates different accounts:
///   - self-hosted / single-account mode: `"self"`
///   - multi-account SaaS mode:           the customer's AWS account ID
///
/// This means:
///   - Service A alerting never blocks Service B (independent SSM keys)
///   - Same service in two different accounts alerts independently
///   - SSM failures degrade gracefully: assume "not yet alerted" and let the alert through
pub async fn filter_new_spikes(
    ssm: &SsmClient,
    spikes: Vec<Spike>,
    namespace: &str,
) -> Vec<Spike> {
    if spikes.is_empty() {
        return spikes;
    }

    let now = Utc::now().timestamp();
    let mut new_spikes = Vec::new();

    for spike in spikes {
        let key = ssm_key(namespace, &spike.service);

        let last_alerted = fetch_timestamp(ssm, &key).await;

        match last_alerted {
            Some(ts) if now - ts < COOLDOWN_SECS => {
                let secs_ago = now - ts;
                let mins_ago = secs_ago / 60;
                debug!(
                    service = %spike.service,
                    mins_ago,
                    "Dedup: skipping spike — alerted recently"
                );
            }
            Some(_) => {
                debug!(service = %spike.service, "Dedup: cooldown expired — re-alerting");
                new_spikes.push(spike);
            }
            None => {
                debug!(service = %spike.service, "Dedup: first alert for this service");
                new_spikes.push(spike);
            }
        }
    }

    new_spikes
}

/// Write the current timestamp to SSM for each alerted spike.
/// Called after a Telegram message is successfully sent.
///
/// Failures are logged as warnings but do NOT abort the invocation.
pub async fn mark_alerted(ssm: &SsmClient, spikes: &[Spike], namespace: &str) {
    let now_str = Utc::now().timestamp().to_string();

    for spike in spikes {
        let key = ssm_key(namespace, &spike.service);

        match ssm
            .put_parameter()
            .name(&key)
            .value(&now_str)
            .r#type(ParameterType::String)
            .overwrite(true)
            .send()
            .await
        {
            Ok(_) => info!(service = %spike.service, "Dedup: marked alerted in SSM"),
            Err(e) => warn!(
                service = %spike.service,
                error = %e,
                "Dedup: failed to write SSM timestamp — will re-alert next cycle"
            ),
        }
    }
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
