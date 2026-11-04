//! The tracking state machine.
//!
//! All state lives in one struct owned by the daemon loop. Every method takes
//! `now` (unix seconds) so duration math is fully testable with synthetic
//! clocks. Segments accumulate in memory and are written to SQLite only by
//! the daemon's flush step — never per event.
//!
//! Semantics:
//! - Focus change closes the previous segment at the change time.
//! - An Idle notification arrives `idle_timeout` seconds *after* the user
//!   actually stopped, so the segment is cut at `now - idle_timeout` — the
//!   AFK time is never counted as usage.
//! - Resuming after idle/lock re-opens the last known application; if the
//!   compositor reports a focus change instead, that opens the new app.

use crate::classify::{Category, Classifier};

#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// Active window changed. An empty class means "no window focused".
    Focus { class: String, title: String },
    /// User became AFK (the notification fires idle_timeout seconds late).
    Idle,
    /// User is active again.
    Active,
    /// Screen was locked.
    Lock,
    /// Screen was unlocked.
    Unlock,
    /// The machine was suspended (detected via wall/monotonic clock drift).
    /// `now` carries the estimated moment the machine went to sleep.
    Suspend,
    /// The machine resumed from suspend.
    Resume,
    /// Daily retention tick (handled by the daemon, ignored by the machine).
    Retention,
    /// Periodic flush tick (handled by the daemon, ignored by the machine).
    Flush,
    /// Cloud sync tick or manual trigger (handled by the daemon).
    Sync,
    /// A background cloud sync finished (handled by the daemon).
    SyncDone {
        ok: bool,
        error: Option<String>,
        pending: usize,
        last_sync: Option<String>,
    },
    /// Graceful shutdown (handled by the daemon, ignored by the machine).
    Shutdown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub class: String,
    pub display_name: String,
    pub category: Category,
    pub started_at: i64,
    pub ended_at: i64,
}

impl Segment {
    pub fn duration(&self) -> i64 {
        (self.ended_at - self.started_at).max(0)
    }
}

/// Maps a unix timestamp to its local calendar date ("YYYY-MM-DD").
pub type DateFn = Box<dyn Fn(i64) -> String + Send>;

/// Split a segment into pieces that each belong to a single local date.
/// Works with any date function (binary search for midnight boundaries), so
/// it is testable without depending on the host timezone.
pub fn split_by_date(segment: &Segment, date_of: &dyn Fn(i64) -> String) -> Vec<Segment> {
    let mut pieces = Vec::new();
    let mut start = segment.started_at;
    let end = segment.ended_at;
    while start < end {
        let date = date_of(start);
        let boundary = find_date_boundary(start, end, date_of, &date);
        let mut piece = segment.clone();
        piece.started_at = start;
        piece.ended_at = boundary;
        pieces.push(piece);
        start = boundary;
    }
    pieces
}

