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
    window_runtime: Arc<Mutex<WindowRuntime>>,
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
            window_runtime: Arc::new(Mutex::new(WindowRuntime::default())),
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
    let current = state
        .settings
        .read()
        .map_err(|_| "Settings unavailable".to_string())?
        .clone();
    let mut validated = settings.validate();

    // Detail and settings surfaces can move temporarily. Persist the compact
    // anchor only, never the current surface's transient position.
    validated.widget_x = current.widget_x;
    validated.widget_y = current.widget_y;

    apply_runtime_settings(&app, &current, &validated)?;
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
    let (width, height) = surface_size(&surface)?;
    let state = window.state::<SharedState>();
    let before = window
        .outer_position()
        .ok()
        .zip(window.outer_size().ok())
        .and_then(|(position, size)| {
            window.current_monitor().ok().flatten().map(|monitor| {
                (
                    WindowRect {
                        x: position.x,
                        y: position.y,
                        width: i32::try_from(size.width).unwrap_or(i32::MAX),
                        height: i32::try_from(size.height).unwrap_or(i32::MAX),
                    },
                    work_area_rect(&monitor),
                )
            })
        })
        .ok_or_else(|| "Unable to read Usage window position".to_string())?;

    let (before_rect, work_area) = before;
    let (previous_surface, compact_anchor) = {
        let mut runtime = state
            .window_runtime
            .lock()
            .map_err(|_| "Window state unavailable".to_string())?;
        let previous_surface = runtime.current_surface.clone();
        let compact_anchor = begin_surface_transition(&mut runtime, &surface, before_rect);
        (previous_surface, compact_anchor)
    };
    diagnostic(
        &state.paths,
        &format!("surface transition: {previous_surface} -> {surface}"),
    );

    let result = (|| {
        window
            .set_size(tauri::LogicalSize::new(width, height))
            .map_err(|_| "Unable to resize Usage".to_string())?;
        diagnostic(&state.paths, "surface resize ok");

        let size = window
            .outer_size()
            .map_err(|_| "Unable to read resized Usage window".to_string())?;
        let target = if surface == "compact" {
            compact_anchor.unwrap_or_else(|| {
                resized_window_rect(
                    before_rect,
                    i32::try_from(size.width).unwrap_or(i32::MAX),
                    i32::try_from(size.height).unwrap_or(i32::MAX),
                    work_area,
                )
            })
        } else {
            resized_window_rect(
                before_rect,
                i32::try_from(size.width).unwrap_or(i32::MAX),
                i32::try_from(size.height).unwrap_or(i32::MAX),
                work_area,
            )
        };

        // The Moved handler can run synchronously from set_position. Mark the
        // target before calling into the window API, but never hold the lock
        // across that call.
        {
            let mut runtime = state
                .window_runtime
                .lock()
                .map_err(|_| "Window state unavailable".to_string())?;
            mark_programmatic_position(&mut runtime, target);
        }
        window
            .set_position(PhysicalPosition::new(target.x, target.y))
            .map_err(|_| "Unable to position Usage".to_string())?;
        diagnostic(&state.paths, "surface position ok");

        {
            let mut runtime = state
                .window_runtime
                .lock()
                .map_err(|_| "Window state unavailable".to_string())?;
            finish_surface_transition(&mut runtime, &surface, target);
        }

        let always_on_top = state
            .settings
            .read()
            .map_err(|_| "Settings unavailable".to_string())?
            .always_on_top;
        // No runtime/settings guard is held while native window APIs run.
        enforce_windows_topmost(&window, always_on_top)
    })();

    if let Err(error) = &result {
        if let Ok(mut runtime) = state.window_runtime.lock() {
            abort_surface_transition(&mut runtime, &previous_surface);
        }
        diagnostic(&state.paths, &format!("surface transition failed: {error}"));
    }
    result
}

#[derive(Debug, Default)]
struct WindowRuntime {
    current_surface: String,
    compact_anchor: Option<WindowRect>,
    programmatic_position: Option<(i32, i32)>,
}

