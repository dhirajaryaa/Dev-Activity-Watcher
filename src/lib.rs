//! atw — Activity Time Watch.
//!
//! A local-only activity tracker for Omarchy/Hyprland. The daemon watches
//! Hyprland's event socket, keeps application usage segments in memory, and
//! flushes aggregated rows to SQLite in batches. Nothing ever leaves the
//! machine.
//!
//! Architecture: one owner task holds the `Tracker` state machine and the
//! `Db`; every input (Hyprland events, idle notifications, flush ticks,
//! shutdown signals) arrives as an `Input` message over a channel. This
//! keeps state single-owner with no locks and no polling loops.

pub mod classify;
pub mod config;
pub mod db;
pub mod hypr;
pub mod status;
pub mod tracker;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::classify::Classifier;
use crate::db::Db;
use crate::status::{Status, STATUS_VERSION, write_status_file};
use crate::tracker::{Input, Tracker};

#[derive(Debug, thiserror::Error)]
pub enum AtwError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("invalid config file {1}: {0}")]
    Toml(toml::de::Error, PathBuf),
    #[error("database corrupted: {0}")]
    Corrupt(String),
}

pub type Result<T> = std::result::Result<T, AtwError>;

/// Current unix time in seconds. Wrapped for tests and reuse.
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Local calendar date (YYYY-MM-DD) for a unix timestamp.
pub fn local_date_string(ts: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|d| d.date_naive().format("%F").to_string())
        .unwrap_or_default()
}

fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

#[derive(Parser)]
#[command(
    name = "atw",
    version,
    about = "Activity Time Watch — local-only activity tracking for Omarchy"
)]
struct Cli {
    /// Path to config.toml (default: ~/.config/atw/config.toml)
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the tracking daemon (used by the systemd service)
    Run,
    /// Print current status and today's totals
    Status,
    /// Delete activity older than the retention window
    Cleanup,
}

/// Shared entry point for the `atw` and `atwd` binaries.
pub fn cli_main<I, T>(args: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::parse_from(args);
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("atw: cannot start async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(async move {
        match cli.command.unwrap_or(Command::Run) {
            Command::Run => run_daemon(cli.config.as_deref()).await,
            Command::Status => print_status(),
            Command::Cleanup => run_cleanup(cli.config.as_deref()),
        }
    });
    drop(runtime);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("atw: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Daemon main loop. Owns the tracker and the database; everything else is a
/// message producer.
pub async fn run_daemon(config_path: Option<&Path>) -> Result<()> {
    init_logging();

    let config = config::load_or_init(config_path)?;
    if !config.tracking.enabled {
        tracing::info!(target: "atw", "tracking is disabled in config; nothing to do");
        return Ok(());
    }

    let dirs = config.resolved_dirs();
    std::fs::create_dir_all(&dirs.data)?;
    std::fs::create_dir_all(&dirs.state)?;
    let status_path = dirs.state.join("status.json");
    let db_path = dirs.data.join("activity.db");
    tracing::info!(target: "atw", db = %db_path.display(), "starting atw daemon");

    let mut db = Db::open(&db_path)?;

    // Retention on startup: covers machines that were off for a while.
    let cutoff = config::retention_cutoff_date(config.tracking.retention_days);
    match db.retain(&cutoff) {
        Ok(n) if n > 0 => tracing::info!(target: "atw", deleted = n, cutoff = %cutoff, "retention cleanup"),
        Ok(_) => {}
        Err(e) => tracing::warn!(target: "atw", error = %e, "retention cleanup failed"),
    }

    let classifier = Classifier::from_config(&config.tracking);
    let date_of: tracker::DateFn = Box::new(local_date_string);
    let mut tracker = Tracker::new(classifier, config.tracking.idle_timeout_seconds as i64, date_of);

    // Seed the tracker with the currently focused window, if any.
    match hypr::hypr_dir() {
        Some(dir) => match hypr::query_active_window(&dir) {
            Ok(Some(aw)) => tracker.handle(Input::Focus { class: aw.class, title: aw.title }, unix_now()),
            Ok(None) => {}
            Err(e) => tracing::warn!(target: "atw", error = %e, "initial window snapshot failed"),
        },
        None => tracing::warn!(target: "atw", "no Hyprland instance found yet; event listener will keep retrying"),
    }

    // Write a status snapshot immediately so the UI has data before the
    // first event or flush arrives.
    let now = unix_now();
    write_status(&db, &tracker, &status_path, now);

    let (tx, mut rx) = tokio::sync::mpsc::channel::<Input>(64);

    // Hyprland events (with automatic reconnect + re-snapshot).
    tokio::spawn(hypr::run_event_listener(tx.clone()));

    // Periodic flush of the open segment.
    {
        let tx = tx.clone();
        let every = std::time::Duration::from_secs(config.tracking.flush_interval_seconds.max(5) as u64);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(every);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticker.tick().await; // first tick fires immediately; skip it
            loop {
                ticker.tick().await;
                if tx.send(Input::Flush).await.is_err() {
                    break;
                }
            }
        });
    }

    // Graceful shutdown on SIGTERM / SIGINT.
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{signal, SignalKind};
            let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
            let mut int = signal(SignalKind::interrupt()).expect("SIGINT handler");
            tokio::select! {
                _ = term.recv() => {},
                _ = int.recv() => {},
            }
            tracing::info!(target: "atw", "shutdown signal received");
            let _ = tx.send(Input::Shutdown).await;
        });
    }

    while let Some(input) = rx.recv().await {
        let now = unix_now();
        match input {
            Input::Flush => {
                if let Err(e) = flush_once(&mut db, &mut tracker, now) {
                    tracing::error!(target: "atw", error = %e, "flush failed");
                }
                write_status(&db, &tracker, &status_path, now);
            }
            Input::Shutdown => {
                // Ends the open segment, then flushes everything.
                tracker.handle(input, now);
                if let Err(e) = flush_once(&mut db, &mut tracker, now) {
                    tracing::error!(target: "atw", error = %e, "final flush failed");
                }
                let _ = db.checkpoint();
                write_status(&db, &tracker, &status_path, now);
                tracing::info!(target: "atw", "bye");
                break;
            }
            other => {
                tracker.handle(other, now);
                write_status(&db, &tracker, &status_path, now);
            }
        }
    }
    Ok(())
}

