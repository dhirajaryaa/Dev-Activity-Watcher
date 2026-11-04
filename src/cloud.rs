//! Cloud-side data model and repository for the user-owned Turso database.
//!
//! BYOD: the database itself is the user scope — one database per user, so
//! there is no separate user id. Aggregation runs **device-side,
//! transactionally**: every syncing device executes the same statements, so
//! the merge logic lives in one shared, versioned routine
//! (`AGGREGATE_VERSION` in the `meta` table; a mismatch triggers a full
//! recompute of every stored date).
//!
//! `CloudBackend` abstracts statement execution so tests run against a local
//! rusqlite connection (`LocalBackend`) while production talks HTTP to Turso
//! (`http_pipeline::HttpBackend`). Both must behave identically: statements
//! run in one transaction — either all apply or none do.
//!
//! Interval merging in SQL uses the classic running-max window trick: a
//! segment starts a new merged run when its `start` is past the running max
//! `end` of everything before it (per app, or globally for unique time).
//! `src/merge.rs` is the pure-Rust oracle these queries are tested against.

use crate::{AtwError, Result};

/// Bump when the aggregation SQL changes shape; devices then recompute all
/// stored dates once, so aggregates written by older versions are replaced.
pub const AGGREGATE_VERSION: i64 = 1;

/// A statement with bound parameters. Values are typed coarsely because the
/// cloud schema only uses TEXT and INTEGER.
#[derive(Debug, Clone, PartialEq)]
pub enum CloudValue {
    Null,
    Text(String),
    Int(i64),
}

impl CloudValue {
    pub fn text(s: impl Into<String>) -> CloudValue {
        CloudValue::Text(s.into())
    }
    pub fn int(i: i64) -> CloudValue {
        CloudValue::Int(i)
    }
}

/// One SQL statement plus its bound parameters.
#[derive(Debug, Clone)]
pub struct Stmt {
    pub sql: String,
    pub params: Vec<CloudValue>,
}

impl Stmt {
    pub fn new(sql: impl Into<String>, params: Vec<CloudValue>) -> Stmt {
        Stmt {
            sql: sql.into(),
            params,
        }
    }
}

/// Transactional execution against the cloud database.
pub trait CloudBackend {
    /// Run all statements in one transaction: either every statement applies
    /// or none do. A UNIQUE-constraint failure is reported as
    /// `AtwError::UniqueConstraint` so callers can treat it as a signal
    /// (duplicate batch) rather than a hard error.
    fn transaction(&mut self, stmts: Vec<Stmt>) -> Result<()>;

    /// Run a read-only query and return the resulting rows.
    fn query(&mut self, sql: &str, params: Vec<CloudValue>) -> Result<Vec<Vec<CloudValue>>>;
}

/// Schema for the cloud database. Idempotent (IF NOT EXISTS) so the first
/// syncing device creates everything and later devices are no-ops.
pub const CLOUD_DDL: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    // Device registry — identity is the UUID, the label is cosmetic only.
    "CREATE TABLE IF NOT EXISTS devices (
        device_id  TEXT PRIMARY KEY,
        label      TEXT,
        first_seen INTEGER NOT NULL,
        last_sync  INTEGER
     );",
    // Batch identity: replay protection. Checked INSIDE the transaction,
    // never trusted to UPSERT alone.
    "CREATE TABLE IF NOT EXISTS batches (
        batch_id      TEXT PRIMARY KEY,
        device_id     TEXT NOT NULL,
        segment_count INTEGER NOT NULL,
        processed_at  INTEGER NOT NULL
     );",
    // Immutable raw segments (90-day retention; aggregates outlive them).
    "CREATE TABLE IF NOT EXISTS segments (
        segment_id       TEXT PRIMARY KEY,
        device_id        TEXT NOT NULL,
        app_name         TEXT NOT NULL,
        app_class        TEXT NOT NULL,
        category         TEXT NOT NULL,
        start_utc        INTEGER NOT NULL,
        end_utc          INTEGER NOT NULL,
        duration_seconds INTEGER NOT NULL,
        activity_date    TEXT NOT NULL,
        ingested_at      INTEGER NOT NULL
     );",
    "CREATE INDEX IF NOT EXISTS idx_segments_device_date ON segments(device_id, activity_date);",
    "CREATE INDEX IF NOT EXISTS idx_segments_date ON segments(activity_date);",
    "CREATE INDEX IF NOT EXISTS idx_segments_range ON segments(start_utc, end_utc);",
    // Pre-aggregated daily stats, recomputed from segments on every write.
    // combined_seconds is the plain Σ (may exceed 24 h across devices);
    // merged_seconds is the union of that app's intervals.
    "CREATE TABLE IF NOT EXISTS daily_app (
        activity_date    TEXT NOT NULL,
        app_name         TEXT NOT NULL,
        combined_seconds INTEGER NOT NULL,
        merged_seconds   INTEGER NOT NULL,
        updated_at       INTEGER NOT NULL,
        PRIMARY KEY (activity_date, app_name)
     );",
    // unique_active_seconds: union across ALL apps and devices (≤ 24 h).
    // overlap_seconds: Σ per-app merged − global union, attributed to the
    // explicit "(overlapping)" category instead of being double-counted.
    "CREATE TABLE IF NOT EXISTS daily_unique (
        activity_date         TEXT PRIMARY KEY,
        unique_active_seconds INTEGER NOT NULL,
        overlap_seconds       INTEGER NOT NULL,
        updated_at            INTEGER NOT NULL
     );",
];

