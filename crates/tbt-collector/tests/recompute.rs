use std::collections::BTreeMap;
use std::path::Path;

use tbt_collector::recompute::{
    RecomputeReport, recompute, recompute_for_price, recompute_for_price_paged, recompute_paged,
    recompute_unpriced,
};
use tbt_core::{
    Decimal, HarnessId, Micros, ModelPrice, PriceId, PriceSource, TimeRange, Timestamp, UsageEvent,
};
use tbt_pricing::PriceCatalog;
use tbt_store::{BucketGroup, BucketSize, EventRow, QueryFilter, RecomputeFilter, Store};

const CC: HarnessId = HarnessId::CLAUDE_CODE;
const SONNET: &str = "claude-sonnet-4-5";

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn price(
    provider: &str,
    pattern: &str,
    from: &str,
    to: Option<&str>,
    input: &str,
    source: PriceSource,
) -> ModelPrice {
    ModelPrice {
        id: None,
        provider: provider.into(),
        model_pattern: pattern.into(),
        effective_from: ts(from),
        effective_to: to.map(ts),
        input_per_mtok: input.parse().unwrap(),
        output_per_mtok: "15".parse().unwrap(),
        cache_write_5m_per_mtok: None,
        cache_write_1h_per_mtok: None,
        cache_read_per_mtok: None,
        long_context: None,
        source,
    }
}

fn bundled(pattern: &str, input: &str) -> ModelPrice {
    price(
        "anthropic",
        pattern,
        "2025-01-01T00:00:00Z",
        None,
        input,
        PriceSource::Bundled,
    )
}

fn event(key: &str, session: &str, at: &str, model: &str) -> UsageEvent {
    UsageEvent {
        harness: CC,
        dedup_key: key.into(),
        session_ext_id: session.into(),
        ts: ts(at),
        provider: "anthropic".into(),
        model: model.into(),
        input_tokens: 1000,
        output_tokens: 100,
        cache_write_5m_tokens: 0,
        cache_write_1h_tokens: 0,
        cache_read_tokens: 0,
        reasoning_tokens: 0,
        reported_cost: None,
        project_path: Some("/proj".into()),
        git_branch: None,
        raw: None,
    }
}

fn cost_at(input_per_mtok: i64) -> Micros {
    Micros(1000 * input_per_mtok + 100 * 15)
}

fn ingest(store: &mut Store, source: &str, events: Vec<UsageEvent>) {
    let catalog = PriceCatalog::load(store.model_prices().unwrap()).unwrap();
    let priced: Vec<_> = events.into_iter().map(|e| catalog.apply(e)).collect();
    let mut tx = store.begin_source_tx(&CC, Path::new(source)).unwrap();
    tx.insert_events(&priced).unwrap();
    tx.commit().unwrap();
}

fn all_time() -> QueryFilter {
    QueryFilter::new(
        TimeRange::new(ts("2000-01-01T00:00:00Z"), ts("2100-01-01T00:00:00Z")),
        BucketSize::Hour,
    )
}

fn events(store: &Store) -> BTreeMap<String, EventRow> {
    let mut out = BTreeMap::new();
    for s in store.sessions(&all_time()).unwrap() {
        let detail = store.session_detail(s.id).unwrap().unwrap();
        for e in detail.events {
            let key = format!("{}@{}", s.ext_id, e.ts);
            out.insert(key, e);
        }
    }
    out
}

fn event_at(store: &Store, at: &str) -> EventRow {
    events(store)
        .into_values()
        .find(|e| e.ts == ts(at))
        .unwrap()
}

