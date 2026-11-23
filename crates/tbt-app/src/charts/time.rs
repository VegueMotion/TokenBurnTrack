use std::collections::BTreeMap;

use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};
use tbt_core::{Hour, TimeRange};
use tbt_store::{BucketSize, CostBucket};

pub fn time_zone(name: &str) -> TimeZone {
    TimeZone::get(name).unwrap_or(TimeZone::UTC)
}

pub fn bucket_start(ts: Timestamp, bucket: BucketSize, tz: &TimeZone) -> Timestamp {
    match bucket {
        BucketSize::Hour => Hour::containing(ts).start(),
        BucketSize::Day => ts
            .to_zoned_datetime(tz.clone())
            .start_of_day()
            .map(|z| z.timestamp())
            .unwrap_or(ts),
    }
}

pub fn bucket_starts(range: TimeRange, bucket: BucketSize, tz: &TimeZone) -> Vec<Timestamp> {
    let mut out = Vec::new();
    let mut cur = bucket_start(range.start, bucket, tz).to_zoned_datetime(tz.clone());
    while cur.timestamp() < range.end && out.len() < 10_000 {
        out.push(cur.timestamp());
        let next = match bucket {
            BucketSize::Hour => cur.checked_add(1.hour()),
            BucketSize::Day => cur.checked_add(1.day()),
        };
        match next {
            Ok(n) => cur = n,
            Err(_) => break,
        }
    }
    out
}

pub fn bucket_index(starts: &[Timestamp], ts: Timestamp) -> Option<usize> {
    let idx = starts.partition_point(|s| *s <= ts);
    idx.checked_sub(1)
}

pub fn bucket_label(ts: Timestamp, bucket: BucketSize, tz: &TimeZone) -> String {
    let z = ts.to_zoned_datetime(tz.clone());
    match bucket {
        BucketSize::Hour => z.strftime("%H:%M").to_string(),
        BucketSize::Day => z.strftime("%b %-d").to_string(),
    }
}

pub fn bucket_title(ts: Timestamp, bucket: BucketSize, tz: &TimeZone) -> String {
    let z = ts.to_zoned_datetime(tz.clone());
    match bucket {
        BucketSize::Hour => z.strftime("%a %b %-d, %H:%M").to_string(),
        BucketSize::Day => z.strftime("%a %b %-d").to_string(),
    }
}

pub fn pivot(
    buckets: &[CostBucket],
    starts: &[Timestamp],
    value: impl Fn(&CostBucket) -> f64,
) -> BTreeMap<String, Vec<f64>> {
    let mut out: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for b in buckets {
        let Some(i) = bucket_index(starts, b.start) else {
            continue;
        };
        out.entry(b.key.clone())
            .or_insert_with(|| vec![0.0; starts.len()])[i] += value(b);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tbt_core::{Micros, TokenCounts};

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn hourly_buckets() {
        let r = TimeRange::new(ts("2026-11-23T10:30:00Z"), ts("2026-11-23T13:00:00Z"));
        let s = bucket_starts(r, BucketSize::Hour, &TimeZone::UTC);
        assert_eq!(
            s,
            vec![
                ts("2026-11-23T10:00:00Z"),
                ts("2026-11-23T11:00:00Z"),
                ts("2026-11-23T12:00:00Z")
            ]
        );
        assert_eq!(bucket_index(&s, ts("2026-11-23T09:59:59Z")), None);
        assert_eq!(bucket_index(&s, ts("2026-11-23T11:00:00Z")), Some(1));
        assert_eq!(bucket_index(&s, ts("2026-11-23T10:59:59Z")), Some(0));
        assert_eq!(bucket_index(&s, ts("2026-11-23T23:00:00Z")), Some(2));
        assert_eq!(
            bucket_label(s[1], BucketSize::Hour, &TimeZone::UTC),
            "11:00"
        );
    }

    #[test]
    fn daily_buckets_follow_local_midnight_across_dst() {
        let tz = time_zone("Europe/Berlin");
        let r = TimeRange::new(ts("2026-10-24T22:00:00Z"), ts("2026-10-27T23:00:00Z"));
        let s = bucket_starts(r, BucketSize::Day, &tz);
        assert_eq!(
            s,
            vec![
                ts("2026-10-24T22:00:00Z"),
                ts("2026-10-25T23:00:00Z"),
                ts("2026-10-26T23:00:00Z"),
            ]
        );
        assert_eq!(bucket_label(s[2], BucketSize::Day, &tz), "Oct 27");
        assert_eq!(
            bucket_start(ts("2026-10-26T23:30:00Z"), BucketSize::Day, &tz),
            s[2]
        );
    }

    #[test]
    fn unknown_zone_falls_back_to_utc() {
        assert_eq!(time_zone("Not/AZone"), TimeZone::UTC);
    }

    #[test]
    fn pivot_fills_dense_series() {
        let s = vec![ts("2026-11-23T10:00:00Z"), ts("2026-11-23T11:00:00Z")];
        let b = |start: &str, key: &str, cost: i64| CostBucket {
            start: ts(start),
            key: key.to_owned(),
            cost: Micros(cost),
            tokens: TokenCounts::default().into(),
            events: 1,
        };
        let p = pivot(
            &[
                b("2026-11-23T11:00:00Z", "codex", 2),
                b("2026-11-23T10:00:00Z", "claude_code", 1),
                b("2026-11-23T11:00:00Z", "claude_code", 3),
                b("2026-11-22T11:00:00Z", "claude_code", 99),
            ],
            &s,
            |b| b.cost.0 as f64,
        );
        assert_eq!(p.keys().collect::<Vec<_>>(), ["claude_code", "codex"]);
        assert_eq!(p["claude_code"], vec![1.0, 3.0]);
        assert_eq!(p["codex"], vec![0.0, 2.0]);
    }
}
