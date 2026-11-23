mod mock;

use std::sync::Mutex;

use tbt_core::SessionId;
use tbt_store::{
    BucketGroup, CostBucket, HarnessStatus, HeatCell, IngestRunSummary, KpiSummary, ModelStats,
    ProjectStats, QueryFilter, Result, SessionDetail, SessionRow, Store, UnpricedModel,
};

pub use mock::MockData;

pub trait DataSource: Send + Sync + 'static {
    fn kpi_summary(&self, filter: &QueryFilter) -> Result<KpiSummary>;
    fn cost_buckets(&self, filter: &QueryFilter, group: BucketGroup) -> Result<Vec<CostBucket>>;
    fn heatmap(&self, filter: &QueryFilter) -> Result<Vec<HeatCell>>;
    fn sessions(&self, filter: &QueryFilter) -> Result<Vec<SessionRow>>;
    fn session_detail(&self, id: SessionId) -> Result<Option<SessionDetail>>;
    fn model_stats(&self, filter: &QueryFilter) -> Result<Vec<ModelStats>>;
    fn project_stats(&self, filter: &QueryFilter) -> Result<Vec<ProjectStats>>;
    fn harness_status(&self) -> Result<Vec<HarnessStatus>>;
    fn unpriced_models(&self) -> Result<Vec<UnpricedModel>>;
    fn ingest_runs(&self, limit: usize) -> Result<Vec<IngestRunSummary>>;
}

impl DataSource for Mutex<Store> {
    fn kpi_summary(&self, filter: &QueryFilter) -> Result<KpiSummary> {
        lock(self).kpi_summary(filter)
    }

    fn cost_buckets(&self, filter: &QueryFilter, group: BucketGroup) -> Result<Vec<CostBucket>> {
        lock(self).cost_buckets(filter, group)
    }

    fn heatmap(&self, filter: &QueryFilter) -> Result<Vec<HeatCell>> {
        lock(self).heatmap(filter)
    }

    fn sessions(&self, filter: &QueryFilter) -> Result<Vec<SessionRow>> {
        lock(self).sessions(filter)
    }

    fn session_detail(&self, id: SessionId) -> Result<Option<SessionDetail>> {
        lock(self).session_detail(id)
    }

    fn model_stats(&self, filter: &QueryFilter) -> Result<Vec<ModelStats>> {
        lock(self).model_stats(filter)
    }

    fn project_stats(&self, filter: &QueryFilter) -> Result<Vec<ProjectStats>> {
        lock(self).project_stats(filter)
    }

    fn harness_status(&self) -> Result<Vec<HarnessStatus>> {
        lock(self).harness_status()
    }

    fn unpriced_models(&self) -> Result<Vec<UnpricedModel>> {
        lock(self).unpriced_models()
    }

    fn ingest_runs(&self, limit: usize) -> Result<Vec<IngestRunSummary>> {
        lock(self).ingest_runs(limit)
    }
}

fn lock(store: &Mutex<Store>) -> std::sync::MutexGuard<'_, Store> {
    store.lock().unwrap_or_else(|e| e.into_inner())
}
