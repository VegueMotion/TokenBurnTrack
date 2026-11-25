mod mock;
mod store;

use jiff::Timestamp;
use tbt_core::SessionId;
use tbt_store::{
    BucketGroup, CostBucket, HarnessStatus, IngestRunSummary, KpiSummary, ModelStats, ProjectStats,
    QueryFilter, Result, SessionDetail, SessionRow, UnpricedModel,
};

pub use mock::MockData;
pub use store::StoreData;

pub trait DataSource: Send + Sync + 'static {
    fn kpi_summary(&self, filter: &QueryFilter) -> Result<KpiSummary>;
    fn cost_buckets(&self, filter: &QueryFilter, group: BucketGroup) -> Result<Vec<CostBucket>>;
    fn sessions(&self, filter: &QueryFilter) -> Result<Vec<SessionRow>>;
    fn session_detail(&self, id: SessionId) -> Result<Option<SessionDetail>>;
    fn model_stats(&self, filter: &QueryFilter) -> Result<Vec<ModelStats>>;
    fn project_stats(&self, filter: &QueryFilter) -> Result<Vec<ProjectStats>>;
    fn harness_status(&self) -> Result<Vec<HarnessStatus>>;
    fn unpriced_models(&self) -> Result<Vec<UnpricedModel>>;
    fn ingest_runs(&self, limit: usize) -> Result<Vec<IngestRunSummary>>;

    fn has_data(&self) -> Result<bool> {
        Ok(self.harness_status()?.iter().any(|h| h.events > 0))
    }

    fn session_row(&self, id: SessionId) -> Result<Option<SessionRow>> {
        Ok(self.session_detail(id)?.map(|d| d.session))
    }

    fn db_path(&self) -> Option<std::path::PathBuf> {
        None
    }
}

pub const NO_DATA_MESSAGE: &str = "No data yet — run Sync or `tbt-daemon --once`";

pub fn now() -> Timestamp {
    if cfg!(debug_assertions)
        && let Some(ts) = std::env::var("TBT_NOW").ok().and_then(|s| s.parse().ok())
    {
        return ts;
    }
    Timestamp::now()
}
