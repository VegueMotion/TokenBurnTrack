mod common;

use std::collections::BTreeMap;

use common::*;
use proptest::prelude::*;
use tbt_core::PricedEvent;

#[derive(Debug, Clone)]
struct Ev {
    session: u8,
    secs: i64,
    model: u8,
    input: u64,
    cache_read: u64,
    cost: i64,
    priced: bool,
}

fn ev() -> impl Strategy<Value = Ev> {
    (
        0u8..3,
        0i64..36_000,
        0u8..2,
        0u64..1000,
        0u64..5000,
        0i64..10_000,
        any::<bool>(),
    )
        .prop_map(
            |(session, secs, model, input, cache_read, cost, priced)| Ev {
                session,
                secs,
                model,
                input,
                cache_read,
                cost,
                priced,
            },
        )
}

fn to_event(i: usize, e: &Ev) -> PricedEvent {
    let base = ts("2025-06-01T00:00:00Z");
    let at = base
        .checked_add(jiff::SignedDuration::from_secs(e.secs))
        .unwrap();
    let mut u = event(
        &format!("k{i}"),
        &format!("s{}", e.session),
        &at.to_string(),
    );
    u.model = ["claude-sonnet-4-5", "claude-opus-4-5"][usize::from(e.model)].into();
    u.project_path = match e.session {
        0 => None,
        1 => Some("/p/a".into()),
        _ => Some("/p/b".into()),
    };
    u.input_tokens = e.input;
    u.cache_read_tokens = e.cache_read;
    priced(u, if e.priced { e.cost } else { 0 }, e.priced.then_some(1))
}

fn scenario() -> impl Strategy<Value = (Vec<Ev>, Vec<usize>, Vec<usize>)> {
    prop::collection::vec(ev(), 1..40).prop_flat_map(|evs| {
        let n = evs.len();
        let order = prop::collection::vec(0..n, 0..20)
            .prop_map(move |dups| (0..n).chain(dups).collect::<Vec<_>>())
            .prop_shuffle();
        let cuts = prop::collection::vec(1usize..10, 1..10);
        (Just(evs), order, cuts)
    })
}

fn expected_active_seconds(evs: &[Ev]) -> BTreeMap<String, i64> {
    let mut by_session: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    for e in evs {
        by_session
            .entry(format!("s{}", e.session))
            .or_default()
            .push(e.secs);
    }
    by_session
        .into_iter()
        .map(|(s, mut t)| {
            t.sort_unstable();
            let active = t
                .windows(2)
                .map(|w| w[1] - w[0])
                .filter(|g| *g <= 1800)
                .sum();
            (s, active)
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn batch_order_and_duplicates_do_not_change_totals((evs, order, cuts) in scenario()) {
        let events: Vec<PricedEvent> = evs.iter().enumerate().map(|(i, e)| to_event(i, e)).collect();

        let mut reference = Db::new();
        reference.ingest("/ref", &events);
        let ref_raw = reference.raw();

        let mut db = Db::new();
        let shuffled: Vec<PricedEvent> = order.iter().map(|i| events[*i].clone()).collect();
        let mut rest = shuffled.as_slice();
        let mut inserted = 0;
        for size in cuts.iter().cycle() {
            if rest.is_empty() {
                break;
            }
            let (batch, tail) = rest.split_at((*size).min(rest.len()));
            inserted += db.ingest(&format!("/src{}", size % 3), batch);
            rest = tail;
        }
        let raw = db.raw();

        prop_assert_eq!(inserted, evs.len());
        prop_assert_eq!(count(&raw, "usage_event"), evs.len() as i64);
        prop_assert_eq!(sessions(&raw), sessions(&ref_raw));
        prop_assert_eq!(hourly(&raw), hourly(&ref_raw));
        prop_assert_eq!(hourly(&raw), hourly_from_events(&raw));

        let active: BTreeMap<String, i64> = sessions(&raw)
            .into_iter()
            .map(|s| (s.ext_id, s.active_seconds))
            .collect();
        prop_assert_eq!(active, expected_active_seconds(&evs));
        let total_cost: i64 = sessions(&raw).iter().map(|s| s.cost).sum();
        let expected: i64 = events.iter().map(|e| e.cost.0).sum();
        prop_assert_eq!(total_cost, expected);
    }
}
