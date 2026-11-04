//! Suspend/resume detection without extra dependencies.
//!
//! Linux's `CLOCK_MONOTONIC` (what `std::time::Instant` uses) does **not**
//! advance while the machine is suspended, but the wall clock does. Watching
//! the offset between the two clocks therefore reveals suspend/resume
//! cycles: if the offset grows, the machine slept for (growth) seconds.
//!
//! The estimate for *when* the machine went to sleep is the wall time of the
//! last observation before the drift — the last moment we know the machine
//! was awake. Segments are cut there, so sleep time is never counted as
//! usage.

/// Wall/monotonic drift watcher. Feed it paired timestamps; it reports the
/// estimated wall time at which the machine went to sleep.
pub struct SuspendDetector {
    threshold_ms: i64,
    last_offset_ms: Option<i64>,
    last_wall_ms: i64,
}

impl SuspendDetector {
    pub fn new(threshold_ms: i64) -> Self {
        SuspendDetector {
            threshold_ms,
            last_offset_ms: None,
            last_wall_ms: crate::unix_now() * 1000,
        }
    }

    /// Observe one moment. Returns `Some(suspend_started_at)` (unix seconds)
    /// when the observations reveal a suspend happened since the previous
    /// call. The first observation never reports (no baseline yet).
    pub fn observe(&mut self, wall_ms: i64, mono_ms: i64) -> Option<i64> {
        let offset = wall_ms - mono_ms;
        let prev_offset = self.last_offset_ms.replace(offset);
        let prev_wall = self.last_wall_ms;
        self.last_wall_ms = wall_ms;
        match prev_offset {
            None => None,
            Some(prev) if offset - prev >= self.threshold_ms => Some(prev_wall / 1000),
            Some(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulated clocks: wall starts at 1_000_000s, monotonic at 0.
    /// Suspend of `sleep_s` seconds advances the wall clock but not the
    /// monotonic clock.
    fn observe_s(detector: &mut SuspendDetector, wall_s: i64, mono_s: i64) -> Option<i64> {
        detector.observe(wall_s * 1000, mono_s * 1000)
    }

    #[test]
    fn first_observation_never_reports() {
        let mut d = SuspendDetector::new(10_000);
        assert_eq!(observe_s(&mut d, 1_000_000, 0), None);
    }

    #[test]
    fn steady_offset_never_reports() {
        let mut d = SuspendDetector::new(10_000);
        assert_eq!(observe_s(&mut d, 1_000_000, 0), None);
        assert_eq!(observe_s(&mut d, 1_000_060, 60), None);
        assert_eq!(observe_s(&mut d, 1_000_120, 120), None);
        assert_eq!(observe_s(&mut d, 1_000_300, 300), None);
    }

    #[test]
    fn detects_suspend_and_reports_last_awake_moment() {
        let mut d = SuspendDetector::new(10_000);
        assert_eq!(observe_s(&mut d, 1_000_000, 0), None);
        assert_eq!(observe_s(&mut d, 1_000_050, 50), None);
        // Machine sleeps 2 hours after the last observation at 1_000_050.
        let resumed_wall = 1_000_050 + 7200;
        // Monotonic barely advanced (only the resume handling itself).
        let report = observe_s(&mut d, resumed_wall, 55);
        assert_eq!(report, Some(1_000_050));
        // After resume the offset is stable again.
        assert_eq!(observe_s(&mut d, resumed_wall + 30, 85), None);
    }

    #[test]
    fn small_drift_below_threshold_is_ignored() {
        let mut d = SuspendDetector::new(10_000);
        assert_eq!(observe_s(&mut d, 1_000_000, 0), None);
        // 5 seconds of drift (e.g. a small NTP step) stays below the 10s bar.
        assert_eq!(observe_s(&mut d, 1_000_100, 95), None);
        assert_eq!(observe_s(&mut d, 1_000_200, 190), None);
    }
}
