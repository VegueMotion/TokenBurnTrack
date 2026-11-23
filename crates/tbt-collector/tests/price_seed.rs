use std::path::Path;

use tbt_collector::price_seed::{SeedReport, sync_bundled_prices, sync_prices};
use tbt_collector::recompute::recompute_for_price;
use tbt_core::{Decimal, HarnessId, Micros, ModelPrice, PriceSource, Timestamp, UsageEvent};
use tbt_pricing::{PriceCatalog, bundled_prices};
use tbt_store::Store;

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn price(pattern: &str, from: &str, input: &str, source: PriceSource) -> ModelPrice {
    ModelPrice {
        id: None,
        provider: "anthropic".into(),
        model_pattern: pattern.into(),
        effective_from: ts(from),
        effective_to: None,
        input_per_mtok: input.parse().unwrap(),
        output_per_mtok: "15".parse().unwrap(),
        cache_write_5m_per_mtok: None,
        cache_write_1h_per_mtok: None,
        cache_read_per_mtok: Some("0.30".parse().unwrap()),
        long_context: None,
        source,
    }
}

fn bundled(pattern: &str, from: &str, input: &str) -> ModelPrice {
    price(pattern, from, input, PriceSource::Bundled)
}

fn stored(store: &Store) -> Vec<ModelPrice> {
    store.model_prices().unwrap()
}

#[test]
fn seeds_an_empty_db_with_all_bundled_rows() {
    let mut store = Store::open_in_memory().unwrap();
    let expected = bundled_prices().unwrap();
    assert!(!expected.is_empty());

    let report = sync_bundled_prices(&mut store).unwrap();
    assert_eq!(report.inserted, expected.len());
    assert_eq!(report.added.len(), expected.len());
    assert_eq!(report.unchanged, 0);
    assert!(report.corrected.is_empty());

    let rows = stored(&store);
    assert_eq!(rows.len(), expected.len());
    for (row, want) in rows.iter().zip(&expected) {
        assert_eq!(row.source, PriceSource::Bundled);
        assert!(report.added.contains(&row.id.unwrap()));
        assert_eq!(
            &ModelPrice {
                id: None,
                ..row.clone()
            },
            want
        );
    }
    PriceCatalog::load(rows).unwrap();
}

#[test]
fn reseeding_is_a_noop() {
    let mut store = Store::open_in_memory().unwrap();
    sync_bundled_prices(&mut store).unwrap();
    let before = stored(&store);

    let report = sync_bundled_prices(&mut store).unwrap();
    assert_eq!(
        report,
        SeedReport {
            inserted: 0,
            unchanged: before.len(),
            added: Vec::new(),
            corrected: Vec::new(),
        }
    );
    assert_eq!(stored(&store), before);
}

#[test]
fn new_bundled_rows_are_added_on_the_next_sync() {
    let mut store = Store::open_in_memory().unwrap();
    let a = bundled("claude-a", "2025-01-01T00:00:00Z", "3");
    sync_prices(&mut store, std::slice::from_ref(&a)).unwrap();

    let mut a_closed = a.clone();
    a_closed.effective_to = Some(ts("2026-06-01T00:00:00Z"));
    let a_new = bundled("claude-a", "2026-06-01T00:00:00Z", "2");
    let b = bundled("claude-b*", "2025-01-01T00:00:00Z", "5");
    let report = sync_prices(&mut store, &[a_closed.clone(), a_new.clone(), b.clone()]).unwrap();
    assert_eq!(report.inserted, 2);
    assert_eq!(report.added.len(), 2);
    assert_eq!(report.unchanged, 0);
    assert_eq!(report.corrected.len(), 1);

    let rows = stored(&store);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].effective_to, a_closed.effective_to);
    assert_eq!(report.corrected, vec![rows[0].id.unwrap()]);
    assert_eq!(report.added, vec![rows[1].id.unwrap(), rows[2].id.unwrap()]);

    let again = sync_prices(&mut store, &[a_closed, a_new, b]).unwrap();
    assert_eq!(again.inserted, 0);
    assert_eq!(again.unchanged, 3);
    assert!(again.corrected.is_empty());
    assert_eq!(stored(&store), rows);
}

