mod core;
mod providers;

use crate::core::{
    redaction::redact,
    refresh::{merge_with_previous, BackoffState},
    settings::{self, AppPaths, AppSettings},
    types::{ProviderId, ProviderStatus, UsageSnapshot},
};
use chrono::Utc;
use std::{
    collections::{HashMap, HashSet},
    fs::OpenOptions,
    io::Write,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Manager, PhysicalPosition, State, WebviewWindow,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as AutostartManagerExt};
use tauri_plugin_notification::NotificationExt;

#[derive(Clone)]
struct SharedState {
    snapshot: Arc<RwLock<UsageSnapshot>>,
    settings: Arc<RwLock<AppSettings>>,
    paths: AppPaths,
    notifications: Arc<Mutex<HashMap<String, HashSet<u8>>>>,
    openai_poll: Arc<Mutex<ProviderPoll>>,
    anthropic_poll: Arc<Mutex<ProviderPoll>>,
}

impl SharedState {
    fn new(paths: AppPaths) -> Self {
        let now = Utc::now().timestamp();
        let settings_value = settings::load(&paths.settings);
        let cached = std::fs::read_to_string(&paths.cache)
            .ok()
            .and_then(|s| serde_json::from_str::<UsageSnapshot>(&s).ok())
            .map(mark_cache_stale)
            .unwrap_or_else(|| UsageSnapshot::empty(now));
        Self {
            snapshot: Arc::new(RwLock::new(cached)),
            settings: Arc::new(RwLock::new(settings_value)),
            paths,
            notifications: Arc::new(Mutex::new(HashMap::new())),
            openai_poll: Arc::new(Mutex::new(ProviderPoll::default())),
            anthropic_poll: Arc::new(Mutex::new(ProviderPoll::default())),
        }
    }
}

#[derive(Debug, Default)]
struct ProviderPoll {
    backoff: BackoffState,
    next_attempt_at: i64,
}

impl ProviderPoll {
    fn should_attempt(&self, now: i64, force: bool) -> bool {
        force || now >= self.next_attempt_at
    }

    fn record(&mut self, fresh: bool, now: i64, base: Duration) {
        if fresh {
            self.backoff.record_success();
        } else {
            self.backoff.record_failure();
        }
        self.next_attempt_at = now.saturating_add(self.backoff.delay(base).as_secs() as i64);
    }
}

fn mark_cache_stale(mut snapshot: UsageSnapshot) -> UsageSnapshot {
    for provider in &mut snapshot.providers {
        if !provider.windows.is_empty() {
            provider.status = ProviderStatus::Stale;
        }
    }
    snapshot
}

#[tauri::command]
fn get_snapshot(state: State<'_, SharedState>) -> UsageSnapshot {
    state
        .snapshot
        .read()
        .expect("snapshot lock poisoned")
        .clone()
}

#[tauri::command]
fn get_settings(state: State<'_, SharedState>) -> AppSettings {
    state
        .settings
        .read()
        .expect("settings lock poisoned")
        .clone()
}

#[tauri::command]
async fn refresh_now(
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<UsageSnapshot, String> {
    Ok(refresh_all(&app, state.inner().clone(), true).await)
}

#[tauri::command]
fn save_settings(
    app: AppHandle,
    state: State<'_, SharedState>,
    settings: AppSettings,
) -> Result<AppSettings, String> {
    let mut validated = settings.validate();

    // Window coordinates are runtime-owned state. Capture the actual current position
    // instead of trusting potentially stale coordinates sent back by the webview.
    if let Some(window) = app.get_webview_window("main") {
        if let Ok(position) = window.outer_position() {
            validated.widget_x = Some(position.x);
            validated.widget_y = Some(position.y);
        }
    } else if let Ok(current) = state.settings.read() {
        validated.widget_x = current.widget_x;
        validated.widget_y = current.widget_y;
    }

    apply_runtime_settings(&app, &validated)?;
    settings::save(&state.paths.settings, &validated)
        .map_err(|_| "Unable to save settings".to_string())?;
    *state
        .settings
        .write()
        .map_err(|_| "Settings unavailable".to_string())? = validated.clone();
    Ok(validated)
}

#[tauri::command]
fn set_surface(window: WebviewWindow, surface: String) -> Result<(), String> {
    let (width, height) = match surface.as_str() {
        "compact" => (334.0, 60.0),
        "detail" => (366.0, 344.0),
        "settings" => (366.0, 408.0),
        "onboarding" => (366.0, 246.0),
        _ => return Err("Unknown surface".into()),
    };
    window
        .set_size(tauri::LogicalSize::new(width, height))
        .map_err(|_| "Unable to resize Usage".to_string())
}

fn apply_runtime_settings(app: &AppHandle, settings: &AppSettings) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        window
            .set_always_on_top(settings.always_on_top)
            .map_err(|_| "Unable to change always-on-top".to_string())?;
    }
    let manager = app.autolaunch();
    if settings.launch_at_startup {
        manager
            .enable()
            .map_err(|_| "Unable to enable launch at startup".to_string())?;
    } else {
        manager
            .disable()
            .map_err(|_| "Unable to disable launch at startup".to_string())?;
    }
    Ok(())
}