/// Write all currently pending data to SQLite in one transaction.
fn flush_once(db: &mut Db, tracker: &mut Tracker, now: i64) -> Result<()> {
    let open = tracker.open_segment().cloned();
    let mut open_row = tracker.open_row();
    let finalize = tracker.take_finalize();
    let pending = tracker.take_pending();
    db.flush(open.as_ref(), &mut open_row, &finalize, &pending, now, &local_date_string)?;
    tracker.set_open_row(open_row);
    tracing::debug!(target: "atw", finalized = finalize.len(), inserted = pending.len(), "flushed");
    Ok(())
}

/// Rebuild the status file from the database plus in-memory state.
fn write_status(db: &Db, tracker: &Tracker, path: &Path, now: i64) {
    let date = local_date_string(now);
    let mut today: std::collections::BTreeMap<String, u64> = match db.today_totals(&date) {
        Ok(rows) => rows.into_iter().collect(),
        Err(e) => {
            tracing::warn!(target: "atw", error = %e, "could not read today's totals");
            std::collections::BTreeMap::new()
        }
    };
    for (display, seconds) in tracker.today_pieces(now) {
        *today.entry(display).or_insert(0) += seconds;
    }
    let status = Status {
        version: STATUS_VERSION,
        active: tracker.active_display(),
        state: tracker.state_str().to_string(),
        today_total_seconds: today.values().sum(),
        today,
        updated_at: chrono::Local::now().to_rfc3339(),
    };
    if let Err(e) = write_status_file(path, &status) {
        tracing::warn!(target: "atw", error = %e, "could not write status file");
    }
}

/// `atw status` — print the status JSON if the daemon has produced one.
pub fn print_status() -> Result<()> {
    let config = config::load_or_init(None)?;
    let path = config.resolved_dirs().state.join("status.json");
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let value: serde_json::Value = serde_json::from_str(&text)?;
            println!("{}", serde_json::to_string_pretty(&value)?);
            Ok(())
        }
        Err(_) => {
            println!("no status file yet — is the daemon running? (systemctl --user status atwd)");
            Ok(())
        }
    }
}

/// `atw cleanup` — apply the retention window right now.
pub fn run_cleanup(config_path: Option<&Path>) -> Result<()> {
    let config = config::load_or_init(config_path)?;
    let dirs = config.resolved_dirs();
    std::fs::create_dir_all(&dirs.data)?;
    let db = Db::open(&dirs.data.join("activity.db"))?;
    let cutoff = config::retention_cutoff_date(config.tracking.retention_days);
    let deleted = db.retain(&cutoff)?;
    let _ = db.checkpoint();
    println!("deleted {deleted} records older than {cutoff}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_date_string_is_iso_format() {
        use chrono::TimeZone;
        // 2026-11-04 00:30 UTC, independent of the local timezone of the host.
        let ts = chrono::Utc
            .with_ymd_and_hms(2026, 11, 4, 0, 30, 0)
            .unwrap()
            .timestamp();
        let date = local_date_string(ts);
        assert_eq!(date.len(), 10);
        assert!(date.chars().nth(4) == Some('-') && date.chars().nth(7) == Some('-'));
    }
}
