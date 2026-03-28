use serde::{Deserialize, Serialize};

/// Per-account context injected by the dashboard's Step Functions Map state.
///
/// Serialised fields use camelCase to match the DynamoDB schema written
/// by the dashboard. In single-account / open-source mode this struct is
/// never populated — the Lambda reads config from env vars instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountContext {
    pub id: String,

    #[serde(rename = "userId")]
    pub user_id: String,

    #[serde(rename = "awsAccountId")]
    pub aws_account_id: String,

    #[serde(rename = "roleArn")]
    pub role_arn: String,

    #[serde(rename = "externalId")]
    pub external_id: String,

    #[serde(rename = "spikeThreshold")]
    pub spike_threshold: f64,

    #[serde(rename = "telegramChatId")]
    pub telegram_chat_id: Option<String>,

    /// Override NOTIFY_CHANNELS per account (comma-separated).
    #[serde(rename = "notifyChannels", default)]
    pub notify_channels: Option<String>,

    pub region: String,
}