async fn refresh_all(app: &AppHandle, state: SharedState, force: bool) -> UsageSnapshot {
    let now = Utc::now().timestamp();
    let base_minutes = state
        .settings
        .read()
        .expect("settings lock poisoned")
        .refresh_interval_minutes
        .clamp(1, 60);
    let base = Duration::from_secs(base_minutes * 60);
    let openai_due = state
        .openai_poll
        .lock()
        .expect("OpenAI poll lock poisoned")
        .should_attempt(now, force);
    let anthropic_due = state
        .anthropic_poll
        .lock()
        .expect("Anthropic poll lock poisoned")
        .should_attempt(now, force);

    let (openai, anthropic) = tokio::join!(
        async {
            if openai_due {
                Some(providers::openai::fetch_usage().await)
            } else {
                None
            }
        },
        async {
            if anthropic_due {
                Some(providers::anthropic::fetch_usage().await)
            } else {
                None
            }
        }
    );

    let previous = state
        .snapshot
        .read()
        .expect("snapshot lock poisoned")
        .clone();

    let merged_openai = if let Some(current) = openai {
        state
            .openai_poll
            .lock()
            .expect("OpenAI poll lock poisoned")
            .record(current.status == ProviderStatus::Fresh, now, base);
        merge_with_previous(
            previous
                .providers
                .iter()
                .find(|p| p.provider == ProviderId::Openai),
            current,
        )
    } else {
        previous
            .providers
            .iter()
            .find(|p| p.provider == ProviderId::Openai)
            .cloned()
            .unwrap_or_else(|| {
                crate::core::types::ProviderUsage::empty(ProviderId::Openai, "codex_app_server")
            })
    };

    let merged_anthropic = if let Some(current) = anthropic {
        state
            .anthropic_poll
            .lock()
            .expect("Anthropic poll lock poisoned")
            .record(current.status == ProviderStatus::Fresh, now, base);
        merge_with_previous(
            previous
                .providers
                .iter()
                .find(|p| p.provider == ProviderId::Anthropic),
            current,
        )
    } else {
        previous
            .providers
            .iter()
            .find(|p| p.provider == ProviderId::Anthropic)
            .cloned()
            .unwrap_or_else(|| {
                crate::core::types::ProviderUsage::empty(
                    ProviderId::Anthropic,
                    "claude_oauth_usage_unofficial",
                )
            })
    };

    let snapshot = UsageSnapshot {
        providers: vec![merged_openai, merged_anthropic],
        refreshed_at: now,
    };
    if let Err(e) = settings::atomic_json_write(&state.paths.cache, &snapshot) {
        diagnostic(&state.paths, &format!("cache write failed: {e}"));
    }
    maybe_notify(app, &state, &snapshot);
    *state.snapshot.write().expect("snapshot lock poisoned") = snapshot.clone();
    snapshot
}

fn maybe_notify(app: &AppHandle, state: &SharedState, snapshot: &UsageSnapshot) {
    if !state
        .settings
        .read()
        .expect("settings lock poisoned")
        .notifications
    {
        return;
    }
    let mut sent = state
        .notifications
        .lock()
        .expect("notification lock poisoned");
    for provider in &snapshot.providers {
        if provider.status != ProviderStatus::Fresh {
            continue;
        }
        for window in &provider.windows {
            let Some(remaining) = window.remaining_percent else {
                continue;
            };
            let threshold = if remaining <= 5.0 {
                Some(5)
            } else if remaining <= 10.0 {
                Some(10)
            } else if remaining <= 20.0 {
                Some(20)
            } else {
                None
            };
            let Some(threshold) = threshold else {
                continue;
            };
            let key = format!(
                "{:?}:{}:{}",
                provider.provider,
                window.id,
                window.reset_at.unwrap_or_default()
            );
            let thresholds = sent.entry(key).or_default();
            if !thresholds.insert(threshold) {
                continue;
            }
            let name = match provider.provider {
                ProviderId::Openai => "ChatGPT / Codex",
                ProviderId::Anthropic => "Claude",
            };
            let _ = app
                .notification()
                .builder()
                .title("Usage")
                .body(format!(
                    "{name}: {}% remaining ({})",
                    remaining.round(),
                    window.label
                ))
                .show();
        }
    }
}

fn diagnostic(paths: &AppPaths, message: &str) {
    let path = paths.logs.join("usage.log");
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{} {}", Utc::now().to_rfc3339(), redact(message));
    }
}

fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show widget", true, None::<&str>)?;
    let hide = MenuItem::with_id(app, "hide", "Hide widget", true, None::<&str>)?;
    let refresh = MenuItem::with_id(app, "refresh", "Refresh now", true, None::<&str>)?;
    let logs = MenuItem::with_id(app, "logs", "Open logs folder", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Exit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &hide, &refresh, &logs, &quit])?;
    let icon = app.default_window_icon().cloned();
    let mut builder = TrayIconBuilder::with_id("usage-tray")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("Usage");
    if let Some(icon) = icon {
        builder = builder.icon(icon);
    }
    builder
        .on_tray_icon_event(|tray, event| {
            if let tauri::tray::TrayIconEvent::Click {
                button: tauri::tray::MouseButton::Left,
                button_state: tauri::tray::MouseButtonState::Up,
                ..
            } = event
            {
                if let Some(window) = tray.app_handle().get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        })
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            "hide" => {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            }
            "refresh" => {
                let app = app.clone();
                let state = app.state::<SharedState>().inner().clone();
                tauri::async_runtime::spawn(async move {
                    refresh_all(&app, state, true).await;
                });
            }
            "logs" => {
                let state = app.state::<SharedState>();
                #[cfg(windows)]
                {
                    let _ = std::process::Command::new("explorer.exe")
                        .arg(&state.paths.logs)
                        .spawn();
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(())
}

fn restore_position(window: &WebviewWindow, settings: &AppSettings) {
    let (Some(x), Some(y)) = (settings.widget_x, settings.widget_y) else {
        return;
    };
    let Ok(size) = window.outer_size() else {
        return;
    };
    let Ok(monitors) = window.available_monitors() else {
        return;
    };

    let width = i32::try_from(size.width).unwrap_or(i32::MAX);
    let height = i32::try_from(size.height).unwrap_or(i32::MAX);
    for monitor in &monitors {
        let area = monitor.work_area();
        let left = area.position.x;
        let top = area.position.y;
        let right = left.saturating_add(i32::try_from(area.size.width).unwrap_or(i32::MAX));
        let bottom = top.saturating_add(i32::try_from(area.size.height).unwrap_or(i32::MAX));
        let visibly_intersects =
            x.saturating_add(80) > left && x < right && y.saturating_add(40) > top && y < bottom;
        if visibly_intersects {
            let max_x = right.saturating_sub(width).max(left);
            let max_y = bottom.saturating_sub(height).max(top);
            let _ = window.set_position(PhysicalPosition::new(
                x.clamp(left, max_x),
                y.clamp(top, max_y),
            ));
            return;
        }
    }

    if let Ok(Some(primary)) = window.primary_monitor() {
        let area = primary.work_area();
        let _ = window.set_position(PhysicalPosition::new(
            area.position.x.saturating_add(16),
            area.position.y.saturating_add(16),
        ));
    }
}

fn start_scheduler(app: AppHandle, state: SharedState) {
    tauri::async_runtime::spawn(async move {
        refresh_all(&app, state.clone(), true).await;
        loop {
            let minutes = state
                .settings
                .read()
                .expect("settings lock poisoned")
                .refresh_interval_minutes;
            tokio::time::sleep(Duration::from_secs(minutes.clamp(1, 60) * 60)).await;
            refresh_all(&app, state.clone(), false).await;
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let paths = AppPaths::discover().expect("Usage local data directory unavailable");
    let state = SharedState::new(paths);
    let initial_settings = state
        .settings
        .read()
        .expect("settings lock poisoned")
        .clone();

    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_notification::init());
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ));
    }

    builder
        .manage(state.clone())
        .setup(move |app| {
            setup_tray(app)?;
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_always_on_top(initial_settings.always_on_top);
                restore_position(&window, &initial_settings);
            }
            start_scheduler(app.handle().clone(), state.clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Moved(position) = event {
                let state = window.state::<SharedState>();
                let settings_path = state.paths.settings.clone();
                let write_result = state.settings.write();
                if let Ok(mut current) = write_result {
                    current.widget_x = Some(position.x);
                    current.widget_y = Some(position.y);
                    let _ = settings::save(&settings_path, &current);
                }
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            get_settings,
            refresh_now,
            save_settings,
            set_surface
        ])
        .run(tauri::generate_context!())
        .expect("error while running Usage");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_provider_failure_does_not_replace_other_provider() {
        let now = 1;
        let mut snapshot = UsageSnapshot::empty(now);
        snapshot.providers[0].status = ProviderStatus::Fresh;
        snapshot.providers[1].status = ProviderStatus::Unavailable;
        assert_eq!(snapshot.providers[0].status, ProviderStatus::Fresh);
        assert_eq!(snapshot.providers[1].status, ProviderStatus::Unavailable);
    }

    #[test]
    fn cache_is_marked_stale() {
        let mut snapshot = UsageSnapshot::empty(1);
        snapshot.providers[0]
            .windows
            .push(crate::core::normalize::normalize_window(
                "p", "5 hours", 20.0, None,
            ));
        snapshot.providers[0].status = ProviderStatus::Fresh;
        assert_eq!(
            mark_cache_stale(snapshot).providers[0].status,
            ProviderStatus::Stale
        );
    }
}
