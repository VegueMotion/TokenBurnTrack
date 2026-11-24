use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use tbt_core::SessionId;
use tbt_store::{
    BucketGroup, CostBucket, HarnessStatus, HeatCell, IngestRunSummary, KpiSummary, ModelStats,
    ProjectStats, QueryFilter, Result, SessionDetail, SessionRow, Store, StoreError, UnpricedModel,
};

use super::DataSource;

const MAX_IDLE: usize = 4;

pub struct StoreData {
    db: Option<PathBuf>,
    idle: Mutex<Vec<Store>>,
}

impl StoreData {
    pub fn new(db: PathBuf) -> StoreData {
        StoreData {
            db: Some(db),
            idle: Mutex::new(Vec::new()),
        }
    }

    pub fn from_data_dir(dir: Option<PathBuf>) -> StoreData {
        StoreData {
            db: dir.map(|d| tbt_store::paths::db_path(&d)),
            idle: Mutex::new(Vec::new()),
        }
    }

    pub fn db_path(&self) -> Option<&Path> {
        self.db.as_deref()
    }

    fn idle(&self) -> MutexGuard<'_, Vec<Store>> {
        self.idle.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn with<T: Default>(&self, f: impl FnOnce(&Store) -> Result<T>) -> Result<T> {
        let Some(db) = &self.db else {
            return Err(StoreError::NotFound(format!(
                "data directory (set {} or pass --data-dir)",
                tbt_store::paths::DATA_DIR_ENV
            )));
        };
        if !db.exists() {
            self.idle().clear();
            return Ok(T::default());
        }
        let pooled = self.idle().pop();
        let store = match pooled {
            Some(s) => s,
            None => Store::open_read_only(db)?,
        };
        let result = f(&store);
        if result.is_ok() {
            let mut idle = self.idle();
            if idle.len() < MAX_IDLE {
                idle.push(store);
            }
        }
        result
    }
}

impl DataSource for StoreData {
    fn session_row(&self, id: SessionId) -> Result<Option<SessionRow>> {
        self.with(|s| s.session_row(id))
    }

    fn db_path(&self) -> Option<PathBuf> {
        self.db.clone()
    }

    fn kpi_summary(&self, filter: &QueryFilter) -> Result<KpiSummary> {
        self.with(|s| s.kpi_summary(filter))
    }

    fn cost_buckets(&self, filter: &QueryFilter, group: BucketGroup) -> Result<Vec<CostBucket>> {
        self.with(|s| s.cost_buckets(filter, group))
    }

    fn heatmap(&self, filter: &QueryFilter) -> Result<Vec<HeatCell>> {
        self.with(|s| s.heatmap(filter))
    }

    fn sessions(&self, filter: &QueryFilter) -> Result<Vec<SessionRow>> {
        self.with(|s| s.sessions(filter))
    }

    fn session_detail(&self, id: SessionId) -> Result<Option<SessionDetail>> {
        self.with(|s| s.session_detail(id))
    }

    fn model_stats(&self, filter: &QueryFilter) -> Result<Vec<ModelStats>> {
        self.with(|s| s.model_stats(filter))
    }

    fn project_stats(&self, filter: &QueryFilter) -> Result<Vec<ProjectStats>> {
        self.with(|s| s.project_stats(filter))
    }

    fn harness_status(&self) -> Result<Vec<HarnessStatus>> {
        self.with(|s| s.harness_status())
    }

    fn unpriced_models(&self) -> Result<Vec<UnpricedModel>> {
        self.with(|s| s.unpriced_models())
    }

    fn ingest_runs(&self, limit: usize) -> Result<Vec<IngestRunSummary>> {
        self.with(|s| s.ingest_runs(limit))
    }
}
