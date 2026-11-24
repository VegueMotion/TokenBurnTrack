#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::Connection;
use tbt_collector::price_seed::SeedReport;
use tbt_collector::{Collector, CollectorConfig, Registry, RunOutcome, RunReport, RunTrigger};
use tbt_core::{HarnessAdapter, HarnessId};
use tbt_store::Store;

pub fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

pub fn copy_tree(from: &Path, to: &Path) {
    if from.is_dir() {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            copy_tree(&entry.path(), &to.join(entry.file_name()));
        }
    } else {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::copy(from, to).unwrap();
    }
}

pub fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if root.is_dir() {
        for entry in std::fs::read_dir(root).unwrap() {
            out.extend(files_under(&entry.unwrap().path()));
        }
    } else if root.is_file() {
        out.push(root.to_path_buf());
    }
    out.sort();
    out
}

pub fn append(path: &Path, text: &str) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .unwrap();
    f.write_all(text.as_bytes()).unwrap();
}

pub fn claude() -> Arc<dyn HarnessAdapter> {
    Arc::new(tbt_adapters::ClaudeCodeAdapter::new())
}

pub fn seed_bundled_if_empty(store: &mut Store) -> anyhow::Result<SeedReport> {
    store.seed_prices_if_empty(&tbt_pricing::bundled_prices()?)?;
    Ok(SeedReport::default())
}

pub const CLAUDE_FIXTURES: &str = "claude_code";

pub const SHORT_SESSION: &str = "35395b06-00f0-4c8b-9098-93d35ca8f399";
pub const LONG_SESSION: &str = "981f8c38-df8d-441c-bba0-9385343213aa";
pub const MAIN_SESSION: &str = "64ed9da4-d663-42b6-bd5f-db9652067ec7";
pub const SUBAGENT_SESSION: &str = "64ed9da4-d663-42b6-bd5f-db9652067ec7/agent-a919fc890880d1075";
pub const RESUMED_ORIGINAL: &str = "3fc87bac-5bef-406a-bb3b-6447d69a519d";
pub const RESUMED_CONTINUATION: &str = "d1774c7e-944b-47aa-8282-44b01089b283";
pub const TOOL_HEAVY_SESSION: &str = "f001a22b-ee17-44c8-ab5f-c1ab7bd4b32b";
pub const MULTI_MODEL_SESSION: &str = "ee7bdb9e-c647-49d2-837d-71810ebd64c7";

pub const CLAUDE_GOLDEN_EVENTS: &[(&str, i64)] = &[
    (SHORT_SESSION, 1),
    (LONG_SESSION, 64),
    (MAIN_SESSION, 19),
    (SUBAGENT_SESSION, 6),
    (RESUMED_ORIGINAL, 50),
    (RESUMED_CONTINUATION, 5),
    (TOOL_HEAVY_SESSION, 53),
    (MULTI_MODEL_SESSION, 6),
];

pub fn claude_golden_total() -> i64 {
    CLAUDE_GOLDEN_EVENTS.iter().map(|(_, n)| n).sum()
}

pub struct Env {
    pub dir: tempfile::TempDir,
    pub data: PathBuf,
    pub roots: PathBuf,
}