fn assert_rollups_match_events(store: &Store) {
    let mut by_hour: BTreeMap<(Timestamp, String), (Micros, u64)> = BTreeMap::new();
    for s in store.sessions(&all_time()).unwrap() {
        let detail = store.session_detail(s.id).unwrap().unwrap();
        let cost = detail
            .events
            .iter()
            .fold(Micros::ZERO, |acc, e| acc.saturating_add(e.cost));
        let unpriced = detail
            .events
            .iter()
            .filter(|e| e.price_id.is_none())
            .count() as u64;
        assert_eq!(s.cost, cost, "session {} cost", s.ext_id);
        assert_eq!(s.unpriced_events, unpriced, "session {} unpriced", s.ext_id);
        assert_eq!(s.events, detail.events.len() as u64);
        for e in &detail.events {
            let hour = tbt_core::Hour::containing(e.ts).start();
            let slot = by_hour.entry((hour, e.model.clone())).or_default();
            slot.0 = slot.0.saturating_add(e.cost);
            slot.1 += 1;
        }
    }
    let buckets: BTreeMap<_, _> = store
        .cost_buckets(&all_time(), BucketGroup::Model)
        .unwrap()
        .into_iter()
        .map(|b| ((b.start, b.key), (b.cost, b.events)))
        .collect();
    assert_eq!(buckets, by_hour);
    let kpi = store.kpi_summary(&all_time()).unwrap();
    let unpriced: u64 = store
        .sessions(&all_time())
        .unwrap()
        .iter()
        .map(|s| s.unpriced_events)
        .sum();
    assert_eq!(kpi.unpriced_events, unpriced);
}

fn no_change() -> RecomputeReport {
    RecomputeReport {
        events_scanned: 0,
        events_changed: 0,
        sessions_rebuilt: 0,
        hours_rebuilt: 0,
    }
}

#[test]
fn fixing_a_wrong_price_updates_costs_sessions_and_hours() {
    let mut store = Store::open_in_memory().unwrap();
    let id = store.insert_price(&bundled(SONNET, "30")).unwrap();
    let mut reported = event("k1", "s1", "2026-02-01T10:00:00Z", SONNET);
    reported.reported_cost = Some("0.0123".parse::<Decimal>().unwrap());
    ingest(
        &mut store,
        "/a",
        vec![
            reported,
            event("k2", "s1", "2026-02-01T10:30:00Z", SONNET),
            event("k3", "s2", "2026-02-01T11:15:00Z", SONNET),
            event("o1", "s2", "2026-02-01T11:20:00Z", "claude-opus-4-5"),
        ],
    );
    assert_eq!(event_at(&store, "2026-02-01T10:00:00Z").cost, cost_at(30));
    let kpi_before = store.kpi_summary(&all_time()).unwrap();
    assert_eq!(kpi_before.unpriced_events, 1);

    let mut fixed = store.model_prices().unwrap().remove(0);
    fixed.input_per_mtok = Decimal::from(3);
    store.update_price(&fixed).unwrap();
    let report = recompute_for_price(&mut store, id).unwrap();
    assert_eq!(
        report,
        RecomputeReport {
            events_scanned: 3,
            events_changed: 3,
            sessions_rebuilt: 2,
            hours_rebuilt: 2,
        }
    );

    for e in events(&store).values() {
        if e.model == SONNET {
            assert_eq!(e.cost, cost_at(3));
            assert_eq!(e.price_id, Some(id));
        } else {
            assert_eq!(e.cost, Micros::ZERO);
            assert_eq!(e.price_id, None);
        }
    }
    let k1 = event_at(&store, "2026-02-01T10:00:00Z");
    assert_eq!(
        k1.reported_cost,
        Some(Micros::from_usd("0.0123".parse().unwrap()))
    );
    let s1 = &store
        .sessions(&all_time())
        .unwrap()
        .into_iter()
        .find(|s| s.ext_id == "s1")
        .unwrap();
    assert_eq!(s1.cost, Micros(2 * cost_at(3).0));
    let detail = store.session_detail(s1.id).unwrap().unwrap();
    assert_eq!(
        detail.reported_cost,
        Some(Micros::from_usd("0.0123".parse().unwrap()))
    );
    let kpi = store.kpi_summary(&all_time()).unwrap();
    assert_eq!(kpi.cost, Micros(3 * cost_at(3).0));
    assert_eq!(kpi.events, kpi_before.events);
    assert_eq!(kpi.tokens, kpi_before.tokens);
    assert_eq!(kpi.unpriced_events, 1);
    assert_rollups_match_events(&store);

    let again = recompute_for_price(&mut store, id).unwrap();
    assert_eq!(again.events_changed, 0);
    assert_eq!(again.sessions_rebuilt, 0);
    assert_eq!(again.hours_rebuilt, 0);
}

