mod common;

use std::collections::HashMap;
use std::path::PathBuf;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use tbt_app::pages::settings::ops;
use tbt_app::pages::settings::price_form::{
    EffectiveFrom, PriceForm, PricePlan, SaveKind, plan_save,
};
use tbt_core::{EventId, Micros, ModelPrice, PriceId, TimeRange};
use tbt_store::{BucketSize, QueryFilter, RecomputeFilter, RecomputeRow, Store};
use tempfile::TempDir;

const OPUS: &str = "claude-opus-5-5";
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
    let db = dir.path().join("data").join("tbt.sqlite");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    common::build_db(&db, &roots, unpriced, ts("2026-11-23T00:00:00Z")).unwrap();
    Fixture { _dir: dir, db }
}

fn events(db: &PathBuf, model: &str) -> Vec<RecomputeRow> {
    let store = Store::open_read_only(db).unwrap();
    let filter = RecomputeFilter {
        model: Some(model.into()),
        ..Default::default()
    };
    let mut rows = store.events_for_recompute(&filter, None, 100_000).unwrap();
    rows.sort_by_key(|r| (r.event.ts, r.id));
    rows
}

fn total_cost(db: &PathBuf) -> Micros {
    let store = Store::open_read_only(db).unwrap();
    let f = QueryFilter::new(
        TimeRange::new(ts("2020-01-01T00:00:00Z"), ts("2030-01-01T00:00:00Z")),
        BucketSize::Day,
    );
    let kpi = store.kpi_summary(&f).unwrap();
    let sessions: Micros = store.sessions(&f).unwrap().iter().map(|s| s.cost).sum();
    assert_eq!(kpi.cost, sessions, "rollups and session totals disagree");
    kpi.cost
}

fn form(model: &str, input: &str, output: &str, from: Timestamp) -> PriceForm {
    PriceForm {
        provider: "anthropic".into(),
        model_pattern: model.into(),
        input: input.into(),
        output: output.into(),
        cache_write_5m: input.into(),
        cache_write_1h: input.into(),
        cache_read: input.into(),
        effective_from: EffectiveFrom::At(from.to_string()),
        long_context: None,
    }
}

fn apply(db: &PathBuf, plan: &PricePlan, unpriced: Option<(&str, &str)>) -> ops::Outcome {
    ops::with_writable_store(db, |s| ops::apply_plan(s, plan, unpriced)).unwrap()
}

fn user_row(db: &PathBuf, from: Timestamp) -> ModelPrice {
    Store::open_read_only(db)
        .unwrap()
        .model_prices()
        .unwrap()
        .into_iter()
        .find(|p| p.source == tbt_core::PriceSource::User && p.effective_from == from)
        .unwrap()
}