/// First timestamp in (start, end] whose date differs from `date`
/// (or `end` when the whole remainder shares the date).
fn find_date_boundary(start: i64, end: i64, date_of: &dyn Fn(i64) -> String, date: &str) -> i64 {
    if date_of(end) != date {
        // Exponential probe to bracket the boundary, then binary search.
        let mut lo = start;
        let mut step: i64 = 1;
        while lo + step <= end && date_of(lo + step) == date {
            lo += step;
            step *= 2;
        }
        let mut hi = (lo + step).min(end);
        while lo + 1 < hi {
            let mid = lo + (hi - lo) / 2;
            if date_of(mid) == date {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        return hi;
    }
    end
}

/// A segment that was already flushed and later got its final end time:
/// (row_id, full segment, seconds not yet reflected in the database row).
/// The database row is rewritten (delete + re-insert split pieces), the
/// delta is only used for accurate live status reporting.
pub type Finalize = (i64, Segment, i64);

pub struct Tracker {
    classifier: Classifier,
    idle_timeout: i64,
    date_of: DateFn,
    /// Segment currently being accumulated, if any.
    current: Option<Segment>,
    /// Database state of the open segment: (row id, last flush time).
    /// `None` until the first flush of this segment.
    open_row: Option<(i64, i64)>,
    /// Last focused application: (normalized class, raw class, title) —
    /// used to resume after idle and for project-aware display names.
    last_app: Option<(String, String, String)>,
    idle: bool,
    locked: bool,
    /// Closed segments never written to the database yet.
    pending: Vec<Segment>,
    /// Already-flushed segments that need a final UPDATE when they end.
    finalize: Vec<Finalize>,
}

impl Tracker {
    pub fn new(classifier: Classifier, idle_timeout_seconds: i64, date_of: DateFn) -> Self {
        Tracker {
            classifier,
            idle_timeout: idle_timeout_seconds.max(1),
            date_of,
            current: None,
            open_row: None,
            last_app: None,
            idle: false,
            locked: false,
            pending: Vec::new(),
            finalize: Vec::new(),
        }
    }

    pub fn handle(&mut self, input: Input, now: i64) {
        tracing::debug!(
            target: "atw",
            input = ?input,
            now,
            current = ?self.current.as_ref().map(|s| (&s.class, s.started_at)),
            open_row = ?self.open_row,
            pending = self.pending.len(),
            finalize = self.finalize.len(),
            "tracker input"
        );
        match input {
            Input::Focus { class, title } => self.on_focus(class, title, now),
            Input::Idle => self.on_idle(now),
            Input::Active => self.on_active(now),
            Input::Lock => self.on_lock(now),
            Input::Unlock => self.on_unlock(now),
            // Machine slept: end the open segment at the last observed
            // moment the machine was awake (passed as `now`).
            Input::Suspend => self.close_current(now),
            // Machine woke up: if the user was active before sleeping,
            // re-open the last app right away. If they were idle or locked
            // before, wait for a real event (`resumed` / focus change) so
            // post-resume AFK time is not attributed to any app.
            Input::Resume if !self.idle && !self.locked => self.maybe_reopen(now),
            Input::Resume => {}
            Input::Flush | Input::Retention | Input::Sync | Input::SyncDone { .. } => {}
            // Shutdown ends the open segment so the final flush is complete.
            Input::Shutdown => self.close_current(now),
        }
    }

    fn on_focus(&mut self, class: String, title: String, now: i64) {
        let norm = crate::classify::normalize(&class);
        if norm.is_empty() {
            // Desktop focused: usage pauses, but we remember nothing new.
            self.close_current(now);
            self.last_app = None;
            return;
        }
        let info = self.classifier.classify_window(&class, &title);
        self.last_app = Some((norm.clone(), class, title));
        if let Some(current) = &self.current {
            // Same app continues (e.g. only the title changed) — unless the
            // display name changed (project switch within the same editor).
            if current.class == norm && current.display_name == info.display_name {
                return;
            }
        }
        self.close_current(now);
        if info.tracked {
            self.current = Some(Segment {
                class: info.class,
                display_name: info.display_name,
                category: info.category,
                started_at: now,
                ended_at: now,
            });
        }
    }

    fn on_idle(&mut self, now: i64) {
        if self.idle {
            return;
        }
        self.idle = true;
        // The notification is raised idle_timeout seconds after the user
        // actually stopped, so the segment really ended back then.
        let idle_began = (now - self.idle_timeout).max(0);
        if let Some(current) = &self.current {
            let mut closed = current.clone();
            closed.ended_at = closed.started_at.max(idle_began);
            self.push_closed(closed);
        }
        self.current = None;
        self.open_row = None;
    }

    fn on_active(&mut self, now: i64) {
        self.idle = false;
        self.maybe_reopen(now);
    }

    fn on_lock(&mut self, now: i64) {
        if self.locked {
            return;
        }
        self.locked = true;
        self.idle = true;
        self.close_current(now);
    }

    fn on_unlock(&mut self, now: i64) {
        self.locked = false;
        self.idle = false;
        self.maybe_reopen(now);
    }

    fn maybe_reopen(&mut self, now: i64) {
        if self.locked || self.current.is_some() {
            return;
        }
        if let Some((_norm, raw, title)) = &self.last_app {
            let info = self.classifier.classify_window(raw, title);
            if info.tracked {
                self.current = Some(Segment {
                    class: info.class,
                    display_name: info.display_name,
                    category: info.category,
                    started_at: now,
                    ended_at: now,
                });
            }
        }
    }

    /// Close the open segment at `now`, routing it to the right write path.
    fn close_current(&mut self, now: i64) {
        if let Some(mut segment) = self.current.take() {
            segment.ended_at = segment.started_at.max(now);
            self.push_closed(segment);
        }
        self.open_row = None;
    }

    fn push_closed(&mut self, segment: Segment) {
        if let Some((row, flushed_at)) = self.open_row.take() {
            // Already flushed partially: the DB row gets rewritten with the
            // final segment; status counts only the not-yet-flushed tail.
            let unflushed = (segment.ended_at - flushed_at).max(0);
            self.finalize.push((row, segment, unflushed));
        } else if segment.duration() > 0 {
            self.pending.push(segment);
        }
    }

    // -- Accessors used by the daemon -------------------------------------

    pub fn open_segment(&self) -> Option<&Segment> {
        self.current.as_ref()
    }

    pub fn open_row(&self) -> Option<(i64, i64)> {
        self.open_row
    }

    pub fn set_open_row(&mut self, row: Option<(i64, i64)>) {
        self.open_row = row;
    }

    pub fn take_pending(&mut self) -> Vec<Segment> {
        std::mem::take(&mut self.pending)
    }

    pub fn take_finalize(&mut self) -> Vec<Finalize> {
        std::mem::take(&mut self.finalize)
    }

    /// Display name of the app the user last had focused (even while idle),
    /// or None when nothing is tracked.
    pub fn active_display(&self) -> Option<String> {
        if let Some(current) = &self.current {
            return Some(current.display_name.clone());
        }
        if let Some((_norm, raw, title)) = &self.last_app {
            let info = self.classifier.classify_window(raw, title);
            if info.tracked {
                return Some(info.display_name);
            }
        }
        None
    }

    pub fn state_str(&self) -> &'static str {
        if self.locked {
            "locked"
        } else if self.idle {
            "idle"
        } else {
            "active"
        }
    }

    /// (display_name, seconds) contributions for "today" that are NOT yet in
    /// the database: pending segments (today's pieces only) plus the elapsed
    /// time of the open segment.
    pub fn today_pieces(&self, now: i64) -> Vec<(String, u64)> {
        let today = (self.date_of)(now);
        let mut out = Vec::new();

        // Closed segments never written to the database: full duration.
        for segment in &self.pending {
            for piece in split_by_date(segment, &*self.date_of) {
                if piece.duration() > 0 && (self.date_of)(piece.started_at) == today {
                    out.push((piece.display_name.clone(), piece.duration() as u64));
                }
            }
        }

        // Closed segments whose DB row is stale: only the unflushed tail.
        for (_, segment, unflushed) in &self.finalize {
            if *unflushed > 0 && (self.date_of)(segment.ended_at) == today {
                out.push((segment.display_name.clone(), *unflushed as u64));
            }
        }

        // Open segment: only the time after the last flush anchor, so time
        // already stored in the DB is never counted twice.
        if let Some(current) = &self.current {
            let anchor = self
                .open_row
                .as_ref()
                .map(|(_, flushed_at)| *flushed_at)
                .unwrap_or(current.started_at)
                .max(current.started_at);
            let mut live = current.clone();
            live.started_at = anchor;
            live.ended_at = live.started_at.max(now);
            for piece in split_by_date(&live, &*self.date_of) {
                if piece.duration() > 0 && (self.date_of)(piece.started_at) == today {
                    out.push((piece.display_name.clone(), piece.duration() as u64));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Tracking;

    const DAY: i64 = 86_400;

    /// Deterministic UTC date function for tests.
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

    fn focus(class: &str) -> Input {
        Input::Focus {
            class: class.to_string(),
            title: String::new(),
        }
    }

    fn tracker() -> Tracker {
        tracker_with(&[], &[])
    }

    fn tracker_with(display: &[(&str, &str)], excluded: &[&str]) -> Tracker {
        let tracking = Tracking {
            display_names: display
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            excluded_apps: excluded.iter().map(|s| s.to_string()).collect(),
            ..Tracking::default()
        };
        Tracker::new(Classifier::from_config(&tracking), 300, Box::new(utc_date))
    }

    #[test]
    fn app_switch_closes_previous_segment() {
        let mut t = tracker();
        t.handle(focus("firefox"), day5(9, 0, 0));
        t.handle(focus("code"), day5(9, 30, 0));
        assert_eq!(t.pending.len(), 1);
        let seg = &t.pending[0];
        assert_eq!(seg.class, "firefox");
        assert_eq!(seg.duration(), 1800);
        let current = t.current.as_ref().unwrap();
        assert_eq!(current.class, "code");
        assert_eq!(current.started_at, day5(9, 30, 0));
    }

    #[test]
    fn same_app_does_not_recut() {
        let mut t = tracker();
        t.handle(focus("code"), day5(9, 0, 0));
        t.handle(focus("code"), day5(9, 10, 0));
        assert!(t.pending.is_empty());
        assert_eq!(t.current.as_ref().unwrap().started_at, day5(9, 0, 0));
    }

    #[test]
    fn idle_cuts_at_timeout_not_at_notification_time() {
        let mut t = tracker();
        t.handle(focus("code"), day5(9, 0, 0));
        // Notification arrives 20 min after focus; user stopped after 15 min.
        t.handle(Input::Idle, day5(9, 20, 0));
        let seg = &t.pending[0];
        assert_eq!(seg.duration(), 900); // 9:00 -> 9:15, not 9:20
        assert!(t.current.is_none());
        assert_eq!(t.state_str(), "idle");
    }

    #[test]
    fn resume_reopens_last_app() {
        let mut t = tracker();
        t.handle(focus("code"), day5(9, 0, 0));
        t.handle(Input::Idle, day5(9, 20, 0));
        t.handle(Input::Active, day5(9, 40, 0));
        let current = t.current.as_ref().unwrap();
        assert_eq!(current.class, "code");
        assert_eq!(current.started_at, day5(9, 40, 0));
        assert_eq!(t.state_str(), "active");
        // Leaving the app later produces a fresh, non-overlapping segment.
        t.handle(focus("kitty"), day5(9, 50, 0));
        // pending[0] = code 9:00→9:15 (idle cut), pending[1] = code 9:40→9:50.
        assert_eq!(t.pending.len(), 2);
        assert_eq!(t.pending[0].duration(), 900);
        assert_eq!(t.pending[1].class, "code");
        assert_eq!(t.pending[1].duration(), 600);
    }

    #[test]
    fn lock_pauses_and_unlock_resumes() {
        let mut t = tracker();
        t.handle(focus("firefox"), day5(10, 0, 0));
        t.handle(Input::Lock, day5(10, 10, 0));
        assert_eq!(t.pending[0].duration(), 600);
        assert_eq!(t.state_str(), "locked");
        // Idle while locked is ignored.
        t.handle(Input::Idle, day5(10, 12, 0));
        assert!(t.current.is_none());
        t.handle(Input::Unlock, day5(10, 20, 0));
        assert_eq!(t.current.as_ref().unwrap().class, "firefox");
        assert_eq!(t.current.as_ref().unwrap().started_at, day5(10, 20, 0));
        assert_eq!(t.state_str(), "active");
    }

    #[test]
    fn desktop_focus_pauses_usage() {
        let mut t = tracker();
        t.handle(focus("code"), day5(9, 0, 0));
        t.handle(focus(""), day5(9, 5, 0));
        assert_eq!(t.pending[0].duration(), 300);
        assert!(t.current.is_none());
        // Becoming "active" over the bare desktop attributes time to no app:
        // nothing is re-opened. Clicking back into a window always produces
        // a new `activewindow` event, which re-tracks it.
        t.handle(Input::Active, day5(9, 6, 0));
        assert!(t.current.is_none());
    }

    #[test]
    fn excluded_apps_are_not_tracked() {
        // "code" is on the developer allowlist, so this proves the explicit
        // exclusion wins over the allowlist.
        let mut t = tracker_with(&[], &["code"]);
        t.handle(focus("Code"), day5(9, 0, 0));
        assert!(t.current.is_none());
        t.handle(focus("kitty"), day5(9, 5, 0));
        assert!(t.pending.is_empty());
        assert_eq!(t.current.as_ref().unwrap().class, "kitty");
    }

    #[test]
    fn excluded_apps_are_not_reopened_after_idle() {
        let mut t = tracker_with(&[], &["code"]);
        t.handle(focus("code"), day5(9, 0, 0));
        t.handle(Input::Idle, day5(9, 10, 0));
        t.handle(Input::Active, day5(9, 20, 0));
        assert!(t.current.is_none());
    }

    #[test]
    fn non_dev_apps_are_not_tracked() {
        let mut t = tracker();
        t.handle(focus("spotify"), day5(9, 0, 0));
        assert!(t.current.is_none());
        t.handle(focus("SomeWeirdApp"), day5(9, 5, 0));
        assert!(t.current.is_none());
        assert!(t.pending.is_empty());
        // Developer apps still track normally around them.
        t.handle(focus("kitty"), day5(9, 10, 0));
        assert_eq!(t.current.as_ref().unwrap().class, "kitty");
    }

    #[test]
    fn non_dev_apps_are_not_reopened_after_idle() {
        let mut t = tracker();
        t.handle(focus("spotify"), day5(9, 0, 0));
        t.handle(Input::Idle, day5(9, 10, 0));
        t.handle(Input::Active, day5(9, 20, 0));
        assert!(t.current.is_none());
    }

    #[test]
    fn tracked_apps_override_the_category_gate() {
        let tracking = Tracking {
            tracked_apps: vec!["spotify".to_string()],
            ..Tracking::default()
        };
        let mut t = Tracker::new(Classifier::from_config(&tracking), 300, Box::new(utc_date));
        t.handle(focus("Spotify"), day5(9, 0, 0));
        assert_eq!(t.current.as_ref().unwrap().class, "spotify");
    }

    #[test]
    fn switching_to_a_non_dev_app_closes_the_dev_segment() {
        let mut t = tracker();
        t.handle(focus("code"), day5(9, 0, 0));
        t.handle(focus("spotify"), day5(9, 10, 0));
        assert_eq!(t.pending.len(), 1);
        assert_eq!(t.pending[0].class, "code");
        assert_eq!(t.pending[0].duration(), 600);
        assert!(t.current.is_none());
    }

    #[test]
    fn idle_twice_is_idempotent() {
        let mut t = tracker();
        t.handle(focus("code"), day5(9, 0, 0));
        t.handle(Input::Idle, day5(9, 20, 0));
        let count = t.pending.len();
        t.handle(Input::Idle, day5(9, 21, 0));
        assert_eq!(t.pending.len(), count);
    }

    #[test]
    fn today_pieces_reports_pending_and_live() {
        let mut t = tracker();
        t.handle(focus("code"), day5(9, 0, 0));
        t.handle(focus("firefox"), day5(9, 10, 0)); // closes 600s of code
        let pieces = t.today_pieces(day5(9, 15, 0));
        let total: u64 = pieces.iter().map(|(_, s)| *s).sum();
        // 600s closed + 300s live firefox = 900s
        assert_eq!(total, 900);
    }

    #[test]
    fn today_pieces_counts_only_unflushed_time() {
        let mut t = tracker();
        t.handle(focus("code"), day5(9, 0, 0));
        // Simulate a flush at 9:05: the DB row now holds 9:00→9:05.
        t.set_open_row(Some((1, day5(9, 5, 0))));
        let pieces = t.today_pieces(day5(9, 10, 0));
        let total: u64 = pieces.iter().map(|(_, s)| *s).sum();
        assert_eq!(total, 300); // only 9:05→9:10, not from 9:00 again

        // Closing at 9:20 finalizes: DB row rewritten to the full 20 min,
        // status counts only the tail 9:05→9:20.
        let expected = Segment {
            class: "code".to_string(),
            display_name: "Visual Studio Code".to_string(),
            category: Category::Editor,
            started_at: day5(9, 0, 0),
            ended_at: day5(9, 20, 0),
        };
        t.handle(focus("kitty"), day5(9, 20, 0));
        assert!(t.pending.is_empty());
        assert_eq!(t.take_finalize(), vec![(1, expected, 900)]);
        assert!(t.open_row().is_none());
    }

    #[test]
    fn shutdown_closes_the_open_segment() {
        let mut t = tracker();
        t.handle(focus("code"), day5(9, 0, 0));
        t.handle(Input::Shutdown, day5(9, 7, 0));
        assert!(t.current.is_none());
        assert_eq!(t.pending.len(), 1);
        assert_eq!(t.pending[0].duration(), 420);
    }

    #[test]
    fn editor_project_names_split_same_class_segments() {
        let tracking = Tracking {
            editor_project_names: true,
            ..Tracking::default()
        };
        let mut t = Tracker::new(Classifier::from_config(&tracking), 300, Box::new(utc_date));
        let focus_title = |class: &str, title: &str| Input::Focus {
            class: class.to_string(),
            title: title.to_string(),
        };
        t.handle(
            focus_title("code", "main.rs - atw - Visual Studio Code"),
            day5(9, 0, 0),
        );
        assert_eq!(
            t.current.as_ref().unwrap().display_name,
            "Visual Studio Code · atw"
        );
        // Switching projects within the same editor cuts the segment.
        t.handle(
            focus_title("code", "notes.md - diary - Visual Studio Code"),
            day5(9, 5, 0),
        );
        assert_eq!(t.pending[0].display_name, "Visual Studio Code · atw");
        assert_eq!(t.pending[0].duration(), 300);
        assert_eq!(
            t.current.as_ref().unwrap().display_name,
            "Visual Studio Code · diary"
        );
    }

    #[test]
    fn editor_project_names_disabled_by_default() {
        let mut t = tracker();
        t.handle(
            Input::Focus {
                class: "code".to_string(),
                title: "main.rs - atw - Visual Studio Code".to_string(),
            },
            day5(9, 0, 0),
        );
        assert_eq!(
            t.current.as_ref().unwrap().display_name,
            "Visual Studio Code"
        );
    }

    #[test]
    fn suspend_cuts_segment_and_resume_reopens_when_active() {
        let mut t = tracker();
        t.handle(focus("code"), day5(9, 0, 0));
        // Suspend detected: the segment is cut at the last awake moment.
        t.handle(Input::Suspend, day5(9, 10, 0));
        assert_eq!(t.pending[0].duration(), 600);
        assert!(t.current.is_none());
        // Resume 2 hours later: the user was active before sleeping, so the
        // last app is re-opened at the resume time (not during sleep).
        t.handle(Input::Resume, day5(11, 10, 0));
        let current = t.current.as_ref().unwrap();
        assert_eq!(current.class, "code");
        assert_eq!(current.started_at, day5(11, 10, 0));
    }

    #[test]
    fn suspend_while_idle_does_not_reopen_on_resume() {
        let mut t = tracker();
        t.handle(focus("code"), day5(9, 0, 0));
        t.handle(Input::Idle, day5(9, 20, 0));
        t.handle(Input::Suspend, day5(9, 30, 0));
        t.handle(Input::Resume, day5(11, 30, 0));
        // The user was AFK before the machine slept; nothing is re-opened
        // until a real event (input or focus change) arrives.
        assert!(t.current.is_none());
        // Real input after resume re-opens the app.
        t.handle(Input::Active, day5(11, 31, 0));
        assert!(t.current.is_some());
    }

    #[test]
    fn split_by_date_splits_midnight() {
        let seg = Segment {
            class: "code".into(),
            display_name: "Visual Studio Code".into(),
            category: Category::Editor,
            started_at: day5(23, 30, 0),
            ended_at: 6 * DAY + 1800, // next day 00:30
        };
        let pieces = split_by_date(&seg, &utc_date);
        assert_eq!(pieces.len(), 2);
        assert_eq!(utc_date(pieces[0].started_at), "1970-01-06");
        assert_eq!(pieces[0].duration(), 1800); // 23:30 -> 24:00
        assert_eq!(utc_date(pieces[1].started_at), "1970-01-07");
        assert_eq!(pieces[1].duration(), 1800); // 00:00 -> 00:30
        assert_eq!(
            pieces.iter().map(|p| p.duration()).sum::<i64>(),
            seg.duration()
        );
    }

    #[test]
    fn split_by_date_handles_single_date() {
        let seg = Segment {
            class: "code".into(),
            display_name: "Visual Studio Code".into(),
            category: Category::Editor,
            started_at: day5(9, 0, 0),
            ended_at: day5(10, 0, 0),
        };
        let pieces = split_by_date(&seg, &utc_date);
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].duration(), 3600);
    }

    #[test]
    fn split_by_date_handles_empty_segment() {
        let seg = Segment {
            class: "code".into(),
            display_name: "Visual Studio Code".into(),
            category: Category::Editor,
            started_at: day5(9, 0, 0),
            ended_at: day5(9, 0, 0),
        };
        assert!(split_by_date(&seg, &utc_date).is_empty());
    }
}
