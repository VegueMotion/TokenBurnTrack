use tbt_core::{ModelPrice, PriceId, PriceSource};
use tbt_store::Store;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeedReport {
    pub inserted: usize,
    pub unchanged: usize,
    pub added: Vec<PriceId>,
    pub corrected: Vec<PriceId>,
}

pub fn sync_bundled_prices(store: &mut Store) -> anyhow::Result<SeedReport> {
    sync_prices(store, &tbt_pricing::bundled_prices()?)
}

pub fn sync_prices(store: &mut Store, bundled: &[ModelPrice]) -> anyhow::Result<SeedReport> {
    let bundled: Vec<ModelPrice> = bundled
        .iter()
        .map(|p| ModelPrice {
            id: None,
            source: PriceSource::Bundled,
            ..p.clone()
        })
        .collect();
    let mut report = SeedReport::default();
    let mut existing: Vec<ModelPrice> = store
        .model_prices()?
        .into_iter()
        .filter(|p| p.source == PriceSource::Bundled)
        .collect();

    if existing.is_empty() && store.seed_prices_if_empty(&bundled)? > 0 {
        let seeded = store.model_prices()?;
        report.inserted = seeded.len();
        report.added = seeded.iter().filter_map(|p| p.id).collect();
        return Ok(report);
    }

    for new in bundled {
        match existing.iter().find(|old| same_key(old, &new)) {
            None => {
                let id = store.insert_price(&new)?;
                report.inserted += 1;
                report.added.push(id);
                existing.push(ModelPrice {
                    id: Some(id),
                    ..new
                });
            }
            Some(old)
                if ModelPrice {
                    id: old.id,
                    ..new.clone()
                } == *old =>
            {
                report.unchanged += 1;
            }
            Some(old) => {
                let id = old
                    .id
                    .ok_or_else(|| anyhow::anyhow!("stored price without id"))?;
                store.update_price(&ModelPrice {
                    id: Some(id),
                    ..new
                })?;
                report.corrected.push(id);
            }
        }
    }
    Ok(report)
}

fn same_key(a: &ModelPrice, b: &ModelPrice) -> bool {
    a.provider == b.provider
        && a.model_pattern == b.model_pattern
        && a.effective_from == b.effective_from
}