#[test]
fn recompute_by_filter_fixes_only_matching_events() {
    let mut store = Store::open_in_memory().unwrap();
    let id = store.insert_price(&bundled(SONNET, "30")).unwrap();
    ingest(
        &mut store,
        "/a",
        vec![
            event("k1", "s1", "2026-02-01T10:00:00Z", SONNET),
            event("k2", "s1", "2026-02-02T10:00:00Z", SONNET),
        ],
    );
    let mut fixed = store.model_prices().unwrap().remove(0);
    fixed.input_per_mtok = Decimal::from(3);
    store.update_price(&fixed).unwrap();

    let first_day = RecomputeFilter {
        price_id: Some(id),
        range: Some(TimeRange::new(
            ts("2026-02-01T00:00:00Z"),
            ts("2026-02-02T00:00:00Z"),
        )),
        ..Default::default()
    };
    let report = recompute(&mut store, &first_day).unwrap();
    assert_eq!(report.events_changed, 1);
    assert_eq!(report.hours_rebuilt, 1);
    assert_eq!(event_at(&store, "2026-02-01T10:00:00Z").cost, cost_at(3));
    assert_eq!(event_at(&store, "2026-02-02T10:00:00Z").cost, cost_at(30));
    assert_rollups_match_events(&store);
}

#[test]
fn adding_a_price_for_an_unpriced_model_prices_its_events() {
    let mut store = Store::open_in_memory().unwrap();
    store.insert_price(&bundled(SONNET, "3")).unwrap();
    ingest(
        &mut store,
        "/a",
        vec![
            event("n1", "s1", "2026-03-01T10:00:00Z", "claude-new-1"),
            event("n2", "s1", "2026-03-01T12:00:00Z", "claude-new-1"),
            event("m1", "s2", "2026-03-01T12:30:00Z", "claude-other-1"),
            event("k1", "s2", "2026-03-01T12:40:00Z", SONNET),
        ],
    );
    assert_eq!(store.unpriced_models().unwrap().len(), 2);

    let new = store
        .insert_price(&price(
            "anthropic",
            "claude-new-*",
            "2025-01-01T00:00:00Z",
            None,
            "5",
            PriceSource::User,
        ))
        .unwrap();
    let report = recompute_unpriced(&mut store, Some("anthropic"), Some("claude-new-1")).unwrap();
    assert_eq!(report.events_scanned, 2);
    assert_eq!(report.events_changed, 2);
    assert_eq!(report.sessions_rebuilt, 1);
    assert_eq!(report.hours_rebuilt, 2);
    let n1 = event_at(&store, "2026-03-01T10:00:00Z");
    assert_eq!((n1.cost, n1.price_id), (cost_at(5), Some(new)));
    let unpriced = store.unpriced_models().unwrap();
    assert_eq!(unpriced.len(), 1);
    assert_eq!(unpriced[0].model, "claude-other-1");
    assert_rollups_match_events(&store);

    let other = store.insert_price(&bundled("claude-other-1", "7")).unwrap();
    let report = recompute_for_price(&mut store, other).unwrap();
    assert_eq!(report.events_changed, 1);
    let m1 = event_at(&store, "2026-03-01T12:30:00Z");
    assert_eq!((m1.cost, m1.price_id), (cost_at(7), Some(other)));
    assert!(store.unpriced_models().unwrap().is_empty());
    assert_eq!(store.kpi_summary(&all_time()).unwrap().unpriced_events, 0);
    assert_rollups_match_events(&store);

    assert_eq!(
        recompute_unpriced(&mut store, None, None).unwrap(),
        no_change()
    );
}