#[derive(Debug, PartialEq, Eq)]
enum MoveEventDecision {
    Ignore,
    PersistUserMove,
}

fn begin_surface_transition(
    runtime: &mut WindowRuntime,
    surface: &str,
    before_rect: WindowRect,
) -> Option<WindowRect> {
    if runtime.current_surface == "compact" && surface != "compact" {
        runtime.compact_anchor = Some(before_rect);
    }
    runtime.current_surface = "transition".into();
    runtime.programmatic_position = None;
    runtime.compact_anchor
}

fn mark_programmatic_position(runtime: &mut WindowRuntime, target: WindowRect) {
    runtime.programmatic_position = Some((target.x, target.y));
}

fn finish_surface_transition(runtime: &mut WindowRuntime, surface: &str, target: WindowRect) {
    runtime.current_surface = surface.into();
    runtime.programmatic_position = (surface == "compact").then_some((target.x, target.y));
}

fn abort_surface_transition(runtime: &mut WindowRuntime, previous_surface: &str) {
    if runtime.current_surface == "transition" {
        runtime.current_surface = previous_surface.into();
        runtime.programmatic_position = None;
    }
}

fn classify_moved(runtime: &mut WindowRuntime, position: (i32, i32)) -> MoveEventDecision {
    if runtime.current_surface != "compact" {
        return MoveEventDecision::Ignore;
    }
    match runtime.programmatic_position {
        Some(target) if target == position => {
            runtime.programmatic_position = None;
            MoveEventDecision::Ignore
        }
        Some(_) => {
            runtime.programmatic_position = None;
            MoveEventDecision::PersistUserMove
        }
        None => MoveEventDecision::PersistUserMove,
    }
}

