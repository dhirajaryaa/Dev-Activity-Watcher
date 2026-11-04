//! Configuration: ~/.config/atw/config.toml
//!
//! The file is created with documented defaults on first run and is never
//! overwritten afterwards. All paths honour XDG base directories.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{AtwError, Result};

pub const DEFAULT_IDLE_TIMEOUT_SECONDS: u32 = 300;
pub const DEFAULT_FLUSH_INTERVAL_SECONDS: u32 = 420; // 7 minutes, inside the 5–10 window
pub const DEFAULT_RETENTION_DAYS: u32 = 7;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub tracking: Tracking,
    pub storage: Storage,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            tracking: Tracking::default(),
            storage: Storage::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Tracking {
    /// Master switch. When false the daemon exits immediately.
    pub enabled: bool,
    /// Seconds of input inactivity before a user counts as AFK.
    pub idle_timeout_seconds: u32,
    /// How often the open segment is flushed to SQLite.
    pub flush_interval_seconds: u32,
    /// Activity older than this many days is deleted.
    pub retention_days: u32,
    /// Window classes that are never tracked (exact, case-insensitive).
    /// Wins over the allowlist below.
    pub excluded_apps: Vec<String>,
    /// Categories that are tracked. Everything else is ignored — atw is a
    /// developer activity tracker, not a general screen-time logger.
    /// Built-in categories: editor, browser, terminal, agent, media, other.
    pub tracked_categories: Vec<String>,
    /// Window classes tracked even when their category is not in
    /// tracked_categories, e.g. ["postman", "insomnia"].
    pub tracked_apps: Vec<String>,
    /// Derive project names from editor window titles (basename only).
    pub editor_project_names: bool,
    /// Window class -> friendly display name.
    pub display_names: HashMap<String, String>,
}

/// Categories tracked out of the box: the day-to-day developer set.
pub const DEFAULT_TRACKED_CATEGORIES: &[&str] = &["editor", "browser", "terminal", "agent"];