impl Env {
    pub fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let roots = dir.path().join("roots");
        std::fs::create_dir_all(&roots).unwrap();
        Env { dir, data, roots }
    }

    pub fn with_fixtures(sets: &[&str]) -> Env {
        let env = Env::new();
        for set in sets {
            copy_tree(&fixtures_root().join(set), &env.roots.join(set));
        }
        env
    }

    pub fn root(&self, set: &str) -> PathBuf {
        self.roots.join(set)
    }

    pub fn find(&self, file_name: &str) -> PathBuf {
        files_under(&self.roots)
            .into_iter()
            .find(|p| p.file_name().is_some_and(|n| n == file_name))
            .unwrap_or_else(|| panic!("{file_name} not under {}", self.roots.display()))
    }

    pub fn set_roots(&self, id: &HarnessId, display_name: &str, roots: &[PathBuf]) {
        let mut store = Store::open(&tbt_store::paths::db_path(&self.data)).unwrap();
        store.register_harness(id, display_name).unwrap();
        store.set_harness_roots(id, Some(roots)).unwrap();
    }

    pub fn collector_with(
        &self,
        adapters: Vec<(Arc<dyn HarnessAdapter>, Vec<PathBuf>)>,
        config: CollectorConfig,
    ) -> Collector {
        std::fs::create_dir_all(&self.data).unwrap();
        for (a, roots) in &adapters {
            self.set_roots(&a.id(), a.display_name(), roots);
        }
        let registry = Registry::new(adapters.into_iter().map(|(a, _)| a).collect());
        Collector::new(self.data.clone(), registry, config).with_price_seeder(seed_bundled_if_empty)
    }

    pub fn claude_collector(&self) -> Collector {
        self.collector_with(
            vec![(claude(), vec![self.root(CLAUDE_FIXTURES)])],
            CollectorConfig::default(),
        )
    }

    pub fn db(&self) -> Connection {
        Connection::open_with_flags(
            tbt_store::paths::db_path(&self.data),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }

    pub fn store(&self) -> Store {
        Store::open_read_only(&tbt_store::paths::db_path(&self.data)).unwrap()
    }

    pub fn session_events(&self) -> BTreeMap<String, i64> {
        let db = self.db();
        let mut stmt = db
            .prepare("SELECT ext_id, events FROM session WHERE events > 0")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    pub fn count(&self, sql: &str) -> i64 {
        self.db().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    pub fn session_link(&self, ext_id: &str, column: &str) -> Option<String> {
        self.db()
            .query_row(
                &format!(
                    "SELECT t.ext_id FROM session s LEFT JOIN session t ON t.id = s.{column}
                     WHERE s.ext_id = ?1"
                ),
                [ext_id],
                |r| r.get(0),
            )
            .unwrap()
    }
}

pub fn run(collector: &Collector) -> RunReport {
    match collector.ingest_run(RunTrigger::Manual).unwrap() {
        RunOutcome::Finished(r) => r,
        RunOutcome::Busy => panic!("collector unexpectedly busy"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Totals {
    pub events: i64,
    pub event_sums: [i64; 7],
    pub messages: i64,
    pub hourly: BTreeMap<(String, String, String, String), [i64; 7]>,
    pub chains: BTreeMap<String, [i64; 7]>,
    pub parents: BTreeSet<(String, String)>,
}

fn sums(r: &rusqlite::Row<'_>, from: usize) -> rusqlite::Result<[i64; 7]> {
    let mut out = [0; 7];
    for (i, v) in out.iter_mut().enumerate() {
        *v = r.get(from + i)?;
    }
    Ok(out)
}

pub fn totals(db: &Connection) -> Totals {
    let event_sums = db
        .query_row(
            "SELECT count(*), coalesce(sum(input_tokens),0), coalesce(sum(output_tokens),0),
                    coalesce(sum(cache_write_5m_tokens + cache_write_1h_tokens),0),
                    coalesce(sum(cache_read_tokens),0), coalesce(sum(cost_micros),0),
                    coalesce(sum(price_id IS NULL),0)
             FROM usage_event",
            [],
            |r| sums(r, 0),
        )
        .unwrap();

    let mut hourly = BTreeMap::new();
    let mut stmt = db
        .prepare(
            "SELECT h.hour, h.harness_id, h.model, coalesce(p.path, ''), h.events,
                    h.input_tokens, h.output_tokens, h.cache_write_tokens,
                    h.cache_read_tokens, h.cost_micros, h.unpriced_events
             FROM hourly_usage h JOIN project p ON p.id = h.project_id",
        )
        .unwrap();
    for row in stmt
        .query_map([], |r| {
            Ok(((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?), sums(r, 4)?))
        })
        .unwrap()
    {
        let (k, v) = row.unwrap();
        hourly.insert(k, v);
    }

    let mut stmt = db
        .prepare(
            "SELECT id, harness_id || ':' || ext_id, resumed_from_session_id, events,
                    input_tokens, output_tokens, cache_write_tokens, cache_read_tokens,
                    cost_micros, unpriced_events
             FROM session",
        )
        .unwrap();
    let rows: Vec<(i64, String, Option<i64>, [i64; 7])> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, sums(r, 3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut component: BTreeMap<i64, i64> = rows.iter().map(|r| (r.0, r.0)).collect();
    fn find(c: &mut BTreeMap<i64, i64>, x: i64) -> i64 {
        let p = c[&x];
        if p == x {
            return x;
        }
        let root = find(c, p);
        c.insert(x, root);
        root
    }
    for r in &rows {
        if let Some(to) = r.2 {
            let (a, b) = (find(&mut component, r.0), find(&mut component, to));
            component.insert(a, b);
        }
    }
    let mut members: BTreeMap<i64, Vec<&String>> = BTreeMap::new();
    for r in &rows {
        let root = find(&mut component, r.0);
        members.entry(root).or_default().push(&r.1);
    }
    let mut chains: BTreeMap<String, [i64; 7]> = BTreeMap::new();
    for r in &rows {
        let root = find(&mut component, r.0);
        let name = members[&root].iter().min().unwrap().to_string();
        let acc = chains.entry(name).or_default();
        for (a, v) in acc.iter_mut().zip(r.3) {
            *a += v;
        }
    }
    chains.retain(|_, v| v[0] > 0);

    let mut stmt = db
        .prepare(
            "SELECT s.ext_id, p.ext_id FROM session s JOIN session p ON p.id = s.parent_session_id",
        )
        .unwrap();
    let parents = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();

    Totals {
        events: event_sums[0],
        event_sums,
        messages: db
            .query_row("SELECT count(*) FROM message", [], |r| r.get(0))
            .unwrap(),
        hourly,
        chains,
        parents,
    }
}

pub fn assert_session_totals_consistent(db: &Connection) {
    let mismatches: i64 = db
        .query_row(
            "SELECT count(*) FROM session s LEFT JOIN (
                SELECT session_id, count(*) n, sum(input_tokens) i, sum(output_tokens) o,
                       sum(cache_write_5m_tokens + cache_write_1h_tokens) w,
                       sum(cache_read_tokens) r, sum(cost_micros) c
                FROM usage_event GROUP BY session_id) e ON e.session_id = s.id
             WHERE s.events != coalesce(e.n, 0) OR s.input_tokens != coalesce(e.i, 0)
                OR s.output_tokens != coalesce(e.o, 0) OR s.cache_write_tokens != coalesce(e.w, 0)
                OR s.cache_read_tokens != coalesce(e.r, 0) OR s.cost_micros != coalesce(e.c, 0)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(mismatches, 0, "session totals differ from their events");
}
