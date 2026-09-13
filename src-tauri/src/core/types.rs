use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum ProviderId {
    Openai,
    Anthropic,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStatus {
    Fresh,
    Stale,
    Unavailable,
    AuthenticationRequired,
    RateLimited,
    Unsupported,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    pub id: String,
    pub label: String,
    pub used_percent: Option<f64>,
    pub remaining_percent: Option<f64>,
    pub reset_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsage {
    pub provider: ProviderId,
    pub windows: Vec<UsageWindow>,
    pub last_updated: Option<i64>,
    pub source: String,
    pub status: ProviderStatus,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    pub providers: Vec<ProviderUsage>,
    pub refreshed_at: i64,
}

impl UsageSnapshot {
    pub fn empty(now: i64) -> Self {
        Self {
            providers: vec![
                ProviderUsage::empty(ProviderId::Openai, "codex_app_server"),
                ProviderUsage::empty(ProviderId::Anthropic, "claude_oauth_usage"),
            ],
            refreshed_at: now,
        }
    }
}

impl ProviderUsage {
    pub fn empty(provider: ProviderId, source: &str) -> Self {
        Self {
            provider,
            windows: Vec::new(),
            last_updated: None,
            source: source.to_string(),
            status: ProviderStatus::Unavailable,
            error: None,
        }
    }
}
