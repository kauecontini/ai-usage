use crate::core::{
    normalize::normalize_window,
    types::{ProviderId, ProviderStatus, ProviderUsage},
};
#[cfg(windows)]
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
#[cfg(windows)]
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use chrono::{DateTime, Utc};
use reqwest::{redirect::Policy, Client, StatusCode};
use serde::Deserialize;
use serde_json::Value;
use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};
#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::Cryptography::{CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB},
};

const SOURCE: &str = "claude_oauth_usage_unofficial";
const ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";
const USER_AGENT: &str = "claude-code/usage-0.1.0 (external, cli)";

#[derive(Debug, Deserialize)]
struct ApiResponse {
    limits: Option<Vec<Limit>>,
    five_hour: Option<LegacyWindow>,
    seven_day: Option<LegacyWindow>,
}
#[derive(Debug, Deserialize)]
struct Limit {
    kind: Option<String>,
    percent: Option<f64>,
    resets_at: Option<String>,
}
#[derive(Debug, Deserialize)]
struct LegacyWindow {
    utilization: Option<f64>,
    resets_at: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CredentialSource {
    ClaudeCode,
    ClaudeDesktop,
}

struct AccessToken {
    value: String,
    source: CredentialSource,
}

pub async fn fetch_usage() -> ProviderUsage {
    match fetch_inner().await {
        Ok(v) => v,
        Err((status, message)) => ProviderUsage {
            provider: ProviderId::Anthropic,
            windows: vec![],
            last_updated: None,
            source: SOURCE.into(),
            status,
            error: Some(message),
        },
    }
}

async fn fetch_inner() -> Result<ProviderUsage, (ProviderStatus, String)> {
    let token = read_access_token()?;
    let client = Client::builder()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(12))
        .user_agent(USER_AGENT)
        .build()
        .map_err(|_| {
            (
                ProviderStatus::Unavailable,
                "Unable to initialize Anthropic client".into(),
            )
        })?;

    let mut response = send_usage_request(&client, &token.value).await?;
    #[cfg(windows)]
    if token.source == CredentialSource::ClaudeCode
        && matches!(
            response.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        )
    {
        match read_claude_desktop_access_token() {
            Ok(desktop_token) => {
                response = send_usage_request(&client, &desktop_token).await?;
            }
            Err((ProviderStatus::Unavailable, message)) => {
                return Err((ProviderStatus::Unavailable, message));
            }
            Err(_) => {}
        }
    }

    if response.status().is_redirection() {
        return Err((
            ProviderStatus::Unavailable,
            "Anthropic usage endpoint redirected unexpectedly".into(),
        ));
    }
    match response.status() {
        StatusCode::OK => {}
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            return Err((
                ProviderStatus::AuthenticationRequired,
                "Claude sign-in needs attention".into(),
            ))
        }
        StatusCode::TOO_MANY_REQUESTS => {
            return Err((
                ProviderStatus::RateLimited,
                "Claude usage endpoint rate limited".into(),
            ))
        }
        _ => {
            return Err((
                ProviderStatus::Unavailable,
                format!(
                    "Claude usage endpoint returned HTTP {}",
                    response.status().as_u16()
                ),
            ))
        }
    }
    let payload: ApiResponse = response.json().await.map_err(|_| {
        (
            ProviderStatus::Unavailable,
            "Claude usage response schema changed".into(),
        )
    })?;
    let windows = parse_windows(payload)?;
    Ok(ProviderUsage {
        provider: ProviderId::Anthropic,
        windows,
        last_updated: Some(Utc::now().timestamp()),
        source: SOURCE.into(),
        status: ProviderStatus::Fresh,
        error: None,
    })
}

async fn send_usage_request(
    client: &Client,
    token: &str,
) -> Result<reqwest::Response, (ProviderStatus, String)> {
    client
        .get(ENDPOINT)
        .bearer_auth(token)
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("anthropic-version", "2023-06-01")
        .header("x-app", "cli")
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|_| {
            (
                ProviderStatus::Unavailable,
                "Anthropic usage request failed".into(),
            )
        })
}

