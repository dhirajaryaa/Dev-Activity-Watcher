//! Interval arithmetic for multi-device overlap resolution.
//!
//! A single human cannot code 23 hours in one day even if a desktop, two
//! laptops and a workstation all report VS Code usage. Adding device
//! durations double-counts overlapping wall-clock time, so "unique active
//! time" is computed as the **union** of `[start, end)` intervals across
//! all devices, never the sum.
//!
//! The cloud recomputes daily aggregates with SQL window functions; this
//! module is the pure-Rust oracle those queries are tested against (same
//! inputs must produce the same totals) and the reference implementation
//! for the attribution policy:
//!
//! - `combined_seconds` — plain Σ of durations per app (may exceed 24 h)
//! - `merged_seconds`   — union of that app's intervals across devices
//! - `overlapping`      — Σ app merged − global union: time covered by
//!   two or more *different* apps at once, attributed to an explicit
//!   category instead of being double-counted or guessed away.

/// Merge `[start, end)` intervals into a disjoint, sorted cover.
/// Zero-length intervals are dropped; adjacent intervals fuse.
pub fn merge_intervals(mut intervals: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
    intervals.retain(|(s, e)| e > s);
    intervals.sort_unstable();
    let mut out: Vec<(i64, i64)> = Vec::with_capacity(intervals.len());
    for (start, end) in intervals {
        match out.last_mut() {
            // Overlapping or touching: extend the current run.
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => out.push((start, end)),
        }
    }
    out
}

/// Total covered seconds of a (not necessarily disjoint) interval list.
pub fn union_seconds(intervals: &[(i64, i64)]) -> i64 {
    merge_intervals(intervals.to_vec()).iter().map(|(s, e)| e - s).sum()
}

/// Attribution outcome for one reporting day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyMerge {
    /// Union of all intervals across all apps and devices — the honest
    /// "human was active" total, never above 24 h for one day.
    pub unique_active_seconds: i64,
    /// Per app: (combined Σ durations, union of that app's intervals).
    pub per_app: Vec<(String, i64, i64)>,
    /// Time covered by ≥2 different apps at once:
    /// Σ per-app merged − global union. Attributed to "(overlapping)".
    pub overlap_seconds: i64,
}

