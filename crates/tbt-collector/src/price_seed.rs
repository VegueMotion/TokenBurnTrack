use tbt_core::ModelPrice;
use tbt_store::Store;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeedReport {
    pub inserted: usize,
    pub unchanged: usize,
}

pub fn sync_bundled_prices(store: &mut Store) -> anyhow::Result<SeedReport> {
    let _ = store;
    todo!()
}

pub fn sync_prices(store: &mut Store, bundled: &[ModelPrice]) -> anyhow::Result<SeedReport> {
    let _ = (store, bundled);
    todo!()
}
