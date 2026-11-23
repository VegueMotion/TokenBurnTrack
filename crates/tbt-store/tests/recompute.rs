mod common;

use common::*;
use tbt_core::{Hour, LongContextTier, Micros, PriceId, PriceSource, TimeRange};
use tbt_store::{CostUpdate, RecomputeFilter, StoreError};

fn seed(db: &mut Db) -> PriceId {
    let p = db
        .store
        .insert_price(&price("claude-sonnet-4-5", "3", Some("0.3")))
        .unwrap();
    let mut opus = event("o1", "s2", "2026-02-01T09:00:00Z");
    opus.model = "claude-opus-4-5".into();
    let mut raw = event("k3", "s1", "2026-02-01T11:00:00Z");
    raw.raw = Some(tbt_core::RawValue::from_string(r#"{"x":1}"#.into()).unwrap());
    db.ingest(
        "/a",
        &[
            priced(event("k1", "s1", "2026-02-01T10:00:00Z"), 100, Some(p.0)),
            priced(event("k2", "s1", "2026-02-01T10:10:00Z"), 100, Some(p.0)),
            priced(raw, 100, Some(p.0)),
            priced(event("u1", "s2", "2026-02-01T10:20:00Z"), 0, None),
            priced(opus, 0, None),
        ],
    );
    p
}

#[test]
fn events_for_recompute_pages_and_ands_filters() {
    let mut db = Db::new();
    let p = seed(&mut db);
    let all = RecomputeFilter::default();
    let mut seen = Vec::new();
    let mut after = None;
    loop {
        let page = db.store.events_for_recompute(&all, after, 2).unwrap();
        if page.is_empty() {
            break;
        }
        assert!(page.len() <= 2);
        after = Some(page.last().unwrap().id);
        seen.extend(page);
    }
    assert_eq!(seen.len(), 5);
    assert!(seen.windows(2).all(|w| w[0].id < w[1].id));
    assert!(seen.iter().all(|r| r.event.raw.is_none()));
    assert_eq!(seen[0].event.session_ext_id, "s1");
    assert_eq!(seen[0].event.tokens().cache_write_1h, 5);

    let by_price = RecomputeFilter {
        price_id: Some(p),
        ..Default::default()
    };
    assert_eq!(
        db.store
            .events_for_recompute(&by_price, None, 100)
            .unwrap()
            .len(),
        3
    );
    let unpriced_opus = RecomputeFilter {
        unpriced_only: true,
        model: Some("claude-opus-4-5".into()),
        provider: Some("anthropic".into()),
        ..Default::default()
    };
    let rows = db
        .store
        .events_for_recompute(&unpriced_opus, None, 100)
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].event.dedup_key, "o1");
    let ranged = RecomputeFilter {
        price_id: Some(p),
        range: Some(TimeRange::new(
            ts("2026-02-01T10:05:00Z"),
            ts("2026-02-01T11:00:00Z"),
        )),
        ..Default::default()
    };
    let rows = db.store.events_for_recompute(&ranged, None, 100).unwrap();
    assert_eq!(
        rows.iter()
            .map(|r| r.event.dedup_key.as_str())
            .collect::<Vec<_>>(),
        ["k2"]
    );
}

#[test]
fn recompute_rebuilds_session_totals_and_rollups() {
    let mut db = Db::new();
    seed(&mut db);
    let new_price = db
        .store
        .insert_price(&price("claude-opus-4-5", "5", None))
        .unwrap();
    let raw = db.raw();
    let sessions_before = sessions(&raw);

    let rows = db
        .store
        .events_for_recompute(&RecomputeFilter::default(), None, 100)
        .unwrap();
    let updates: Vec<CostUpdate> = rows
        .iter()
        .map(|r| match r.event.model.as_str() {
            "claude-opus-4-5" => CostUpdate {
                event: r.id,
                cost: Micros(777),
                price_id: Some(new_price),
            },
            _ => CostUpdate {
                event: r.id,
                cost: if r.price_id.is_some() {
                    Micros(250)
                } else {
                    r.cost
                },
                price_id: r.price_id,
            },
        })
        .collect();
    let mut tx = db.store.begin_recompute_tx().unwrap();
    let affected = tx.update_event_costs(&updates).unwrap();
    assert_eq!(affected.sessions.len(), 2);
    assert_eq!(
        affected.hours,
        [
            Hour::containing(ts("2026-02-01T09:00:00Z")),
            Hour::containing(ts("2026-02-01T10:00:00Z")),
            Hour::containing(ts("2026-02-01T11:00:00Z")),
        ]
    );
    tx.rebuild_session_totals(&affected.sessions).unwrap();
    tx.rebuild_hourly(&affected.hours).unwrap();
    tx.commit().unwrap();

    assert_eq!(hourly(&raw), hourly_from_events(&raw));
    let after = sessions(&raw);
    assert_eq!(after[0].cost, 750);
    assert_eq!(after[1].cost, 777);
    assert_eq!(after[1].unpriced, 1);
    assert_eq!(after[0].active_seconds, sessions_before[0].active_seconds);
    assert_eq!(after[0].events, sessions_before[0].events);

    let mut tx = db.store.begin_recompute_tx().unwrap();
    let again = tx.update_event_costs(&updates).unwrap();
    assert!(again.sessions.is_empty() && again.hours.is_empty());
    drop(tx);

    assert_eq!(db.store.unpriced_models().unwrap()[0].events, 1);
}

