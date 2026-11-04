//! Hyprland IPC: event socket listener + one-shot queries.
//!
//! Hyprland exposes two unix sockets per instance:
//!   - `.socket.sock`  — command socket (request/response)
//!   - `.socket2.sock` — event stream (newline-separated events)
//!
//! We consume the event stream (event-driven, no polling) and use the command
//! socket only for the initial snapshot after connecting. The client here is
//! intentionally hand-rolled: the protocol is a handful of text events, and
//! this avoids a dependency that may drift between Hyprland versions.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::AsyncBufReadExt;
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use crate::tracker::Input;
use crate::Result;

#[derive(Debug, Clone, PartialEq)]
pub struct ActiveWindow {
    pub class: String,
    pub title: String,
}

/// Directory of the running Hyprland instance.
pub fn hypr_dir() -> Option<PathBuf> {
    let base = runtime_dir().join("hypr");
    if let Ok(sig) = std::env::var("HYPRLAND_INSTANCE_SIGNATURE") {
        if !sig.is_empty() {
            let candidate = base.join(&sig);
            if candidate.join(".socket2.sock").exists() {
                return Some(candidate);
            }
        }
    }
    // Fallback: the most recently active instance (multi-instance setups).
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(&base).ok()?.filter_map(|e| e.ok()) {
        let socket = entry.path().join(".socket2.sock");
        if let Ok(mtime) = std::fs::metadata(&socket).and_then(|m| m.modified()) {
            if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
                best = Some((mtime, entry.path()));
            }
        }
    }
    best.map(|(_, p)| p)
}

fn runtime_dir() -> PathBuf {
    std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/run/user").join(default_uid()))
}

fn default_uid() -> String {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("Uid:") {
                if let Some(uid) = rest.split_whitespace().next() {
                    return uid.to_string();
                }
            }
        }
    }
    "1000".to_string()
}

/// Ask the command socket for the currently focused window.
/// Returns `None` when the desktop (no window) is focused.
pub fn query_active_window(dir: &Path) -> Result<Option<ActiveWindow>> {
    let mut stream = StdUnixStream::connect(dir.join(".socket.sock"))?;
    // NOTE: no trailing newline — Hyprland treats the request bytes verbatim,
    // and "command\n" is rejected as "unknown request".
    stream.write_all(b"j/activewindow")?;
    // Hyprland sends the response quickly but does NOT close the socket for
    // every command, so reading "until EOF" can block indefinitely. Read
    // opportunistically and stop once the socket goes quiet.
    stream.set_read_timeout(Some(Duration::from_millis(120)))?;
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break, // EOF
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                // A complete JSON document at any size means we are done.
                if buf.first() == Some(&b'{')
                    && serde_json::from_slice::<serde::de::IgnoredAny>(&buf).is_ok()
                {
                    break;
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                break; // socket went quiet: response is complete (or absent)
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    let text = String::from_utf8_lossy(&buf).trim().to_string();
    if text.is_empty() || !text.starts_with('{') {
        return Ok(None); // "Invalid" or empty when nothing is focused
    }
    let value: serde_json::Value = serde_json::from_str(&text)?;
    let class = value
        .get("class")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if class.is_empty() {
        return Ok(None);
    }
    let title = value
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok(Some(ActiveWindow { class, title }))
}

#[derive(Debug, Clone, PartialEq)]
pub enum HyprEvent {
    /// Active window changed. Empty class means "no focused window".
    ActiveWindow {
        class: String,
        title: String,
    },
    Locked,
    Unlocked,
    Other,
}

/// Parse one event line from `.socket2.sock`.
/// Format: `EVENT_NAME>>payload` (payload optional; title may contain commas).
pub fn parse_event(line: &str) -> HyprEvent {
    if let Some(rest) = line.strip_prefix("activewindow>>") {
        let (class, title) = match rest.split_once(',') {
            Some((c, t)) => (c.to_string(), t.to_string()),
            None => (rest.to_string(), String::new()),
        };
        HyprEvent::ActiveWindow { class, title }
    } else if line == "lock" {
        HyprEvent::Locked
    } else if line == "unlock" {
        HyprEvent::Unlocked
    } else {
        HyprEvent::Other
    }
}

fn to_input(event: HyprEvent) -> Option<Input> {
    match event {
        HyprEvent::ActiveWindow { class, title } => Some(Input::Focus { class, title }),
        HyprEvent::Locked => Some(Input::Lock),
        HyprEvent::Unlocked => Some(Input::Unlock),
        HyprEvent::Other => None,
    }
}

/// Long-running task: stream Hyprland events into the tracker channel,
/// reconnecting with exponential backoff (max 60 s) when the socket drops.
/// After every (re)connect, the active window is re-queried so the tracker
/// resyncs even when no focus event arrives.
pub async fn run_event_listener(tx: mpsc::Sender<Input>) {
    let mut backoff: u64 = 1;
    loop {
        let Some(dir) = hypr_dir() else {
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        };
        let socket = dir.join(".socket2.sock");
        match UnixStream::connect(&socket).await {
            Ok(stream) => {
                backoff = 1;
                tracing::info!(target: "atw", path = %socket.display(), "connected to Hyprland event socket");

                let dir_snapshot = dir.clone();
                let snapshot =
                    tokio::task::spawn_blocking(move || query_active_window(&dir_snapshot)).await;
                if let Ok(Ok(Some(aw))) = snapshot {
                    if tx
                        .send(Input::Focus {
                            class: aw.class,
                            title: aw.title,
                        })
                        .await
                        .is_err()
                    {
                        return; // daemon is shutting down
                    }
                }

                let mut lines = tokio::io::BufReader::new(stream).lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            let line = line.trim_end();
                            if line.is_empty() {
                                continue;
                            }
                            if let Some(input) = to_input(parse_event(line)) {
                                if tx.send(input).await.is_err() {
                                    return;
                                }
                            }
                        }
                        Ok(None) => break, // EOF: compositor closed the stream
                        Err(e) => {
                            tracing::warn!(target: "atw", error = %e, "event socket read error");
                            break;
                        }
                    }
                }
                tracing::warn!(target: "atw", "event socket disconnected; reconnecting");
            }
            Err(e) => {
                tracing::warn!(target: "atw", error = %e, "cannot connect to Hyprland event socket")
            }
        }
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = backoff.saturating_mul(2).min(60);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_active_window_with_commas_in_title() {
        let ev = parse_event("activewindow>>code,main.rs - atw — Visual Studio Code");
        assert_eq!(
            ev,
            HyprEvent::ActiveWindow {
                class: "code".to_string(),
                title: "main.rs - atw — Visual Studio Code".to_string()
            }
        );
    }

    #[test]
    fn parses_empty_active_window() {
        assert_eq!(
            parse_event("activewindow>>"),
            HyprEvent::ActiveWindow {
                class: String::new(),
                title: String::new()
            }
        );
        assert_eq!(
            to_input(parse_event("activewindow>>")),
            Some(Input::Focus {
                class: String::new(),
                title: String::new()
            })
        );
    }

    #[test]
    fn parses_lock_events() {
        assert_eq!(parse_event("lock"), HyprEvent::Locked);
        assert_eq!(parse_event("unlock"), HyprEvent::Unlocked);
        assert_eq!(to_input(parse_event("lock")), Some(Input::Lock));
    }

    #[test]
    fn ignores_other_events() {
        assert_eq!(parse_event("workspace>>2"), HyprEvent::Other);
        assert_eq!(
            parse_event("openwindow>>addr,1,firefox,Title"),
            HyprEvent::Other
        );
        assert_eq!(to_input(parse_event("workspace>>2")), None);
    }
}
