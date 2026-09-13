use serde::{Deserialize, Serialize};
use std::{fs, io, path::{Path, PathBuf}};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    pub launch_at_startup: bool,
    pub always_on_top: bool,
    pub refresh_interval_minutes: u64,
    pub notifications: bool,
    pub compact_mode: bool,
    pub show_reset_countdown: bool,
    pub show_long_window: bool,
    pub first_run_complete: bool,
    pub widget_x: Option<i32>,
    pub widget_y: Option<i32>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            launch_at_startup: false,
            always_on_top: false,
            refresh_interval_minutes: 3,
            notifications: true,
            compact_mode: true,
            show_reset_countdown: true,
            show_long_window: true,
            first_run_complete: false,
            widget_x: None,
            widget_y: None,
        }
    }
}

impl AppSettings {
    pub fn validate(mut self) -> Self {
        self.refresh_interval_minutes = self.refresh_interval_minutes.clamp(1, 60);
        self
    }
}

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub settings: PathBuf,
    pub cache: PathBuf,
    pub logs: PathBuf,
}

impl AppPaths {
    pub fn discover() -> io::Result<Self> {
        let base = dirs::data_local_dir().or_else(dirs::home_dir).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "local data directory unavailable"))?;
        let data_dir = base.join("Usage");
        let logs = data_dir.join("logs");
        fs::create_dir_all(&logs)?;
        Ok(Self { settings: data_dir.join("settings.json"), cache: data_dir.join("cache.json"), logs })
    }
}

pub fn load(path: &Path) -> AppSettings {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<AppSettings>(&s).ok())
        .unwrap_or_default()
        .validate()
}

pub fn save(path: &Path, settings: &AppSettings) -> io::Result<()> {
    atomic_json_write(path, settings)
}

pub fn atomic_json_write<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
    let data = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    fs::write(path, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_refresh_bounds() {
        let s = AppSettings {
            refresh_interval_minutes: 0,
            ..AppSettings::default()
        };
        assert_eq!(s.validate().refresh_interval_minutes, 1);
    }
}
