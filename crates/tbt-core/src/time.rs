use std::fmt;

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Hour(Timestamp);

impl Hour {
    pub fn containing(ts: Timestamp) -> Hour {
        let secs = ts.as_second().div_euclid(3600) * 3600;
        Hour(Timestamp::from_second(secs).unwrap_or(Timestamp::MIN))
    }

    pub fn start(self) -> Timestamp {
        self.0
    }

    pub fn end(self) -> Timestamp {
        self.0
            .checked_add(SignedDuration::from_hours(1))
            .unwrap_or(Timestamp::MAX)
    }
}

impl fmt::Display for Hour {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: Timestamp,
    pub end: Timestamp,
}

impl TimeRange {
    pub fn new(start: Timestamp, end: Timestamp) -> TimeRange {
        TimeRange { start, end }
    }

    pub fn contains(&self, ts: Timestamp) -> bool {
        self.start <= ts && ts < self.end
    }

    pub fn duration(&self) -> SignedDuration {
        self.end.duration_since(self.start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn hour_truncates() {
        let h = Hour::containing(ts("2026-11-23T10:59:59.999Z"));
        assert_eq!(h.start(), ts("2026-11-23T10:00:00Z"));
        assert_eq!(h.end(), ts("2026-11-23T11:00:00Z"));
        assert_eq!(Hour::containing(h.start()), h);
    }

    #[test]
    fn hour_before_epoch() {
        let h = Hour::containing(ts("1969-12-31T23:30:00Z"));
        assert_eq!(h.start(), ts("1969-12-31T23:00:00Z"));
    }

    #[test]
    fn range_is_half_open() {
        let r = TimeRange::new(ts("2026-11-23T00:00:00Z"), ts("2026-11-24T00:00:00Z"));
        assert!(r.contains(ts("2026-11-23T00:00:00Z")));
        assert!(!r.contains(ts("2026-11-24T00:00:00Z")));
        assert_eq!(r.duration(), SignedDuration::from_hours(24));
    }
}