fn credential_path() -> Result<PathBuf, (ProviderStatus, String)> {
    if let Some(config) = env::var_os("CLAUDE_CONFIG_DIR") {
        return Ok(PathBuf::from(config).join(".credentials.json"));
    }
    let home = dirs::home_dir().ok_or((
        ProviderStatus::Unsupported,
        "User profile directory unavailable".into(),
    ))?;
    Ok(home.join(".claude").join(".credentials.json"))
}

fn read_access_token() -> Result<AccessToken, (ProviderStatus, String)> {
    if let Some(token) = read_claude_code_access_token()? {
        return Ok(AccessToken {
            value: token,
            source: CredentialSource::ClaudeCode,
        });
    }
    #[cfg(windows)]
    {
        read_claude_desktop_access_token().map(|value| AccessToken {
            value,
            source: CredentialSource::ClaudeDesktop,
        })
    }
    #[cfg(not(windows))]
    Err((
        ProviderStatus::AuthenticationRequired,
        "Claude sign-in not detected".into(),
    ))
}

fn read_claude_code_access_token() -> Result<Option<String>, (ProviderStatus, String)> {
    let path = credential_path()?;
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err((
                ProviderStatus::Unavailable,
                "Unable to read Claude Code credential store".into(),
            ))
        }
    };
    let root: Value = serde_json::from_str(&text).map_err(|_| {
        (
            ProviderStatus::Unavailable,
            "Claude Code credential store format changed".into(),
        )
    })?;
    let token = root
        .pointer("/claudeAiOauth/accessToken")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty());
    Ok(token.map(str::to_owned))
}

#[cfg(windows)]
fn read_claude_desktop_access_token() -> Result<String, (ProviderStatus, String)> {
    let app_data = env::var_os("APPDATA").map(PathBuf::from);
    let local_app_data = env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let user_data_dirs =
        claude_desktop_user_data_dirs(app_data.as_deref(), local_app_data.as_deref());
    if user_data_dirs.is_empty() {
        return Err((
            ProviderStatus::Unsupported,
            "Claude Desktop installation not detected".into(),
        ));
    }

    let mut found_cache = false;
    let mut storage_unavailable = false;
    for user_data in user_data_dirs {
        let config_text = match fs::read_to_string(user_data.join("config.json")) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                storage_unavailable = true;
                continue;
            }
        };
        let config: Value = match serde_json::from_str(&config_text) {
            Ok(value) => value,
            Err(_) => {
                storage_unavailable = true;
                continue;
            }
        };
        let encrypted_caches = match encrypted_cache_values(&config) {
            Ok(values) => values,
            Err(()) => {
                storage_unavailable = true;
                continue;
            }
        };
        if encrypted_caches.is_empty() {
            continue;
        }
        found_cache = true;

        let state_text = match fs::read_to_string(user_data.join("Local State")) {
            Ok(text) => text,
            Err(_) => {
                storage_unavailable = true;
                continue;
            }
        };
        let protected_key = match protected_key_from_local_state(&state_text) {
            Ok(key) => key,
            Err(()) => {
                storage_unavailable = true;
                continue;
            }
        };
        let key = match dpapi_unprotect(&protected_key) {
            Ok(key) => key,
            Err(()) => {
                storage_unavailable = true;
                continue;
            }
        };

        for encrypted_cache in encrypted_caches {
            let plaintext = match decrypt_chromium_v10(&key, encrypted_cache) {
                Ok(plaintext) => plaintext,
                Err(()) => {
                    storage_unavailable = true;
                    continue;
                }
            };
            let cache: Value = match serde_json::from_slice(&plaintext) {
                Ok(value) => value,
                Err(_) => {
                    storage_unavailable = true;
                    continue;
                }
            };
            if let Some(token) = select_anthropic_inference_token(&cache) {
                return Ok(token.to_owned());
            }
        }
    }

    if storage_unavailable && found_cache {
        Err((
            ProviderStatus::Unavailable,
            "Claude Desktop credential storage format is unavailable".into(),
        ))
    } else {
        Err((
            ProviderStatus::AuthenticationRequired,
            "Claude Desktop sign-in not detected".into(),
        ))
    }
}

