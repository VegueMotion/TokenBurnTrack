#![allow(unused_variables, dead_code)]

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tbt_core::{
    Cursor, EventId, HarnessId, Hour, MessageKind, MessageRecord, MessageRole, Micros, ModelPrice,
    ParseWarning, PriceId, PricedEvent, ProjectId, RunId, SessionId, SessionUpsert, SourceFileId,
    TimeRange, Timestamp, TokenCounts, UsageEvent,
};

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

/// Sync SQLite store. One writer (the collector) plus any number of read-only
/// connections (UI pages, one per background query).
pub struct Store {
    conn: rusqlite::Connection,
}

impl Store {
    /// Opens or creates the DB, applies WAL + `foreign_keys=ON`, runs migrations and
    /// seeds the `project(id = 0)` "no project" row.
    pub fn open(path: &Path) -> Result<Store> {
        todo!()
    }

    pub fn open_in_memory() -> Result<Store> {
        todo!()
    }

    /// Read-only connection; never migrates. Write calls return [`StoreError::ReadOnly`].
    pub fn open_read_only(path: &Path) -> Result<Store> {
        todo!()
    }

    pub fn start_run(&mut self, trigger: RunTrigger, started_at: Timestamp) -> Result<RunId> {
        todo!()
    }

    pub fn finish_run(
        &mut self,
        run: RunId,
        finished_at: Timestamp,
        stats: &RunStats,
        warnings: &[ParseWarning],
        error: Option<&str>,
    ) -> Result<()> {
        todo!()
    }

    /// `None` if the source was never read.
    pub fn cursor_for(&self, harness: &HarnessId, path: &Path) -> Result<Option<Cursor>> {
        todo!()
    }

    /// Paths of all `source_file` rows of `harness` that are not marked missing.
    pub fn known_sources(&self, harness: &HarnessId) -> Result<Vec<PathBuf>> {
        todo!()
    }

    /// Flags a source that `discover` no longer returns. Its data stays.
    /// A later `SourceTx::save_cursor` for the same path clears the flag.
    pub fn mark_source_missing(&mut self, harness: &HarnessId, path: &Path) -> Result<()> {
        todo!()
    }

