//! Cloud sync engine.
//!
//! Runs outside the owner task: it opens its own SQLite connection (WAL lets
//! the daemon keep writing), gathers closed unsynced rows, uploads them in
//! batches of at most `BATCH_CAP`, and marks them synced only **after** the
//! cloud acknowledged the batch. Local tracking never depends on this module.
//!
//! Batch ids are derived deterministically from the device id plus the sorted
//! segment UUIDs. A crash between the cloud commit and the local ack therefore
//! retries with the **same** batch id — the cloud sees a duplicate and acks it
//! without applying anything twice. Two concurrent syncs (daemon + `atw sync`)
//! also converge: one applies, the other is acked as duplicate.

use std::path::{Path, PathBuf};

use crate::cloud::{self, CloudBackend, SegmentInput, UploadOutcome};
use crate::config::{Config, Sync as SyncConfig};
use crate::db::Db;
use crate::http_pipeline::HttpBackend;
use crate::Result;

/// Maximum segments per upload batch (small payloads, quick retries).
pub const BATCH_CAP: usize = 500;
/// First retry delay after a failure; doubles per consecutive failure…
pub const BACKOFF_BASE_SECONDS: u64 = 30;
/// …up to this ceiling (also the automatic interval's safety cap).
pub const BACKOFF_CAP_SECONDS: u64 = 900;

/// Persisted sync state, so "last sync" survives daemon restarts and the
/// panel can show it without the daemon running.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SyncRecord {
    /// RFC 3339 timestamp of the last fully acknowledged sync.
    pub last_sync: Option<String>,
    /// RFC 3339 timestamp of the last attempt (success or failure).
    pub last_attempt: Option<String>,
    /// Last error message (None after a success). Never contains secrets.
    pub last_error: Option<String>,
    /// Rows still waiting for upload.
    pub pending: u64,
    /// Unix time of the last cloud-retention pass.
    pub last_retention_at: Option<i64>,
    /// Highest cloud `ingested_at` boundary already pulled. The next pull
    /// restarts just before it (inclusive overlap), so a cloud write that
    /// committed at the boundary second is still seen, and uuid-based
    /// deduplication makes the overlap free.
    #[serde(default)]
    pub last_pull_cursor: Option<i64>,
}

/// Outcome of one sync attempt.
#[derive(Debug, Clone)]
pub struct SyncReport {
    pub uploaded: usize,
    pub pulled: usize,
    pub batches: usize,
    pub pending: usize,
    /// None on success; Some(message) when the attempt failed. The message
    /// is safe to show in logs/UI (never contains the token).
    pub error: Option<String>,
    /// True when sync is disabled or not configured (no attempt made).
    pub skipped: bool,
}

impl SyncReport {
    fn skipped(pending: usize) -> Self {
        SyncReport {
            uploaded: 0,
            pulled: 0,
            batches: 0,
            pending,
            error: None,
            skipped: true,
        }
    }
}

/// RFC 3339 local timestamp, or "" on the impossible clock error.
fn rfc3339_now() -> String {
    chrono::Local::now().to_rfc3339()
}

/// Persistent per-installation device id at `<state>/device-id`.
pub fn device_id(state_dir: &Path) -> Result<String> {
    let path = state_dir.join("device-id");
    if let Ok(text) = std::fs::read_to_string(&path) {
        let trimmed = text.trim().to_string();
        if !trimmed.is_empty() {
            return Ok(trimmed);
        }
    }
    std::fs::create_dir_all(state_dir)?;
    let id = uuid::Uuid::new_v4().to_string();
    std::fs::write(&path, format!("{id}\n"))?;
    tracing::info!(target: "atw", "generated device id");
    Ok(id)
}

pub fn record_path(state_dir: &Path) -> PathBuf {
    state_dir.join("sync.json")
}

