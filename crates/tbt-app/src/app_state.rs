use std::collections::BTreeSet;

use gpui::{Context, EventEmitter};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp, ToSpan};
use tbt_core::{HarnessId, TimeRange};
use tbt_store::{BucketSize, IngestRunSummary, QueryFilter};

use crate::charts::time::time_zone;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateRange {
    Today,
    Last7Days,
    Last30Days,
    Custom(TimeRange),
}

impl DateRange {
    pub const PRESETS: [DateRange; 3] = [
        DateRange::Today,
        DateRange::Last7Days,
        DateRange::Last30Days,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            DateRange::Today => "Today",
            DateRange::Last7Days => "7d",
            DateRange::Last30Days => "30d",
            DateRange::Custom(_) => "Custom",
        }
    }

    pub fn time_range(&self, now: Timestamp, tz: &TimeZone) -> TimeRange {
        let days = match self {
            DateRange::Custom(r) => return *r,
            DateRange::Today => 1,
            DateRange::Last7Days => 7,
            DateRange::Last30Days => 30,
        };
        let today = now.to_zoned_datetime(tz.clone()).start_of_day();
        let Ok(today) = today else {
            return TimeRange::new(now - SignedDuration::from_hours(24 * days), now);
        };
        let end = today.checked_add(1.day()).unwrap_or_else(|_| today.clone());
        let start = end
            .checked_sub(days.days())
            .unwrap_or_else(|_| today.clone());
        TimeRange::new(start.timestamp(), end.timestamp())
    }

    pub fn bucket(&self) -> BucketSize {
        match self {
            DateRange::Today => BucketSize::Hour,
            DateRange::Last7Days | DateRange::Last30Days => BucketSize::Day,
            DateRange::Custom(r) if r.duration() <= SignedDuration::from_hours(48) => {
                BucketSize::Hour
            }
            DateRange::Custom(_) => BucketSize::Day,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessOption {
    pub id: HarnessId,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastSync {
    pub finished_at: Timestamp,
    pub events_new: u64,
    pub error: Option<String>,
}

impl LastSync {
    pub fn from_run(run: &IngestRunSummary) -> Option<LastSync> {
        Some(LastSync {
            finished_at: run.finished_at?,
            events_new: run.stats.events_new,
            error: run.error.clone(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppStateEvent {
    FiltersChanged,
    DataChanged,
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub range: DateRange,
    pub selected: BTreeSet<HarnessId>,
    pub harnesses: Vec<HarnessOption>,
    pub time_zone: String,
    pub syncing: bool,
    pub last_sync: Option<LastSync>,
}

impl EventEmitter<AppStateEvent> for AppState {}

impl Default for AppState {
    fn default() -> Self {
        AppState::new(TimeZone::system().iana_name().unwrap_or("UTC").to_owned())
    }
}

impl AppState {
    pub fn new(time_zone: String) -> Self {
        AppState {
            range: DateRange::Last7Days,
            selected: BTreeSet::new(),
            harnesses: Vec::new(),
            time_zone,
            syncing: false,
            last_sync: None,
        }
    }

    pub fn tz(&self) -> TimeZone {
        time_zone(&self.time_zone)
    }

    pub fn query_filter(&self, now: Timestamp) -> QueryFilter {
        let mut f = QueryFilter::new(self.range.time_range(now, &self.tz()), self.range.bucket());
        f.harnesses = self.selected.iter().cloned().collect();
        f.time_zone = self.time_zone.clone();
        f
    }

    pub fn is_all(&self) -> bool {
        self.selected.is_empty()
    }

    pub fn is_selected(&self, id: &HarnessId) -> bool {
        self.selected.contains(id)
    }

    pub fn set_range(&mut self, range: DateRange) -> bool {
        let changed = self.range != range;
        self.range = range;
        changed
    }

    pub fn toggle_harness(&mut self, id: &HarnessId) {
        if !self.selected.remove(id) {
            self.selected.insert(id.clone());
        }
        let all = !self.harnesses.is_empty()
            && self.harnesses.iter().all(|h| self.selected.contains(&h.id));
        if all {
            self.selected.clear();
        }
    }

    pub fn sync_status(&self, now: Timestamp) -> String {
        if self.syncing {
            return "Syncing…".to_owned();
        }
        match &self.last_sync {
            None => "Never synced".to_owned(),
            Some(s) => {
                let ago = relative_time(now, s.finished_at);
                match &s.error {
                    Some(e) => format!("Sync failed {ago}: {e}"),
                    None => format!("Synced {ago} · +{}", s.events_new),
                }
            }
        }
    }

    pub fn change_range(&mut self, range: DateRange, cx: &mut Context<Self>) {
        if self.set_range(range) {
            cx.emit(AppStateEvent::FiltersChanged);
            cx.notify();
        }
    }

    pub fn change_harness(&mut self, id: Option<&HarnessId>, cx: &mut Context<Self>) {
        match id {
            Some(id) => self.toggle_harness(id),
            None if self.selected.is_empty() => return,
            None => self.selected.clear(),
        }
        cx.emit(AppStateEvent::FiltersChanged);
        cx.notify();
    }
}

pub fn relative_time(now: Timestamp, then: Timestamp) -> String {
    let secs = now.as_second() - then.as_second();
    match secs {
        ..60 => "just now".to_owned(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86_400 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tbt_core::RunId;
    use tbt_store::{RunStats, RunTrigger};

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn state() -> AppState {
        let mut s = AppState::new("Europe/Berlin".to_owned());
        s.harnesses = [
            HarnessId::CLAUDE_CODE,
            HarnessId::CODEX,
            HarnessId::OPENCODE,
        ]
        .into_iter()
        .map(|id| HarnessOption {
            name: id.to_string(),
            id,
        })
        .collect();
        s
    }

    #[test]
    fn presets_map_to_local_day_ranges() {
        let now = ts("2026-11-23T15:20:00Z");
        let mut s = state();
        s.set_range(DateRange::Today);
        let f = s.query_filter(now);
        assert_eq!(
            f.range,
            TimeRange::new(ts("2026-11-22T23:00:00Z"), ts("2026-11-23T23:00:00Z"))
        );
        assert_eq!(f.bucket, BucketSize::Hour);
        assert_eq!(f.time_zone, "Europe/Berlin");
        assert!(f.harnesses.is_empty());

        s.set_range(DateRange::Last7Days);
        let f = s.query_filter(now);
        assert_eq!(
            f.range,
            TimeRange::new(ts("2026-11-16T23:00:00Z"), ts("2026-11-23T23:00:00Z"))
        );
        assert_eq!(f.bucket, BucketSize::Day);

        s.set_range(DateRange::Last30Days);
        let f = s.query_filter(now);
        assert_eq!(f.range.start, ts("2026-10-24T22:00:00Z"));
        assert_eq!(f.range.end, ts("2026-11-23T23:00:00Z"));
    }

    #[test]
    fn custom_range_passes_through() {
        let r = TimeRange::new(ts("2026-11-01T00:00:00Z"), ts("2026-11-02T12:00:00Z"));
        let mut s = state();
        assert!(s.set_range(DateRange::Custom(r)));
        assert!(!s.set_range(DateRange::Custom(r)));
        let f = s.query_filter(ts("2026-11-23T15:20:00Z"));
        assert_eq!(f.range, r);
        assert_eq!(f.bucket, BucketSize::Hour);
        let long = TimeRange::new(r.start, ts("2026-11-20T00:00:00Z"));
        assert_eq!(DateRange::Custom(long).bucket(), BucketSize::Day);
    }

    #[test]
    fn harness_selection_maps_to_sorted_filter() {
        let now = ts("2026-11-23T15:20:00Z");
        let mut s = state();
        s.toggle_harness(&HarnessId::OPENCODE);
        s.toggle_harness(&HarnessId::CLAUDE_CODE);
        assert_eq!(
            s.query_filter(now).harnesses,
            vec![HarnessId::CLAUDE_CODE, HarnessId::OPENCODE]
        );
        s.toggle_harness(&HarnessId::OPENCODE);
        assert_eq!(s.query_filter(now).harnesses, vec![HarnessId::CLAUDE_CODE]);
        s.toggle_harness(&HarnessId::CODEX);
        s.toggle_harness(&HarnessId::OPENCODE);
        assert!(s.is_all());
        assert!(s.query_filter(now).harnesses.is_empty());
    }

    #[test]
    fn sync_status_text() {
        let now = ts("2026-11-23T15:20:00Z");
        let mut s = state();
        assert_eq!(s.sync_status(now), "Never synced");
        let run = IngestRunSummary {
            id: RunId(1),
            trigger: RunTrigger::Hourly,
            started_at: ts("2026-11-23T15:00:00Z"),
            finished_at: Some(ts("2026-11-23T15:00:05Z")),
            stats: RunStats {
                sources_scanned: 3,
                events_new: 42,
            },
            warnings: vec![],
            error: None,
        };
        s.last_sync = LastSync::from_run(&run);
        assert_eq!(s.sync_status(now), "Synced 19 min ago · +42");
        s.syncing = true;
        assert_eq!(s.sync_status(now), "Syncing…");
        assert_eq!(relative_time(now, now), "just now");
        assert_eq!(relative_time(now, ts("2026-11-23T12:00:00Z")), "3 h ago");
        assert_eq!(relative_time(now, ts("2026-11-20T12:00:00Z")), "3 d ago");
    }
}
