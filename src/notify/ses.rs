use std::future::Future;
use std::pin::Pin;

use anyhow::{Context, Result};
use tracing::info;

use super::{NotifyChannel, escape_html, fmt_pct, ranked_services};
use crate::anomaly::Spike;
use crate::config::Config;
use crate::cost_explorer::SpendHistory;

pub struct Ses {
    client: aws_sdk_ses::Client,
}

impl Ses {
    pub fn new(base_cfg: &aws_config::SdkConfig) -> Self {
        Self {
            client: aws_sdk_ses::Client::new(base_cfg),
        }
    }
}

impl NotifyChannel for Ses {
    fn name(&self) -> &'static str {
        "ses"
    }

    fn send_spike_alert<'a>(
        &'a self,
        cfg: &'a Config,
        spikes: &'a [Spike],
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(async move {
            if spikes.is_empty() {
                return Ok(false);
            }
            let scfg = cfg
                .ses
                .as_ref()
                .context("SES channel active but config missing")?;
            let subject = format!(
                "{}: Cost Spike — {} (+${:.2})",
                cfg.setup_name, spikes[0].service, spikes[0].extra_usd
            );
            let body = build_spike_html(
                spikes,
                &cfg.setup_name,
                cfg.dedup_cooldown_hours,
                cfg.max_spike_display,
            );
            send_email(&self.client, scfg, &subject, &body).await
        })
    }

    fn send_daily_digest<'a>(
        &'a self,
        cfg: &'a Config,
        spend_data: &'a SpendHistory,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(async move {
            let scfg = cfg
                .ses
                .as_ref()
                .context("SES channel active but config missing")?;
            let body = build_digest_html(
                spend_data,
                &cfg.setup_name,
                cfg.min_avg_daily_usd,
                cfg.max_digest_display,
            );
            send_email(
                &self.client,
                scfg,
                &format!("{} Daily Digest", cfg.setup_name),
                &body,
            )
            .await
        })
    }
}

use crate::config::SesConfig;

async fn send_email(
    client: &aws_sdk_ses::Client,
    scfg: &SesConfig,
    subject: &str,
    html_body: &str,
) -> Result<bool> {
    use aws_sdk_ses::types::{Body, Content, Destination, Message};

    let dest = Destination::builder()
        .set_to_addresses(Some(scfg.to_addresses.clone()))
        .build();

    let msg = Message::builder()
        .subject(
            Content::builder()
                .data(subject)
                .charset("UTF-8")
                .build()
                .context("subject")?,
        )
        .body(
            Body::builder()
                .html(
                    Content::builder()
                        .data(html_body)
                        .charset("UTF-8")
                        .build()
                        .context("body")?,
                )
                .build(),
        )
        .build();

    let result = client
        .send_email()
        .source(&scfg.from_address)
        .destination(dest)
        .message(msg)
        .send()
        .await;

    match result {
        Ok(_) => {
            info!("SES email sent");
            Ok(true)
        }
        Err(e) => Err(anyhow::anyhow!("SES send failed: {e}")),
    }
}

// ── HTML formatting ─────────────────────────────────────────────────────────

fn build_spike_html(
    spikes: &[Spike],
    setup_name: &str,
    check_interval_hours: u32,
    max_display: usize,
) -> String {
    assert!(
        !spikes.is_empty(),
        "build_spike_html called with empty spikes"
    );
    let total_extra: f64 = spikes.iter().map(|s| s.extra_usd).sum();
    let top = &spikes[0];

    let mut html = format!("<h2>{}: Cost Spike Detected</h2>", escape_html(setup_name));
    html.push_str(&format!(
        "<p><strong>{}</strong> spiked {} (${:.2} today vs ${:.2}/day avg)</p>",
        escape_html(&top.service),
        fmt_pct(top.pct_increase),
        top.today,
        top.avg_daily,
    ));
    html.push_str(&format!("<p>Total extra spend: ~${:.2}</p>", total_extra));

    html.push_str("<h3>Affected services</h3><ul>");
    for spike in spikes.iter().take(max_display) {
        html.push_str(&format!(
            "<li><strong>{}</strong> {} (${:.2} today vs ${:.2} avg)</li>",
            escape_html(&spike.service),
            fmt_pct(spike.pct_increase),
            spike.today,
            spike.avg_daily,
        ));
    }
    html.push_str("</ul>");

    if spikes.len() > max_display {
        html.push_str(&format!(
            "<p><em>...and {} more services</em></p>",
            spikes.len() - max_display
        ));
    }

    html.push_str(&format!(
        "<hr><p><small>{} &middot; Checks every {check_interval_hours}h</small></p>",
        escape_html(setup_name)
    ));
    html
}

fn build_digest_html(
    spend_data: &SpendHistory,
    setup_name: &str,
    min_avg_daily: f64,
    max_display: usize,
) -> String {
    let (services, grand_total) = ranked_services(spend_data, min_avg_daily);

    let mut html = format!("<h2>{}: Daily Digest</h2>", escape_html(setup_name));
    html.push_str(&format!(
        "<p>Avg daily spend: <strong>${:.2}</strong></p>",
        grand_total
    ));

    html.push_str("<h3>Top services (avg/day)</h3><ul>");
    for (service, avg) in services.iter().take(max_display) {
        html.push_str(&format!(
            "<li>{} &mdash; ${:.2}/day</li>",
            escape_html(service),
            avg
        ));
    }
    html.push_str("</ul>");

    if services.len() > max_display {
        html.push_str(&format!(
            "<p><em>...and {} more services</em></p>",
            services.len() - max_display
        ));
    }

    html.push_str(&format!(
        "<hr><p><small>{} Daily Digest</small></p>",
        escape_html(setup_name)
    ));
    html
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn test_spike_html() {
        let spikes = vec![Spike {
            service: "Amazon EC2".to_string(),
            avg_daily: 18.0,
            today: 75.0,
            pct_increase: 316.67,
            extra_usd: 57.0,
        }];
        let html = build_spike_html(&spikes, "StackAlert", 6, 5);
        assert!(html.contains("<h2>StackAlert: Cost Spike Detected</h2>"));
        assert!(html.contains("Amazon EC2"));
        assert!(html.contains("+317%"));
    }

    #[test]
    fn test_digest_html() {
        let mut data = HashMap::new();
        data.insert("Amazon EC2".to_string(), vec![18.0, 19.0]);
        let html = build_digest_html(&data, "StackAlert", 0.10, 10);
        assert!(html.contains("<h2>StackAlert: Daily Digest</h2>"));
        assert!(html.contains("Amazon EC2"));
    }
}