pub fn read_record(state_dir: &Path) -> SyncRecord {
    std::fs::read_to_string(record_path(state_dir))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn write_record(state_dir: &Path, record: &SyncRecord) -> Result<()> {
    std::fs::create_dir_all(state_dir)?;
    let json = serde_json::to_string_pretty(record)?;
    let path = record_path(state_dir);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Resolve the database token: `token_cmd` (run through `sh -c`, stdout) wins
/// over the literal `token` field. Errors never include the token itself.
pub fn resolve_token(cfg: &SyncConfig) -> Result<String> {
    let cmd = cfg.token_cmd.trim();
    if !cmd.is_empty() {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .output()
            .map_err(|e| crate::AtwError::Config(format!("token_cmd failed to run: {e}")))?;
        if !output.status.success() {
            return Err(crate::AtwError::Config(
                "token_cmd exited unsuccessfully (is the keyring unlocked?)".into(),
            ));
        }
        let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if token.is_empty() {
            return Err(crate::AtwError::Config("token_cmd printed nothing".into()));
        }
        return Ok(token);
    }
    let token = cfg.token.trim().to_string();
    if token.is_empty() {
        return Err(crate::AtwError::Config(
            "no cloud token configured — paste one in the panel's Cloud section".into(),
        ));
    }
    Ok(token)
}

/// Deterministic batch id: FNV-1a over the device id and the sorted segment
/// UUIDs. Stable across retries and binary upgrades (the algorithm is ours).
fn batch_id_for(device_id: &str, segment_ids: &[String]) -> String {
    fn fnv1a(bytes: &[u8], seed: u64) -> u64 {
        let mut hash = seed;
        for b in bytes {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }
    let mut sorted: Vec<&String> = segment_ids.iter().collect();
    sorted.sort();
    let mut a: u64 = 0xcbf2_9ce4_8422_2325;
    let mut b: u64 = 0x9e37_79b9_7f4a_7c15; // different seed for the second half
    for id in sorted {
        a = fnv1a(id.as_bytes(), a);
        b = fnv1a(id.as_bytes(), b);
    }
    a = fnv1a(device_id.as_bytes(), a);
    b = fnv1a(device_id.as_bytes(), b);
    format!("{a:016x}{b:016x}")
}

/// Exponential backoff after `failures` consecutive failures.
pub fn backoff_seconds(failures: u32) -> u64 {
    let shift = failures.min(5);
    (BACKOFF_BASE_SECONDS.saturating_mul(1 << shift)).min(BACKOFF_CAP_SECONDS)
}

/// Cutoff date for cloud retention (device-local date `days` days ago).
pub fn retention_cutoff(days: u32, now: i64) -> String {
    crate::local_date_string(now - (days as i64) * 86_400)
}

/// Upload every closed unsynced row through `backend`. Returns the number
/// uploaded. Rows are marked only after acknowledgement; on error the
/// already-acked batches stay acked and the rest remain pending.
pub fn upload_pending(
    db: &Db,
    backend: &mut dyn CloudBackend,
    device_id: &str,
    open_row_id: i64,
    now: i64,
) -> Result<(usize, usize)> {
    let mut uploaded = 0;
    let mut batches = 0;
    loop {
        let rows = db.unsynced_rows(open_row_id, BATCH_CAP)?;
        if rows.is_empty() {
            break;
        }
        let count = rows.len();
        let segment_ids: Vec<String> = rows.iter().map(|r| r.segment_uuid.clone()).collect();
        let batch_id = batch_id_for(device_id, &segment_ids);
        let segments: Vec<SegmentInput> = rows
            .iter()
            .map(|r| SegmentInput {
                segment_id: r.segment_uuid.clone(),
                app_name: r.display_name.clone(),
                app_class: r.application_class.clone(),
                category: r.category.clone(),
                start_utc: r.started_at,
                end_utc: r.ended_at,
                duration_seconds: r.duration_seconds,
                activity_date: r.date.clone(),
            })
            .collect();
        let outcome = cloud::upload_batch(backend, device_id, &batch_id, &segments, now)?;
        let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
        // Applied and Duplicate are both acknowledgements: the cloud has the
        // segments. Only then are the local rows marked.
        db.mark_synced(&ids, &batch_id, now)?;
        match outcome {
            UploadOutcome::Applied { segments, .. } => uploaded += segments,
            UploadOutcome::Duplicate => uploaded += count,
        }
        batches += 1;
        if count < BATCH_CAP {
            break;
        }
    }
    Ok((uploaded, batches))
}

/// Download closed segments of other devices and import them locally.
///
/// Returns `(imported, boundary)` — the number of database rows actually
/// inserted (re-downloads deduplicate to zero) and the highest cloud
/// `ingested_at` seen this round, which the caller turns into the next
/// pull cursor. Two devices pointing at the same Turso database converge:
/// each pushes its own rows and pulls everyone else's, and the interval
/// union in the cloud keeps simultaneous multi-machine use honest.
pub fn pull_remote(
    db: &Db,
    backend: &mut dyn CloudBackend,
    device_id: &str,
    cursor: Option<i64>,
    since_date: &str,
    now: i64,
) -> Result<(usize, i64)> {
    let rows = cloud::pull_segments(backend, device_id, cursor, since_date)?;
    if rows.is_empty() {
        return Ok((0, cursor.unwrap_or(0)));
    }
    let boundary = rows
        .iter()
        .map(|row| row.ingested_at)
        .max()
        .unwrap_or_default();
    let imported = db.import_remote_segments(&rows, now)?;
    Ok((imported, boundary))
}

/// Full sync attempt: schema bootstrap → optional daily cloud retention →
/// upload pending rows → persist the record. Network/database problems are
/// reported in the returned `SyncReport`, never as a panic; local rows stay
/// untouched on failure.
pub fn sync_once(
    config: &Config,
    data_dir: &Path,
    state_dir: &Path,
    open_row_id: i64,
) -> SyncReport {
    let now = crate::unix_now();
    let db = match Db::open(&data_dir.join("activity.db")) {
        Ok(db) => db,
        Err(e) => {
            return SyncReport {
                uploaded: 0,
                pulled: 0,
                batches: 0,
                pending: 0,
                error: Some(format!("cannot open local database: {e}")),
                skipped: false,
            }
        }
    };
    let pending = db.pending_count(open_row_id).unwrap_or(0) as u64;

    if !config.sync.enabled {
        return SyncReport::skipped(pending as usize);
    }
    if !config.sync.is_configured() {
        return SyncReport::skipped(pending as usize);
    }

    let token = match resolve_token(&config.sync) {
        Ok(t) => t,
        Err(e) => {
            let report = SyncReport {
                uploaded: 0,
                pulled: 0,
                batches: 0,
                pending: pending as usize,
                error: Some(e.to_string()),
                skipped: false,
            };
            record_failure(state_dir, &report, pending);
            return report;
        }
    };
    let mut backend = match HttpBackend::new(&config.sync.url, &token) {
        Ok(b) => b,
        Err(e) => {
            let report = SyncReport {
                uploaded: 0,
                pulled: 0,
                batches: 0,
                pending: pending as usize,
                error: Some(e.to_string()),
                skipped: false,
            };
            record_failure(state_dir, &report, pending);
            return report;
        }
    };

    let device = match device_id(state_dir) {
        Ok(d) => d,
        Err(e) => {
            let report = SyncReport {
                uploaded: 0,
                pulled: 0,
                batches: 0,
                pending: pending as usize,
                error: Some(format!("device id: {e}")),
                skipped: false,
            };
            record_failure(state_dir, &report, pending);
            return report;
        }
    };

    // Schema bootstrap (idempotent). A failure here is a real error.
    let ddl: Vec<cloud::Stmt> = cloud::CLOUD_DDL
        .iter()
        .map(|sql| cloud::Stmt::new(*sql, vec![]))
        .collect();
    if let Err(e) = backend.transaction(ddl) {
        let report = SyncReport {
            uploaded: 0,
            pulled: 0,
            batches: 0,
            pending: pending as usize,
            error: Some(format!("cloud schema setup failed: {e}")),
            skipped: false,
        };
        record_failure(state_dir, &report, pending);
        return report;
    }

    // Cloud retention runs at most once a day, on successful contact.
    let mut record = read_record(state_dir);
    let retention_due = record
        .last_retention_at
        .map(|ts| now - ts >= 86_400)
        .unwrap_or(true);
    if retention_due {
        let cutoff = retention_cutoff(config.sync.cloud_retention_days, now);
        match cloud::cloud_retention(&mut backend, &cutoff, now) {
            Ok(_) => record.last_retention_at = Some(now),
            Err(e) => tracing::warn!(target: "atw", error = %e, "cloud retention failed"),
        }
    }

    match upload_pending(&db, &mut backend, &device, open_row_id, now) {
        Ok((uploaded, batches)) => {
            // Pull runs on every successful contact, upload or not — that is
            // what lets a laptop's day show up on this PC and vice versa.
            let pull_since = retention_cutoff(config.sync.cloud_retention_days, now);
            let (pulled, pull_boundary) = match pull_remote(
                &db,
                &mut backend,
                &device,
                record.last_pull_cursor,
                &pull_since,
                now,
            ) {
                Ok((imported, boundary)) => (imported, boundary),
                Err(e) => {
                    // The push succeeded; only the download tripped. Surface
                    // what is actually stale instead of faking success.
                    tracing::warn!(target: "atw", error = %e, "cloud pull failed");
                    let message = format!("pull: {e}");
                    let pending_now = db.pending_count(open_row_id).unwrap_or(0) as u64;
                    record.last_attempt = Some(rfc3339_now());
                    record.last_error = Some(message.clone());
                    record.pending = pending_now;
                    let _ = write_record(state_dir, &record);
                    return SyncReport {
                        uploaded,
                        pulled: 0,
                        batches,
                        pending: pending_now as usize,
                        error: Some(message),
                        skipped: false,
                    };
                }
            };
            if pull_boundary > record.last_pull_cursor.unwrap_or(0) {
                // Inclusive overlap: the next walk starts at the page's last
                // ingested second, so a concurrent-write tie cannot fall
                // through a gap; replays are uuid-no-ops.
                record.last_pull_cursor = Some((pull_boundary - 1).max(0));
            }
            let pending_now = db.pending_count(open_row_id).unwrap_or(0) as u64;
            record.last_sync = Some(rfc3339_now());
            record.last_attempt = record.last_sync.clone();
            record.last_error = None;
            record.pending = pending_now;
            let _ = write_record(state_dir, &record);
            SyncReport {
                uploaded,
                pulled,
                batches,
                pending: pending_now as usize,
                error: None,
                skipped: false,
            }
        }
        Err(e) => {
            let message = e.to_string();
            let pending_now = db.pending_count(open_row_id).unwrap_or(0) as u64;
            record.last_attempt = Some(rfc3339_now());
            record.last_error = Some(message.clone());
            record.pending = pending_now;
            let _ = write_record(state_dir, &record);
            SyncReport {
                uploaded: 0,
                pulled: 0,
                batches: 0,
                pending: pending_now as usize,
                error: Some(message),
                skipped: false,
            }
        }
    }
}

fn record_failure(state_dir: &Path, report: &SyncReport, pending: u64) {
    let mut record = read_record(state_dir);
    record.last_attempt = Some(rfc3339_now());
    record.last_error = report.error.clone();
    record.pending = pending;
    let _ = write_record(state_dir, &record);
}

/// `atw sync --check`: verify credentials and schema without uploading.
/// Returns a human-readable summary.
pub fn check_connection(config: &Config) -> Result<String> {
    if !config.sync.is_configured() {
        return Err(crate::AtwError::Config(
            "no database url configured — paste one in the panel's Cloud section".into(),
        ));
    }
    let token = resolve_token(&config.sync)?;
    let mut backend = HttpBackend::new(&config.sync.url, &token)?;
    let ddl: Vec<cloud::Stmt> = cloud::CLOUD_DDL
        .iter()
        .map(|sql| cloud::Stmt::new(*sql, vec![]))
        .collect();
    backend.transaction(ddl)?;
    let rows = backend.query("SELECT COUNT(*) FROM segments", vec![])?;
    let count = match rows.first().and_then(|r| r.first()) {
        Some(cloud::CloudValue::Int(n)) => *n,
        Some(cloud::CloudValue::Text(s)) => s.parse::<i64>().unwrap_or(0),
        _ => 0,
    };
    Ok(format!("connected — cloud has {count} stored segment(s)"))
}

/// Read one stdin line (used for the token-safe `atw config set sync.token -`).
/// A newline is enough; the panel keeps its end of the pipe open, so waiting
/// for EOF would hang the Save flow.
pub fn read_stdin_trimmed() -> Result<String> {
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf)?;
    Ok(buf.trim_end_matches(['\n', '\r']).to_string())
}

/// Directory helper used by the CLI paths.
pub fn dirs(config: &Config) -> crate::config::ResolvedDirs {
    config.resolved_dirs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::{CloudValue, LocalBackend};
    use crate::config::Config;

    const H: i64 = 3600;
    const DAY: i64 = 86_400;

    fn tmpdir(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("atw-sync-test-{}-{}", std::process::id(), label));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn deterministic_batch_ids_are_stable_and_order_independent() {
        let ids = vec!["b".to_string(), "a".to_string(), "c".to_string()];
        let ids2 = vec!["c".to_string(), "b".to_string(), "a".to_string()];
        assert_eq!(
            batch_id_for("device-1", &ids),
            batch_id_for("device-1", &ids2)
        );
        assert_ne!(
            batch_id_for("device-1", &ids),
            batch_id_for("device-2", &ids)
        );
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff_seconds(0), 30);
        assert_eq!(backoff_seconds(1), 60);
        assert_eq!(backoff_seconds(2), 120);
        assert_eq!(backoff_seconds(3), 240);
        assert_eq!(backoff_seconds(4), 480);
        assert_eq!(backoff_seconds(5), 900);
        assert_eq!(backoff_seconds(99), 900);
    }

    #[test]
    fn device_id_persists() {
        let dir = tmpdir("device");
        let a = device_id(&dir).unwrap();
        let b = device_id(&dir).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 36, "uuid v4 format");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn token_resolution_prefers_command() {
        let mut cfg = SyncConfig {
            token: "literal-token".into(),
            ..Default::default()
        };
        assert_eq!(resolve_token(&cfg).unwrap(), "literal-token");
        cfg.token_cmd = "printf 'from-keyring\\n'".into();
        assert_eq!(resolve_token(&cfg).unwrap(), "from-keyring");
        cfg.token_cmd = "false".into();
        assert!(resolve_token(&cfg).is_err());
        cfg.token_cmd = String::new();
        cfg.token = String::new();
        assert!(resolve_token(&cfg).is_err());
    }

    #[test]
    fn upload_pending_is_resumable_and_marks_only_after_ack() {
        // A backend that always fails simulates an offline period.
        struct Failing;
        impl CloudBackend for Failing {
            fn transaction(&mut self, _s: Vec<cloud::Stmt>) -> Result<()> {
                Err(crate::AtwError::Http("offline".into()))
            }
            fn query(&mut self, _sql: &str, _p: Vec<CloudValue>) -> Result<Vec<Vec<CloudValue>>> {
                Err(crate::AtwError::Http("offline".into()))
            }
        }

        let dir = tmpdir("offline");
        let data = dir.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let mut db = Db::open(&data.join("activity.db")).unwrap();
        let segs = vec![crate::tracker::Segment {
            class: "code".into(),
            display_name: "VS Code".into(),
            category: crate::classify::Category::Editor,
            started_at: 5 * DAY,
            ended_at: 5 * DAY + H,
        }];
        let mut open_row = None;
        db.flush(
            None,
            &mut open_row,
            &[],
            &segs,
            0,
            &crate::local_date_string,
        )
        .unwrap();
        assert_eq!(db.pending_count(-1).unwrap(), 1);

        // Offline: nothing is marked, the row stays pending.
        assert!(upload_pending(&db, &mut Failing, "dev", -1, 100).is_err());
        assert_eq!(db.pending_count(-1).unwrap(), 1);

        // Back online: same row uploads exactly once.
        let mut cloud_backend = LocalBackend::open_in_memory().unwrap();
        cloud_backend.init_schema().unwrap();
        let (uploaded, batches) = upload_pending(&db, &mut cloud_backend, "dev", -1, 200).unwrap();
        assert_eq!((uploaded, batches), (1, 1));
        assert_eq!(db.pending_count(-1).unwrap(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn crash_between_commit_and_ack_retries_as_duplicate() {
        let dir = tmpdir("crash");
        let data = dir.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let mut db = Db::open(&data.join("activity.db")).unwrap();
        let segs = vec![crate::tracker::Segment {
            class: "code".into(),
            display_name: "VS Code".into(),
            category: crate::classify::Category::Editor,
            started_at: 5 * DAY,
            ended_at: 5 * DAY + H,
        }];
        let mut open_row = None;
        db.flush(
            None,
            &mut open_row,
            &[],
            &segs,
            0,
            &crate::local_date_string,
        )
        .unwrap();

        let mut cloud_backend = LocalBackend::open_in_memory().unwrap();
        cloud_backend.init_schema().unwrap();
        // Simulate: cloud commit succeeded, then the process died before
        // mark_synced. The rows are still unsynced locally.
        let rows = db.unsynced_rows(-1, BATCH_CAP).unwrap();
        let ids: Vec<String> = rows.iter().map(|r| r.segment_uuid.clone()).collect();
        let batch_id = batch_id_for("dev", &ids);
        let segments: Vec<SegmentInput> = rows
            .iter()
            .map(|r| SegmentInput {
                segment_id: r.segment_uuid.clone(),
                app_name: r.display_name.clone(),
                app_class: r.application_class.clone(),
                category: r.category.clone(),
                start_utc: r.started_at,
                end_utc: r.ended_at,
                duration_seconds: r.duration_seconds,
                activity_date: r.date.clone(),
            })
            .collect();
        cloud::upload_batch(&mut cloud_backend, "dev", &batch_id, &segments, 100).unwrap();
        assert_eq!(db.pending_count(-1).unwrap(), 1);

        // Retry: same deterministic batch id → duplicate ack, local mark.
        let (uploaded, batches) = upload_pending(&db, &mut cloud_backend, "dev", -1, 200).unwrap();
        assert_eq!((uploaded, batches), (1, 1));
        assert_eq!(db.pending_count(-1).unwrap(), 0);
        // Cloud has exactly one segment and one batch.
        let segs_count = cloud_backend
            .query("SELECT COUNT(*) FROM segments", vec![])
            .unwrap();
        assert_eq!(segs_count[0][0], CloudValue::Int(1));
        let batches_count = cloud_backend
            .query("SELECT COUNT(*) FROM batches", vec![])
            .unwrap();
        assert_eq!(batches_count[0][0], CloudValue::Int(1));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sync_disabled_or_unconfigured_is_skipped() {
        let dir = tmpdir("skip");
        let data = dir.join("data");
        let state = dir.join("state");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        Db::open(&data.join("activity.db")).unwrap();

        let mut cfg = Config::default();
        cfg.sync.url = "https://example.turso.io".into();
        cfg.sync.token = "t".into();
        cfg.sync.enabled = false;
        let report = sync_once(&cfg, &data, &state, -1);
        assert!(report.skipped);

        cfg.sync.enabled = true;
        cfg.sync.url = String::new();
        let report = sync_once(&cfg, &data, &state, -1);
        assert!(report.skipped);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn retention_cutoff_is_ninety_days_back() {
        let now = 200 * DAY;
        assert_eq!(
            retention_cutoff(90, now),
            crate::local_date_string(110 * DAY)
        );
    }

    #[test]
    fn pull_walks_the_ingest_log_once_and_dedupes() {
        let dir = tmpdir("pull");
        let data = dir.join("data");
        std::fs::create_dir_all(&data).unwrap();

        // The laptop records and uploads one segment.
        let mut backend = LocalBackend::open_in_memory().unwrap();
        backend.init_schema().unwrap();
        let mut laptop = Db::open(&data.join("laptop.db")).unwrap();
        let up = vec![crate::tracker::Segment {
            class: "code".into(),
            display_name: "VS Code".into(),
            category: crate::classify::Category::Editor,
            started_at: 5 * DAY,
            ended_at: 5 * DAY + 3600,
        }];
        let mut open_row = None;
        laptop
            .flush(None, &mut open_row, &[], &up, 0, &crate::local_date_string)
            .unwrap();
        let (_, batches) =
            upload_pending(&laptop, &mut backend, "laptop", -1, 100).unwrap();
        assert_eq!(batches, 1);

        // This PC pulls the same cloud database in.
        let pc = Db::open(&data.join("pc.db")).unwrap();
        let record = SyncRecord::default();
        let since = "1970-01-01".to_string();
        let (pulled, boundary) =
            pull_remote(&pc, &mut backend, "pc", record.last_pull_cursor, &since, 200)
                .unwrap();
        assert_eq!(pulled, 1);
        assert_eq!(boundary, 100, "boundary is the page's ingested_at");

        // The laptop's hour is now on this PC...
        let totals = pc
            .today_totals(&crate::local_date_string(5 * DAY))
            .unwrap();
        assert_eq!(totals, vec![("VS Code".to_string(), 3600)]);
        // ...born synced, so it is never uploaded back...
        assert_eq!(pc.pending_count(-1).unwrap(), 0);
        // ...and the next pull returns nothing new (dedupe + cursor).
        let (pulled_second, _) =
            pull_remote(&pc, &mut backend, "pc", Some(boundary - 1), &since, 300).unwrap();
        assert_eq!(pulled_second, 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
