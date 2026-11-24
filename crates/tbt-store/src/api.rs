use std::path::{Path, PathBuf};

use rusqlite::TransactionBehavior;
use serde::{Deserialize, Serialize};
use tbt_core::{
    Cursor, EventId, HarnessId, Hour, MessageKind, MessageRecord, MessageRole, Micros, ModelPrice,
    ParseWarning, PriceId, PricedEvent, ProjectId, RunId, SessionId, SessionUpsert, SourceFileId,
    TimeRange, Timestamp, TokenCounts, UsageEvent,
};

use crate::{admin, ingest, query, recompute, schema};

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("migration error: {0}")]
    Migration(#[from] rusqlite_migration::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid stored data: {0}")]
    Corrupt(String),
    #[error("database is read-only")]
    ReadOnly,
}

pub struct Store {
    conn: rusqlite::Connection,
}

impl Store {
    fn writable(&self) -> Result<()> {
        if self.conn.is_readonly(rusqlite::MAIN_DB)? {
            return Err(StoreError::ReadOnly);
        }
        Ok(())
    }

    pub fn open(path: &Path) -> Result<Store> {
        Ok(Store {
            conn: schema::open(path)?,
        })
    }

    pub fn open_in_memory() -> Result<Store> {
        Ok(Store {
            conn: schema::open_in_memory()?,
        })
    }

    pub fn open_read_only(path: &Path) -> Result<Store> {
        Ok(Store {
            conn: schema::open_read_only(path)?,
        })
    }

    pub fn start_run(&mut self, trigger: RunTrigger, started_at: Timestamp) -> Result<RunId> {
        self.writable()?;
        admin::start_run(&self.conn, trigger, started_at)
    }

    pub fn finish_run(
        &mut self,
        run: RunId,
        finished_at: Timestamp,
        stats: &RunStats,
        warnings: &[ParseWarning],
        error: Option<&str>,
    ) -> Result<()> {
        self.writable()?;
        admin::finish_run(&self.conn, run, finished_at, stats, warnings, error)
    }

    pub fn cursor_for(&self, harness: &HarnessId, path: &Path) -> Result<Option<Cursor>> {
        admin::cursor_for(&self.conn, harness, path)
    }

    pub fn known_sources(&self, harness: &HarnessId) -> Result<Vec<PathBuf>> {
        admin::known_sources(&self.conn, harness)
    }

    pub fn mark_source_missing(&mut self, harness: &HarnessId, path: &Path) -> Result<()> {
        self.writable()?;
        admin::mark_source_missing(&self.conn, harness, path)
    }