impl Default for Tracking {
    fn default() -> Self {
        Tracking {
            enabled: true,
            idle_timeout_seconds: DEFAULT_IDLE_TIMEOUT_SECONDS,
            flush_interval_seconds: DEFAULT_FLUSH_INTERVAL_SECONDS,
            retention_days: DEFAULT_RETENTION_DAYS,
            excluded_apps: Vec::new(),
            tracked_categories: DEFAULT_TRACKED_CATEGORIES
                .iter()
                .map(|s| s.to_string())
                .collect(),
            tracked_apps: Vec::new(),
            editor_project_names: false,
            display_names: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Storage {
    /// Defaults to $XDG_DATA_HOME/atw (~/.local/share/atw).
    pub data_dir: Option<PathBuf>,
    /// Defaults to $XDG_STATE_HOME/atw (~/.local/state/atw).
    pub state_dir: Option<PathBuf>,
}

pub struct ResolvedDirs {
    pub config: PathBuf,
    pub data: PathBuf,
    pub state: PathBuf,
}

impl Config {
    pub fn resolved_dirs(&self) -> ResolvedDirs {
        ResolvedDirs {
            config: xdg_dir("XDG_CONFIG_HOME", ".config").join("atw"),
            data: self
                .storage
                .data_dir
                .clone()
                .unwrap_or_else(|| xdg_dir("XDG_DATA_HOME", ".local/share").join("atw")),
            state: self
                .storage
                .state_dir
                .clone()
                .unwrap_or_else(|| xdg_dir("XDG_STATE_HOME", ".local/state").join("atw")),
        }
    }
}

pub fn default_config_path() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config").join("atw").join("config.toml")
}

pub const CONFIG_TEMPLATE: &str = r#"# atw — Activity Time Watch configuration
# All activity data stays on this machine (SQLite, ~/.local/share/atw).
# This file is only created once; deleting it restores the defaults.

[tracking]
# Master switch. false makes the daemon exit immediately.
enabled = true

# Seconds without input before you count as AFK (default 5 minutes).
idle_timeout_seconds = 300

# How often the open usage segment is written to SQLite (300–600 recommended).
flush_interval_seconds = 420

# Activity older than this many days is deleted automatically.
retention_days = 7

# Derive project names from editor window titles, e.g. the display name
# becomes "Visual Studio Code · myproject". Only the project folder's
# basename is ever stored — never file names or paths. Off by default.
editor_project_names = false

# Only developer-relevant apps are counted. These are the categories that
# track: editor (VS Code, Zed, JetBrains…), browser, terminal, and agent
# (Cursor, Windsurf, Claude…). Everything else — media players, file
# managers, games, unclassified apps — is ignored.
tracked_categories = ["editor", "browser", "terminal", "agent"]

# Individual window classes to track even when their category is not
# listed above, e.g. ["postman", "insomnia"].
tracked_apps = []

# Window classes that are never tracked, even when their category is
# listed above. e.g. ["chromium"] to count Firefox but not Chromium.
excluded_apps = []

# Friendly names for window classes, e.g. { "foot" = "Terminal" }
# Built-in names exist for common apps; entries here win.
[tracking.display_names]
"#;

/// Load the config, creating the default file if it does not exist yet.
/// An existing file is never modified.
pub fn load_or_init(path: Option<&Path>) -> Result<Config> {
    let path = path.map(|p| p.to_path_buf()).unwrap_or_else(default_config_path);
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, CONFIG_TEMPLATE)?;
        tracing::info!(target: "atw", path = %path.display(), "wrote default config");
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(&path)?;
    toml::from_str(&text).map_err(|e| AtwError::Toml(e, path))
}

/// Local date strictly before which records are deleted.
/// With retention_days = 7 this keeps today plus the previous 6 days.
pub fn retention_cutoff_date(days: u32) -> String {
    let today = chrono::Local::now().date_naive();
    (today - chrono::Duration::days(days as i64))
        .format("%F")
        .to_string()
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    std::env::var(var)
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(fallback))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let t = Tracking::default();
        assert!(t.enabled);
        assert_eq!(t.idle_timeout_seconds, 300);
        assert!(t.flush_interval_seconds >= 300 && t.flush_interval_seconds <= 600);
        assert_eq!(t.retention_days, 7);
    }

    #[test]
    fn partial_toml_gets_defaults() {
        let cfg: Config = toml::from_str("[tracking]\nidle_timeout_seconds = 60").unwrap();
        assert_eq!(cfg.tracking.idle_timeout_seconds, 60);
        assert!(cfg.tracking.enabled);
        assert_eq!(cfg.tracking.flush_interval_seconds, DEFAULT_FLUSH_INTERVAL_SECONDS);
    }

    #[test]
    fn template_parses_as_config() {
        let cfg: Config = toml::from_str(CONFIG_TEMPLATE).unwrap();
        assert!(cfg.tracking.enabled);
        assert!(cfg.tracking.excluded_apps.is_empty());
        // The developer-app allowlist is the default scope.
        assert_eq!(
            cfg.tracking.tracked_categories,
            vec!["editor", "browser", "terminal", "agent"]
        );
        assert!(cfg.tracking.tracked_apps.is_empty());
    }

    #[test]
    fn defaults_track_developer_categories_only() {
        let t = Tracking::default();
        assert!(t.tracked_categories.contains(&"editor".to_string()));
        assert!(t.tracked_categories.contains(&"agent".to_string()));
        assert!(!t.tracked_categories.contains(&"media".to_string()));
    }

    #[test]
    fn xdg_fallbacks() {
        // Without XDG vars set, falls back to $HOME/.local/...
        let dirs = Config::default().resolved_dirs();
        let s = dirs.data.to_string_lossy().to_string();
        assert!(s.contains("atw"));
    }
}
