use crate::cost_explorer::{MIN_COST_THRESHOLD, SpendHistory};

/// A detected cost spike for a single AWS service
#[derive(Debug, Clone)]
pub struct Spike {
    pub service: String,
    /// Average daily cost (USD) over the configured history window
    pub avg_daily: f64,
    /// Today's cost so far (USD)
    pub today: f64,
    /// Percentage increase vs average (e.g. 312.0 = 312% above average)
    pub pct_increase: f64,
    /// Extra spend vs average (USD)
    pub extra_usd: f64,
}

/// Detect services whose today's spend exceeds the historical average by threshold_pct percent.
///
/// Parameters:
///   - `history`         — daily spend map from `fetch_spend`
///   - `threshold_pct`   — % above avg to trigger alert (e.g. 50.0 = 50% above average)
///   - `history_days`    — how many historical days to use for the baseline average
///   - `min_avg_daily`   — services with avg below this value are skipped (noise filter)
///
/// For example with threshold_pct=50.0:
///   avg=$18.40/day, today=$75.20 → pct_increase=309% → SPIKE
///   avg=$5.00/day,  today=$7.00 → pct_increase=40%  → no spike
pub fn detect_spikes(
    history: &SpendHistory,
    threshold_pct: f64,
    history_days: u32,
    min_avg_daily: f64,
) -> Vec<Spike> {
    let mut spikes = Vec::new();

    for (service, daily_costs) in history {
        if daily_costs.len() < 2 {
            continue;
        }

        // Last element = today (partial day), rest = historical
        let today = *daily_costs.last().unwrap();
        let historical = &daily_costs[..daily_costs.len() - 1];

        // Use up to `history_days` days of history (most recent first)
        let window: Vec<f64> = historical
            .iter()
            .rev()
            .take(history_days as usize)
            .copied()
            .collect();

        if window.is_empty() {
            continue;
        }

        // Filter out zero days (service wasn't running those days)
        let non_zero: Vec<f64> = window
            .iter()
            .copied()
            .filter(|&v| v > MIN_COST_THRESHOLD)
            .collect();

        if non_zero.is_empty() {
            // Service appeared for the first time in the window — flag any cost as a spike.
            // Sub-cent amounts are already filtered out by fetch_spend (amount < 0.01),
            // so any value reaching here is real spend on a previously unseen service.
            if today > 0.0 {
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

        if avg < min_avg_daily {
            // Skip services below the configured noise threshold
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

    // Sort by extra spend descending (biggest surprise first).
    // total_cmp handles NaN without panicking (NaN sorts last).
    spikes.sort_by(|a, b| b.extra_usd.total_cmp(&a.extra_usd));
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
        let spikes = detect_spikes(&history, 50.0, 7, 0.10);
        assert_eq!(spikes.len(), 1);
        assert!(spikes[0].pct_increase > 300.0);
    }

    #[test]
    fn test_no_spike_normal_variation() {
        // ±10% variation — not a spike
        let history = make_history(vec![18.0, 19.0, 17.5, 18.5, 18.0, 19.5, 17.0, 19.0]);
        let spikes = detect_spikes(&history, 50.0, 7, 0.10);
        assert!(spikes.is_empty());
    }

    #[test]
    fn test_new_service_flagged() {
        // Service appeared today for the first time with significant spend
        let history = make_history(vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 45.0]);
        let spikes = detect_spikes(&history, 50.0, 7, 0.10);
        assert_eq!(spikes.len(), 1);
        assert_eq!(spikes[0].avg_daily, 0.0);
    }

    #[test]
    fn test_new_service_partial_day_flagged() {
        // EC2 started partway through the day — any cost from a new service is flagged
        let history = make_history(vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.50]);
        let spikes = detect_spikes(&history, 50.0, 7, 0.10);
        assert_eq!(spikes.len(), 1);
    }

    #[test]
    fn test_new_service_small_cost_flagged() {
        // EC2 started late in the day — even $0.10 is real spend and should alert
        let history = make_history(vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.10]);
        let spikes = detect_spikes(&history, 50.0, 7, 0.10);
        assert_eq!(spikes.len(), 1);
    }

    #[test]
    fn test_tiny_spend_ignored() {
        // $0.05/day service spiking — too noisy to alert on
        let history = make_history(vec![0.05, 0.04, 0.06, 0.05, 0.04, 0.05, 0.06, 0.50]);
        let spikes = detect_spikes(&history, 50.0, 7, 0.10);
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

        let spikes = detect_spikes(&history, 50.0, 7, 0.10);
        assert_eq!(spikes.len(), 2);
        assert_eq!(spikes[0].service, "Amazon EC2");
    }

    #[test]
    fn test_single_day_history() {
        // Only 1 day of data — not enough to detect spikes
        let history = make_history(vec![75.0]);
        let spikes = detect_spikes(&history, 50.0, 7, 0.10);
        assert!(spikes.is_empty());
    }

    #[test]
    fn test_threshold_boundary() {
        // Exactly at threshold — should not trigger (> not >=)
        // avg = $10, today = $15, pct_increase = 50% with threshold 50%
        let history = make_history(vec![10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 15.0]);
        let spikes = detect_spikes(&history, 50.0, 7, 0.10);
        assert!(spikes.is_empty());
    }

    #[test]
    fn test_just_above_threshold() {
        // Just above threshold — should trigger
        // avg = $10, today = $15.01, pct_increase = 50.1%
        let history = make_history(vec![10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 15.01]);
        let spikes = detect_spikes(&history, 50.0, 7, 0.10);
        assert_eq!(spikes.len(), 1);
    }
}