    pub fn begin_source_tx(&mut self, harness: &HarnessId, path: &Path) -> Result<SourceTx<'_>> {
        self.writable()?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let source_file = ingest::source_file(&tx, harness.as_str(), path)?;
        Ok(SourceTx {
            tx,
            harness: harness.clone(),
            source_file,
        })
    }

    pub fn register_harness(&mut self, id: &HarnessId, display_name: &str) -> Result<()> {
        self.writable()?;
        admin::register_harness(&self.conn, id, display_name)
    }

    pub fn harnesses(&self) -> Result<Vec<HarnessConfig>> {
        admin::harnesses(&self.conn)
    }

    pub fn set_harness_enabled(&mut self, id: &HarnessId, enabled: bool) -> Result<()> {
        self.writable()?;
        admin::set_harness_enabled(&self.conn, id, enabled)
    }

    pub fn set_harness_roots(&mut self, id: &HarnessId, roots: Option<&[PathBuf]>) -> Result<()> {
        self.writable()?;
        admin::set_harness_roots(&self.conn, id, roots)
    }

    pub fn model_prices(&self) -> Result<Vec<ModelPrice>> {
        admin::model_prices(&self.conn)
    }

    pub fn insert_price(&mut self, price: &ModelPrice) -> Result<PriceId> {
        self.writable()?;
        admin::insert_price(&self.conn, price)
    }

    pub fn close_price(&mut self, id: PriceId, effective_to: Timestamp) -> Result<()> {
        self.writable()?;
        admin::close_price(&self.conn, id, effective_to)
    }

    pub fn update_price(&mut self, price: &ModelPrice) -> Result<()> {
        self.writable()?;
        admin::update_price(&self.conn, price)
    }

    pub fn seed_prices_if_empty(&mut self, prices: &[ModelPrice]) -> Result<usize> {
        self.writable()?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let n = admin::seed_prices_if_empty(&tx, prices)?;
        tx.commit()?;
        Ok(n)
    }

    pub fn events_for_recompute(
        &self,
        filter: &RecomputeFilter,
        after: Option<EventId>,
        limit: usize,
    ) -> Result<Vec<RecomputeRow>> {
        recompute::events_for_recompute(&self.conn, filter, after, limit)
    }

    pub fn begin_recompute_tx(&mut self) -> Result<RecomputeTx<'_>> {
        self.writable()?;
        Ok(RecomputeTx {
            tx: self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?,
        })
    }

    pub fn drop_raw_json_older_than(&mut self, days: u32, now: Timestamp) -> Result<usize> {
        self.writable()?;
        recompute::drop_raw_json_older_than(&self.conn, days, now)
    }

    pub fn kpi_summary(&self, filter: &QueryFilter) -> Result<KpiSummary> {
        query::kpi_summary(&self.conn, filter)
    }

    pub fn cost_buckets(
        &self,
        filter: &QueryFilter,
        group: BucketGroup,
    ) -> Result<Vec<CostBucket>> {
        query::cost_buckets(&self.conn, filter, group)
    }

    pub fn heatmap(&self, filter: &QueryFilter) -> Result<Vec<HeatCell>> {
        query::heatmap(&self.conn, filter)
    }

    pub fn sessions(&self, filter: &QueryFilter) -> Result<Vec<SessionRow>> {
        query::sessions(&self.conn, filter)
    }

    pub fn session_row(&self, id: SessionId) -> Result<Option<SessionRow>> {
        query::session_row_by_id(&self.conn, id)
    }

    pub fn session_detail(&self, id: SessionId) -> Result<Option<SessionDetail>> {
        query::session_detail(&self.conn, id)
    }

    pub fn model_stats(&self, filter: &QueryFilter) -> Result<Vec<ModelStats>> {
        query::model_stats(&self.conn, filter)
    }

    pub fn project_stats(&self, filter: &QueryFilter) -> Result<Vec<ProjectStats>> {
        query::project_stats(&self.conn, filter)
    }

    pub fn harness_status(&self) -> Result<Vec<HarnessStatus>> {
        query::harness_status(&self.conn)
    }

    pub fn unpriced_models(&self) -> Result<Vec<UnpricedModel>> {
        query::unpriced_models(&self.conn)
    }

    pub fn ingest_runs(&self, limit: usize) -> Result<Vec<IngestRunSummary>> {
        admin::ingest_runs(&self.conn, limit)
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        if !self.conn.is_readonly(rusqlite::MAIN_DB).unwrap_or(true) {
            let _ = self.conn.execute_batch("PRAGMA optimize;");
        }
    }
}

pub struct SourceTx<'a> {
    tx: rusqlite::Transaction<'a>,
    harness: HarnessId,
    source_file: SourceFileId,
}

