use crate::core::{normalize::normalize_window, types::{ProviderId, ProviderStatus, ProviderUsage}};
use chrono::{DateTime, Utc};
use reqwest::{redirect::Policy, Client, StatusCode};
use serde::Deserialize;
use serde_json::Value;
use std::{env, fs, path::PathBuf, time::Duration};

const SOURCE: &str = "claude_oauth_usage_unofficial";
const ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";

#[derive(Debug, Deserialize)]
struct ApiResponse {
    limits: Option<Vec<Limit>>,
    five_hour: Option<LegacyWindow>,
    seven_day: Option<LegacyWindow>,
}
#[derive(Debug, Deserialize)]
struct Limit { kind: Option<String>, percent: Option<f64>, resets_at: Option<String>, is_active: Option<bool> }
#[derive(Debug, Deserialize)]
struct LegacyWindow { utilization: Option<f64>, resets_at: Option<String> }

pub async fn fetch_usage() -> ProviderUsage {
    match fetch_inner().await {
        Ok(v) => v,
        Err((status, message)) => ProviderUsage { provider: ProviderId::Anthropic, windows: vec![], last_updated: None, source: SOURCE.into(), status, error: Some(message) },
    }
}

async fn fetch_inner() -> Result<ProviderUsage, (ProviderStatus, String)> {
    let token = read_access_token()?;
    let client = Client::builder()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(12))
        .user_agent("Usage/0.1.0")
        .build().map_err(|_| (ProviderStatus::Unavailable, "Unable to initialize Anthropic client".into()))?;

    let response = client.get(ENDPOINT)
        .bearer_auth(&token)
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("accept", "application/json")
        .send().await.map_err(|_| (ProviderStatus::Unavailable, "Anthropic usage request failed".into()))?;

    if response.status().is_redirection() { return Err((ProviderStatus::Unavailable, "Anthropic usage endpoint redirected unexpectedly".into())); }
    match response.status() {
        StatusCode::OK => {}
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => return Err((ProviderStatus::AuthenticationRequired, "Claude sign-in needs attention".into())),
        StatusCode::TOO_MANY_REQUESTS => return Err((ProviderStatus::RateLimited, "Claude usage endpoint rate limited".into())),
        _ => return Err((ProviderStatus::Unavailable, format!("Claude usage endpoint returned HTTP {}", response.status().as_u16()))),
    }
    let payload: ApiResponse = response.json().await.map_err(|_| (ProviderStatus::Unavailable, "Claude usage response schema changed".into()))?;
    let windows = parse_windows(payload)?;
    Ok(ProviderUsage { provider: ProviderId::Anthropic, windows, last_updated: Some(Utc::now().timestamp()), source: SOURCE.into(), status: ProviderStatus::Fresh, error: None })
}

fn credential_path() -> Result<PathBuf, (ProviderStatus, String)> {
    if let Some(config) = env::var_os("CLAUDE_CONFIG_DIR") {
        return Ok(PathBuf::from(config).join(".credentials.json"));
    }
    let home = dirs::home_dir().ok_or((ProviderStatus::Unsupported, "User profile directory unavailable".into()))?;
    Ok(home.join(".claude").join(".credentials.json"))
}

fn read_access_token() -> Result<String, (ProviderStatus, String)> {
    let path = credential_path()?;
    let text = fs::read_to_string(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            (ProviderStatus::AuthenticationRequired, "Claude Code sign-in not detected".into())
        } else {
            (ProviderStatus::Unavailable, "Unable to read Claude Code credential store".into())
        }
    })?;
    let root: Value = serde_json::from_str(&text).map_err(|_| (ProviderStatus::Unavailable, "Claude Code credential store format changed".into()))?;
    let token = root.pointer("/claudeAiOauth/accessToken").and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or((ProviderStatus::AuthenticationRequired, "Claude OAuth session not found".into()))?;
    Ok(token.to_owned())
}

fn parse_windows(payload: ApiResponse) -> Result<Vec<crate::core::types::UsageWindow>, (ProviderStatus, String)> {
    let mut session = None;
    let mut weekly = None;
    if let Some(limits) = payload.limits {
        for limit in limits.into_iter().filter(|l| l.is_active.unwrap_or(true)) {
            let Some(percent) = limit.percent else { continue; };
            match limit.kind.as_deref() {
                Some("session") => session = Some(normalize_window("session", "5 hours", percent, parse_reset(limit.resets_at.as_deref()))),
                Some("weekly_all") => weekly = Some(normalize_window("weekly_all", "7 days", percent, parse_reset(limit.resets_at.as_deref()))),
                _ => {}
            }
        }
    }
    if session.is_none() {
        if let Some(w) = payload.five_hour.and_then(|w| w.utilization.map(|u| (u, w.resets_at))) {
            session = Some(normalize_window("session", "5 hours", w.0, parse_reset(w.1.as_deref())));
        }
    }
    if weekly.is_none() {
        if let Some(w) = payload.seven_day.and_then(|w| w.utilization.map(|u| (u, w.resets_at))) {
            weekly = Some(normalize_window("weekly_all", "7 days", w.0, parse_reset(w.1.as_deref())));
        }
    }
    let windows: Vec<_> = [session, weekly].into_iter().flatten().collect();
    if windows.is_empty() { Err((ProviderStatus::Unavailable, "Claude returned no supported usage windows".into())) } else { Ok(windows) }
}

fn parse_reset(value: Option<&str>) -> Option<i64> {
    value.and_then(|raw| DateTime::parse_from_rfc3339(raw).ok()).map(|dt| dt.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_current_limits_array() {
        let payload: ApiResponse = serde_json::from_value(serde_json::json!({"limits":[{"kind":"session","percent":18.0,"resets_at":"2026-09-12T20:00:00Z","is_active":true},{"kind":"weekly_all","percent":67.0,"resets_at":"2026-09-14T20:00:00Z","is_active":true}]})).unwrap();
        let windows = parse_windows(payload).unwrap();
        assert_eq!(windows[0].remaining_percent, Some(82.0));
        assert_eq!(windows[1].remaining_percent, Some(33.0));
    }

    #[test]
    fn parses_legacy_shape() {
        let payload: ApiResponse = serde_json::from_value(serde_json::json!({"five_hour":{"utilization":20.0,"resets_at":"2026-09-12T20:00:00+00:00"},"seven_day":{"utilization":40.0,"resets_at":null}})).unwrap();
        let windows = parse_windows(payload).unwrap();
        assert_eq!(windows.len(), 2);
    }

    #[test]
    fn malformed_response_fails_closed() {
        let payload: ApiResponse = serde_json::from_value(serde_json::json!({"limits":[]})).unwrap();
        assert!(parse_windows(payload).is_err());
    }
}
