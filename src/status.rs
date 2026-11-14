//! Status file for the Omarchy shell plugin.
//!
//! The daemon writes a small JSON snapshot to
//! `~/.local/state/atw/status.json` after state changes and flushes. The
//! plugin reads it with Quickshell's `FileView { watchChanges: true }`
//! (inotify-based) — no polling, no sockets, no HTTP.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Result;

pub const STATUS_VERSION: u32 = 2;

/// One day of history for the panel's optional history view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DayStatus {
    pub date: String,
    pub total_seconds: u64,
    /// Top apps that day, sorted by usage.
    pub apps: BTreeMap<String, u64>,
}

/// Cloud sync state for the panel (last sync, pending rows, errors).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncStatus {
    /// Whether sync is switched on in config.
    pub enabled: bool,
    /// Whether a database URL + token are configured.
    pub configured: bool,
    /// The configured database URL (not a secret; shown in the panel).
    #[serde(default)]
    pub url: Option<String>,
    /// RFC 3339 timestamp of the last successful sync, if any.
    pub last_sync: Option<String>,
    /// Rows waiting for upload right now.
    pub pending: u64,
    /// Last sync error message, if the last attempt failed.
    pub last_error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Status {
    /// Schema version for the QML plugin.
    pub version: u32,
    /// Display name of the last focused application, if any.
    pub active: Option<String>,
    /// "active" | "idle" | "locked"
    pub state: String,
    /// Total tracked seconds today.
    pub today_total_seconds: u64,
    /// Per-application seconds today, sorted by name for stable output.
    pub today: BTreeMap<String, u64>,
    /// Per-application seconds over the last 7 days (top apps only).
    pub week: BTreeMap<String, u64>,
    /// Per-day history, newest first (panel shows it only on demand).
    #[serde(default)]
    pub history: Vec<DayStatus>,
    /// Cloud sync state.
    #[serde(default)]
    pub sync: SyncStatus,
    /// RFC 3339 timestamp of when this snapshot was written.
    pub updated_at: String,
}

/// Atomic write: serialize to a temp file in the same directory, then rename.
/// Readers never observe a half-written file.
pub fn write_status_file(path: &Path, status: &Status) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(status)?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_atomicity() {
        let dir = std::env::temp_dir().join(format!("atw-status-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("status.json");
        let status = Status {
            version: STATUS_VERSION,
            active: Some("Visual Studio Code".to_string()),
            state: "active".to_string(),
            today_total_seconds: 11_520,
            today: BTreeMap::from([
                ("Firefox".to_string(), 2400),
                ("Visual Studio Code".to_string(), 9120),
            ]),
            week: BTreeMap::from([
                ("Firefox".to_string(), 18_400),
                ("Visual Studio Code".to_string(), 52_300),
            ]),
            history: vec![DayStatus {
                date: "2026-11-04".to_string(),
                total_seconds: 11_520,
                apps: BTreeMap::from([("Visual Studio Code".to_string(), 9120)]),
            }],
            sync: SyncStatus {
                enabled: true,
                configured: false,
                url: Some("https://demo-org.turso.io".to_string()),
                last_sync: None,
                pending: 0,
                last_error: None,
            },
            updated_at: "2026-11-04T10:00:00+05:45".to_string(),
        };
        write_status_file(&path, &status).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: Status = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.active.as_deref(), Some("Visual Studio Code"));
        assert_eq!(parsed.today_total_seconds, 11_520);
        assert_eq!(parsed.version, STATUS_VERSION);
        // No temp file left behind.
        assert!(!path.with_extension("json.tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