#[test]
fn corrected_bundled_price_is_updated_reported_and_recomputable() {
    let mut store = Store::open_in_memory().unwrap();
    sync_prices(
        &mut store,
        &[bundled("claude-a", "2025-01-01T00:00:00Z", "30")],
    )
    .unwrap();
    let id = stored(&store)[0].id.unwrap();

    let catalog = PriceCatalog::load(stored(&store)).unwrap();
    let event = UsageEvent {
        harness: HarnessId::CLAUDE_CODE,
        dedup_key: "k1".into(),
        session_ext_id: "s1".into(),
        ts: ts("2026-02-01T10:00:00Z"),
        provider: "anthropic".into(),
        model: "claude-a".into(),
        input_tokens: 1000,
        output_tokens: 0,
        cache_write_5m_tokens: 0,
        cache_write_1h_tokens: 0,
        cache_read_tokens: 0,
        reasoning_tokens: 0,
        reported_cost: None,
        project_path: None,
        git_branch: None,
        raw: None,
    };
    let mut tx = store
        .begin_source_tx(&HarnessId::CLAUDE_CODE, Path::new("/a"))
        .unwrap();
    tx.insert_events(&[catalog.apply(event)]).unwrap();
    tx.commit().unwrap();

    let fixed = bundled("claude-a", "2025-01-01T00:00:00Z", "3");
    let report = sync_prices(&mut store, std::slice::from_ref(&fixed)).unwrap();
    assert_eq!(report.corrected, vec![id]);
    assert_eq!(report.inserted, 0);
    assert_eq!(report.unchanged, 0);
    let rows = stored(&store);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, Some(id));
    assert_eq!(rows[0].input_per_mtok, Decimal::from(3));

    for corrected in &report.corrected {
        recompute_for_price(&mut store, *corrected).unwrap();
    }
    let kpi = store
        .kpi_summary(&tbt_store::QueryFilter::new(
            tbt_core::TimeRange::new(ts("2026-01-01T00:00:00Z"), ts("2027-01-01T00:00:00Z")),
            tbt_store::BucketSize::Hour,
        ))
        .unwrap();
    assert_eq!(kpi.cost, Micros(3000));

    let again = sync_prices(&mut store, &[fixed]).unwrap();
    assert!(again.corrected.is_empty());
    assert_eq!(again.unchanged, 1);
}

#[test]
fn imported_and_user_rows_are_never_touched() {
    let mut store = Store::open_in_memory().unwrap();
    store
        .insert_price(&price(
            "claude-a",
            "2025-01-01T00:00:00Z",
            "1",
            PriceSource::User,
        ))
        .unwrap();
    store
        .insert_price(&price(
            "claude-a",
            "2025-01-01T00:00:00Z",
            "2",
            PriceSource::Imported,
        ))
        .unwrap();
    let others = stored(&store);

    let report = sync_prices(
        &mut store,
        &[bundled("claude-a", "2025-01-01T00:00:00Z", "3")],
    )
    .unwrap();
    assert_eq!(report.inserted, 1);
    assert!(report.corrected.is_empty());

    let report = sync_prices(
        &mut store,
        &[bundled("claude-a", "2025-01-01T00:00:00Z", "4")],
    )
    .unwrap();
    assert_eq!(report.inserted, 0);
    assert_eq!(report.corrected.len(), 1);

    let rows = stored(&store);
    assert_eq!(rows.len(), 3);
    assert_eq!(&rows[..2], &others[..]);
    assert_eq!(rows[2].source, PriceSource::Bundled);
    assert_eq!(rows[2].input_per_mtok, Decimal::from(4));
    assert_eq!(report.corrected, vec![rows[2].id.unwrap()]);
}

#[test]
fn rows_passed_in_are_stored_as_bundled() {
    let mut store = Store::open_in_memory().unwrap();
    let report = sync_prices(
        &mut store,
        &[price(
            "claude-a",
            "2025-01-01T00:00:00Z",
            "3",
            PriceSource::User,
        )],
    )
    .unwrap();
    assert_eq!(report.inserted, 1);
    assert_eq!(stored(&store)[0].source, PriceSource::Bundled);
    assert_eq!(
        sync_prices(
            &mut store,
            &[bundled("claude-a", "2025-01-01T00:00:00Z", "3")]
        )
        .unwrap()
        .unchanged,
        1
    );
}

#[test]
fn empty_bundled_list_on_empty_db_inserts_nothing() {
    let mut store = Store::open_in_memory().unwrap();
    assert_eq!(sync_prices(&mut store, &[]).unwrap(), SeedReport::default());
    assert!(stored(&store).is_empty());
}