#[test]
fn dropped_recompute_tx_rolls_back() {
    let mut db = Db::new();
    seed(&mut db);
    let raw = db.raw();
    let before = (hourly(&raw), sessions(&raw));
    let rows = db
        .store
        .events_for_recompute(&RecomputeFilter::default(), None, 100)
        .unwrap();
    {
        let mut tx = db.store.begin_recompute_tx().unwrap();
        let a = tx
            .update_event_costs(&[CostUpdate {
                event: rows[0].id,
                cost: Micros(1),
                price_id: None,
            }])
            .unwrap();
        tx.rebuild_session_totals(&a.sessions).unwrap();
        tx.rebuild_hourly(&a.hours).unwrap();
    }
    assert_eq!((hourly(&raw), sessions(&raw)), before);
}

#[test]
fn drop_raw_json() {
    let mut db = Db::new();
    seed(&mut db);
    let now = ts("2026-02-11T12:00:00Z");
    assert_eq!(db.store.drop_raw_json_older_than(30, now).unwrap(), 0);
    assert_eq!(db.store.drop_raw_json_older_than(10, now).unwrap(), 1);
    assert_eq!(db.store.drop_raw_json_older_than(10, now).unwrap(), 0);
}

#[test]
fn price_rows() {
    let mut store = tbt_store::Store::open_in_memory().unwrap();
    let mut a = price("claude-sonnet-4-5*", "3", Some("0.3"));
    a.cache_write_5m_per_mtok = Some("3.75".parse().unwrap());
    a.long_context = Some(LongContextTier {
        threshold_tokens: 200_000,
        input: "2".parse().unwrap(),
        output: "1.5".parse().unwrap(),
        cache_write: "2".parse().unwrap(),
        cache_read: "2".parse().unwrap(),
    });
    assert_eq!(
        store
            .seed_prices_if_empty(&[a.clone(), price("x", "1", None)])
            .unwrap(),
        2
    );
    assert_eq!(store.seed_prices_if_empty(&[a.clone()]).unwrap(), 0);

    let rows = store.model_prices().unwrap();
    assert_eq!(rows.len(), 2);
    let id = rows[0].id.unwrap();
    let mut expect = a.clone();
    expect.id = Some(id);
    assert_eq!(rows[0], expect);

    store.close_price(id, ts("2026-06-01T00:00:00Z")).unwrap();
    let mut b = a.clone();
    b.effective_from = ts("2026-06-01T00:00:00Z");
    b.source = PriceSource::Imported;
    let id_b = store.insert_price(&b).unwrap();
    assert_ne!(id_b, id);

    let mut edited = store.model_prices().unwrap().remove(0);
    assert_eq!(edited.effective_to, Some(ts("2026-06-01T00:00:00Z")));
    edited.input_per_mtok = "4".parse().unwrap();
    edited.source = PriceSource::User;
    store.update_price(&edited).unwrap();
    assert_eq!(store.model_prices().unwrap()[0], edited);
    assert_eq!(store.model_prices().unwrap().len(), 3);

    assert!(matches!(
        store.close_price(PriceId(999), ts("2026-06-01T00:00:00Z")),
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        store.update_price(&a),
        Err(StoreError::NotFound(_))
    ));
}