#[test]
fn user_override_back_in_time_wins_only_inside_its_window() {
    let mut store = Store::open_in_memory().unwrap();
    let base = store.insert_price(&bundled(SONNET, "3")).unwrap();
    let imported = store
        .insert_price(&price(
            "anthropic",
            "claude-sonnet-4-5-special",
            "2025-01-01T00:00:00Z",
            None,
            "4",
            PriceSource::Imported,
        ))
        .unwrap();
    let mut other_provider = event("x1", "s3", "2026-02-10T10:00:00Z", SONNET);
    other_provider.provider = "bedrock".into();
    ingest(
        &mut store,
        "/a",
        vec![
            event("jan", "s1", "2026-01-15T10:00:00Z", SONNET),
            event("feb", "s1", "2026-02-10T10:00:00Z", SONNET),
            event(
                "feb2",
                "s2",
                "2026-02-20T10:00:00Z",
                "claude-sonnet-4-5-special",
            ),
            event("mar", "s2", "2026-03-10T10:00:00Z", SONNET),
            event("opus", "s2", "2026-02-11T10:00:00Z", "claude-opus-4-5"),
            other_provider,
        ],
    );
    assert_eq!(
        event_at(&store, "2026-02-20T10:00:00Z").price_id,
        Some(imported)
    );

    let user = store
        .insert_price(&price(
            "anthropic",
            "claude-sonnet-4-5*",
            "2026-02-01T00:00:00Z",
            Some("2026-03-01T00:00:00Z"),
            "2",
            PriceSource::User,
        ))
        .unwrap();
    let before = events(&store);
    let report = recompute_for_price(&mut store, user).unwrap();
    assert_eq!(report.events_scanned, 2);
    assert_eq!(report.events_changed, 2);
    assert_eq!(report.sessions_rebuilt, 2);
    assert_eq!(report.hours_rebuilt, 2);

    let after = events(&store);
    for (key, e) in &after {
        let changed = ["2026-02-10T10:00:00Z", "2026-02-20T10:00:00Z"]
            .iter()
            .any(|t| e.ts == ts(t) && e.provider == "anthropic");
        if changed {
            assert_eq!((e.cost, e.price_id), (cost_at(2), Some(user)), "{key}");
        } else {
            assert_eq!(e, &before[key], "{key}");
        }
    }
    assert_eq!(
        event_at(&store, "2026-01-15T10:00:00Z").price_id,
        Some(base)
    );
    assert_eq!(
        event_at(&store, "2026-03-10T10:00:00Z").price_id,
        Some(base)
    );
    assert_rollups_match_events(&store);

    let mut narrowed = store
        .model_prices()
        .unwrap()
        .into_iter()
        .find(|p| p.id == Some(user))
        .unwrap();
    narrowed.effective_to = Some(ts("2026-02-15T00:00:00Z"));
    store.update_price(&narrowed).unwrap();
    let report = recompute_for_price(&mut store, user).unwrap();
    assert_eq!(report.events_changed, 1);
    let feb2 = event_at(&store, "2026-02-20T10:00:00Z");
    assert_eq!((feb2.cost, feb2.price_id), (cost_at(4), Some(imported)));
    let feb = event_at(&store, "2026-02-10T10:00:00Z");
    assert_eq!(feb.price_id, Some(user));
    assert_rollups_match_events(&store);

    store
        .close_price(
            user,
            ts("2026-02-01T00:00:00Z") + jiff::SignedDuration::from_hours(1),
        )
        .unwrap();
    let report = recompute_for_price(&mut store, user).unwrap();
    assert_eq!(report.events_changed, 1);
    let feb = event_at(&store, "2026-02-10T10:00:00Z");
    assert_eq!((feb.cost, feb.price_id), (cost_at(3), Some(base)));
    assert_rollups_match_events(&store);
}

#[test]
fn lower_priority_row_change_does_not_override_a_user_price() {
    let mut store = Store::open_in_memory().unwrap();
    let base = store.insert_price(&bundled(SONNET, "3")).unwrap();
    let user = store
        .insert_price(&price(
            "anthropic",
            SONNET,
            "2025-01-01T00:00:00Z",
            None,
            "1",
            PriceSource::User,
        ))
        .unwrap();
    ingest(
        &mut store,
        "/a",
        vec![event("k1", "s1", "2026-02-01T10:00:00Z", SONNET)],
    );
    let mut edited = store.model_prices().unwrap().remove(0);
    assert_eq!(edited.id, Some(base));
    edited.input_per_mtok = Decimal::from(9);
    store.update_price(&edited).unwrap();
    let report = recompute_for_price(&mut store, base).unwrap();
    assert_eq!(report.events_scanned, 1);
    assert_eq!(report.events_changed, 0);
    let k1 = event_at(&store, "2026-02-01T10:00:00Z");
    assert_eq!((k1.cost, k1.price_id), (cost_at(1), Some(user)));
}