impl SourceTx<'_> {
    pub fn source_file_id(&self) -> SourceFileId {
        self.source_file
    }

    pub fn upsert_project(&mut self, path: Option<&str>) -> Result<ProjectId> {
        ingest::upsert_project(&self.tx, path)
    }

    pub fn upsert_session(
        &mut self,
        session: &SessionUpsert,
        project: ProjectId,
    ) -> Result<SessionId> {
        ingest::upsert_session(&self.tx, session, project)
    }

    pub fn session_id(&self, harness: &HarnessId, ext_id: &str) -> Result<Option<SessionId>> {
        ingest::session_id(&self.tx, harness.as_str(), ext_id)
    }

    pub fn link_resumed_session(&mut self, new: SessionId, original: SessionId) -> Result<()> {
        ingest::link_session(&self.tx, "resumed_from_session_id", new, original)
    }

    pub fn set_parent_session(&mut self, subagent: SessionId, parent: SessionId) -> Result<()> {
        ingest::link_session(&self.tx, "parent_session_id", subagent, parent)
    }

    pub fn session_holding_dedup_key(
        &self,
        harness: &HarnessId,
        dedup_key: &str,
    ) -> Result<Option<SessionId>> {
        ingest::session_holding_dedup_key(&self.tx, harness.as_str(), dedup_key)
    }

    pub fn last_cumulative_total(&self, session: SessionId) -> Result<Option<TokenCounts>> {
        ingest::last_cumulative_total(&self.tx, session)
    }

    pub fn save_cumulative_total(&mut self, session: SessionId, total: &TokenCounts) -> Result<()> {
        ingest::save_cumulative_total(&self.tx, session, total)
    }

    pub fn insert_events(&mut self, events: &[PricedEvent]) -> Result<usize> {
        ingest::insert_events(&self.tx, self.source_file, events)
    }

    pub fn insert_messages(&mut self, messages: &[MessageRecord]) -> Result<usize> {
        ingest::insert_messages(&self.tx, self.harness.as_str(), messages)
    }

    pub fn save_cursor(&mut self, cursor: &Cursor, now: Timestamp) -> Result<()> {
        ingest::save_cursor(&self.tx, self.source_file, cursor, now)
    }

    pub fn commit(self) -> Result<()> {
        self.tx.commit()?;
        Ok(())
    }
}

pub struct RecomputeTx<'a> {
    tx: rusqlite::Transaction<'a>,
}

