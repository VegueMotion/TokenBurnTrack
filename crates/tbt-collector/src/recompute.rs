use std::collections::{BTreeSet, HashSet};

use anyhow::Context;
use tbt_core::{EventId, Hour, ModelPrice, PriceId, SessionId, TimeRange, Timestamp};
use tbt_pricing::PriceCatalog;
use tbt_store::{CostUpdate, RecomputeFilter, RecomputeRow, Store};

pub const PAGE_SIZE: usize = 1000;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecomputeReport {
    pub events_scanned: usize,
    pub events_changed: usize,
    pub sessions_rebuilt: usize,
    pub hours_rebuilt: usize,
}

pub fn recompute(store: &mut Store, filter: &RecomputeFilter) -> anyhow::Result<RecomputeReport> {
    recompute_paged(store, filter, PAGE_SIZE)
}

pub fn recompute_paged(
    store: &mut Store,
    filter: &RecomputeFilter,
    page_size: usize,
) -> anyhow::Result<RecomputeReport> {
    let catalog = load_catalog(store)?;
    let mut run = Run::default();
    run.pass(store, &catalog, filter, page_size, |_| true)?;
    Ok(run.report())
}

pub fn recompute_for_price(
    store: &mut Store,
    price_id: PriceId,
) -> anyhow::Result<RecomputeReport> {
    recompute_for_price_paged(store, price_id, PAGE_SIZE)
}

pub fn recompute_for_price_paged(
    store: &mut Store,
    price_id: PriceId,
    page_size: usize,
) -> anyhow::Result<RecomputeReport> {
    let prices = store.model_prices()?;
    let row = prices
        .iter()
        .find(|p| p.id == Some(price_id))
        .cloned()
        .with_context(|| format!("price row {price_id} not found"))?;
    let catalog = PriceCatalog::load(prices)?;
    let mut run = Run::default();

    let mut done = HashSet::new();
    let by_id = RecomputeFilter {
        price_id: Some(price_id),
        ..Default::default()
    };
    run.pass(store, &catalog, &by_id, page_size, |r| {
        done.insert(r.id);
        true
    })?;

    let only_row = PriceCatalog::load(vec![row.clone()])?;
    let in_window = window_filter(&row);
    run.pass(store, &catalog, &in_window, page_size, |r| {
        !done.contains(&r.id)
            && only_row
                .price_for(&r.event.provider, &r.event.model, r.event.ts)
                .price()
                .is_some()
    })?;
    Ok(run.report())
}

pub fn recompute_unpriced(
    store: &mut Store,
    provider: Option<&str>,
    model: Option<&str>,
) -> anyhow::Result<RecomputeReport> {
    let filter = RecomputeFilter {
        provider: provider.map(str::to_owned),
        model: model.map(str::to_owned),
        unpriced_only: true,
        ..Default::default()
    };
    recompute(store, &filter)
}

fn load_catalog(store: &Store) -> anyhow::Result<PriceCatalog> {
    Ok(PriceCatalog::load(store.model_prices()?)?)
}

fn window_filter(row: &ModelPrice) -> RecomputeFilter {
    RecomputeFilter {
        range: Some(TimeRange::new(
            row.effective_from,
            row.effective_to.unwrap_or(Timestamp::MAX),
        )),
        provider: Some(row.provider.clone()),
        model: (!row.is_glob()).then(|| row.model_pattern.clone()),
        ..Default::default()
    }
}

#[derive(Default)]
struct Run {
    scanned: usize,
    changed: usize,
    sessions: BTreeSet<SessionId>,
    hours: BTreeSet<Hour>,
}

impl Run {
    fn pass(
        &mut self,
        store: &mut Store,
        catalog: &PriceCatalog,
        filter: &RecomputeFilter,
        page_size: usize,
        mut select: impl FnMut(&RecomputeRow) -> bool,
    ) -> anyhow::Result<()> {
        let page_size = page_size.max(1);
        let mut after: Option<EventId> = None;
        loop {
            let page = store.events_for_recompute(filter, after, page_size)?;
            let Some(last) = page.last() else {
                break;
            };
            after = Some(last.id);
            let full = page.len() == page_size;

            let mut updates = Vec::new();
            for row in page {
                if !select(&row) {
                    continue;
                }
                self.scanned += 1;
                let (id, old_cost, old_price) = (row.id, row.cost, row.price_id);
                let priced = catalog.apply(row.event);
                if priced.cost != old_cost || priced.price_id != old_price {
                    updates.push(CostUpdate {
                        event: id,
                        cost: priced.cost,
                        price_id: priced.price_id,
                    });
                }
            }

            if !updates.is_empty() {
                let mut tx = store.begin_recompute_tx()?;
                let affected = tx.update_event_costs(&updates)?;
                tx.rebuild_session_totals(&affected.sessions)?;
                tx.rebuild_hourly(&affected.hours)?;
                tx.commit()?;
                self.changed += updates.len();
                self.sessions.extend(affected.sessions);
                self.hours.extend(affected.hours);
            }

            if !full {
                break;
            }
        }
        Ok(())
    }

    fn report(self) -> RecomputeReport {
        RecomputeReport {
            events_scanned: self.scanned,
            events_changed: self.changed,
            sessions_rebuilt: self.sessions.len(),
            hours_rebuilt: self.hours.len(),
        }
    }
}
