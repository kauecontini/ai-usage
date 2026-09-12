use crate::core::{normalize::{label_for_minutes, normalize_window}, types::{ProviderId, ProviderStatus, ProviderUsage}};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{io, process::Stdio, time::Duration};
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, process::{Child, Command}, time::timeout};

const SOURCE: &str = "codex_app_server";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimitWindow { used_percent: f64, window_duration_mins: Option<i64>, resets_at: Option<i64> }

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimitSnapshot { limit_id: Option<String>, limit_name: Option<String>, primary: Option<RateLimitWindow>, secondary: Option<RateLimitWindow> }

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimitsResponse { rate_limits: RateLimitSnapshot, rate_limits_by_limit_id: Option<std::collections::HashMap<String, RateLimitSnapshot>> }

pub async fn fetch_usage() -> ProviderUsage {
    match fetch_inner().await {
        Ok(usage) => usage,
        Err(error) => {
            let status = classify_error(&error);
            ProviderUsage { provider: ProviderId::Openai, windows: vec![], last_updated: None, source: SOURCE.into(), status, error: Some(safe_error(&error)) }
        }
    }
}

async fn fetch_inner() -> Result<ProviderUsage, String> {
    let mut child = spawn_codex().map_err(|e| format!("Codex CLI unavailable: {e}"))?;
    let stdin = child.stdin.take().ok_or("Codex app-server stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("Codex app-server stdout unavailable")?;
    let mut writer = stdin;
    let mut reader = BufReader::new(stdout).lines();

    send(&mut writer, &json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"usage","title":"Usage","version":"0.1.0"},"capabilities":{"experimentalApi":true}}})).await?;
    wait_result(&mut reader, 1).await?;
    send(&mut writer, &json!({"method":"initialized"})).await?;
    send(&mut writer, &json!({"id":2,"method":"account/rateLimits/read","params":{"excludeResetCreditDetails":true}})).await?;
    let value = wait_result(&mut reader, 2).await?;
    let _ = child.kill().await;

    let response: RateLimitsResponse = serde_json::from_value(value).map_err(|_| "Codex rate-limit response schema changed".to_string())?;
    let snapshot = response.rate_limits_by_limit_id
        .and_then(|mut m| m.remove("codex"))
        .unwrap_or(response.rate_limits);

    let mut windows = Vec::new();
    if let Some(w) = snapshot.primary {
        windows.push(normalize_window("primary", label_for_minutes(w.window_duration_mins, "Primary"), w.used_percent, w.resets_at));
    }
    if let Some(w) = snapshot.secondary {
        windows.push(normalize_window("secondary", label_for_minutes(w.window_duration_mins, "Secondary"), w.used_percent, w.resets_at));
    }
    if windows.is_empty() { return Err("Codex returned no subscription usage windows".into()); }

    let suffix = snapshot.limit_name.or(snapshot.limit_id).unwrap_or_else(|| "codex".into());
    Ok(ProviderUsage { provider: ProviderId::Openai, windows, last_updated: Some(Utc::now().timestamp()), source: format!("{SOURCE}:{suffix}"), status: ProviderStatus::Fresh, error: None })
}

fn spawn_codex() -> io::Result<Child> {
    let mut command = Command::new("codex");
    command.args(["app-server", "--listen", "stdio://"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn()
}

async fn send(writer: &mut tokio::process::ChildStdin, message: &Value) -> Result<(), String> {
    let mut line = serde_json::to_vec(message).map_err(|_| "Unable to encode Codex request")?;
    line.push(b'\n');
    writer.write_all(&line).await.map_err(|_| "Unable to write to Codex app-server")?;
    writer.flush().await.map_err(|_| "Unable to flush Codex app-server request")
}

async fn wait_result(reader: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>, id: i64) -> Result<Value, String> {
    timeout(REQUEST_TIMEOUT, async {
        while let Some(line) = reader.next_line().await.map_err(|_| "Unable to read Codex app-server")? {
            let value: Value = match serde_json::from_str(&line) { Ok(v) => v, Err(_) => continue };
            if value.get("id").and_then(Value::as_i64) != Some(id) { continue; }
            if let Some(error) = value.get("error") {
                let message = error.get("message").and_then(Value::as_str).unwrap_or("Codex request failed");
                return Err(message.to_string());
            }
            return value.get("result").cloned().ok_or_else(|| "Codex response missing result".to_string());
        }
        Err("Codex app-server closed unexpectedly".into())
    }).await.map_err(|_| "Codex app-server timed out".to_string())?
}

fn classify_error(error: &str) -> ProviderStatus {
    let lower = error.to_ascii_lowercase();
    if lower.contains("not found") || lower.contains("unavailable") || lower.contains("cannot find") { ProviderStatus::Unsupported }
    else if lower.contains("auth") || lower.contains("login") || lower.contains("sign in") || lower.contains("401") { ProviderStatus::AuthenticationRequired }
    else if lower.contains("429") || lower.contains("rate limit") { ProviderStatus::RateLimited }
    else { ProviderStatus::Unavailable }
}

fn safe_error(error: &str) -> String {
    if error.len() > 180 { "Codex usage query failed".into() } else { error.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_current_rate_limit_shape() {
        let raw = json!({"rateLimits":{"limitId":"codex","limitName":"Codex","primary":{"usedPercent":28.0,"windowDurationMins":300,"resetsAt":2000},"secondary":{"usedPercent":54.0,"windowDurationMins":10080,"resetsAt":3000}},"rateLimitsByLimitId":null});
        let p: RateLimitsResponse = serde_json::from_value(raw).unwrap();
        assert_eq!(p.rate_limits.primary.unwrap().used_percent, 28.0);
    }
}