impl RecomputeTx<'_> {
    pub fn update_event_costs(&mut self, updates: &[CostUpdate]) -> Result<Affected> {
        recompute::update_event_costs(&self.tx, updates)
    }

    pub fn rebuild_session_totals(&mut self, sessions: &[SessionId]) -> Result<()> {
        recompute::rebuild_session_totals(&self.tx, sessions)
    }

    pub fn rebuild_hourly(&mut self, hours: &[Hour]) -> Result<()> {
        recompute::rebuild_hourly(&self.tx, hours)
    }

    pub fn commit(self) -> Result<()> {
        self.tx.commit()?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunTrigger {
    Hourly,
    Startup,
    Manual,
    Watch,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunStats {
    pub sources_scanned: u64,
    pub events_new: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessConfig {
    pub id: HarnessId,
    pub display_name: String,
    pub enabled: bool,
    pub roots: Option<Vec<PathBuf>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecomputeFilter {
    pub price_id: Option<PriceId>,
    pub range: Option<TimeRange>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub unpriced_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecomputeRow {
    pub id: EventId,
    pub session: SessionId,
    pub event: UsageEvent,
    pub cost: Micros,
    pub price_id: Option<PriceId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostUpdate {
    pub event: EventId,
    pub cost: Micros,
    pub price_id: Option<PriceId>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Affected {
    pub sessions: Vec<SessionId>,
    pub hours: Vec<Hour>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryFilter {
    pub range: TimeRange,
    pub harnesses: Vec<HarnessId>,
    pub bucket: BucketSize,
    pub time_zone: String,
}

impl QueryFilter {
    pub fn new(range: TimeRange, bucket: BucketSize) -> QueryFilter {
        QueryFilter {
            range,
            harnesses: Vec::new(),
            bucket,
            time_zone: "UTC".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BucketSize {
    Hour,
    Day,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BucketGroup {
    Harness,
    Model,
    Project,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TokenSums {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

impl TokenSums {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_write + self.cache_read
    }

    pub fn cache_hit_ratio(&self) -> Option<f64> {
        let prompt = self.input + self.cache_write + self.cache_read;
        (prompt > 0).then(|| self.cache_read as f64 / prompt as f64)
    }
}

impl From<TokenCounts> for TokenSums {
    fn from(t: TokenCounts) -> TokenSums {
        TokenSums {
            input: t.input,
            output: t.output,
            cache_write: t.cache_write(),
            cache_read: t.cache_read,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KpiSummary {
    pub cost: Micros,
    pub tokens: TokenSums,
    pub events: u64,
    pub sessions: u64,
    pub unpriced_events: u64,
    pub active_hours: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostBucket {
    pub start: Timestamp,
    pub key: String,
    pub cost: Micros,
    pub tokens: TokenSums,
    pub events: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeatCell {
    /// 0 = Monday … 6 = Sunday.
    pub weekday: u8,
    pub hour: u8,
    pub cost: Micros,
    pub events: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRow {
    pub id: SessionId,
    pub harness: HarnessId,
    pub ext_id: String,
    pub title: Option<String>,
    pub project_path: Option<String>,
    pub project_name: Option<String>,
    pub git_branch: Option<String>,
    pub models: Vec<String>,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub active_seconds: u64,
    pub events: u64,
    pub tokens: TokenSums,
    pub cost: Micros,
    pub unpriced_events: u64,
    pub parent: Option<SessionId>,
    pub resumed_from: Option<SessionId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDetail {
    pub session: SessionRow,
    pub harness_version: Option<String>,
    pub events: Vec<EventRow>,
    pub messages: Vec<MessageRow>,
    pub tool_counts: Vec<(String, u64)>,
    pub reported_cost: Option<Micros>,
    pub resume_chain: Vec<SessionRow>,
    pub chain_cost: Micros,
    pub subagents: Vec<SessionRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRow {
    pub id: EventId,
    pub ts: Timestamp,
    pub provider: String,
    pub model: String,
    pub tokens: TokenCounts,
    pub cost: Micros,
    pub reported_cost: Option<Micros>,
    pub price_id: Option<PriceId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageRow {
    pub ts: Timestamp,
    pub role: MessageRole,
    pub kind: Option<MessageKind>,
    pub tool_name: Option<String>,
    pub preview: Option<String>,
    pub event: Option<EventId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelStats {
    pub provider: String,
    pub model: String,
    pub events: u64,
    pub tokens: TokenSums,
    pub cost: Micros,
    pub unpriced_events: u64,
    pub cache_savings: Micros,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectStats {
    pub id: ProjectId,
    pub path: Option<String>,
    pub name: Option<String>,
    pub sessions: u64,
    pub events: u64,
    pub tokens: TokenSums,
    pub cost: Micros,
    pub last_active: Option<Timestamp>,
    pub branches: Vec<BranchStats>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchStats {
    pub branch: Option<String>,
    pub sessions: u64,
    pub cost: Micros,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessStatus {
    pub config: HarnessConfig,
    pub files_tracked: u64,
    pub files_missing: u64,
    pub last_read: Option<Timestamp>,
    pub events: u64,
    pub sessions: u64,
    pub last_warnings: Vec<ParseWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnpricedModel {
    pub provider: String,
    pub model: String,
    pub events: u64,
    pub tokens: TokenSums,
    pub first_seen: Timestamp,
    pub last_seen: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestRunSummary {
    pub id: RunId,
    pub trigger: RunTrigger,
    pub started_at: Timestamp,
    pub finished_at: Option<Timestamp>,
    pub stats: RunStats,
    pub warnings: Vec<ParseWarning>,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_sums_from_counts() {
        let t = TokenCounts {
            input: 10,
            output: 5,
            cache_write_5m: 2,
            cache_write_1h: 3,
            cache_read: 85,
            reasoning: 1,
        };
        let s = TokenSums::from(t);
        assert_eq!(s.cache_write, 5);
        assert_eq!(s.total(), t.total());
        assert_eq!(s.cache_hit_ratio(), Some(0.85));
        assert_eq!(TokenSums::default().cache_hit_ratio(), None);
    }

    #[test]
    fn query_types_serialize() {
        let f = QueryFilter::new(
            TimeRange::new(
                "2026-11-01T00:00:00Z".parse().unwrap(),
                "2026-11-02T00:00:00Z".parse().unwrap(),
            ),
            BucketSize::Day,
        );
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("\"bucket\":\"day\""));
        let back: QueryFilter = serde_json::from_str(&json).unwrap();
        assert_eq!(back, f);
    }
}
