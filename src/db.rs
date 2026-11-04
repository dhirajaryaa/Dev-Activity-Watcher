//! SQLite storage: batched writes, WAL mode, migrations, 7-day retention.
//!
//! Design notes:
//! - One row per app usage segment (aggregated), not per event.
//! - The open segment is upserted in place at every flush, so there is
//!   exactly one row per segment — no duplicates, no per-second writes.
//! - Segments crossing midnight are split so `date` is always correct.
//! - `PRAGMA user_version` drives migrations; the schema is intentionally
//!   tiny because retention keeps the dataset at about a week.

use std::path::Path;

use rusqlite::{params, Connection};

use crate::tracker::Segment;
use crate::{AtwError, Result};

const SCHEMA_VERSION: i32 = 2;

pub struct Db {
    conn: Connection,
}

impl Db {
    /// Open (or create) the database. If the file cannot be opened at all,
    /// it is quarantined (renamed) and a fresh one is created — the activity
    /// DB is a small rolling cache, and staying functional matters more
    /// than preserving a corrupted file. The quarantine is never deleted
    /// automatically, so nothing is lost silently.
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match Self::open_inner(path) {
            Ok(db) => Ok(db),
            Err(err) => {
                let reason = match &err {
                    AtwError::Corrupt(msg) => msg.clone(),
                    AtwError::Db(rusqlite::Error::SqliteFailure(_, Some(msg))) if is_corruption(msg) => {
                        msg.clone()
                    }
                    _ => return Err(err),
                };
                quarantine_and_reopen(path, &reason)
            }
        }
    }

    fn open_inner(path: &Path) -> Result<Db> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;",
        )?;
        let check: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if check != "ok" {
            return Err(AtwError::Corrupt(check));
        }
        let version: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(AtwError::Config(format!(
                "database schema v{version} is newer than this build supports (v{SCHEMA_VERSION})"
            )));
        }
        if version < SCHEMA_VERSION {
            migrate(&conn, version)?;
        }
        Ok(Db { conn })
    }

    /// Write everything that changed in one transaction:
    /// 1. finalize already-flushed segments that ended since the last flush,
    /// 2. upsert the open segment (INSERT on first flush, UPDATE afterwards),
    /// 3. insert newly closed segments (split per date).
    pub fn flush(
        &mut self,
        open: Option<&Segment>,
        open_row: &mut Option<(i64, i64)>,
        finalize: &[(i64, Segment, i64)],
        pending: &[Segment],
        now: i64,
        date_of: &dyn Fn(i64) -> String,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;

        for (row_id, segment, _unflushed) in finalize {
            // Rewrite the row with the final, date-split segment. Each split
            // piece becomes its own immutable cloud segment with a fresh UUID
            // (the open row's UUID was never uploaded — only closed rows sync).
            tx.execute("DELETE FROM activity WHERE id = ?1", params![row_id])?;
            if segment.duration() > 0 {
                for piece in crate::tracker::split_by_date(segment, date_of) {
                    if piece.duration() > 0 {
                        tx.execute(
                            "INSERT INTO activity
                             (application_class, display_name, category, started_at, ended_at, duration_seconds, date, segment_uuid)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                            params![
                                piece.class,
                                piece.display_name,
                                piece.category.as_str(),
                                piece.started_at,
                                piece.ended_at,
                                piece.duration(),
                                date_of(piece.started_at),
                                uuid::Uuid::now_v7().to_string()
                            ],
                        )?;
                    }
                }
            }
        }

        // Defensive: if the finalized row is the one `open_row` still points
        // at, it is closed now — the caller must not upsert it again.
        if let Some((row_id, _)) = *open_row {
            if finalize.iter().any(|(id, _, _)| *id == row_id) {
                *open_row = None;
            }
        }

        if let Some(segment) = open {
            let elapsed = (now - segment.started_at).max(0);
            match *open_row {
                Some((row_id, _)) => {
                    tx.execute(
                        "UPDATE activity SET ended_at = ?1, duration_seconds = ?2 WHERE id = ?3",
                        params![now, elapsed, row_id],
                    )?;
                    // The flush anchor moves with the update: everything up
                    // to `now` is now reflected in the database.
                    *open_row = Some((row_id, now));
                }
                None => {
                    if elapsed > 0 {
                        tx.execute(
                            "INSERT INTO activity
                             (application_class, display_name, category, started_at, ended_at, duration_seconds, date, segment_uuid)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                            params![
                                segment.class,
                                segment.display_name,
                                segment.category.as_str(),
                                segment.started_at,
                                now,
                                elapsed,
                                date_of(segment.started_at),
                                uuid::Uuid::now_v7().to_string()
                            ],
                        )?;
                        *open_row = Some((tx.last_insert_rowid(), now));
                    }
                }
            }
        }

        for segment in pending {
            for piece in crate::tracker::split_by_date(segment, date_of) {
                if piece.duration() > 0 {
                    tx.execute(
                        "INSERT INTO activity
                         (application_class, display_name, category, started_at, ended_at, duration_seconds, date, segment_uuid)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![
                            piece.class,
                            piece.display_name,
                            piece.category.as_str(),
                            piece.started_at,
                            piece.ended_at,
                            piece.duration(),
                            date_of(piece.started_at),
                            uuid::Uuid::now_v7().to_string()
                        ],
                    )?;
                }
            }
        }

        tx.commit()?;
        Ok(())
    }

    /// Total seconds per display name for one local date, sorted by usage.
    pub fn today_totals(&self, date: &str) -> Result<Vec<(String, u64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT display_name, SUM(duration_seconds)
             FROM activity WHERE date = ?1
             GROUP BY display_name ORDER BY 2 DESC",
        )?;
        let rows = stmt.query_map(params![date], |row| {
            let seconds: i64 = row.get(1)?;
            Ok((row.get::<_, String>(0)?, seconds.max(0) as u64))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Top applications by total seconds since `since_date` (inclusive),
    /// sorted by usage, capped at 12 entries for the UI.
    pub fn week_totals(&self, since_date: &str) -> Result<Vec<(String, u64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT display_name, SUM(duration_seconds)
             FROM activity WHERE date >= ?1
             GROUP BY display_name ORDER BY 2 DESC LIMIT 12",
        )?;
        let rows = stmt.query_map(params![since_date], |row| {
            let seconds: i64 = row.get(1)?;
            Ok((row.get::<_, String>(0)?, seconds.max(0) as u64))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Per-day history since `since_date` (inclusive), newest day first,
    /// for the panel's optional history view. `top` caps the per-day app list.
    pub fn history(&self, since_date: &str, top: usize) -> Result<Vec<DayHistory>> {
        let mut totals = std::collections::BTreeMap::<String, u64>::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT date, SUM(duration_seconds)
                 FROM activity WHERE date >= ?1 GROUP BY date",
            )?;
            let rows = stmt.query_map(params![since_date], |row| {
                let seconds: i64 = row.get(1)?;
                Ok((row.get::<_, String>(0)?, seconds.max(0) as u64))
            })?;
            for row in rows {
                let (d, s) = row?;
                totals.insert(d, s);
            }
        }
        // Per-day per-app seconds, already sorted by date then usage.
        let mut per_day: std::collections::BTreeMap<String, Vec<(String, u64)>> =
            std::collections::BTreeMap::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT date, display_name, SUM(duration_seconds)
                 FROM activity WHERE date >= ?1
                 GROUP BY date, display_name ORDER BY date DESC, 3 DESC",
            )?;
            let rows = stmt.query_map(params![since_date], |row| {
                let seconds: i64 = row.get(2)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    seconds.max(0) as u64,
                ))
            })?;
            for row in rows {
                let (d, name, s) = row?;
                let list = per_day.entry(d).or_default();
                if list.len() < top {
                    list.push((name, s));
                }
            }
        }
        let mut out: Vec<DayHistory> = totals
            .into_iter()
            .map(|(date, total_seconds)| DayHistory {
                apps: per_day.remove(&date).unwrap_or_default(),
                date,
                total_seconds,
            })
            .collect();
        out.sort_by(|a, b| b.date.cmp(&a.date));
        Ok(out)
    }

    /// Delete everything strictly older than `cutoff_date` ("YYYY-MM-DD").
    /// When `only_synced` is true (cloud sync configured), rows that were
    /// never acknowledged by the cloud are kept — pending data is never
    /// deleted before its upload is confirmed.
    pub fn retain(&self, cutoff_date: &str, only_synced: bool) -> Result<usize> {
        let deleted = if only_synced {
            self.conn.execute(
                "DELETE FROM activity WHERE date < ?1 AND synced_at IS NOT NULL",
                params![cutoff_date],
            )?
        } else {
            self.conn
                .execute("DELETE FROM activity WHERE date < ?1", params![cutoff_date])?
        };
        Ok(deleted)
    }

    /// Rows waiting to be uploaded, excluding the open segment row.
    pub fn pending_count(&self, exclude_row_id: i64) -> Result<usize> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM activity WHERE synced_at IS NULL AND id != ?1",
            params![exclude_row_id],
            |r| r.get(0),
        )?;
        Ok(n.max(0) as usize)
    }

    /// Flag rows as uploaded after the cloud acknowledged their batch.
    pub fn mark_synced(&self, ids: &[i64], batch_id: &str, synced_at: i64) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let sql = format!(
            "UPDATE activity SET synced_at = ?1, sync_batch_id = ?2
             WHERE synced_at IS NULL AND id IN ({placeholders})"
        );
        let mut values = vec![
            rusqlite::types::Value::Integer(synced_at),
            rusqlite::types::Value::Text(batch_id.to_string()),
        ];
        for id in ids {
            values.push(rusqlite::types::Value::Integer(*id));
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let n = stmt.execute(rusqlite::params_from_iter(values))?;
        Ok(n)
    }

    /// Fold the WAL back into the main file (used on graceful shutdown).
    pub fn checkpoint(&self) -> Result<()> {
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }
}

