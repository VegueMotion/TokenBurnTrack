mod common;

use std::path::PathBuf;
use std::sync::Arc;

use jiff::Timestamp;
use tbt_app::charts::format::micros_to_usd;
use tbt_app::data::{DataSource, StoreData};
use tbt_app::pages::dashboard::DashboardData;
use tbt_app::pages::sessions::{SessionTable, SortKey};
use tbt_core::{HarnessId, Micros, TimeRange};
use tbt_store::{BucketSize, QueryFilter, Store};
use tempfile::TempDir;

const HAIKU: &str = "claude-haiku-5-5";

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

struct Fixture {
    _dir: TempDir,
    db: PathBuf,
}

fn fixture(unpriced: &[&str]) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let roots = common::copy_claude_code_fixtures(&dir.path().join("logs")).unwrap();
    assert!(!roots.is_empty());
    let db = dir.path().join("data").join("tbt.sqlite");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let n = common::build_db(&db, &roots, unpriced, ts("2026-11-23T00:00:00Z")).unwrap();
    assert!(n > 0);
    Fixture { _dir: dir, db }
}

fn filter(start: &str, end: &str, bucket: BucketSize) -> QueryFilter {
    QueryFilter::new(TimeRange::new(ts(start), ts(end)), bucket)
}

fn all_time() -> QueryFilter {
    filter(
        "2026-11-01T00:00:00Z",
        "2026-12-01T00:00:00Z",
        BucketSize::Day,
    )
}

fn assert_close(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-6, "{a} != {b}");
}

fn assert_buckets_match_kpi(d: &DashboardData) {
    let usd = micros_to_usd(d.kpi.cost);
    assert_close(d.cost_per_bucket.iter().sum(), usd);
    assert_close(
        d.cost.series.iter().flat_map(|s| s.values.iter()).sum(),
        usd,
    );
    let tokens: f64 = d.tokens.series.iter().flat_map(|s| s.values.iter()).sum();
    assert_eq!(tokens as u64, d.kpi.tokens.total());
    let heat_cost: f64 = d.heat.cost.iter().flatten().sum();
    assert_close(heat_cost, usd);
    let heat_calls: u64 = d.heat.calls.iter().flatten().sum();
    assert_eq!(heat_calls, d.kpi.events);
}

#[test]
fn kpi_totals_equal_session_totals() {
    let fx = fixture(&[]);
    let data = StoreData::new(fx.db.clone());
    let f = all_time();
    let kpi = data.kpi_summary(&f).unwrap();
    let table = SessionTable::load(&data, &f).unwrap();
    let rows = &table.rows;
    assert!(kpi.events > 0 && kpi.cost > Micros::ZERO);
    assert!(!table.no_data);
    assert_eq!(kpi.sessions, rows.len() as u64);
    assert_eq!(kpi.cost, rows.iter().map(|r| r.cost).sum::<Micros>());
    assert_eq!(kpi.events, rows.iter().map(|r| r.events).sum::<u64>());
    assert_eq!(
        kpi.tokens.total(),
        rows.iter().map(|r| r.tokens.total()).sum::<u64>()
    );
    assert_eq!(kpi.unpriced_events, 0);

    let view = table.view("", SortKey::Cost, true);
    assert_eq!(view.indices.len(), rows.len());
    assert_eq!(view.cost, kpi.cost);
    let costs: Vec<Micros> = view.indices.iter().map(|i| rows[*i].cost).collect();
    assert!(costs.windows(2).all(|w| w[0] >= w[1]));
}

#[test]
fn day_and_hour_buckets_sum_to_kpi() {
    let fx = fixture(&[]);
    let data = StoreData::new(fx.db.clone());

    let d = DashboardData::load(&data, &all_time()).unwrap();
    assert!(!d.no_data);
    assert_eq!(d.cost.labels.len(), 30);
    assert_buckets_match_kpi(&d);
    assert!(!d.top_models.is_empty());
    assert!(!d.top_projects.is_empty());
    assert_eq!(
        d.top_models.iter().map(|m| m.cost).sum::<Micros>(),
        d.kpi.cost
    );

    let day = filter(
        "2026-11-21T00:00:00Z",
        "2026-11-22T00:00:00Z",
        BucketSize::Hour,
    );
    let d = DashboardData::load(&data, &day).unwrap();
    assert_eq!(d.cost.labels.len(), 24);
    assert!(d.kpi.events > 0);
    assert_buckets_match_kpi(&d);
}

#[test]
fn unpriced_events_count_as_zero_cost() {
    let priced = fixture(&[]);
    let partly = fixture(&[HAIKU]);
    let f = all_time();
    let full = StoreData::new(priced.db.clone()).kpi_summary(&f).unwrap();
    let data = StoreData::new(partly.db.clone());
    let d = DashboardData::load(&data, &f).unwrap();
    let k = &d.kpi;
    assert_eq!(k.events, full.events);
    assert!(k.unpriced_events > 0);
    assert!(k.cost < full.cost);
    assert_buckets_match_kpi(&d);

    let haiku = data
        .model_stats(&f)
        .unwrap()
        .into_iter()
        .find(|m| m.model == HAIKU)
        .unwrap();
    assert_eq!(haiku.cost, Micros::ZERO);
    assert_eq!(haiku.unpriced_events, haiku.events);
    assert_eq!(haiku.events, k.unpriced_events);

    let table = SessionTable::load(&data, &f).unwrap();
    let sub = table
        .rows
        .iter()
        .find(|r| r.models == [HAIKU])
        .expect("subagent session");
    assert_eq!(sub.cost, Micros::ZERO);
    assert_eq!(sub.unpriced_events, sub.events);
    assert!(sub.parent.is_some());
}