#[cfg(windows)]
fn claude_desktop_user_data_dirs(
    app_data: Option<&Path>,
    local_app_data: Option<&Path>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(app_data) = app_data {
        candidates.push(app_data.join("Claude"));
    }
    if let Some(local_app_data) = local_app_data {
        let packages = local_app_data.join("Packages");
        let mut msix_candidates = Vec::new();
        if let Ok(entries) = fs::read_dir(packages) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) && name.starts_with("claude_")
                {
                    msix_candidates.push(
                        entry
                            .path()
                            .join("LocalCache")
                            .join("Roaming")
                            .join("Claude"),
                    );
                }
            }
        }
        msix_candidates.sort();
        candidates.extend(msix_candidates);
    }
    candidates.retain(|candidate| candidate.is_dir());
    candidates.dedup();
    candidates
}

#[cfg(windows)]
fn encrypted_cache_values(config: &Value) -> Result<Vec<&str>, ()> {
    let mut values = Vec::new();
    for key in ["oauth:tokenCacheV2", "oauth:tokenCache"] {
        if let Some(value) = config.get(key) {
            let encoded = value.as_str().ok_or(())?;
            if !encoded.is_empty() {
                values.push(encoded);
            }
        }
    }
    Ok(values)
}

#[cfg(windows)]
fn protected_key_from_local_state(local_state: &str) -> Result<Vec<u8>, ()> {
    let state: Value = serde_json::from_str(local_state).map_err(|_| ())?;
    let encoded = state
        .pointer("/os_crypt/encrypted_key")
        .and_then(Value::as_str)
        .ok_or(())?;
    let decoded = BASE64.decode(encoded).map_err(|_| ())?;
    decoded.strip_prefix(b"DPAPI").map(Vec::from).ok_or(())
}

#[cfg(windows)]
fn dpapi_unprotect(protected: &[u8]) -> Result<Vec<u8>, ()> {
    let input_len = u32::try_from(protected.len()).map_err(|_| ())?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: input_len,
        pbData: protected.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let result = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if result == 0 || output.pbData.is_null() {
        return Err(());
    }
    let plaintext =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        LocalFree(output.pbData.cast());
    }
    Ok(plaintext)
}

#[cfg(windows)]
fn decrypt_chromium_v10(key: &[u8], encoded: &str) -> Result<Vec<u8>, ()> {
    let blob = BASE64.decode(encoded).map_err(|_| ())?;
    let payload = blob.strip_prefix(b"v10").ok_or(())?;
    if payload.len() < 12 + 16 {
        return Err(());
    }
    let (nonce, ciphertext_and_tag) = payload.split_at(12);
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| ())?;
    cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext_and_tag)
        .map_err(|_| ())
}

#[cfg(windows)]
fn select_anthropic_inference_token(value: &Value) -> Option<&str> {
    let now_ms = Utc::now().timestamp_millis().max(0) as u64;
    select_anthropic_inference_token_at(value, now_ms)
}

#[cfg(windows)]
fn select_anthropic_inference_token_at(value: &Value, now_ms: u64) -> Option<&str> {
    let mut candidates = Vec::new();
    collect_inference_tokens(value, false, false, false, now_ms, &mut candidates);
    candidates
        .into_iter()
        .max_by_key(|(claude_code_scope, expires_at, _)| {
            (*claude_code_scope, expires_at.unwrap_or(0))
        })
        .map(|(_, _, token)| token)
}