/// Compute the daily aggregate for one reporting day from raw segments.
/// `segments` are `(app_name, start, end)` triples in any order.
pub fn merge_day(segments: &[(String, i64, i64)]) -> DailyMerge {
    // Per-app combined and merged totals.
    let mut by_app: Vec<(String, Vec<(i64, i64)>)> = Vec::new();
    for (app, start, end) in segments {
        if end <= start {
            continue;
        }
        match by_app.iter_mut().find(|(a, _)| a == app) {
            Some((_, ivs)) => ivs.push((*start, *end)),
            None => by_app.push((app.clone(), vec![(*start, *end)])),
        }
    }
    by_app.sort_by(|a, b| a.0.cmp(&b.0));

    let mut per_app = Vec::with_capacity(by_app.len());
    let mut all_intervals: Vec<(i64, i64)> = Vec::new();
    for (app, mut ivs) in by_app {
        let combined: i64 = ivs.iter().map(|(s, e)| e - s).sum();
        let merged = merge_intervals(std::mem::take(&mut ivs));
        all_intervals.extend_from_slice(&merged);
        per_app.push((app, combined, union_seconds(&merged)));
    }

    let unique_active_seconds = union_seconds(&all_intervals);
    let app_merged_total: i64 = per_app.iter().map(|(_, _, m)| *m).sum();
    DailyMerge {
        unique_active_seconds,
        per_app,
        overlap_seconds: app_merged_total - unique_active_seconds,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: i64 = 3600;

    fn app(name: &str, start: i64, end: i64) -> (String, i64, i64) {
        (name.to_string(), start, end)
    }

    #[test]
    fn disjoint_intervals_stay_separate() {
        let m = merge_intervals(vec![(0, 10), (20, 30), (40, 50)]);
        assert_eq!(m, vec![(0, 10), (20, 30), (40, 50)]);
        assert_eq!(union_seconds(&m), 30);
    }

    #[test]
    fn overlapping_and_adjacent_intervals_fuse() {
        // Overlap fuses; touching (end == next start) fuses too.
        let m = merge_intervals(vec![(0, 10), (5, 15), (15, 20), (30, 40)]);
        assert_eq!(m, vec![(0, 20), (30, 40)]);
        assert_eq!(union_seconds(&m), 30);
    }

    #[test]
    fn nested_and_unsorted_input() {
        let m = merge_intervals(vec![(10, 30), (0, 100), (15, 20)]);
        assert_eq!(m, vec![(0, 100)]);
        assert_eq!(union_seconds(&m), 100);
    }

    #[test]
    fn zero_length_and_inverted_intervals_are_dropped() {
        let m = merge_intervals(vec![(5, 5), (10, 3), (0, 10)]);
        assert_eq!(m, vec![(0, 10)]);
    }

    #[test]
    fn spec_example_never_exceeds_24h() {
        // §7: VS Code on 4 devices — 8 h + 6 h + 5 h + 4 h = 23 h combined,
        // but all overlapping the same working day: unique time stays ≤ 24 h.
        let segs = vec![
            app("VS Code", 0, 8 * H),
            app("VS Code", 2 * H, 8 * H),
            app("VS Code", 4 * H, 9 * H),
            app("VS Code", 6 * H, 10 * H),
        ];
        let d = merge_day(&segs);
        assert_eq!(d.per_app[0].1, 23 * H, "combined Σ may exceed a day");
        assert_eq!(d.per_app[0].2, 10 * H, "merged union is the honest total");
        assert_eq!(d.unique_active_seconds, 10 * H);
        assert_eq!(d.overlap_seconds, 0, "same app everywhere: no cross-app overlap");
    }

    #[test]
    fn different_apps_overlapping_become_overlap_category() {
        // Desktop VS Code 9–17, laptop terminal 13–15: the 2 h overlap is
        // counted once, attributed to "(overlapping)", not to either app.
        let segs = vec![
            app("VS Code", 9 * H, 17 * H),
            app("Terminal", 13 * H, 15 * H),
        ];
        let d = merge_day(&segs);
        assert_eq!(d.unique_active_seconds, 8 * H);
        // per_app is sorted by name: Terminal, then VS Code.
        assert_eq!(d.per_app[0].0, "Terminal");
        assert_eq!(d.per_app[0].2, 2 * H);
        assert_eq!(d.per_app[1].2, 8 * H);
        assert_eq!(d.overlap_seconds, 2 * H);
        // Σ per-app merged − overlap == unique (the invariant).
        let sum: i64 = d.per_app.iter().map(|(_, _, m)| *m).sum();
        assert_eq!(sum - d.overlap_seconds, d.unique_active_seconds);
    }

    #[test]
    fn late_arriving_segment_recompute_is_deterministic() {
        // A late batch adds a segment fully inside known coverage: totals
        // must not change (idempotent recompute from immutable segments).
        let base = vec![app("VS Code", 0, 4 * H), app("Foot", 5 * H, 6 * H)];
        let late = {
            let mut v = base.clone();
            v.push(app("VS Code", 1 * H, 2 * H));
            v
        };
        let a = merge_day(&base);
        let b = merge_day(&late);
        assert_eq!(a.unique_active_seconds, b.unique_active_seconds);
        // Merged totals are unchanged (the late segment is inside coverage);
        // combined Σ legitimately grows by the duplicate 1 h.
        let merged = |d: &DailyMerge| d.per_app.iter().map(|(_, _, m)| *m).collect::<Vec<_>>();
        assert_eq!(merged(&a), merged(&b));
    }

    #[test]
    fn midnight_boundary_split_segments_stay_correct() {
        // One 2 h session crossing midnight, split per local date: each day
        // sees exactly its own hour.
        let day_start = 5 * 86_400;
        let segs = vec![app("VS Code", day_start + 23 * H, day_start + 25 * H)];
        // (The caller splits by date before calling; verify one day's slice.)
        let d = merge_day(&[app("VS Code", day_start + 23 * H, day_start + 24 * H)]);
        assert_eq!(d.unique_active_seconds, H);
        let _ = segs;
    }
}