#[test]
fn harness_filter_applies_to_dashboard_and_sessions() {
    let fx = fixture(&[]);
    let data = StoreData::new(fx.db.clone());
    let all = DashboardData::load(&data, &all_time()).unwrap();

    let mut f = all_time();
    f.harnesses = vec![HarnessId::CLAUDE_CODE];
    let cc = DashboardData::load(&data, &f).unwrap();
    assert_eq!(cc.kpi, all.kpi);

    f.harnesses = vec![HarnessId::CODEX];
    let codex = DashboardData::load(&data, &f).unwrap();
    assert_eq!(codex.kpi.events, 0);
    assert_eq!(codex.kpi.cost, Micros::ZERO);
    assert!(!codex.no_data);
    assert!(codex.cost.series.is_empty());
    assert!(codex.top_models.is_empty());
    let table = SessionTable::load(&data, &f).unwrap();
    assert!(table.rows.is_empty());
    assert!(!table.no_data);
}

#[test]
fn range_without_usage_is_not_the_no_data_state() {
    let fx = fixture(&[]);
    let data = StoreData::new(fx.db.clone());
    let f = filter(
        "2026-10-01T00:00:00Z",
        "2026-10-08T00:00:00Z",
        BucketSize::Day,
    );
    let d = DashboardData::load(&data, &f).unwrap();
    assert_eq!(d.kpi.events, 0);
    assert!(!d.no_data);
    assert_eq!(d.cost.labels.len(), 7);
    assert!(d.cost_per_bucket.iter().all(|v| *v == 0.0));
}

#[test]
fn empty_db_shows_empty_state() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("tbt.sqlite");
    drop(Store::open(&db).unwrap());
    let data = StoreData::new(db);
    let d = DashboardData::load(&data, &all_time()).unwrap();
    assert!(d.no_data);
    assert_eq!(d.kpi.events, 0);
    let table = SessionTable::load(&data, &all_time()).unwrap();
    assert!(table.no_data);
    assert!(data.ingest_runs(1).unwrap().is_empty());
}

#[test]
fn missing_db_file_shows_empty_state_and_is_not_created() {
    let dir = tempfile::tempdir().unwrap();
    let data = StoreData::from_data_dir(Some(dir.path().join("nope")));
    let db = data.db_path().unwrap().to_path_buf();
    let d = DashboardData::load(&data, &all_time()).unwrap();
    assert!(d.no_data);
    assert!(SessionTable::load(&data, &all_time()).unwrap().no_data);
    assert!(data.harness_status().unwrap().is_empty());
    assert!(
        data.session_detail(tbt_core::SessionId(1))
            .unwrap()
            .is_none()
    );
    assert!(!db.exists());
}

#[test]
fn db_created_later_is_picked_up() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("tbt.sqlite");
    let data = StoreData::new(db.clone());
    assert!(!data.has_data().unwrap());
    let roots = common::copy_claude_code_fixtures(&dir.path().join("logs")).unwrap();
    common::build_db(&db, &roots, &[], ts("2026-11-23T00:00:00Z")).unwrap();
    assert!(data.has_data().unwrap());
    assert_eq!(data.ingest_runs(1).unwrap().len(), 1);
}

#[test]
fn no_data_dir_is_an_error() {
    let data = StoreData::from_data_dir(None);
    let Err(err) = DashboardData::load(&data, &all_time()) else {
        panic!("expected an error without a data dir");
    };
    assert!(format!("{err:#}").contains("TBT_DATA_DIR"));
}

#[test]
fn read_only_connections_never_write() {
    let fx = fixture(&[]);
    let before = std::fs::read(&fx.db).unwrap();
    let data = StoreData::new(fx.db.clone());
    DashboardData::load(&data, &all_time()).unwrap();
    assert!(data.has_data().unwrap());
    assert_eq!(std::fs::read(&fx.db).unwrap(), before);
}

#[test]
fn concurrent_page_loads_agree() {
    let fx = fixture(&[]);
    let data = Arc::new(StoreData::new(fx.db.clone()));
    let f = all_time();
    let expected = DashboardData::load(&*data, &f).unwrap().kpi;
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let (data, f) = (data.clone(), f.clone());
            std::thread::spawn(move || {
                (0..5)
                    .map(|_| {
                        let d = DashboardData::load(&*data, &f).unwrap();
                        let t = SessionTable::load(&*data, &f).unwrap();
                        assert_eq!(t.rows.len() as u64, d.kpi.sessions);
                        d.kpi
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    for h in handles {
        for kpi in h.join().unwrap() {
            assert_eq!(kpi, expected);
        }
    }
}
