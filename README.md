# StackAlert Lambda

[![CI](https://github.com/stackalertapp/stackalert-lambda/actions/workflows/ci.yml/badge.svg)](https://github.com/stackalertapp/stackalert-lambda/actions/workflows/ci.yml)

AWS cost spike detection Lambda written in Rust. Monitors your AWS spending and alerts you when costs spike unexpectedly. Supports multiple notification channels: Telegram, Slack, Microsoft Teams, PagerDuty, email (SES), SNS, and custom webhooks.

**Perfect for:**
- Small teams monitoring their own AWS costs
- Anyone giving AI agents (Claude Code, Cursor, Devin) AWS access and wanting cost spike alerts

## How It Works

```
EventBridge (every 6h) → Lambda → Cost Explorer API → Spike Detection → Notify (Telegram, Slack, ...)
EventBridge (daily 8:00 UTC) → Lambda → Cost Explorer API → Daily Digest → Notify (Telegram, Slack, ...)
```

The Lambda queries AWS Cost Explorer for daily spend grouped by service, compares today's spend against a 7-day rolling average, and sends alerts to your configured notification channels if any service exceeds the configured threshold (default: 50% above average).

## What You Get: Per-Service Alerts

StackAlert monitors each AWS service **independently** — so you know exactly what spiked, not just that your bill went up.

**Spike alert example (Telegram):**
```
⚠️ Cost Spike Detected

🔴 Amazon Bedrock    $0.80 → $8.20   (+925%)
🔴 Amazon EC2        $12.00 → $18.50  (+54%)
✅ Amazon S3         $0.42             (stable)
✅ AWS Lambda        $0.01             (stable)
```

**Daily digest example:**
```
📊 Daily AWS Cost Digest — Mon 23 Mar

Amazon Bedrock    $8.20
Amazon EC2       $18.50
Amazon S3         $0.42
AWS Lambda        $0.01
──────────────────────
Total            $27.13
```

### Why this matters vs AWS native tools

| Tool | What it tells you | Delivery | Cost |
|---|---|---|---|
| **StackAlert** | Which service + by how much | Telegram, Slack, Teams, PagerDuty, email, SNS, webhook | ~$0/mo |
| AWS Budgets | Total bill crossed threshold | Email | Free |
| Cost Anomaly Detection | Something looks unusual (ML) | Email | Free |
| CloudZero / nOps | Everything | Slack/email | $500+/mo |

AWS Budgets fires when you've already overspent. Cost Anomaly Detection needs 2–4 weeks of ML training. StackAlert fires on day 1 with a 50% default threshold, tells you the service by name, and delivers to your channel of choice.

## Features

- **Spike Detection**: Alerts when any service's daily cost exceeds the 7-day average by a configurable threshold
- **Daily Digest**: Optional daily summary of your top AWS services by cost
- **New Service Detection**: Flags services that appear for the first time with significant spend (>$1)
- **Noise Filtering**: Ignores services with <$0.10/day average to reduce alert fatigue
- **Multi-Channel Notifications**: Send alerts to one or more channels simultaneously — Telegram, Slack, Teams, PagerDuty, email (SES), SNS, or a custom webhook
- **Cross-Account Support**: Optionally assume an IAM role to monitor a different AWS account
- **Secure**: Secrets stored in SSM Parameter Store (encrypted)
- **Fast & Cheap**: Rust on ARM64 (Graviton) — sub-100ms cold start, minimal memory

## Notification Channels

StackAlert supports multiple notification channels that can run simultaneously. Set `NOTIFY_CHANNELS` to a comma-separated list (default: `telegram`).

| Channel | Feature Flag | Secrets / Config | Notes |
|---------|-------------|-----------------|-------|
| **Telegram** | `telegram` (default) | `TELEGRAM_BOT_TOKEN_SSM_PARAM` (SSM), `TELEGRAM_CHAT_ID` (env) | Bot token stored in SSM Parameter Store |
| **Slack** | `slack` | `SLACK_WEBHOOK_URL_SSM_PARAM` (SSM) | Uses Slack incoming webhooks |
| **Microsoft Teams** | `teams` | `TEAMS_WEBHOOK_URL_SSM_PARAM` (SSM) | Sends Adaptive Cards |
| **PagerDuty** | `pagerduty` | `PAGERDUTY_ROUTING_KEY_SSM_PARAM` (SSM) | Spike alerts only (digests skipped — not incidents) |
| **Email (SES)** | `ses` | `SES_FROM_ADDRESS`, `SES_TO_ADDRESSES` (env) | Comma-separated recipient list; requires verified SES identities |
| **SNS** | `sns` | `SNS_TOPIC_ARN` (env) | Publishes to any SNS topic (email, SMS, Lambda, etc.) |
| **Webhook** | `webhook` | `WEBHOOK_URL_SSM_PARAM` (SSM) or `WEBHOOK_URL` (env), optional `WEBHOOK_AUTH_HEADER_SSM_PARAM` (SSM) | JSON payload with structured spike/digest data |

### Compile-time feature flags

Each channel is a Cargo feature flag. Only enabled channels are compiled into the binary, keeping it small.

```bash
# Default (Telegram only)
cargo build --release

# Telegram + Slack
cargo build --release --features telegram,slack

# All channels
cargo build --release --features all-channels
```

### Example: multiple channels

```bash
# Environment variables
NOTIFY_CHANNELS=telegram,slack,pagerduty
TELEGRAM_BOT_TOKEN_SSM_PARAM=/stackalert/telegram-bot-token
TELEGRAM_CHAT_ID=-100123456789
SLACK_WEBHOOK_URL_SSM_PARAM=/stackalert/slack-webhook-url
PAGERDUTY_ROUTING_KEY_SSM_PARAM=/stackalert/pagerduty-routing-key
```

Alerts fan out to all configured channels concurrently. A failure in one channel does not block the others.

## Deployment

Deploy using one of the official IaC modules:

| Tool | Repository |
|------|-----------|
| Terraform | [stackalertapp/stackalert-terraform](https://github.com/stackalertapp/stackalert-terraform) |
| AWS CDK | [stackalertapp/stackalert-cdk](https://github.com/stackalertapp/stackalert-cdk) |
| Pulumi | [stackalertapp/stackalert-pulumi](https://github.com/stackalertapp/stackalert-pulumi) |

### Manual Deployment

<details>
<summary>Click to expand</summary>

1. **Build the Lambda binary:**

```bash
# Install cross-compilation target
rustup target add aarch64-unknown-linux-musl

# Build (add --features as needed, e.g. --features all-channels)
cargo build --release --target aarch64-unknown-linux-musl

# Package
mkdir -p dist
cp target/aarch64-unknown-linux-musl/release/bootstrap dist/bootstrap
cd dist && zip lambda.zip bootstrap
```

2. **Store secrets in SSM:**

```bash
aws ssm put-parameter \
  --name "/stackalert/telegram-bot-token" \
  --type SecureString \
  --value "YOUR_BOT_TOKEN"
```

3. **Create the Lambda** with these environment variables:

| Variable | Required | Description |
|----------|----------|-------------|
| `NOTIFY_CHANNELS` | No | Comma-separated channel list (default: `telegram`). See [Notification Channels](#notification-channels) |
| `SPIKE_THRESHOLD_PCT` | No | Spike threshold percentage (default: `50`) |
| `CROSS_ACCOUNT_ROLE_ARN` | No | IAM role ARN for cross-account monitoring |
| `TELEGRAM_BOT_TOKEN_SSM_PARAM` | If using Telegram | SSM parameter name for the bot token |
| `TELEGRAM_CHAT_ID` | If using Telegram | Telegram chat ID for alerts |

See the [Notification Channels](#notification-channels) table for channel-specific variables.

4. **Create EventBridge rules:**

- Spike check: `rate(6 hours)` with input `{"mode": "spike"}`
- Daily digest: `cron(0 8 * * ? *)` with input `{"mode": "digest"}`

</details>

## Required IAM Permissions

The Lambda execution role needs:

```json
{
  "Effect": "Allow",
  "Action": [
    "ce:GetCostAndUsage"
  ],
  "Resource": "*"
},
{
  "Effect": "Allow",
  "Action": [
    "ssm:GetParameter",
    "ssm:PutParameter"
  ],
  "Resource": "arn:aws:ssm:*:*:parameter/stackalert/*"
}
```

For cross-account mode, add:

```json
{
  "Effect": "Allow",
  "Action": "sts:AssumeRole",
  "Resource": "arn:aws:iam::MONITORED_ACCOUNT:role/StackAlertReadOnly"
}
```

For SES or SNS channels, add the relevant permissions:

```json
{
  "Effect": "Allow",
  "Action": "ses:SendEmail",
  "Resource": "*"
},
{
  "Effect": "Allow",
  "Action": "sns:Publish",
  "Resource": "arn:aws:sns:*:*:your-topic-name"
}
```

## Development

```bash
# Run tests
cargo test

# Run all checks (fmt, clippy, test, deny)
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo deny check
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for contribution guidelines.

## License

Apache License 2.0 — see [LICENSE](LICENSE) for details.