/// One day of history for the panel's history view.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DayHistory {
    pub date: String,
    pub total_seconds: u64,
    /// Top apps for that day, sorted by usage (capped by the caller).
    pub apps: Vec<(String, u64)>,
}

/// One closed activity row, ready for cloud upload.
#[derive(Debug, Clone)]
pub struct SyncRow {
    pub id: i64,
    pub segment_uuid: String,
    pub application_class: String,
    pub display_name: String,
    pub category: String,
    pub started_at: i64,
    pub ended_at: i64,
    pub duration_seconds: i64,
    pub date: String,
}

impl Db {
    /// Closed, never-synced rows, oldest first, capped per batch.
    /// `exclude_row_id` is the open segment's row id (it is still growing —
    /// it syncs only after it closes); pass a negative id when there is none.
    pub fn unsynced_rows(&self, exclude_row_id: i64, limit: usize) -> Result<Vec<SyncRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, segment_uuid, application_class, display_name, category,
                    started_at, ended_at, duration_seconds, date
             FROM activity
             WHERE synced_at IS NULL AND id != ?1
             ORDER BY started_at
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![exclude_row_id, limit as i64], |row| {
            Ok(SyncRow {
                id: row.get(0)?,
                segment_uuid: row.get(1)?,
                application_class: row.get(2)?,
                display_name: row.get(3)?,
                category: row.get(4)?,
                started_at: row.get(5)?,
                ended_at: row.get(6)?,
                duration_seconds: row.get(7)?,
                date: row.get(8)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

fn migrate(conn: &Connection, from: i32) -> Result<()> {
    if from < 1 {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS activity (
                 id INTEGER PRIMARY KEY,
                 application_class TEXT NOT NULL,
                 display_name TEXT NOT NULL,
                 category TEXT NOT NULL DEFAULT 'other',
                 started_at INTEGER NOT NULL,
                 ended_at INTEGER NOT NULL,
                 duration_seconds INTEGER NOT NULL,
                 date TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_activity_date ON activity(date);
             CREATE INDEX IF NOT EXISTS idx_activity_class_date ON activity(application_class, date);
             PRAGMA user_version = 1;
             COMMIT;",
        )?;
    }
    if from < 2 {
        // v2 — cloud sync: every segment row gets a globally unique id so the
        // cloud can deduplicate across devices, plus per-row sync state.
        // The local autoincrement `id` is not unique across devices.
        conn.execute_batch(
            "BEGIN;
             ALTER TABLE activity ADD COLUMN segment_uuid TEXT;
             ALTER TABLE activity ADD COLUMN synced_at INTEGER;
             ALTER TABLE activity ADD COLUMN sync_batch_id TEXT;
             COMMIT;",
        )?;
        // Backfill UUIDs for rows created before this migration. SQLite has
        // no UUID function, so this happens here in Rust, row by row.
        let ids: Vec<i64> = {
            let mut stmt = conn.prepare(
                "SELECT id FROM activity WHERE segment_uuid IS NULL",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, i64>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        for id in ids {
            conn.execute(
                "UPDATE activity SET segment_uuid = ?1 WHERE id = ?2",
                params![uuid::Uuid::now_v7().to_string(), id],
            )?;
        }
        conn.execute_batch(
            "BEGIN;
             CREATE UNIQUE INDEX IF NOT EXISTS idx_activity_segment_uuid
                 ON activity(segment_uuid);
             PRAGMA user_version = 2;
             COMMIT;",
        )?;
    }
    Ok(())
}

fn is_corruption(message: &str) -> bool {
    let lowered = message.to_lowercase();
    lowered.contains("corrupt")
        || lowered.contains("not a database")
        || lowered.contains("malformed")
        || lowered.contains("encrypted")
}

fn quarantine_and_reopen(path: &Path, reason: &str) -> Result<Db> {
    let quarantine = path.with_extension(format!("corrupt-{}", crate::unix_now()));
    tracing::error!(
        target: "atw",
        reason = %reason,
        from = %path.display(),
        to = %quarantine.display(),
        "database is corrupted; quarantining it and starting a fresh one"
    );
    std::fs::rename(path, &quarantine)?;
    // Stale sidecar files of the corrupted database.
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
    let _ = std::fs::remove_file(path.with_extension("db-journal"));
    Db::open_inner(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::Category;
    use crate::tracker::{split_by_date, Segment};

    const DAY: i64 = 86_400;

    fn utc_date(ts: i64) -> String {
        use chrono::TimeZone;
        chrono::Utc
            .timestamp_opt(ts, 0)
            .single()
            .unwrap()
            .date_naive()
            .to_string()
    }

    fn day5(hours: i64, minutes: i64, seconds: i64) -> i64 {
        5 * DAY + hours * 3600 + minutes * 60 + seconds
    }

    fn seg(class: &str, display: &str, started: i64, ended: i64) -> Segment {
        Segment {
            class: class.to_string(),
            display_name: display.to_string(),
            category: Category::Other,
            started_at: started,
            ended_at: ended,
        }
    }

    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!("atw-test-{}-{}", std::process::id(), label));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn open_creates_schema_once() {
        let dir = TempDir::new("schema");
        let path = dir.0.join("activity.db");
        drop(Db::open(&path).unwrap());
        drop(Db::open(&path).unwrap()); // second open must not fail on version
        let conn = Connection::open(&path).unwrap();
        let version: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn flush_inserts_pending_and_splits_midnight() {
        let dir = TempDir::new("midnight");
        let mut db = Db::open(&dir.0.join("activity.db")).unwrap();
        let segments = vec![
            seg("code", "Visual Studio Code", day5(23, 30, 0), 6 * DAY + 1800),
            seg("firefox", "Firefox", day5(9, 0, 0), day5(10, 0, 0)),
        ];
        let mut open_row = None;
        db.flush(None, &mut open_row, &[], &segments, 0, &utc_date).unwrap();
        assert_eq!(open_row, None);

        let day5_totals = db.today_totals("1970-01-06").unwrap();
        // Jan 6 holds the full Firefox hour plus the pre-midnight half hour of VS Code.
        assert_eq!(
            day5_totals,
            vec![("Firefox".to_string(), 3600), ("Visual Studio Code".to_string(), 1800)]
        );
        let day6_totals = db.today_totals("1970-01-07").unwrap();
        assert_eq!(day6_totals, vec![("Visual Studio Code".to_string(), 1800)]);
    }

    #[test]
    fn open_segment_upserts_single_row() {
        let dir = TempDir::new("upsert");
        let mut db = Db::open(&dir.0.join("activity.db")).unwrap();
        let live = seg("code", "Visual Studio Code", day5(9, 0, 0), 0);

        let mut open_row = None;
        db.flush(Some(&live), &mut open_row, &[], &[], day5(9, 5, 0), &utc_date).unwrap();
        let first = open_row.unwrap();
        db.flush(Some(&live), &mut open_row, &[], &[], day5(9, 10, 0), &utc_date).unwrap();
        assert_eq!(open_row.unwrap().0, first.0); // same row, no duplicates

        // App switch finalizes the row at its real end time: the row is
        // rewritten with the full 20-minute segment.
        let full = seg("code", "Visual Studio Code", day5(9, 0, 0), day5(9, 20, 0));
        let finalize = vec![(first.0, full, 900)];
        db.flush(None, &mut open_row, &finalize, &[], 0, &utc_date).unwrap();
        assert_eq!(open_row, None);

        let conn_count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM activity", [], |r| r.get(0))
            .unwrap();
        assert_eq!(conn_count, 1);
        let total: i64 = db
            .conn
            .query_row("SELECT duration_seconds FROM activity", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 1200);
    }

    #[test]
    fn finalize_with_zero_duration_removes_stub_row() {
        let dir = TempDir::new("stub");
        let mut db = Db::open(&dir.0.join("activity.db")).unwrap();
        let live = seg("code", "Visual Studio Code", day5(9, 0, 0), 0);
        let mut open_row = None;
        db.flush(Some(&live), &mut open_row, &[], &[], day5(9, 2, 0), &utc_date).unwrap();
        let row = open_row.unwrap().0;
        // A truly zero-duration segment (e.g. idle fired right after focus).
        let empty = seg("code", "Visual Studio Code", day5(9, 0, 0), day5(9, 0, 0));
        db.flush(None, &mut open_row, &[(row, empty, 0)], &[], 0, &utc_date).unwrap();
        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM activity", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn retention_deletes_only_old_rows() {
        let dir = TempDir::new("retention");
        let mut db = Db::open(&dir.0.join("activity.db")).unwrap();
        let pending = vec![
            seg("old1", "Old1", 1 * DAY, 1 * DAY + 100),  // 1970-01-02
            seg("old2", "Old2", 2 * DAY, 2 * DAY + 100),  // 1970-01-03
            seg("new", "New", day5(9, 0, 0), day5(9, 30, 0)), // 1970-01-06
        ];
        let mut open_row = None;
        db.flush(None, &mut open_row, &[], &pending, 0, &utc_date).unwrap();

        let deleted = db.retain("1970-01-04", false).unwrap();
        assert_eq!(deleted, 2);
        assert!(db.today_totals("1970-01-02").unwrap().is_empty());
        assert!(db.today_totals("1970-01-03").unwrap().is_empty());
        assert_eq!(db.today_totals("1970-01-06").unwrap().len(), 1);
    }

    #[test]
    fn migration_v2_backfills_segment_uuids() {
        let dir = TempDir::new("v2");
        let path = dir.0.join("activity.db");
        // Create a v1 database by hand.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE activity (
                     id INTEGER PRIMARY KEY,
                     application_class TEXT NOT NULL,
                     display_name TEXT NOT NULL,
                     category TEXT NOT NULL DEFAULT 'other',
                     started_at INTEGER NOT NULL,
                     ended_at INTEGER NOT NULL,
                     duration_seconds INTEGER NOT NULL,
                     date TEXT NOT NULL
                 );
                 INSERT INTO activity (application_class, display_name, category, started_at, ended_at, duration_seconds, date)
                 VALUES ('code', 'VS Code', 'editor', 100, 200, 100, '1970-01-01');
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }
        drop(Db::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        let version: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, 2);
        let (uuid, is_null): (String, i64) = conn
            .query_row(
                "SELECT segment_uuid, synced_at IS NULL FROM activity WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(!uuid.is_empty());
        assert_eq!(is_null, 1, "backfilled rows are not marked synced");
    }

    #[test]
    fn sync_roundtrip_marks_and_excludes() {
        let dir = TempDir::new("syncq");
        let mut db = Db::open(&dir.0.join("activity.db")).unwrap();
        let pending = vec![
            seg("code", "VS Code", day5(9, 0, 0), day5(9, 30, 0)),
            seg("foot", "Foot", day5(10, 0, 0), day5(10, 15, 0)),
        ];
        let mut open_row = None;
        db.flush(Some(&pending[0].clone()), &mut open_row, &[], &pending[1..], day5(10, 20, 0), &utc_date)
            .unwrap();
        let open_id = open_row.unwrap().0;

        // The open row is excluded from sync candidates.
        let rows = db.unsynced_rows(open_id, 500).unwrap();
        assert_eq!(rows.len(), 1, "only the closed Foot row is a candidate");
        assert_eq!(rows[0].display_name, "Foot");
        assert!(!rows[0].segment_uuid.is_empty());

        // With no open row, both rows are candidates, oldest first.
        db.flush(None, &mut open_row, &[], &[], 0, &utc_date).unwrap();
        let rows = db.unsynced_rows(-1, 500).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].started_at <= rows[1].started_at);

        // Ack marks exactly the requested rows.
        let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
        let n = db.mark_synced(&ids, "batch-1", 12345).unwrap();
        assert_eq!(n, 2);
        assert_eq!(db.pending_count(-1).unwrap(), 0);
        let rows = db.unsynced_rows(-1, 500).unwrap();
        assert!(rows.is_empty());

        // A second mark of the same batch is a no-op (idempotent ack).
        let n = db.mark_synced(&ids, "batch-1", 12345).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn unsynced_rows_respects_limit() {
        let dir = TempDir::new("limit");
        let mut db = Db::open(&dir.0.join("activity.db")).unwrap();
        let pending: Vec<Segment> = (0..5)
            .map(|i| seg("code", "VS Code", day5(9 + i, 0, 0), day5(9 + i, 0, 30)))
            .collect();
        let mut open_row = None;
        db.flush(None, &mut open_row, &[], &pending, 0, &utc_date).unwrap();
        let rows = db.unsynced_rows(-1, 3).unwrap();
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn retention_keeps_unsynced_rows_when_syncing() {
        let dir = TempDir::new("retsync");
        let mut db = Db::open(&dir.0.join("activity.db")).unwrap();
        let pending = vec![
            seg("old1", "Old1", 1 * DAY, 1 * DAY + 100), // 1970-01-02
            seg("old2", "Old2", 2 * DAY, 2 * DAY + 100), // 1970-01-03
            seg("new", "New", day5(9, 0, 0), day5(9, 30, 0)),
        ];
        let mut open_row = None;
        db.flush(None, &mut open_row, &[], &pending, 0, &utc_date).unwrap();
        // Ack only the first old row so it becomes deletable.
        let ids: Vec<i64> = db.unsynced_rows(-1, 500).unwrap().iter().map(|r| r.id).collect();
        db.mark_synced(&ids[..1], "b", 5).unwrap();

        // With sync: the acked old row goes; the un-acked old row survives.
        let deleted = db.retain("1970-01-04", true).unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(db.pending_count(-1).unwrap(), 2); // old2 unsynced + today's

        // Without sync: everything old goes regardless of sync state.
        let deleted = db.retain("1970-01-04", false).unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(db.pending_count(-1).unwrap(), 1); // today's row only
    }

    #[test]
    fn corrupted_database_is_quarantined_and_recreated() {
        let dir = TempDir::new("corrupt");
        let path = dir.0.join("activity.db");
        std::fs::write(&path, b"this is definitely not a sqlite database").unwrap();
        let mut db = Db::open(&path).unwrap();
        // Fresh database works.
        let mut open_row = None;
        let pending = vec![seg("code", "Visual Studio Code", day5(9, 0, 0), day5(9, 5, 0))];
        db.flush(None, &mut open_row, &[], &pending, 0, &utc_date).unwrap();
        assert_eq!(db.today_totals("1970-01-06").unwrap().len(), 1);
        // Quarantine copy still exists for inspection.
        let quarantine: Vec<_> = std::fs::read_dir(&dir.0)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("corrupt"))
            .collect();
        assert_eq!(quarantine.len(), 1);
    }

    #[test]
    fn split_then_flush_preserves_total_seconds() {
        let dir = TempDir::new("preserve");
        let mut db = Db::open(&dir.0.join("activity.db")).unwrap();
        let long = seg("code", "Visual Studio Code", day5(22, 0, 0), 6 * DAY + 600);
        let pieces = split_by_date(&long, &utc_date);
        let mut open_row = None;
        db.flush(None, &mut open_row, &[], &pieces, 0, &utc_date).unwrap();
        let sum: i64 = db
            .conn
            .query_row("SELECT SUM(duration_seconds) FROM activity", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sum, long.duration());
    }
}
