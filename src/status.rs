//! Status file for the Omarchy shell plugin.
//!
//! The daemon writes a small JSON snapshot to
//! `~/.local/state/atw/status.json` after state changes and flushes. The
//! plugin reads it with Quickshell's `FileView { watchChanges: true }`
//! (inotify-based) — no polling, no sockets, no HTTP.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Result;

pub const STATUS_VERSION: u32 = 1;

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
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
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