fn update_compact_anchor(runtime: &mut WindowRuntime, anchor: WindowRect) -> bool {
    if runtime.current_surface != "compact" || runtime.programmatic_position.is_some() {
        return false;
    }
    runtime.compact_anchor = Some(anchor);
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WindowRect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

fn work_area_rect(monitor: &tauri::Monitor) -> WindowRect {
    let area = monitor.work_area();
    WindowRect {
        x: area.position.x,
        y: area.position.y,
        width: i32::try_from(area.size.width).unwrap_or(i32::MAX),
        height: i32::try_from(area.size.height).unwrap_or(i32::MAX),
    }
}

fn resized_window_rect(
    before: WindowRect,
    width: i32,
    height: i32,
    work_area: WindowRect,
) -> WindowRect {
    let right_edge = work_area.x.saturating_add(work_area.width);
    let bottom_edge = work_area.y.saturating_add(work_area.height);
    let before_center_x = before.x.saturating_add(before.width / 2);
    let before_center_y = before.y.saturating_add(before.height / 2);
    let area_center_x = work_area.x.saturating_add(work_area.width / 2);
    let area_center_y = work_area.y.saturating_add(work_area.height / 2);
    let x = if before_center_x >= area_center_x {
        let right_margin = right_edge.saturating_sub(before.x.saturating_add(before.width));
        right_edge
            .saturating_sub(width)
            .saturating_sub(right_margin)
    } else {
        before.x
    };
    let y = if before_center_y >= area_center_y {
        let bottom_margin = bottom_edge.saturating_sub(before.y.saturating_add(before.height));
        bottom_edge
            .saturating_sub(height)
            .saturating_sub(bottom_margin)
    } else {
        before.y
    };
    WindowRect {
        x: clamp_window_axis(x, width, work_area.x, work_area.width),
        y: clamp_window_axis(y, height, work_area.y, work_area.height),
        width,
        height,
    }
}

fn clamp_window_axis(origin: i32, size: i32, area_origin: i32, area_size: i32) -> i32 {
    let max_origin = area_origin.saturating_add(area_size).saturating_sub(size);
    origin.clamp(area_origin, max_origin.max(area_origin))
}

fn surface_size(surface: &str) -> Result<(f64, f64), String> {
    match surface {
        "compact" => Ok((240.0, 36.0)),
        "detail" => Ok((366.0, 344.0)),
        "settings" => Ok((366.0, 408.0)),
        "onboarding" => Ok((366.0, 246.0)),
        _ => Err("Unknown surface".into()),
    }
}

fn apply_runtime_settings(
    app: &AppHandle,
    current: &AppSettings,
    updated: &AppSettings,
) -> Result<(), String> {
    if current.always_on_top != updated.always_on_top {
        if let Some(window) = app.get_webview_window("main") {
            window
                .set_always_on_top(updated.always_on_top)
                .map_err(|_| "Unable to change always-on-top".to_string())?;
            enforce_windows_topmost(&window, updated.always_on_top)?;
        }
    }

    if let Some(desired) = autostart_change(current, updated) {
        let manager = app.autolaunch();
        let enabled = manager
            .is_enabled()
            .map_err(|_| "Unable to read launch at startup".to_string())?;
        if enabled != desired {
            if desired {
                manager
                    .enable()
                    .map_err(|_| "Unable to enable launch at startup".to_string())?;
            } else {
                manager
                    .disable()
                    .map_err(|_| "Unable to disable launch at startup".to_string())?;
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn enforce_windows_topmost(window: &WebviewWindow, enabled: bool) -> Result<(), String> {
    use windows_sys::Win32::{
        Foundation::GetLastError,
        UI::WindowsAndMessaging::{
            SetWindowPos, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        },
    };

    let hwnd = window
        .hwnd()
        .map_err(|_| "Unable to access the Usage window handle".to_string())?;
    let insert_after = if enabled {
        HWND_TOPMOST
    } else {
        HWND_NOTOPMOST
    };
    let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
    // This explicitly changes the Windows z-order without activating the widget.
    if unsafe { SetWindowPos(hwnd.0 as _, insert_after, 0, 0, 0, 0, flags) } == 0 {
        let error = unsafe { GetLastError() };
        return Err(format!("Unable to update Windows topmost state ({error})"));
    }
    Ok(())
}

#[cfg(not(windows))]
fn enforce_windows_topmost(_window: &WebviewWindow, _enabled: bool) -> Result<(), String> {
    Ok(())
}

fn autostart_change(current: &AppSettings, updated: &AppSettings) -> Option<bool> {
    (current.launch_at_startup != updated.launch_at_startup).then_some(updated.launch_at_startup)
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
                    let always_on_top = tray
                        .app_handle()
                        .state::<SharedState>()
                        .settings
                        .read()
                        .map(|settings| settings.always_on_top)
                        .unwrap_or(false);
                    let _ = enforce_windows_topmost(&window, always_on_top);
                }
            }
        })
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                    let always_on_top = app
                        .state::<SharedState>()
                        .settings
                        .read()
                        .map(|settings| settings.always_on_top)
                        .unwrap_or(false);
                    let _ = enforce_windows_topmost(&w, always_on_top);
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
    let Ok(size) = window.outer_size() else {
        return;
    };
    let (Some(x), Some(y)) = (settings.widget_x, settings.widget_y) else {
        place_near_system_tray(window, &size);
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

    place_near_system_tray(window, &size);
}

fn place_near_system_tray(window: &WebviewWindow, size: &tauri::PhysicalSize<u32>) {
    if let Ok(Some(primary)) = window.primary_monitor() {
        let area = primary.work_area();
        let width = i32::try_from(size.width).unwrap_or(i32::MAX);
        let height = i32::try_from(size.height).unwrap_or(i32::MAX);
        let right = area
            .position
            .x
            .saturating_add(i32::try_from(area.size.width).unwrap_or(i32::MAX));
        let bottom = area
            .position
            .y
            .saturating_add(i32::try_from(area.size.height).unwrap_or(i32::MAX));
        let _ = window.set_position(PhysicalPosition::new(
            right.saturating_sub(width).saturating_sub(8),
            bottom.saturating_sub(height).saturating_sub(8),
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
                window.set_always_on_top(initial_settings.always_on_top)?;
                enforce_windows_topmost(&window, initial_settings.always_on_top)
                    .map_err(std::io::Error::other)?;
                restore_position(&window, &initial_settings);
                if let (Ok(position), Ok(size), Ok(mut runtime)) = (
                    window.outer_position(),
                    window.outer_size(),
                    state.window_runtime.lock(),
                ) {
                    runtime.current_surface = "compact".into();
                    runtime.compact_anchor = Some(WindowRect {
                        x: position.x,
                        y: position.y,
                        width: i32::try_from(size.width).unwrap_or(i32::MAX),
                        height: i32::try_from(size.height).unwrap_or(i32::MAX),
                    });
                }
            }
            start_scheduler(app.handle().clone(), state.clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Moved(position) = event {
                let state = window.state::<SharedState>();
                let settings_path = state.paths.settings.clone();
                let should_persist = state
                    .window_runtime
                    .lock()
                    .map(|mut runtime| {
                        classify_moved(&mut runtime, (position.x, position.y))
                            == MoveEventDecision::PersistUserMove
                    })
                    .unwrap_or(true);

                if should_persist {
                    // Read the native size only after releasing window_runtime.
                    if let Ok(size) = window.outer_size() {
                        let anchor = WindowRect {
                            x: position.x,
                            y: position.y,
                            width: i32::try_from(size.width).unwrap_or(i32::MAX),
                            height: i32::try_from(size.height).unwrap_or(i32::MAX),
                        };
                        let should_save = state
                            .window_runtime
                            .lock()
                            .map(|mut runtime| update_compact_anchor(&mut runtime, anchor))
                            .unwrap_or(false);
                        if should_save {
                            let updated = state.settings.write().ok().map(|mut current| {
                                current.widget_x = Some(position.x);
                                current.widget_y = Some(position.y);
                                current.clone()
                            });
                            if let Some(updated) = updated {
                                let _ = settings::save(&settings_path, &updated);
                            }
                        }
                    }
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

    #[test]
    fn unchanged_autostart_has_no_runtime_side_effect() {
        let settings = AppSettings::default();
        assert_eq!(autostart_change(&settings, &settings), None);

        let enabled = AppSettings {
            launch_at_startup: true,
            ..AppSettings::default()
        };
        assert_eq!(autostart_change(&enabled, &enabled), None);
    }

    #[test]
    fn changed_autostart_requests_the_desired_state() {
        let disabled = AppSettings::default();
        let enabled = AppSettings {
            launch_at_startup: true,
            ..AppSettings::default()
        };
        assert_eq!(autostart_change(&disabled, &enabled), Some(true));
        assert_eq!(autostart_change(&enabled, &disabled), Some(false));
    }

    #[test]
    fn completing_onboarding_does_not_touch_autostart() {
        let current = AppSettings::default();
        let completed = AppSettings {
            first_run_complete: true,
            ..current.clone()
        };
        assert_eq!(autostart_change(&current, &completed), None);
    }

    #[test]
    fn compact_surface_uses_taskbar_friendly_size() {
        assert_eq!(surface_size("compact"), Ok((240.0, 36.0)));
    }

    #[test]
    fn anchored_bottom_right_grows_towards_top_left() {
        let work_area = WindowRect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1040,
        };
        let compact = WindowRect {
            x: 1672,
            y: 996,
            width: 240,
            height: 36,
        };
        assert_eq!(
            resized_window_rect(compact, 366, 344, work_area),
            WindowRect {
                x: 1546,
                y: 688,
                width: 366,
                height: 344,
            }
        );
        assert_eq!(
            resized_window_rect(
                WindowRect {
                    x: 1546,
                    y: 688,
                    width: 366,
                    height: 344,
                },
                240,
                36,
                work_area,
            ),
            compact
        );
    }

    #[test]
    fn centered_window_preserves_top_left() {
        let work_area = WindowRect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1040,
        };
        let centered = WindowRect {
            x: 700,
            y: 300,
            width: 240,
            height: 36,
        };
        assert_eq!(
            resized_window_rect(centered, 366, 344, work_area),
            WindowRect {
                x: 700,
                y: 300,
                width: 366,
                height: 344,
            }
        );
    }

    #[test]
    fn resize_clamps_right_and_bottom_edges() {
        let work_area = WindowRect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1040,
        };
        assert_eq!(
            resized_window_rect(
                WindowRect {
                    x: 1900,
                    y: 1000,
                    width: 20,
                    height: 20,
                },
                366,
                344,
                work_area,
            ),
            WindowRect {
                x: 1554,
                y: 676,
                width: 366,
                height: 344,
            }
        );
    }

    #[test]
    fn negative_second_monitor_coordinates_are_supported() {
        let work_area = WindowRect {
            x: -1920,
            y: -40,
            width: 1920,
            height: 1040,
        };
        let compact = WindowRect {
            x: -240,
            y: 964,
            width: 240,
            height: 36,
        };
        assert_eq!(
            resized_window_rect(compact, 366, 344, work_area),
            WindowRect {
                x: -366,
                y: 656,
                width: 366,
                height: 344,
            }
        );
    }

    #[test]
    fn every_work_area_quadrant_selects_a_deterministic_growth_direction() {
        let work_area = WindowRect {
            x: 100,
            y: 50,
            width: 1000,
            height: 800,
        };
        let compact_size = (240, 36);
        let detail_size = (366, 344);
        let cases = [
            (
                WindowRect {
                    x: 120,
                    y: 70,
                    width: compact_size.0,
                    height: compact_size.1,
                },
                (120, 70),
            ),
            (
                WindowRect {
                    x: 840,
                    y: 70,
                    width: compact_size.0,
                    height: compact_size.1,
                },
                (714, 70),
            ),
            (
                WindowRect {
                    x: 120,
                    y: 794,
                    width: compact_size.0,
                    height: compact_size.1,
                },
                (120, 486),
            ),
            (
                WindowRect {
                    x: 840,
                    y: 794,
                    width: compact_size.0,
                    height: compact_size.1,
                },
                (714, 486),
            ),
        ];

        for (compact, expected_origin) in cases {
            let detail = resized_window_rect(compact, detail_size.0, detail_size.1, work_area);
            assert_eq!((detail.x, detail.y), expected_origin);
        }
    }

    #[test]
    fn center_halves_choose_down_or_up_without_edge_heuristics() {
        let work_area = WindowRect {
            x: 0,
            y: 0,
            width: 1000,
            height: 800,
        };
        let upper = WindowRect {
            x: 450,
            y: 250,
            width: 240,
            height: 36,
        };
        let lower = WindowRect {
            x: 450,
            y: 550,
            width: 240,
            height: 36,
        };
        assert_eq!(resized_window_rect(upper, 366, 344, work_area).y, 250);
        assert_eq!(resized_window_rect(lower, 366, 344, work_area).y, 242);
    }

    #[test]
    fn saved_compact_anchor_survives_programmatic_surface_positions() {
        let anchor = WindowRect {
            x: -900,
            y: 300,
            width: 240,
            height: 36,
        };
        let detail = WindowRect {
            x: -900,
            y: 0,
            width: 366,
            height: 344,
        };
        let runtime = WindowRuntime {
            current_surface: "detail".into(),
            compact_anchor: Some(anchor),
            programmatic_position: Some((detail.x, detail.y)),
        };
        assert_eq!(runtime.compact_anchor, Some(anchor));
        assert_ne!(runtime.compact_anchor, Some(detail));
    }

    #[test]
    fn compact_to_detail_marks_transition_without_losing_anchor() {
        let anchor = WindowRect {
            x: 100,
            y: 200,
            width: 240,
            height: 36,
        };
        let mut runtime = WindowRuntime {
            current_surface: "compact".into(),
            compact_anchor: None,
            programmatic_position: None,
        };

        let compact_anchor = begin_surface_transition(
            &mut runtime,
            "detail",
            WindowRect {
                width: 240,
                height: 36,
                ..anchor
            },
        );

        assert_eq!(runtime.current_surface, "transition");
        assert_eq!(compact_anchor, Some(anchor));
        assert_eq!(runtime.compact_anchor, Some(anchor));
    }

    #[test]
    fn programmatic_compact_move_does_not_become_user_anchor() {
        let anchor = WindowRect {
            x: 100,
            y: 200,
            width: 240,
            height: 36,
        };
        let target = WindowRect {
            x: 300,
            y: 400,
            width: 240,
            height: 36,
        };
        let mut runtime = WindowRuntime {
            current_surface: "detail".into(),
            compact_anchor: Some(anchor),
            programmatic_position: None,
        };

        begin_surface_transition(&mut runtime, "compact", target);
        mark_programmatic_position(&mut runtime, target);
        finish_surface_transition(&mut runtime, "compact", target);

        assert_eq!(
            classify_moved(&mut runtime, (target.x, target.y)),
            MoveEventDecision::Ignore
        );
        assert_eq!(runtime.compact_anchor, Some(anchor));
    }

    #[test]
    fn user_move_in_compact_updates_anchor() {
        let mut runtime = WindowRuntime {
            current_surface: "compact".into(),
            compact_anchor: None,
            programmatic_position: None,
        };
        let anchor = WindowRect {
            x: 40,
            y: 50,
            width: 240,
            height: 36,
        };

        assert_eq!(
            classify_moved(&mut runtime, (anchor.x, anchor.y)),
            MoveEventDecision::PersistUserMove
        );
        assert!(update_compact_anchor(&mut runtime, anchor));
        assert_eq!(runtime.compact_anchor, Some(anchor));
    }

    #[test]
    fn detail_move_does_not_update_anchor() {
        let anchor = WindowRect {
            x: 40,
            y: 50,
            width: 240,
            height: 36,
        };
        let mut runtime = WindowRuntime {
            current_surface: "detail".into(),
            compact_anchor: Some(anchor),
            programmatic_position: None,
        };

        assert_eq!(
            classify_moved(&mut runtime, (400, 500)),
            MoveEventDecision::Ignore
        );
        assert!(!update_compact_anchor(
            &mut runtime,
            WindowRect {
                x: 400,
                y: 500,
                ..anchor
            }
        ));
        assert_eq!(runtime.compact_anchor, Some(anchor));
    }

    #[test]
    fn detail_to_compact_restores_saved_anchor() {
        let anchor = WindowRect {
            x: 40,
            y: 50,
            width: 240,
            height: 36,
        };
        let mut runtime = WindowRuntime {
            current_surface: "detail".into(),
            compact_anchor: Some(anchor),
            programmatic_position: None,
        };

        let restored = begin_surface_transition(
            &mut runtime,
            "compact",
            WindowRect {
                x: 400,
                y: 500,
                width: 366,
                height: 344,
            },
        );
        assert_eq!(restored, Some(anchor));
        finish_surface_transition(&mut runtime, "compact", anchor);
        assert_eq!(runtime.current_surface, "compact");
        assert_eq!(runtime.compact_anchor, Some(anchor));
    }

    #[test]
    fn successful_and_failed_transitions_do_not_leave_transition_flag() {
        let anchor = WindowRect {
            x: 40,
            y: 50,
            width: 240,
            height: 36,
        };
        let mut successful = WindowRuntime {
            current_surface: "compact".into(),
            compact_anchor: Some(anchor),
            programmatic_position: None,
        };
        begin_surface_transition(&mut successful, "detail", anchor);
        mark_programmatic_position(&mut successful, anchor);
        finish_surface_transition(&mut successful, "detail", anchor);
        assert_eq!(successful.current_surface, "detail");
        assert_eq!(successful.programmatic_position, None);

        let mut failed = WindowRuntime {
            current_surface: "compact".into(),
            compact_anchor: Some(anchor),
            programmatic_position: None,
        };
        begin_surface_transition(&mut failed, "detail", anchor);
        mark_programmatic_position(&mut failed, anchor);
        abort_surface_transition(&mut failed, "compact");
        assert_eq!(failed.current_surface, "compact");
        assert_eq!(failed.programmatic_position, None);
    }
}
