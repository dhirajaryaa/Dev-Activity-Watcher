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

const SCHEMA_VERSION: i32 = 1;

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
            // Rewrite the row with the final, date-split segment.
            tx.execute("DELETE FROM activity WHERE id = ?1", params![row_id])?;
            if segment.duration() > 0 {
                for piece in crate::tracker::split_by_date(segment, date_of) {
                    if piece.duration() > 0 {
                        tx.execute(
                            "INSERT INTO activity
                             (application_class, display_name, category, started_at, ended_at, duration_seconds, date)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                            params![
                                piece.class,
                                piece.display_name,
                                piece.category.as_str(),
                                piece.started_at,
                                piece.ended_at,
                                piece.duration(),
                                date_of(piece.started_at)
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
                             (application_class, display_name, category, started_at, ended_at, duration_seconds, date)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                            params![
                                segment.class,
                                segment.display_name,
                                segment.category.as_str(),
                                segment.started_at,
                                now,
                                elapsed,
                                date_of(segment.started_at)
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
                         (application_class, display_name, category, started_at, ended_at, duration_seconds, date)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![
                            piece.class,
                            piece.display_name,
                            piece.category.as_str(),
                            piece.started_at,
                            piece.ended_at,
                            piece.duration(),
                            date_of(piece.started_at)
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

    /// Delete everything strictly older than `cutoff_date` ("YYYY-MM-DD").
    /// Returns the number of deleted rows.
    pub fn retain(&self, cutoff_date: &str) -> Result<usize> {
        let deleted = self
            .conn
            .execute("DELETE FROM activity WHERE date < ?1", params![cutoff_date])?;
        Ok(deleted)
    }

    /// Fold the WAL back into the main file (used on graceful shutdown).
    pub fn checkpoint(&self) -> Result<()> {
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
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

        let deleted = db.retain("1970-01-04").unwrap();
        assert_eq!(deleted, 2);
        assert!(db.today_totals("1970-01-02").unwrap().is_empty());
        assert!(db.today_totals("1970-01-03").unwrap().is_empty());
        assert_eq!(db.today_totals("1970-01-06").unwrap().len(), 1);
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