    /// Starts the single transaction in which one source's batch is written.
    /// Creates the `source_file` row if needed. Dropping the `SourceTx` without
    /// [`SourceTx::commit`] rolls everything back, cursor included.
    pub fn begin_source_tx(&mut self, harness: &HarnessId, path: &Path) -> Result<SourceTx<'_>> {
        todo!()
    }

    /// Inserts the `harness` row if missing (keeps `enabled` / roots of an existing row,
    /// updates `display_name`). Called for every registered adapter at startup.
    pub fn register_harness(&mut self, id: &HarnessId, display_name: &str) -> Result<()> {
        todo!()
    }

    pub fn harnesses(&self) -> Result<Vec<HarnessConfig>> {
        todo!()
    }

    pub fn set_harness_enabled(&mut self, id: &HarnessId, enabled: bool) -> Result<()> {
        todo!()
    }

    /// `None` = use the adapter's default roots.
    pub fn set_harness_roots(&mut self, id: &HarnessId, roots: Option<&[PathBuf]>) -> Result<()> {
        todo!()
    }

    /// All rows, open and closed, with `id` set.
    pub fn model_prices(&self) -> Result<Vec<ModelPrice>> {
        todo!()
    }

    /// Ignores `price.id`; returns the new id.
    pub fn insert_price(&mut self, price: &ModelPrice) -> Result<PriceId> {
        todo!()
    }

    pub fn close_price(&mut self, id: PriceId, effective_to: Timestamp) -> Result<()> {
        todo!()
    }

    /// Overwrites the row `price.id` (must be `Some`). Does not touch event costs;
    /// run a recompute for `RecomputeFilter { price_id: Some(id), .. }` afterwards.
    pub fn update_price(&mut self, price: &ModelPrice) -> Result<()> {
        todo!()
    }

    /// Inserts `prices` only if `model_price` is empty. Returns rows inserted.
    pub fn seed_prices_if_empty(&mut self, prices: &[ModelPrice]) -> Result<usize> {
        todo!()
    }

    /// One page of events matching `filter`, ordered by id, starting after `after`.
    /// `RecomputeRow::event.raw` is always `None`.
    pub fn events_for_recompute(
        &self,
        filter: &RecomputeFilter,
        after: Option<EventId>,
        limit: usize,
    ) -> Result<Vec<RecomputeRow>> {
        todo!()
    }

    pub fn begin_recompute_tx(&mut self) -> Result<RecomputeTx<'_>> {
        todo!()
    }

    /// Sets `raw_json = NULL` on events older than `now - days`. Returns rows changed.
    pub fn drop_raw_json_older_than(&mut self, days: u32, now: Timestamp) -> Result<usize> {
        todo!()
    }

    pub fn kpi_summary(&self, filter: &QueryFilter) -> Result<KpiSummary> {
        todo!()
    }

    /// One row per (bucket, group key) with usage; empty buckets are omitted.
    pub fn cost_buckets(
        &self,
        filter: &QueryFilter,
        group: BucketGroup,
    ) -> Result<Vec<CostBucket>> {
        todo!()
    }

    /// Weekday × hour-of-day in `filter.time_zone`; only cells with usage.
    pub fn heatmap(&self, filter: &QueryFilter) -> Result<Vec<HeatCell>> {
        todo!()
    }

    /// Sessions with at least one event in `filter.range`, newest first.
    pub fn sessions(&self, filter: &QueryFilter) -> Result<Vec<SessionRow>> {
        todo!()
    }

    pub fn session_detail(&self, id: SessionId) -> Result<Option<SessionDetail>> {
        todo!()
    }

    pub fn model_stats(&self, filter: &QueryFilter) -> Result<Vec<ModelStats>> {
        todo!()
    }

    pub fn project_stats(&self, filter: &QueryFilter) -> Result<Vec<ProjectStats>> {
        todo!()
    }

    pub fn harness_status(&self) -> Result<Vec<HarnessStatus>> {
        todo!()
    }

    /// Models that have events with `price_id IS NULL`, all time.
    pub fn unpriced_models(&self) -> Result<Vec<UnpricedModel>> {
        todo!()
    }

    /// Newest first.
    pub fn ingest_runs(&self, limit: usize) -> Result<Vec<IngestRunSummary>> {
        todo!()
    }
}

/// Write transaction for one source. Every method runs inside it; nothing is visible
/// to other connections until [`SourceTx::commit`].
pub struct SourceTx<'a> {
    tx: rusqlite::Transaction<'a>,
    harness: HarnessId,
    source_file: SourceFileId,
}

