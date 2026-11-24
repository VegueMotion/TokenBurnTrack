use std::time::{Duration, Instant};

use jiff::Timestamp;
use tbt_app::data::{DataSource, StoreData};
use tbt_app::pages::dashboard::DashboardData;
use tbt_app::pages::sessions::{SessionTable, SortKey};
use tbt_core::TimeRange;
use tbt_store::{BucketSize, QueryFilter};

fn best_of<T>(runs: usize, mut f: impl FnMut() -> T) -> (Duration, T) {
    let mut best = Duration::MAX;
    let mut out = None;
    for _ in 0..runs {
        let t = Instant::now();
        let v = f();
        best = best.min(t.elapsed());
        out = Some(v);
    }
    (best, out.unwrap())
}

fn main() {
    let db = std::env::args().nth(1).expect("path to tbt.sqlite");
    let data = StoreData::new(db.into());
    let now: Timestamp = "2026-11-24T23:00:00Z".parse().unwrap();
    println!("| step | range | best ms | rows |");
    println!("|---|---|---|---|");
    for (label, days) in [("30d", 30), ("1y", 365), ("all", 600)] {
        let mut f = QueryFilter::new(
            TimeRange::new(now - jiff::SignedDuration::from_hours(24 * days), now),
            BucketSize::Day,
        );
        f.time_zone = "Europe/Berlin".into();

        let (t, table) = best_of(5, || SessionTable::load(&data, &f).unwrap());
        let n = table.rows.len();
        println!("| SessionTable::load | {label} | {:.1} | {n} |", ms(t));
        for (name, query, key, desc) in [
            ("view, no search, by start", "", SortKey::Start, true),
            ("view, no search, by cost", "", SortKey::Cost, true),
            ("view, no search, by project", "", SortKey::Project, false),
            ("view, no search, by models", "", SortKey::Models, false),
            (
                "view, search \"claude main\"",
                "claude main",
                SortKey::Start,
                true,
            ),
            (
                "view, search \"project-0\"",
                "project-0",
                SortKey::Cost,
                true,
            ),
        ] {
            let (t, v) = best_of(10, || table.view(query, key, desc));
            println!("| {name} | {label} | {:.2} | {} |", ms(t), v.indices.len());
        }
        let (t, _) = best_of(5, || DashboardData::load(&data, &f).unwrap());
        println!(
            "| DashboardData::load (all dashboard queries) | {label} | {:.1} | - |",
            ms(t)
        );
    }
    let (t, _) = best_of(5, || data.has_data().unwrap());
    println!("| has_data | - | {:.1} | - |", ms(t));
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}