#[cfg(windows)]
fn collect_inference_tokens<'a>(
    value: &'a Value,
    host_context: bool,
    scope_context: bool,
    claude_code_context: bool,
    now_ms: u64,
    candidates: &mut Vec<(bool, Option<u64>, &'a str)>,
) {
    match value {
        Value::Object(object) => {
            let direct_host = object.iter().any(|(key, value)| {
                key.eq_ignore_ascii_case("host")
                    && scalar_contains_marker(value, "api.anthropic.com")
            });
            let direct_scope = object.iter().any(|(key, value)| {
                (key.eq_ignore_ascii_case("scope") || key.eq_ignore_ascii_case("scopes"))
                    && scalar_contains_marker(value, "user:inference")
            });
            let direct_claude_code_scope = object.iter().any(|(key, value)| {
                (key.eq_ignore_ascii_case("scope") || key.eq_ignore_ascii_case("scopes"))
                    && scalar_contains_marker(value, "user:sessions:claude_code")
            });
            let host_context = host_context || direct_host;
            let scope_context = scope_context || direct_scope;
            let claude_code_context = claude_code_context || direct_claude_code_scope;
            if host_context && scope_context {
                let expires_at = object
                    .get("expiresAt")
                    .or_else(|| object.get("expires_at"))
                    .and_then(expiration_millis);
                let is_current = expires_at.is_none_or(|expires_at| expires_at > now_ms);
                if is_current {
                    for key in ["token", "accessToken"] {
                        if let Some(token) = object
                            .get(key)
                            .and_then(Value::as_str)
                            .filter(|token| !token.is_empty())
                        {
                            candidates.push((claude_code_context, expires_at, token));
                            break;
                        }
                    }
                }
            }
            for (key, child) in object {
                if key == "token" || key == "accessToken" || key == "refreshToken" {
                    continue;
                }
                collect_inference_tokens(
                    child,
                    host_context || key.contains("api.anthropic.com"),
                    scope_context || key.contains("user:inference"),
                    claude_code_context || key.contains("user:sessions:claude_code"),
                    now_ms,
                    candidates,
                );
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_inference_tokens(
                    child,
                    host_context,
                    scope_context,
                    claude_code_context,
                    now_ms,
                    candidates,
                );
            }
        }
        _ => {}
    }
}

#[cfg(windows)]
fn scalar_contains_marker(value: &Value, marker: &str) -> bool {
    match value {
        Value::String(text) => text.contains(marker),
        Value::Array(values) => values
            .iter()
            .any(|value| scalar_contains_marker(value, marker)),
        _ => false,
    }
}

#[cfg(windows)]
fn expiration_millis(value: &Value) -> Option<u64> {
    if let Some(number) = value.as_u64() {
        return Some(if number < 10_000_000_000 {
            number.saturating_mul(1_000)
        } else {
            number
        });
    }
    value
        .as_str()
        .and_then(|text| text.parse::<u64>().ok())
        .or_else(|| {
            value
                .as_str()
                .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
                .map(|date| date.timestamp_millis().max(0) as u64)
        })
}

fn parse_windows(
    payload: ApiResponse,
) -> Result<Vec<crate::core::types::UsageWindow>, (ProviderStatus, String)> {
    let mut session = None;
    let mut weekly = None;
    if let Some(limits) = payload.limits {
        for limit in limits {
            let Some(percent) = limit.percent else {
                continue;
            };
            match limit.kind.as_deref() {
                Some("session") => {
                    session = Some(normalize_window(
                        "session",
                        "5 hours",
                        percent,
                        parse_reset(limit.resets_at.as_deref()),
                    ))
                }
                Some("weekly_all") => {
                    weekly = Some(normalize_window(
                        "weekly_all",
                        "7 days",
                        percent,
                        parse_reset(limit.resets_at.as_deref()),
                    ))
                }
                _ => {}
            }
        }
    }
    if session.is_none() {
        if let Some(w) = payload
            .five_hour
            .and_then(|w| w.utilization.map(|u| (u, w.resets_at)))
        {
            session = Some(normalize_window(
                "session",
                "5 hours",
                w.0,
                parse_reset(w.1.as_deref()),
            ));
        }
    }
    if weekly.is_none() {
        if let Some(w) = payload
            .seven_day
            .and_then(|w| w.utilization.map(|u| (u, w.resets_at)))
        {
            weekly = Some(normalize_window(
                "weekly_all",
                "7 days",
                w.0,
                parse_reset(w.1.as_deref()),
            ));
        }
    }
    let windows: Vec<_> = [session, weekly].into_iter().flatten().collect();
    if windows.is_empty() {
        Err((
            ProviderStatus::Unavailable,
            "Claude returned no supported usage windows".into(),
        ))
    } else {
        Ok(windows)
    }
}