impl SourceTx<'_> {
    pub fn source_file_id(&self) -> SourceFileId {
        self.source_file
    }

    /// `None` → [`ProjectId::NONE`]. Otherwise inserts the project by path if missing.
    pub fn upsert_project(&mut self, path: Option<&str>) -> Result<ProjectId> {
        todo!()
    }

    /// Inserts or merges by `(harness, ext_id)`: non-`None` fields overwrite,
    /// `started_at` / `ended_at` merge as min / max. A new row without a `started_at`
    /// hint gets the current time until events move it. Does not resolve
    /// `parent_ext_id` / `resumed_from_ext_id`; see `set_parent_session` and
    /// `link_resumed_session`.
    pub fn upsert_session(
        &mut self,
        session: &SessionUpsert,
        project: ProjectId,
    ) -> Result<SessionId> {
        todo!()
    }

    pub fn session_id(&self, harness: &HarnessId, ext_id: &str) -> Result<Option<SessionId>> {
        todo!()
    }

    pub fn link_resumed_session(&mut self, new: SessionId, original: SessionId) -> Result<()> {
        todo!()
    }

    pub fn set_parent_session(&mut self, subagent: SessionId, parent: SessionId) -> Result<()> {
        todo!()
    }

    /// Session of the already stored event with this dedup key (sees this tx's inserts).
    pub fn session_holding_dedup_key(
        &self,
        harness: &HarnessId,
        dedup_key: &str,
    ) -> Result<Option<SessionId>> {
        todo!()
    }

    /// Raw running total last saved for `session` (cumulative-counter harnesses only).
    pub fn last_cumulative_total(&self, session: SessionId) -> Result<Option<TokenCounts>> {
        todo!()
    }

    pub fn save_cumulative_total(&mut self, session: SessionId, total: &TokenCounts) -> Result<()> {
        todo!()
    }

    /// `INSERT … ON CONFLICT(harness_id, dedup_key) DO NOTHING`. For every row actually
    /// inserted it also updates the session totals, `active_seconds`, `ended_at` and the
    /// `hourly_usage` bucket (project from `event.project_path`, else the session's).
    /// A session not yet upserted in this tx is created with defaults.
    /// Returns the number of new events.
    pub fn insert_events(&mut self, events: &[PricedEvent]) -> Result<usize> {
        todo!()
    }

    /// Skips messages whose `(session, key)` is already stored; links
    /// `event_dedup_key` to `usage_event.id`. Returns the number of new messages.
    pub fn insert_messages(&mut self, messages: &[MessageRecord]) -> Result<usize> {
        todo!()
    }

    /// Also clears the source's `missing` flag and sets `last_read = now`.
    pub fn save_cursor(&mut self, cursor: &Cursor, now: Timestamp) -> Result<()> {
        todo!()
    }

    pub fn commit(self) -> Result<()> {
        todo!()
    }
}

/// Write transaction for a recompute job (F).
pub struct RecomputeTx<'a> {
    tx: rusqlite::Transaction<'a>,
}

impl RecomputeTx<'_> {
    /// Sets `cost_micros` and `price_id` per event. Returns the sessions and hours the
    /// changed events belong to, to pass to the two rebuild calls.
    pub fn update_event_costs(&mut self, updates: &[CostUpdate]) -> Result<Affected> {
        todo!()
    }

    pub fn rebuild_session_totals(&mut self, sessions: &[SessionId]) -> Result<()> {
        todo!()
    }

    pub fn rebuild_hourly(&mut self, hours: &[Hour]) -> Result<()> {
        todo!()
    }

    pub fn commit(self) -> Result<()> {
        todo!()
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
    /// `None` = adapter defaults.
    pub roots: Option<Vec<PathBuf>>,
}

/// Criteria are ANDed; an all-default filter matches every event.
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
    /// Empty = all harnesses.
    pub harnesses: Vec<HarnessId>,
    pub bucket: BucketSize,
    /// IANA zone used for day buckets and the heatmap, e.g. `Europe/Berlin`.
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

/// Token sums as kept in rollups (cache writes not split by TTL, no reasoning).
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

    /// `cache_read / (input + cache_write + cache_read)`; `None` without prompt tokens.
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
    /// Distinct hours with at least one event; the burn rate is `cost / active_hours`.
    pub active_hours: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostBucket {
    /// Bucket start (hour, or local midnight for day buckets).
    pub start: Timestamp,
    /// Harness id, model id, or project path ("" for no project), per [`BucketGroup`].
    pub key: String,
    pub cost: Micros,
    pub tokens: TokenSums,
    pub events: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeatCell {
    /// 0 = Monday … 6 = Sunday.
    pub weekday: u8,
    /// 0..=23
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
    /// Ordered by `ts`.
    pub events: Vec<EventRow>,
    /// Empty unless messages were stored (privacy level).
    pub messages: Vec<MessageRow>,
    /// `(tool_name, count)`, most used first.
    pub tool_counts: Vec<(String, u64)>,
    /// Sum of `reported_cost` over events that have one.
    pub reported_cost: Option<Micros>,
    /// Oldest first, ending with this session.
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
    /// What cache reads saved compared with paying the input rate, per the events' price rows.
    pub cache_savings: Micros,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectStats {
    pub id: ProjectId,
    /// `None` for the "no project" row.
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
    /// Warnings of this harness from the latest finished run.
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
