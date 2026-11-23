use tbt_store::{RecomputeFilter, Store};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecomputeReport {
    pub events_scanned: usize,
    pub events_changed: usize,
    pub sessions_rebuilt: usize,
    pub hours_rebuilt: usize,
}

pub fn recompute(store: &mut Store, filter: &RecomputeFilter) -> anyhow::Result<RecomputeReport> {
    let _ = (store, filter);
    todo!()
}