fn parse_reset(value: Option<&str>) -> Option<i64> {
    value
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|dt| dt.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    struct TestDir(PathBuf);

    #[cfg(windows)]
    impl TestDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "usage-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    #[cfg(windows)]
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn parses_current_limits_array_even_when_weekly_is_not_active() {
        let payload: ApiResponse = serde_json::from_value(serde_json::json!({"limits":[{"kind":"session","percent":18.0,"resets_at":"2026-09-12T20:00:00Z","is_active":true},{"kind":"weekly_all","percent":67.0,"resets_at":"2026-09-14T20:00:00Z","is_active":false}]})).unwrap();
        let windows = parse_windows(payload).unwrap();
        assert_eq!(windows.len(), 2);
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
        let payload: ApiResponse =
            serde_json::from_value(serde_json::json!({"limits":[]})).unwrap();
        assert!(parse_windows(payload).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn resolves_classic_and_msix_claude_user_data() {
        let fixture = TestDir::new("claude-paths");
        let app_data = fixture.0.join("Roaming");
        let local_app_data = fixture.0.join("Local");
        let classic = app_data.join("Claude");
        let msix = local_app_data
            .join("Packages")
            .join("Claude_synthetic")
            .join("LocalCache")
            .join("Roaming")
            .join("Claude");
        fs::create_dir_all(&classic).unwrap();
        fs::create_dir_all(&msix).unwrap();
        let candidates = claude_desktop_user_data_dirs(Some(&app_data), Some(&local_app_data));
        assert_eq!(candidates, vec![classic, msix]);
    }

    #[cfg(windows)]
    #[test]
    fn parses_desktop_cache_keys_without_exposing_values() {
        let config = serde_json::json!({
            "oauth:tokenCacheV2": "synthetic-v2",
            "oauth:tokenCache": "synthetic-legacy"
        });
        assert_eq!(
            encrypted_cache_values(&config).unwrap(),
            vec!["synthetic-v2", "synthetic-legacy"]
        );
    }

    #[cfg(windows)]
    #[test]
    fn selects_only_anthropic_inference_access_token() {
        let cache = serde_json::json!({"entries":[
            {"host":"other.example","scope":"user:inference","token":"wrong-host"},
            {"host":"api.anthropic.com","scope":["profile:read"],"token":"wrong-scope"},
            {"host":"api.anthropic.com","scope":["user:inference"],"accessToken":"synthetic-access","refreshToken":"never-select"}
        ]});
        assert_eq!(
            select_anthropic_inference_token(&cache),
            Some("synthetic-access")
        );
    }

    #[cfg(windows)]
    #[test]
    fn selects_current_token_from_serialized_cache_key_context() {
        let cache = serde_json::json!({
            "{\"host\":\"api.anthropic.com\",\"scope\":\"profile:read\"}": {
                "token": "wrong-scope",
                "expiresAt": 4_000_000_000_000_u64
            },
            "{\"host\":\"api.anthropic.com\",\"scope\":\"user:inference\",\"account\":\"old\"}": {
                "token": "expired",
                "expiresAt": 1_000_u64
            },
            "{\"host\":\"api.anthropic.com\",\"scope\":\"user:inference user:file_upload\",\"account\":\"generic\"}": {
                "token": "generic-longer-lived",
                "expiresAt": 4_000_000_u64
            },
            "{\"host\":\"api.anthropic.com\",\"scope\":\"user:inference user:sessions:claude_code\",\"account\":\"current\"}": {
                "token": "synthetic-current",
                "expiresAt": 3_000_000_u64
            }
        });
        assert_eq!(
            select_anthropic_inference_token_at(&cache, 2_000_000),
            Some("synthetic-current")
        );
    }

    #[cfg(windows)]
    #[test]
    fn decrypts_synthetic_chromium_v10_blob() {
        let key = [7_u8; 32];
        let nonce = [3_u8; 12];
        let plaintext = br#"{"entries":[{"host":"api.anthropic.com","scope":"user:inference","token":"synthetic-token"}]}"#;
        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        let encrypted = cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext.as_ref())
            .unwrap();
        let mut blob = b"v10".to_vec();
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&encrypted);
        let encoded = BASE64.encode(blob);
        assert_eq!(decrypt_chromium_v10(&key, &encoded).unwrap(), plaintext);
    }

    #[cfg(windows)]
    #[test]
    fn malformed_and_unknown_desktop_blobs_fail_closed() {
        assert!(decrypt_chromium_v10(&[0_u8; 32], "not-base64").is_err());
        assert!(decrypt_chromium_v10(&[0_u8; 32], &BASE64.encode(b"v11unknown")).is_err());
        assert!(protected_key_from_local_state("{}").is_err());
    }
}
