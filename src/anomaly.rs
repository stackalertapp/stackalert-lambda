use crate::cost_explorer::SpendHistory;

/// A detected cost spike for a single AWS service
#[derive(Debug, Clone)]
pub struct Spike {
    pub service: String,
    /// 7-day average daily cost (USD)
    pub avg_daily: f64,
    /// Today's cost so far (USD)
    pub today: f64,
    /// Percentage increase vs average (e.g. 312.0 = 312% above average)
    pub pct_increase: f64,
    /// Extra spend vs average (USD)
    pub extra_usd: f64,
}

/// Detect services whose today's spend exceeds the 7-day average by threshold_pct percent.
///
/// For example with threshold_pct=50.0:
///   avg=$18.40/day, today=$75.20 → pct_increase=309% → SPIKE
///   avg=$5.00/day,  today=$7.00 → pct_increase=40%  → no spike
pub fn detect_spikes(history: &SpendHistory, threshold_pct: f64) -> Vec<Spike> {
    let mut spikes = Vec::new();

    for (service, daily_costs) in history {
        if daily_costs.len() < 2 {
            continue;
        }

        // Last element = today (partial day), rest = historical
        let today = *daily_costs.last().unwrap();
        let historical = &daily_costs[..daily_costs.len() - 1];

        // Use up to 7 days of history
        let window: Vec<f64> = historical.iter().rev().take(7).copied().collect();

        if window.is_empty() {
            continue;
        }

        // Filter out zero days (service wasn't running those days)
        let non_zero: Vec<f64> = window.iter().copied().filter(|&v| v > 0.01).collect();

        if non_zero.is_empty() {
            // Service appeared for the first time in the window — flag as new spend.
            // Threshold: $0.25/day catches real new usage (e.g. an EC2 instance running
            // a few hours) while ignoring sub-cent noise from new AWS services appearing
            // with trivial amounts.
            if today > 0.25 {
                spikes.push(Spike {
                    service: service.clone(),
                    avg_daily: 0.0,
                    today,
                    pct_increase: f64::INFINITY,
                    extra_usd: today,
                });
            }
            continue;
        }

        let avg = non_zero.iter().sum::<f64>() / non_zero.len() as f64;

        if avg < 0.10 {
            // Skip services with sub-$0.10/day average — too noisy
            continue;
        }

        let pct_increase = ((today - avg) / avg) * 100.0;

        if pct_increase > threshold_pct {
            spikes.push(Spike {
                service: service.clone(),
                avg_daily: round2(avg),
                today: round2(today),
                pct_increase: round2(pct_increase),
                extra_usd: round2(today - avg),
            });
        }
    }

    // Sort by extra spend descending (biggest surprise first)
    spikes.sort_by(|a, b| b.extra_usd.partial_cmp(&a.extra_usd).unwrap());
    spikes
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_history(days: Vec<f64>) -> SpendHistory {
        let mut h = HashMap::new();
        h.insert("Amazon EC2".to_string(), days);
        h
    }

    #[test]
    fn test_spike_detected() {
        // 7 days avg ~$18, today $75 → 316% spike
        let history = make_history(vec![18.0, 19.0, 17.5, 18.5, 18.0, 19.5, 17.0, 75.0]);
        let spikes = detect_spikes(&history, 50.0);
        assert_eq!(spikes.len(), 1);
        assert!(spikes[0].pct_increase > 300.0);
    }

    #[test]
    fn test_no_spike_normal_variation() {
        // ±10% variation — not a spike
        let history = make_history(vec![18.0, 19.0, 17.5, 18.5, 18.0, 19.5, 17.0, 19.0]);
        let spikes = detect_spikes(&history, 50.0);
        assert!(spikes.is_empty());
    }

    #[test]
    fn test_new_service_flagged() {
        // Service appeared today for the first time with significant spend
        let history = make_history(vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 45.0]);
        let spikes = detect_spikes(&history, 50.0);
        assert_eq!(spikes.len(), 1);
        assert_eq!(spikes[0].avg_daily, 0.0);
    }

    #[test]
    fn test_new_service_partial_day_flagged() {
        // EC2 started partway through the day — $0.50 is below old $1.0 threshold
        // but should still alert (above new $0.25 threshold)
        let history = make_history(vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.50]);
        let spikes = detect_spikes(&history, 50.0);
        assert_eq!(spikes.len(), 1);
    }

    #[test]
    fn test_new_service_noise_ignored() {
        // Sub-$0.25 new service (e.g. CloudTrail or Config logging a few cents)
        let history = make_history(vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.10]);
        let spikes = detect_spikes(&history, 50.0);
        assert!(spikes.is_empty());
    }

    #[test]
    fn test_tiny_spend_ignored() {
        // $0.05/day service spiking — too noisy to alert on
        let history = make_history(vec![0.05, 0.04, 0.06, 0.05, 0.04, 0.05, 0.06, 0.50]);
        let spikes = detect_spikes(&history, 50.0);
        assert!(spikes.is_empty());
    }

    #[test]
    fn test_spikes_sorted_by_extra_spend() {
        let mut history = HashMap::new();
        history.insert(
            "Amazon EC2".to_string(),
            vec![18.0, 18.0, 18.0, 18.0, 18.0, 18.0, 18.0, 75.0],
        );
        history.insert(
            "Amazon RDS".to_string(),
            vec![5.0, 5.0, 5.0, 5.0, 5.0, 5.0, 5.0, 20.0],
        );

        let spikes = detect_spikes(&history, 50.0);
        assert_eq!(spikes.len(), 2);
        assert_eq!(spikes[0].service, "Amazon EC2");
    }

    #[test]
    fn test_single_day_history() {
        // Only 1 day of data — not enough to detect spikes
        let history = make_history(vec![75.0]);
        let spikes = detect_spikes(&history, 50.0);
        assert!(spikes.is_empty());
    }

    #[test]
    fn test_threshold_boundary() {
        // Exactly at threshold — should not trigger (> not >=)
        // avg = $10, today = $15, pct_increase = 50% with threshold 50%
        let history = make_history(vec![10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 15.0]);
        let spikes = detect_spikes(&history, 50.0);
        assert!(spikes.is_empty());
    }

    #[test]
    fn test_just_above_threshold() {
        // Just above threshold — should trigger
        // avg = $10, today = $15.01, pct_increase = 50.1%
        let history = make_history(vec![10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 15.01]);
        let spikes = detect_spikes(&history, 50.0);
        assert_eq!(spikes.len(), 1);
    }
}