#[test]
fn user_override_reprices_only_its_window() {
    let fx = fixture(&[]);
    let before = events(&fx.db, OPUS);
    assert!(before.len() > 20);
    let cost_before = total_cost(&fx.db);
    let base: HashMap<EventId, (Micros, Option<PriceId>)> = before
        .iter()
        .map(|r| (r.id, (r.cost, r.price_id)))
        .collect();
    let n = before.len();
    let (a, b, c) = (
        before[n / 4].event.ts,
        before[n / 2].event.ts,
        before[3 * n / 4].event.ts,
    );
    assert!(a < b && b < c);
    let tz = TimeZone::UTC;
    let now = ts("2026-11-23T00:00:00Z");

    let first = form(OPUS, "50", "250", a).build(now, &tz).unwrap();
    let plan = plan_save(SaveKind::New, None, first).unwrap();
    let out = apply(&fx.db, &plan, None);
    let inside_a = before.iter().filter(|r| r.event.ts >= a).count();
    assert_eq!(out.events_changed, inside_a);
    let row1 = user_row(&fx.db, a);

    let second = form(OPUS, "500", "2500", b).build(now, &tz).unwrap();
    let plan = plan_save(SaveKind::PriceChanged, Some(&row1), second).unwrap();
    assert!(matches!(plan, PricePlan::Supersede { .. }));
    let out = apply(&fx.db, &plan, None);
    assert_eq!(
        out.events_changed,
        before.iter().filter(|r| r.event.ts >= b).count()
    );
    let row2 = user_row(&fx.db, b);
    assert_eq!(user_row(&fx.db, a).effective_to, Some(b));

    let after = events(&fx.db, OPUS);
    let mut delta = 0i64;
    for r in &after {
        let (old_cost, old_price) = base[&r.id];
        let expected_row = if r.event.ts < a {
            None
        } else if r.event.ts < b {
            Some(&row1)
        } else {
            Some(&row2)
        };
        match expected_row {
            None => {
                assert_eq!((r.cost, r.price_id), (old_cost, old_price), "{:?}", r.id);
            }
            Some(row) => {
                assert_eq!(r.price_id, row.id);
                assert_eq!(r.cost, tbt_pricing::cost(&r.event, row));
                assert!(r.cost > old_cost || old_cost == Micros::ZERO);
            }
        }
        delta += r.cost.0 - old_cost.0;
    }
    assert!(delta > 0);
    assert_eq!(total_cost(&fx.db), Micros(cost_before.0 + delta));

    let out = ops::with_writable_store(&fx.db, |s| ops::close_row(s, row2.id.unwrap(), c)).unwrap();
    assert_eq!(
        out.events_changed,
        before.iter().filter(|r| r.event.ts >= c).count()
    );
    for r in events(&fx.db, OPUS) {
        if r.event.ts >= c {
            assert_eq!((r.cost, r.price_id), base[&r.id]);
        }
    }
}

#[test]
fn fix_in_place_reprices_whole_row() {
    let fx = fixture(&[]);
    let before = events(&fx.db, OPUS);
    let a = before[before.len() / 2].event.ts;
    let tz = TimeZone::UTC;
    let now = ts("2026-11-23T00:00:00Z");
    let first = form(OPUS, "50", "250", a).build(now, &tz).unwrap();
    apply(&fx.db, &plan_save(SaveKind::New, None, first).unwrap(), None);
    let row = user_row(&fx.db, a);

    let fixed = form(OPUS, "5", "25", a).build(now, &tz).unwrap();
    let plan = plan_save(SaveKind::FixRow, Some(&row), fixed).unwrap();
    let out = apply(&fx.db, &plan, None);
    assert_eq!(
        out.events_changed,
        before.iter().filter(|r| r.event.ts >= a).count()
    );
    let prices = Store::open_read_only(&fx.db).unwrap().model_prices().unwrap();
    assert_eq!(
        prices
            .iter()
            .filter(|p| p.source == tbt_core::PriceSource::User)
            .count(),
        1
    );
    for r in events(&fx.db, OPUS).iter().filter(|r| r.event.ts >= a) {
        assert_eq!(r.price_id, row.id);
    }
}

#[test]
fn adding_price_for_unpriced_model_prices_its_events() {
    let fx = fixture(&[HAIKU]);
    let store = Store::open_read_only(&fx.db).unwrap();
    let unpriced = store.unpriced_models().unwrap();
    let haiku = unpriced.iter().find(|u| u.model == HAIKU).unwrap().clone();
    drop(store);
    let tz = TimeZone::UTC;
    let mut f = PriceForm::for_model(&haiku.provider, &haiku.model, haiku.first_seen, &tz);
    f.input = "1".into();
    f.output = "5".into();
    let price = f.build(ts("2026-11-23T00:00:00Z"), &tz).unwrap();
    let plan = plan_save(SaveKind::New, None, price).unwrap();
    let cost_before = total_cost(&fx.db);
    let out = apply(&fx.db, &plan, Some((&haiku.provider, &haiku.model)));
    assert_eq!(out.events_changed as u64, haiku.events);
    let store = Store::open_read_only(&fx.db).unwrap();
    assert!(
        store
            .unpriced_models()
            .unwrap()
            .iter()
            .all(|u| u.model != HAIKU)
    );
    drop(store);
    assert!(total_cost(&fx.db) > cost_before);
    assert!(events(&fx.db, HAIKU).iter().all(|r| r.price_id.is_some()));
}