/// One segment as it travels from the local DB to the cloud.
#[derive(Debug, Clone)]
pub struct SegmentInput {
    pub segment_id: String,
    pub app_name: String,
    pub app_class: String,
    pub category: String,
    pub start_utc: i64,
    pub end_utc: i64,
    pub duration_seconds: i64,
    /// Device-local reporting date (YYYY-MM-DD).
    pub activity_date: String,
}

/// Result of an upload attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadOutcome {
    /// Batch was new and fully applied.
    Applied { segments: usize, dates: Vec<String> },
    /// This exact batch was already processed before — safe to ack.
    Duplicate,
}

/// SQL that recomputes `daily_app` for one date from immutable segments.
/// Uses bare `?` placeholders (no `?NNN` reuse) so the same string works for
/// rusqlite and the HTTP pipeline.
const RECOMPUTE_DAILY_APP: &str = r#"
WITH ordered AS (
    SELECT app_name, start_utc AS s, end_utc AS e
    FROM segments WHERE activity_date = ?
),
marks AS (
    SELECT app_name, s, e,
        MAX(e) OVER (
            PARTITION BY app_name ORDER BY s, e
            ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING
        ) AS prev_end
    FROM ordered
),
starts AS (
    SELECT app_name, s, e,
        CASE WHEN prev_end IS NULL OR s > prev_end THEN 1 ELSE 0 END AS is_new
    FROM marks
),
tagged AS (
    SELECT app_name, s, e,
        SUM(is_new) OVER (
            PARTITION BY app_name ORDER BY s, e
            ROWS UNBOUNDED PRECEDING
        ) AS grp
    FROM starts
),
merged AS (
    SELECT app_name, MIN(s) AS ms, MAX(e) AS me
    FROM tagged GROUP BY app_name, grp
),
combined AS (
    SELECT app_name, SUM(duration_seconds) AS total
    FROM segments WHERE activity_date = ?
    GROUP BY app_name
)
INSERT INTO daily_app (activity_date, app_name, combined_seconds, merged_seconds, updated_at)
SELECT ?, m.app_name, c.total, SUM(m.me - m.ms), ?
FROM merged m JOIN combined c ON c.app_name = m.app_name
GROUP BY m.app_name, c.total
"#;

/// SQL that recomputes `daily_unique` for one date. Must run AFTER
/// `RECOMPUTE_DAILY_APP` for the same date (overlap reads daily_app).
const RECOMPUTE_DAILY_UNIQUE: &str = r#"
WITH ordered AS (
    SELECT start_utc AS s, end_utc AS e
    FROM segments WHERE activity_date = ?
),
marks AS (
    SELECT s, e,
        MAX(e) OVER (
            ORDER BY s, e
            ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING
        ) AS prev_end
    FROM ordered
),
starts AS (
    SELECT s, e,
        CASE WHEN prev_end IS NULL OR s > prev_end THEN 1 ELSE 0 END AS is_new
    FROM marks
),
tagged AS (
    SELECT s, e,
        SUM(is_new) OVER (
            ORDER BY s, e
            ROWS UNBOUNDED PRECEDING
        ) AS grp
    FROM starts
),
merged AS (
    SELECT MIN(s) AS ms, MAX(e) AS me FROM tagged GROUP BY grp
)
INSERT INTO daily_unique (activity_date, unique_active_seconds, overlap_seconds, updated_at)
SELECT ?,
    SUM(me - ms),
    (SELECT COALESCE(SUM(merged_seconds), 0) FROM daily_app WHERE activity_date = ?)
        - SUM(me - ms),
    ?
FROM merged
"#;

