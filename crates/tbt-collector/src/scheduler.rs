use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError};
use jiff::Timestamp;
use tbt_store::RunTrigger;

pub const MAX_SLEEP_CHUNK: Duration = Duration::from_secs(30);

pub fn next_tick(now: Timestamp, interval_minutes: u32) -> Timestamp {
    let step = i64::from(interval_minutes.max(1)) * 60;
    let next = (now.as_second().div_euclid(step) + 1).saturating_mul(step);
    Timestamp::from_second(next).unwrap_or(Timestamp::MAX)
}

pub fn sleep_until(deadline: Timestamp, stop: &Receiver<()>) -> bool {
    loop {
        let now = Timestamp::now();
        if now >= deadline {
            return true;
        }
        let remaining = Duration::try_from(deadline.duration_since(now)).unwrap_or_default();
        match stop.recv_timeout(remaining.min(MAX_SLEEP_CHUNK)) {
            Err(RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return false,
        }
    }
}

pub fn run_loop(
    interval_minutes: u32,
    first: RunTrigger,
    stop: &Receiver<()>,
    mut run: impl FnMut(RunTrigger),
) {
    run(first);
    loop {
        let next = next_tick(Timestamp::now(), interval_minutes);
        tracing::debug!(%next, "waiting for next collector tick");
        if !sleep_until(next, stop) {
            return;
        }
        run(RunTrigger::Hourly);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn hourly_ticks_align_to_the_full_hour() {
        assert_eq!(
            next_tick(ts("2026-11-23T10:17:42.5Z"), 60),
            ts("2026-11-23T11:00:00Z")
        );
        assert_eq!(
            next_tick(ts("2026-11-23T11:00:00Z"), 60),
            ts("2026-11-23T12:00:00Z")
        );
        assert_eq!(
            next_tick(ts("2026-11-23T11:00:00.001Z"), 60),
            ts("2026-11-23T12:00:00Z")
        );
        assert_eq!(
            next_tick(ts("2026-11-23T23:59:59Z"), 60),
            ts("2026-11-24T00:00:00Z")
        );
    }

    #[test]
    fn other_intervals() {
        let now = ts("2026-11-23T10:17:00Z");
        assert_eq!(next_tick(now, 15), ts("2026-11-23T10:30:00Z"));
        assert_eq!(next_tick(now, 120), ts("2026-11-23T12:00:00Z"));
        assert_eq!(next_tick(now, 1440), ts("2026-11-24T00:00:00Z"));
        assert_eq!(next_tick(now, 0), ts("2026-11-23T10:18:00Z"));
        assert_eq!(
            next_tick(ts("1969-12-31T23:30:00Z"), 60),
            ts("1970-01-01T00:00:00Z")
        );
    }

    #[test]
    fn sleep_until_past_deadline_returns_at_once_and_stop_interrupts() {
        let (tx, rx) = crossbeam_channel::bounded(1);
        assert!(sleep_until(ts("2000-01-01T00:00:00Z"), &rx));
        tx.send(()).unwrap();
        let far = Timestamp::now() + jiff::SignedDuration::from_hours(1);
        assert!(!sleep_until(far, &rx));
        drop(tx);
        assert!(!sleep_until(far, &rx));
    }

    #[test]
    fn run_loop_runs_first_trigger_then_stops() {
        let (tx, rx) = crossbeam_channel::bounded(1);
        tx.send(()).unwrap();
        let mut seen = Vec::new();
        run_loop(60, RunTrigger::Manual, &rx, |t| seen.push(t));
        assert_eq!(seen, [RunTrigger::Manual]);
    }
}