#[test]
fn full_recompute_is_idempotent() {
    let mut store = Store::open_in_memory().unwrap();
    store.insert_price(&bundled(SONNET, "30")).unwrap();
    ingest(
        &mut store,
        "/a",
        vec![
            event("k1", "s1", "2026-02-01T10:00:00Z", SONNET),
            event("k2", "s2", "2026-02-01T11:00:00Z", SONNET),
            event("u1", "s2", "2026-02-01T12:00:00Z", "unknown-model"),
        ],
    );
    assert_eq!(
        recompute(&mut store, &RecomputeFilter::default()).unwrap(),
        RecomputeReport {
            events_scanned: 3,
            ..no_change()
        }
    );

    let mut fixed = store.model_prices().unwrap().remove(0);
    fixed.input_per_mtok = Decimal::from(3);
    store.update_price(&fixed).unwrap();
    let first = recompute(&mut store, &RecomputeFilter::default()).unwrap();
    assert_eq!(first.events_changed, 2);
    let snapshot = events(&store);
    let kpi = store.kpi_summary(&all_time()).unwrap();

    let second = recompute(&mut store, &RecomputeFilter::default()).unwrap();
    assert_eq!(
        second,
        RecomputeReport {
            events_scanned: 3,
            ..no_change()
        }
    );
    assert_eq!(events(&store), snapshot);
    assert_eq!(store.kpi_summary(&all_time()).unwrap(), kpi);
    assert_rollups_match_events(&store);
}

#[test]
fn pages_through_more_events_than_one_page() {
    let mut store = Store::open_in_memory().unwrap();
    let id = store.insert_price(&bundled(SONNET, "30")).unwrap();
    let start = ts("2026-02-01T00:00:00Z");
    let batch: Vec<_> = (0..23)
        .map(|i| {
            let mut e = event(
                &format!("k{i}"),
                &format!("s{}", i % 5),
                "2026-02-01T00:00:00Z",
                SONNET,
            );
            e.ts = start + jiff::SignedDuration::from_mins(37 * i);
            e
        })
        .collect();
    ingest(&mut store, "/a", batch);
    let hours = events(&store)
        .values()
        .map(|e| tbt_core::Hour::containing(e.ts))
        .collect::<std::collections::BTreeSet<_>>()
        .len();

    let mut fixed = store.model_prices().unwrap().remove(0);
    fixed.input_per_mtok = Decimal::from(3);
    store.update_price(&fixed).unwrap();
    let report = recompute_paged(&mut store, &RecomputeFilter::default(), 4).unwrap();
    assert_eq!(
        report,
        RecomputeReport {
            events_scanned: 23,
            events_changed: 23,
            sessions_rebuilt: 5,
            hours_rebuilt: hours,
        }
    );
    assert!(events(&store).values().all(|e| e.cost == cost_at(3)));
    assert_rollups_match_events(&store);

    fixed.input_per_mtok = Decimal::from(4);
    store.update_price(&fixed).unwrap();
    let report = recompute_for_price_paged(&mut store, id, 5).unwrap();
    assert_eq!(report.events_scanned, 23);
    assert_eq!(report.events_changed, 23);
    assert!(events(&store).values().all(|e| e.cost == cost_at(4)));
    assert_rollups_match_events(&store);

    let again = recompute_paged(&mut store, &RecomputeFilter::default(), 23).unwrap();
    assert_eq!(again.events_scanned, 23);
    assert_eq!(again.events_changed, 0);
}

#[test]
fn unknown_price_id_is_an_error() {
    let mut store = Store::open_in_memory().unwrap();
    assert!(recompute_for_price(&mut store, PriceId(42)).is_err());
}