/// Upload one batch of segments in a single atomic transaction:
/// 1. insert the batch identity (duplicate → ack as already processed),
/// 2. insert segments (INSERT OR IGNORE — globally unique segment ids),
/// 3. recompute daily aggregates for every affected date from scratch,
/// 4. register the device + aggregate version.
///
/// The whole thing either commits or rolls back — a failed batch never
/// leaves partial aggregate changes behind.
pub fn upload_batch(
    backend: &mut dyn CloudBackend,
    device_id: &str,
    batch_id: &str,
    segments: &[SegmentInput],
    now: i64,
) -> Result<UploadOutcome> {
    // Duplicate batch? Check inside the flow; the transaction below re-checks
    // via the PRIMARY KEY constraint for the race window.
    let existing = backend.query(
        "SELECT 1 FROM batches WHERE batch_id = ?",
        vec![CloudValue::text(batch_id)],
    )?;
    if !existing.is_empty() {
        return Ok(UploadOutcome::Duplicate);
    }

    // Which dates does this batch touch? (Deduped, sorted for determinism.)
    let mut dates: Vec<String> = segments.iter().map(|s| s.activity_date.clone()).collect();
    dates.sort();
    dates.dedup();

    // Version check: if the stored aggregate version differs (older device
    // wrote it, or this is a newer build), recompute EVERY stored date once.
    let mut recompute_dates = dates.clone();
    let version_rows = backend.query(
        "SELECT value FROM meta WHERE key = 'aggregate_version'",
        vec![],
    )?;
    let stored_version = version_rows
        .first()
        .and_then(|row| row.first())
        .and_then(|v| match v {
            CloudValue::Text(s) => s.parse::<i64>().ok(),
            CloudValue::Int(i) => Some(*i),
            _ => None,
        });
    if stored_version != Some(AGGREGATE_VERSION) {
        let all = backend.query(
            "SELECT DISTINCT activity_date FROM segments ORDER BY activity_date",
            vec![],
        )?;
        for row in all {
            if let Some(CloudValue::Text(d)) = row.first() {
                if !recompute_dates.contains(d) {
                    recompute_dates.push(d.clone());
                }
            }
        }
    }

    let mut stmts: Vec<Stmt> = Vec::new();

    // 1. Batch identity first: a concurrent duplicate insert fails the whole
    //    transaction, which the caller treats as "already processed".
    stmts.push(Stmt::new(
        "INSERT INTO batches (batch_id, device_id, segment_count, processed_at) VALUES (?, ?, ?, ?)",
        vec![
            CloudValue::text(batch_id),
            CloudValue::text(device_id),
            CloudValue::int(segments.len() as i64),
            CloudValue::int(now),
        ],
    ));

    // 2. Segments. OR IGNORE: a repeated segment (e.g. the device crashed
    //    between cloud commit and local ack, then re-uploaded in a new batch)
    //    is a no-op, never a double count.
    for s in segments {
        stmts.push(Stmt::new(
            "INSERT OR IGNORE INTO segments
                (segment_id, device_id, app_name, app_class, category,
                 start_utc, end_utc, duration_seconds, activity_date, ingested_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            vec![
                CloudValue::text(&s.segment_id),
                CloudValue::text(device_id),
                CloudValue::text(&s.app_name),
                CloudValue::text(&s.app_class),
                CloudValue::text(&s.category),
                CloudValue::int(s.start_utc),
                CloudValue::int(s.end_utc),
                CloudValue::int(s.duration_seconds),
                CloudValue::text(&s.activity_date),
                CloudValue::int(now),
            ],
        ));
    }

    // 3. Recompute affected dates from scratch (never incremental mutation —
    //    late-arriving batches simply recompute their date).
    for date in &recompute_dates {
        stmts.push(Stmt::new(
            "DELETE FROM daily_app WHERE activity_date = ?",
            vec![CloudValue::text(date)],
        ));
        stmts.push(Stmt::new(
            "DELETE FROM daily_unique WHERE activity_date = ?",
            vec![CloudValue::text(date)],
        ));
        stmts.push(Stmt::new(
            RECOMPUTE_DAILY_APP,
            vec![
                CloudValue::text(date),
                CloudValue::text(date),
                CloudValue::text(date),
                CloudValue::int(now),
            ],
        ));
        stmts.push(Stmt::new(
            RECOMPUTE_DAILY_UNIQUE,
            vec![
                CloudValue::text(date),
                CloudValue::text(date),
                CloudValue::text(date),
                CloudValue::int(now),
            ],
        ));
    }

    // 4. Device registry + aggregate version marker.
    stmts.push(Stmt::new(
        "INSERT INTO devices (device_id, label, first_seen, last_sync)
         VALUES (?, NULL, ?, ?)
         ON CONFLICT(device_id) DO UPDATE SET last_sync = excluded.last_sync",
        vec![
            CloudValue::text(device_id),
            CloudValue::int(now),
            CloudValue::int(now),
        ],
    ));
    stmts.push(Stmt::new(
        "INSERT INTO meta (key, value) VALUES ('aggregate_version', ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        vec![CloudValue::int(AGGREGATE_VERSION)],
    ));

    match backend.transaction(stmts) {
        Ok(()) => Ok(UploadOutcome::Applied {
            segments: segments.len(),
            dates,
        }),
        Err(AtwError::UniqueConstraint(_)) => Ok(UploadOutcome::Duplicate),
        Err(e) => Err(e),
    }
}

/// Cloud retention: raw segments older than `cutoff_date` are deleted;
/// daily aggregates are preserved (the portfolio's history outlives the
/// raw rows). Batch records age out with the segments — replay protection
/// is not needed once the segments themselves are gone, and segment-level
/// `INSERT OR IGNORE` keeps any late replay harmless anyway.
pub fn cloud_retention(
    backend: &mut dyn CloudBackend,
    cutoff_date: &str,
    now: i64,
) -> Result<usize> {
    let before = backend.query(
        "SELECT COUNT(*) FROM segments WHERE activity_date < ?",
        vec![CloudValue::text(cutoff_date)],
    )?;
    let count = match before.first().and_then(|r| r.first()) {
        Some(CloudValue::Int(n)) => *n,
        _ => 0,
    };
    if count == 0 {
        return Ok(0);
    }
    backend.transaction(vec![
        Stmt::new(
            "DELETE FROM segments WHERE activity_date < ?",
            vec![CloudValue::text(cutoff_date)],
        ),
        Stmt::new(
            "DELETE FROM batches WHERE processed_at < ?",
            vec![CloudValue::int(now - 90 * 86_400)],
        ),
    ])?;
    Ok(count as usize)
}

/// Portfolio read: one day's pre-aggregated stats. Read-only token friendly.
pub fn portfolio_day(
    backend: &mut dyn CloudBackend,
    date: &str,
) -> Result<Vec<(String, i64, i64)>> {
    let rows = backend.query(
        "SELECT app_name, combined_seconds, merged_seconds
         FROM daily_app WHERE activity_date = ? ORDER BY combined_seconds DESC",
        vec![CloudValue::text(date)],
    )?;
    Ok(rows
        .into_iter()
        .filter_map(|row| match (row.first(), row.get(1), row.get(2)) {
            (
                Some(CloudValue::Text(name)),
                Some(CloudValue::Int(combined)),
                Some(CloudValue::Int(merged)),
            ) => Some((name.clone(), *combined, *merged)),
            _ => None,
        })
        .collect())
}

/// Portfolio read: unique active seconds per day, newest first.
pub fn portfolio_unique(
    backend: &mut dyn CloudBackend,
    since_date: &str,
) -> Result<Vec<(String, i64, i64)>> {
    let rows = backend.query(
        "SELECT activity_date, unique_active_seconds, overlap_seconds
         FROM daily_unique WHERE activity_date >= ? ORDER BY activity_date DESC",
        vec![CloudValue::text(since_date)],
    )?;
    Ok(rows
        .into_iter()
        .filter_map(|row| match (row.first(), row.get(1), row.get(2)) {
            (
                Some(CloudValue::Text(date)),
                Some(CloudValue::Int(unique)),
                Some(CloudValue::Int(overlap)),
            ) => Some((date.clone(), *unique, *overlap)),
            _ => None,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// LocalBackend — rusqlite against a real SQLite file (or :memory: for tests).
// Production uses http_pipeline::HttpBackend; both satisfy CloudBackend.
// ---------------------------------------------------------------------------

pub struct LocalBackend {
    conn: rusqlite::Connection,
}

impl LocalBackend {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let conn = rusqlite::Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;")?;
        Ok(LocalBackend { conn })
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = rusqlite::Connection::open_in_memory()?;
        Ok(LocalBackend { conn })
    }

    /// Create the cloud schema (tests and first-run bootstrap).
    pub fn init_schema(&mut self) -> Result<()> {
        let stmts: Vec<Stmt> = CLOUD_DDL.iter().map(|s| Stmt::new(*s, vec![])).collect();
        self.transaction(stmts)
    }
}

impl CloudBackend for LocalBackend {
    fn transaction(&mut self, stmts: Vec<Stmt>) -> Result<()> {
        let tx = self.conn.transaction()?;
        for stmt in stmts {
            execute_rusqlite(&tx, &stmt).map_err(map_constraint)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn query(&mut self, sql: &str, params: Vec<CloudValue>) -> Result<Vec<Vec<CloudValue>>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rusqlite_params = to_rusqlite_params(&params);
        let column_count = stmt.column_count();
        let mut rows = stmt.query(rusqlite::params_from_iter(rusqlite_params.iter()))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let mut values = Vec::with_capacity(column_count);
            for i in 0..column_count {
                values.push(match row.get_ref(i)? {
                    rusqlite::types::ValueRef::Null => CloudValue::Null,
                    rusqlite::types::ValueRef::Integer(i) => CloudValue::Int(i),
                    rusqlite::types::ValueRef::Real(_) => CloudValue::Null, // unused schema
                    rusqlite::types::ValueRef::Text(t) => {
                        CloudValue::Text(String::from_utf8_lossy(t).into_owned())
                    }
                    rusqlite::types::ValueRef::Blob(_) => CloudValue::Null, // unused schema
                });
            }
            out.push(values);
        }
        Ok(out)
    }
}

fn execute_rusqlite(
    tx: &rusqlite::Transaction<'_>,
    stmt: &Stmt,
) -> std::result::Result<usize, rusqlite::Error> {
    let mut prepared = tx.prepare(&stmt.sql)?;
    let params = to_rusqlite_params(&stmt.params);
    let n = prepared.execute(rusqlite::params_from_iter(params.iter()))?;
    Ok(n)
}

/// Surface UNIQUE violations as a distinct error so duplicate-batch handling
/// can treat them as a signal instead of a hard failure.
fn map_constraint(err: rusqlite::Error) -> AtwError {
    if let rusqlite::Error::SqliteFailure(kind, Some(msg)) = &err {
        if matches!(kind.code, rusqlite::ffi::ErrorCode::ConstraintViolation) {
            return AtwError::UniqueConstraint(msg.clone());
        }
    }
    AtwError::from(err)
}

fn to_rusqlite_params(values: &[CloudValue]) -> Vec<rusqlite::types::Value> {
    values
        .iter()
        .map(|v| match v {
            CloudValue::Null => rusqlite::types::Value::Null,
            CloudValue::Text(s) => rusqlite::types::Value::Text(s.clone()),
            CloudValue::Int(i) => rusqlite::types::Value::Integer(*i),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: i64 = 3600;
    const DAY: i64 = 86_400;

    fn backend() -> LocalBackend {
        let mut b = LocalBackend::open_in_memory().unwrap();
        b.init_schema().unwrap();
        b
    }

    fn seg(id: &str, app: &str, start: i64, end: i64, date: &str) -> SegmentInput {
        SegmentInput {
            segment_id: id.to_string(),
            app_name: app.to_string(),
            app_class: app.to_lowercase(),
            category: "editor".to_string(),
            start_utc: start,
            end_utc: end,
            duration_seconds: end - start,
            activity_date: date.to_string(),
        }
    }

    fn daily_unique(b: &mut LocalBackend, date: &str) -> (i64, i64) {
        let rows = b
            .query(
                "SELECT unique_active_seconds, overlap_seconds FROM daily_unique WHERE activity_date = ?",
                vec![CloudValue::text(date)],
            )
            .unwrap();
        match rows.first().map(|r| (r[0].clone(), r[1].clone())) {
            Some((CloudValue::Int(u), CloudValue::Int(o))) => (u, o),
            _ => (0, 0),
        }
    }

    fn daily_app(b: &mut LocalBackend, date: &str, app: &str) -> (i64, i64) {
        let rows = b
            .query(
                "SELECT combined_seconds, merged_seconds FROM daily_app WHERE activity_date = ? AND app_name = ?",
                vec![CloudValue::text(date), CloudValue::text(app)],
            )
            .unwrap();
        match rows.first().map(|r| (r[0].clone(), r[1].clone())) {
            Some((CloudValue::Int(c), CloudValue::Int(m))) => (c, m),
            _ => (0, 0),
        }
    }

    fn count(b: &mut LocalBackend, table: &str) -> i64 {
        let rows = b
            .query(&format!("SELECT COUNT(*) FROM {table}"), vec![])
            .unwrap();
        match rows.first().and_then(|r| r.first()) {
            Some(CloudValue::Int(n)) => *n,
            _ => 0,
        }
    }

    #[test]
    fn single_device_upload_produces_correct_aggregates() {
        let mut b = backend();
        let base = 5 * DAY; // 1970-01-06
        let segs = vec![
            seg("s1", "VS Code", base, base + 4 * H, "1970-01-06"),
            seg("s2", "Foot", base + 5 * H, base + 6 * H, "1970-01-06"),
        ];
        let out = upload_batch(&mut b, "dev1", "b1", &segs, 100).unwrap();
        assert_eq!(
            out,
            UploadOutcome::Applied {
                segments: 2,
                dates: vec!["1970-01-06".to_string()]
            }
        );
        // VS Code: combined == merged == 4 h (single device, no overlap).
        assert_eq!(daily_app(&mut b, "1970-01-06", "VS Code"), (4 * H, 4 * H));
        // Unique = 5 h (4 h VS Code + 1 h Foot, disjoint).
        assert_eq!(daily_unique(&mut b, "1970-01-06"), (5 * H, 0));
    }

    #[test]
    fn duplicate_batch_is_acked_without_changes() {
        let mut b = backend();
        let base = 5 * DAY;
        let segs = vec![seg("s1", "VS Code", base, base + H, "1970-01-06")];
        upload_batch(&mut b, "dev1", "b1", &segs, 100).unwrap();
        // Same batch id again (network retry after lost ack): acked as dup.
        assert_eq!(
            upload_batch(&mut b, "dev1", "b1", &segs, 200).unwrap(),
            UploadOutcome::Duplicate
        );
        assert_eq!(count(&mut b, "segments"), 1, "no double insert");
        assert_eq!(
            daily_app(&mut b, "1970-01-06", "VS Code").0,
            H,
            "aggregate unchanged"
        );
        assert_eq!(count(&mut b, "batches"), 1);
    }

    #[test]
    fn duplicate_segment_in_new_batch_never_double_counts() {
        let mut b = backend();
        let base = 5 * DAY;
        let segs = vec![seg("s1", "VS Code", base, base + H, "1970-01-06")];
        upload_batch(&mut b, "dev1", "b1", &segs, 100).unwrap();
        // Device crashed after commit, before local ack → new batch id, same
        // segment id. INSERT OR IGNORE keeps it a no-op; recompute is stable.
        let out = upload_batch(&mut b, "dev1", "b2", &segs, 200).unwrap();
        assert!(matches!(out, UploadOutcome::Applied { .. }));
        assert_eq!(count(&mut b, "segments"), 1);
        assert_eq!(daily_app(&mut b, "1970-01-06", "VS Code"), (H, H));
    }

    #[test]
    fn two_devices_overlapping_merge_to_union() {
        let mut b = backend();
        let base = 5 * DAY;
        // Desktop VS Code 9–17, laptop VS Code 13–21 (overlapping).
        let desktop = vec![seg(
            "d1",
            "VS Code",
            base + 9 * H,
            base + 17 * H,
            "1970-01-06",
        )];
        let laptop = vec![seg(
            "l1",
            "VS Code",
            base + 13 * H,
            base + 21 * H,
            "1970-01-06",
        )];
        upload_batch(&mut b, "desktop", "bd", &desktop, 100).unwrap();
        upload_batch(&mut b, "laptop", "bl", &laptop, 101).unwrap();
        // Combined Σ = 16 h (exceeds nothing here, but is Σ), merged = 12 h.
        assert_eq!(daily_app(&mut b, "1970-01-06", "VS Code"), (16 * H, 12 * H));
        assert_eq!(daily_unique(&mut b, "1970-01-06"), (12 * H, 0));
    }

    #[test]
    fn different_apps_overlapping_attribution() {
        let mut b = backend();
        let base = 5 * DAY;
        // Desktop VS Code 9–17, laptop terminal 13–15: 2 h cross-app overlap.
        let d = vec![seg(
            "d1",
            "VS Code",
            base + 9 * H,
            base + 17 * H,
            "1970-01-06",
        )];
        let l = vec![seg(
            "l1",
            "Terminal",
            base + 13 * H,
            base + 15 * H,
            "1970-01-06",
        )];
        upload_batch(&mut b, "desktop", "bd", &d, 100).unwrap();
        upload_batch(&mut b, "laptop", "bl", &l, 101).unwrap();
        // Unique = 8 h (never 10 h); the 2 h is attributed to overlap.
        assert_eq!(daily_unique(&mut b, "1970-01-06"), (8 * H, 2 * H));
    }

    #[test]
    fn late_arriving_batch_recomputes_correctly() {
        let mut b = backend();
        let base = 5 * DAY;
        // Laptop syncs today's data…
        let l = vec![seg("l1", "VS Code", base + H, base + 2 * H, "1970-01-06")];
        upload_batch(&mut b, "laptop", "bl", &l, 100).unwrap();
        // …then the offline desktop arrives a day later with overlapping time.
        let d = vec![seg(
            "d1",
            "VS Code",
            base + 90 * 60,
            base + 3 * H,
            "1970-01-06",
        )];
        upload_batch(&mut b, "desktop", "bd", &d, 200).unwrap();
        // Merged: 1:00–3:00 = 2 h; combined = 1 h + 1.5 h = 2.5 h.
        assert_eq!(
            daily_app(&mut b, "1970-01-06", "VS Code"),
            (3 * H / 2 + H, 2 * H)
        );
        assert_eq!(daily_unique(&mut b, "1970-01-06"), (2 * H, 0));
    }

    #[test]
    fn midnight_boundary_dates_aggregate_separately() {
        let mut b = backend();
        let base = 5 * DAY;
        // One session crossing midnight was split locally into two segments.
        let segs = vec![
            seg("s1", "VS Code", base + 23 * H, base + DAY, "1970-01-06"),
            seg("s2", "VS Code", base + DAY, base + DAY + H, "1970-01-07"),
        ];
        upload_batch(&mut b, "dev1", "b1", &segs, 100).unwrap();
        assert_eq!(daily_app(&mut b, "1970-01-06", "VS Code").0, H);
        assert_eq!(daily_app(&mut b, "1970-01-07", "VS Code").0, H);
        assert_eq!(daily_unique(&mut b, "1970-01-06").0, H);
        assert_eq!(daily_unique(&mut b, "1970-01-07").0, H);
    }

    #[test]
    fn failed_transaction_rolls_back_completely() {
        let mut b = backend();
        let base = 5 * DAY;
        let segs = vec![seg("s1", "VS Code", base, base + H, "1970-01-06")];
        // A statement with deliberately broken SQL fails mid-transaction;
        // nothing — segment, batch, aggregates — may survive.
        let stmts: Vec<Stmt> = vec![
            Stmt::new(
                "INSERT INTO batches (batch_id, device_id, segment_count, processed_at) VALUES (?, ?, ?, ?)",
                vec![CloudValue::text("bx"), CloudValue::text("dev"), CloudValue::int(1), CloudValue::int(1)],
            ),
            Stmt::new(
                "INSERT INTO segments (segment_id) VALUES (?)",
                vec![CloudValue::text("s1")], // missing NOT NULL columns → fails
            ),
            Stmt::new(
                "DELETE FROM daily_app WHERE activity_date = '1970-01-06'",
                vec![],
            ),
        ];
        assert!(b.transaction(stmts).is_err());
        assert_eq!(count(&mut b, "batches"), 0, "batch insert rolled back");
        assert_eq!(count(&mut b, "segments"), 0, "segment insert rolled back");
        // The real upload afterwards works cleanly.
        upload_batch(&mut b, "dev1", "b1", &segs, 100).unwrap();
        assert_eq!(count(&mut b, "segments"), 1);
    }

    #[test]
    fn five_devices_concurrent_sync_stay_consistent() {
        let dir = std::env::temp_dir().join(format!("atw-cloud-conc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("cloud.db");
        {
            let mut b = LocalBackend::open(&db_path).unwrap();
            b.init_schema().unwrap();
        }
        let base = 5 * DAY;
        // 5 devices × 1 h each, all overlapping 12:00–13:00 → unique must be
        // exactly 1 h no matter how the writes interleave.
        let handles: Vec<_> = (0..5)
            .map(|i| {
                let path = db_path.clone();
                std::thread::spawn(move || {
                    let mut b = LocalBackend::open(&path).unwrap();
                    let segs = vec![seg(
                        &format!("s{i}"),
                        "VS Code",
                        base + 12 * H,
                        base + 13 * H,
                        "1970-01-06",
                    )];
                    upload_batch(&mut b, &format!("dev{i}"), &format!("b{i}"), &segs, 100 + i)
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap().unwrap();
        }
        let mut b = LocalBackend::open(&db_path).unwrap();
        assert_eq!(count(&mut b, "segments"), 5, "all five segments stored");
        assert_eq!(count(&mut b, "batches"), 5);
        assert_eq!(count(&mut b, "devices"), 5);
        // combined = 5 h, merged = 1 h, unique = 1 h, overlap = 0.
        assert_eq!(daily_app(&mut b, "1970-01-06", "VS Code"), (5 * H, H));
        assert_eq!(daily_unique(&mut b, "1970-01-06"), (H, 0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cloud_retention_deletes_segments_keeps_aggregates() {
        let mut b = backend();
        let old = 5 * DAY; // 1970-01-06
        let recent = 95 * DAY; // 1970-04-06
        let segs = vec![
            seg("s1", "VS Code", old, old + H, "1970-01-06"),
            seg("s2", "VS Code", recent, recent + H, "1970-04-06"),
        ];
        upload_batch(&mut b, "dev1", "b1", &segs, 100).unwrap();
        let n = cloud_retention(&mut b, "1970-04-01", 200).unwrap();
        assert_eq!(n, 1, "only the January segment is purged");
        assert_eq!(count(&mut b, "segments"), 1);
        // Aggregates survive — the portfolio keeps its history.
        assert_eq!(daily_app(&mut b, "1970-01-06", "VS Code").0, H);
        assert_eq!(daily_unique(&mut b, "1970-01-06").0, H);
    }

    #[test]
    fn cloud_history_survives_local_deletion() {
        // The local DB can delete everything (7-day retention) without
        // touching the cloud copy: independent retention domains.
        let mut b = backend();
        let base = 5 * DAY;
        let segs = vec![seg("s1", "VS Code", base, base + H, "1970-01-06")];
        upload_batch(&mut b, "dev1", "b1", &segs, 100).unwrap();
        // Simulate the local side deleting all its rows: cloud untouched.
        assert_eq!(count(&mut b, "segments"), 1);
        assert_eq!(portfolio_unique(&mut b, "1970-01-01").unwrap().len(), 1);
        assert_eq!(portfolio_day(&mut b, "1970-01-06").unwrap().len(), 1);
    }

    #[test]
    fn portfolio_reads_precomputed_stats() {
        let mut b = backend();
        let base = 5 * DAY;
        let segs = vec![
            seg("s1", "VS Code", base, base + 2 * H, "1970-01-06"),
            seg("s2", "Foot", base + 3 * H, base + 4 * H, "1970-01-06"),
        ];
        upload_batch(&mut b, "dev1", "b1", &segs, 100).unwrap();
        let day = portfolio_day(&mut b, "1970-01-06").unwrap();
        assert_eq!(
            day,
            vec![
                ("VS Code".to_string(), 2 * H, 2 * H),
                ("Foot".to_string(), H, H),
            ]
        );
        let uniq = portfolio_unique(&mut b, "1970-01-01").unwrap();
        assert_eq!(uniq, vec![("1970-01-06".to_string(), 3 * H, 0)]);
    }

    #[test]
    fn timezone_difference_attributed_by_device_local_date() {
        // Two devices in different timezones active at the same UTC instant:
        // each attributes the segment to its own local date, so a UTC
        // evening lands on different calendar days — both counted, no loss.
        let mut b = backend();
        let t = 5 * DAY + 20 * H; // 20:00 UTC on 1970-01-06
                                  // Device A (UTC+5:30): local date 1970-01-07 01:30.
        let a = vec![seg("s1", "VS Code", t, t + H, "1970-01-07")];
        // Device B (UTC-5): local date 1970-01-06 15:00.
        let bb = vec![seg("s2", "VS Code", t, t + H, "1970-01-06")];
        upload_batch(&mut b, "a", "ba", &a, 100).unwrap();
        upload_batch(&mut b, "b", "bb", &bb, 101).unwrap();
        assert_eq!(daily_app(&mut b, "1970-01-07", "VS Code").0, H);
        assert_eq!(daily_app(&mut b, "1970-01-06", "VS Code").0, H);
        // Each date's unique time is its own union — the same physical hour
        // appears under two reporting dates, which is the documented
        // device-local-date semantics.
        assert_eq!(daily_unique(&mut b, "1970-01-07").0, H);
        assert_eq!(daily_unique(&mut b, "1970-01-06").0, H);
    }

    #[test]
    fn clock_jump_back_overlap_never_double_counts() {
        // A system clock stepped backwards produces overlapping segments on
        // the same device; the union still counts the wall time once.
        let mut b = backend();
        let base = 5 * DAY;
        let segs = vec![
            seg("s1", "VS Code", base + 10 * H, base + 12 * H, "1970-01-06"),
            // Clock jumped back 30 min mid-session: overlapping rewrite.
            seg(
                "s2",
                "VS Code",
                base + 11 * H + 1800,
                base + 12 * H + H,
                "1970-01-06",
            ),
        ];
        upload_batch(&mut b, "dev1", "b1", &segs, 100).unwrap();
        // Union of 10:00–12:00 and 11:30–13:00 = 10:00–13:00 = 3 h;
        // combined Σ = 2 h + 1.5 h = 3.5 h.
        assert_eq!(
            daily_app(&mut b, "1970-01-06", "VS Code"),
            (3 * H + H / 2, 3 * H)
        );
        assert_eq!(daily_unique(&mut b, "1970-01-06").0, 3 * H);
    }

    #[test]
    fn aggregate_version_mismatch_triggers_full_recompute() {
        let mut b = backend();
        let base = 5 * DAY;
        // Seed as if an older aggregate version had written the meta row.
        let segs = vec![seg("s1", "VS Code", base, base + H, "1970-01-06")];
        upload_batch(&mut b, "dev1", "b1", &segs, 100).unwrap();
        b.transaction(vec![Stmt::new(
            "UPDATE meta SET value = '0' WHERE key = 'aggregate_version'",
            vec![],
        )])
        .unwrap();
        // Corrupt the aggregate, then upload a new batch: the version
        // mismatch forces a full recompute which repairs it.
        b.transaction(vec![Stmt::new(
            "UPDATE daily_app SET merged_seconds = 999999",
            vec![],
        )])
        .unwrap();
        let segs2 = vec![seg("s2", "Foot", base + 2 * H, base + 3 * H, "1970-01-06")];
        upload_batch(&mut b, "dev1", "b2", &segs2, 200).unwrap();
        assert_eq!(
            daily_app(&mut b, "1970-01-06", "VS Code"),
            (H, H),
            "repaired"
        );
        assert_eq!(daily_app(&mut b, "1970-01-06", "Foot"), (H, H));
    }
}
