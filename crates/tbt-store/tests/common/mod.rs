#![allow(dead_code)]

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use tbt_core::{
    Cursor, Decimal, HarnessId, Micros, ModelPrice, PriceId, PriceSource, PricedEvent, Timestamp,
    UsageEvent,
};
use tbt_store::Store;
use tempfile::TempDir;

pub const CC: HarnessId = HarnessId::CLAUDE_CODE;

pub fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

pub struct Db {
    pub dir: TempDir,
    pub path: PathBuf,
    pub store: Store,
}

impl Db {
    pub fn new() -> Db {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tbt.sqlite");
        let mut store = Store::open(&path).unwrap();
        store
            .insert_price(&price("claude-sonnet-4-5", "3", Some("0.30")))
            .unwrap();
        Db { dir, path, store }
    }

    pub fn raw(&self) -> Connection {
        Connection::open(&self.path).unwrap()
    }

    pub fn ingest(&mut self, source: &str, events: &[PricedEvent]) -> usize {
        let mut tx = self.store.begin_source_tx(&CC, Path::new(source)).unwrap();
        let n = tx.insert_events(events).unwrap();
        tx.commit().unwrap();
        n
    }
}

pub fn event(key: &str, session: &str, at: &str) -> UsageEvent {
    UsageEvent {
        harness: CC,
        dedup_key: key.into(),
        session_ext_id: session.into(),
        ts: ts(at),
        provider: "anthropic".into(),
        model: "claude-sonnet-4-5".into(),
        input_tokens: 100,
        output_tokens: 50,
        cache_write_5m_tokens: 10,
        cache_write_1h_tokens: 5,
        cache_read_tokens: 1000,
        reasoning_tokens: 0,
        reported_cost: None,
        project_path: None,
        git_branch: None,
        raw: None,
    }
}

pub fn priced(e: UsageEvent, cost: i64, price: Option<i64>) -> PricedEvent {
    PricedEvent {
        event: e,
        cost: Micros(cost),
        price_id: price.map(PriceId),
    }
}

pub fn price(model: &str, input: &str, cache_read: Option<&str>) -> ModelPrice {
    ModelPrice {
        id: None,
        provider: "anthropic".into(),
        model_pattern: model.into(),
        effective_from: ts("2025-01-01T00:00:00Z"),
        effective_to: None,
        input_per_mtok: input.parse::<Decimal>().unwrap(),
        output_per_mtok: "15".parse().unwrap(),
        cache_write_5m_per_mtok: None,
        cache_write_1h_per_mtok: None,
        cache_read_per_mtok: cache_read.map(|c| c.parse().unwrap()),
        long_context: None,
        source: PriceSource::Bundled,
    }
}

pub fn cursor(offset: u64) -> Cursor {
    Cursor::from_json(serde_json::json!({ "byte_offset": offset }))
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct HourRow {
    pub hour: String,
    pub harness: String,
    pub model: String,
    pub project: String,
    pub events: i64,
    pub input: i64,
    pub output: i64,
    pub cache_write: i64,
    pub cache_read: i64,
    pub cost: i64,
    pub unpriced: i64,
}

pub fn hourly(conn: &Connection) -> Vec<HourRow> {
    let mut stmt = conn
        .prepare(
            "SELECT hour, harness_id, model, p.path, events, input_tokens, output_tokens,
                    cache_write_tokens, cache_read_tokens, cost_micros, unpriced_events
             FROM hourly_usage JOIN project p ON p.id = project_id
             ORDER BY hour, harness_id, model, p.path",
        )
        .unwrap();
    stmt.query_map([], |r| {
        Ok(HourRow {
            hour: r.get(0)?,
            harness: r.get(1)?,
            model: r.get(2)?,
            project: r.get(3)?,
            events: r.get(4)?,
            input: r.get(5)?,
            output: r.get(6)?,
            cache_write: r.get(7)?,
            cache_read: r.get(8)?,
            cost: r.get(9)?,
            unpriced: r.get(10)?,
        })
    })
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

pub fn hourly_from_events(conn: &Connection) -> Vec<HourRow> {
    let mut stmt = conn
        .prepare(
            "SELECT substr(ts, 1, 13) || ':00:00.000000000Z' AS h, harness_id, model, p.path,
                    COUNT(*), SUM(input_tokens), SUM(output_tokens),
                    SUM(cache_write_5m_tokens + cache_write_1h_tokens), SUM(cache_read_tokens),
                    SUM(cost_micros), SUM(price_id IS NULL)
             FROM usage_event JOIN project p ON p.id = project_id
             GROUP BY h, harness_id, model, p.path
             ORDER BY h, harness_id, model, p.path",
        )
        .unwrap();
    stmt.query_map([], |r| {
        Ok(HourRow {
            hour: r.get(0)?,
            harness: r.get(1)?,
            model: r.get(2)?,
            project: r.get(3)?,
            events: r.get(4)?,
            input: r.get(5)?,
            output: r.get(6)?,
            cache_write: r.get(7)?,
            cache_read: r.get(8)?,
            cost: r.get(9)?,
            unpriced: r.get(10)?,
        })
    })
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionTotals {
    pub harness: String,
    pub ext_id: String,
    pub project: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub active_seconds: i64,
    pub events: i64,
    pub input: i64,
    pub output: i64,
    pub cache_write: i64,
    pub cache_read: i64,
    pub cost: i64,
    pub unpriced: i64,
}

pub fn sessions(conn: &Connection) -> Vec<SessionTotals> {
    let mut stmt = conn
        .prepare(
            "SELECT harness_id, ext_id, (SELECT path FROM project WHERE id = project_id), started_at, ended_at, active_seconds, events,
                    input_tokens, output_tokens, cache_write_tokens, cache_read_tokens,
                    cost_micros, unpriced_events
             FROM session ORDER BY harness_id, ext_id",
        )
        .unwrap();
    stmt.query_map([], |r| {
        Ok(SessionTotals {
            harness: r.get(0)?,
            ext_id: r.get(1)?,
            project: r.get(2)?,
            started_at: r.get(3)?,
            ended_at: r.get(4)?,
            active_seconds: r.get(5)?,
            events: r.get(6)?,
            input: r.get(7)?,
            output: r.get(8)?,
            cache_write: r.get(9)?,
            cache_read: r.get(10)?,
            cost: r.get(11)?,
            unpriced: r.get(12)?,
        })
    })
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

pub fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
