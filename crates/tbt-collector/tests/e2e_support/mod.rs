use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, SystemTime};

use rusqlite::Connection;
use tbt_adapters::GeminiCliAdapter;
use tbt_adapters::gemini_cli::DEFAULT_FINALIZE_GRACE;
use tbt_collector::{
    Collector, CollectorConfig, Registry, RunOutcome, RunReport, RunTrigger, builtin_adapters,
};
use tbt_core::{HarnessAdapter, HarnessId, Micros, TimeRange, Timestamp};
use tbt_store::{BucketSize, QueryFilter};

use crate::common::*;

pub const LLM_PROXY: &str = "llm_proxy";
pub const IN_HOUSE: &str = "in_house_agent";

pub const BUILTIN_SETS: &[(HarnessId, &str)] = &[
    (HarnessId::CLAUDE_CODE, "claude_code"),
    (HarnessId::CODEX, "codex"),
    (HarnessId::OPENCODE, "opencode/1.19.5"),
    (HarnessId::GEMINI_CLI, "gemini_cli/0.71.0-nightly"),
    (HarnessId::CURSOR, "cursor/3.31.15/User/globalStorage"),
    (HarnessId::COPILOT_CLI, "copilot_cli/1.0.4/session-state"),
    (HarnessId::GROK_BUILD, "grok_build/1.0.45"),
    (HarnessId::ANTIGRAVITY, "antigravity/1.1.17"),
    (HarnessId::LMSTUDIO, "lmstudio/0.4.20/conversations"),
    (HarnessId::PI, "pi/v3"),
    (HarnessId::ZED, "zed/1.29.2/threads"),
    (HarnessId::GOOSE, "goose/1.61.0/sessions"),
];

pub const GENERIC_SETS: &[(&str, &str)] = &[
    (LLM_PROXY, "generic_jsonl/openai_proxy"),
    (IN_HOUSE, "generic_jsonl/anthropic_style"),
];

pub const GOLDEN: &[(&str, i64)] = &[
    ("antigravity", 5),
    ("claude_code", 204),
    ("codex", 381),
    ("copilot_cli", 6),
    ("cursor", 3),
    ("gemini_cli", 12),
    ("goose", 7),
    ("grok_build", 6),
    (IN_HOUSE, 4),
    (LLM_PROXY, 6),
    ("lmstudio", 4),
    ("opencode", 33),
    ("pi", 6),
    ("zed", 4),
];

pub const GOLDEN_TOTAL: u64 = 681;
pub const SOURCES: u64 = 43;

pub const KNOWN_WARNINGS: &[(&str, &str, u64, &str)] = &[
    (
        "grok_build",
        "usage.json",
        0,
        "turns[1] has no valid `endedAt`: \"\"",
    ),
    (
        "pi",
        "2026-11-20T09-00-00-000Z_3f1c2a9e-5b7d-4e21-9c0a-1d2e3f4a5b01.jsonl",
        11,
        "invalid JSON: EOF while parsing a string at line 1 column 63",
    ),
    (
        IN_HOUSE,
        "s-0001.jsonl",
        7,
        "missing required field(s): `dedup_key` field ($.requestId)",
    ),
    (
        IN_HOUSE,
        "s-0001.jsonl",
        8,
        "invalid timestamp \"yesterday\" at $.timestamp (expected rfc3339)",
    ),
    (
        LLM_PROXY,
        "2026-11-22.jsonl",
        5,
        "invalid JSON: EOF while parsing a value at line 1 column 24",
    ),
    (
        LLM_PROXY,
        "2026-11-22.jsonl",
        7,
        "missing required field(s): `model` ($.model)",
    ),
    (
        LLM_PROXY,
        "2026-11-22.jsonl",
        8,
        "`output_tokens` at $.usage.completion_tokens: expected a non-negative whole number, found \"n/a\"",
    ),
];

pub fn golden() -> BTreeMap<String, i64> {
    GOLDEN.iter().map(|(h, n)| (h.to_string(), *n)).collect()
}

pub fn known_warnings() -> BTreeSet<(String, String, u64, String)> {
    KNOWN_WARNINGS
        .iter()
        .map(|(h, f, l, m)| (h.to_string(), f.to_string(), *l, m.to_string()))
        .collect()
}

pub fn warning_set(report: &RunReport) -> BTreeSet<(String, String, u64, String)> {
    report
        .warnings
        .iter()
        .map(|w| {
            let file = Path::new(&w.source)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            (
                w.harness.to_string(),
                file,
                w.line.unwrap_or(0),
                w.message.clone(),
            )
        })
        .collect()
}

pub fn age_files(root: &Path, age: Duration) {
    let base = SystemTime::now() - age;
    for (i, file) in files_under(root).iter().enumerate() {
        set_mtime(file, base + Duration::from_secs(i as u64));
    }
}

pub fn set_mtime(file: &Path, at: SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(file)
        .unwrap()
        .set_modified(at)
        .unwrap();
}

pub fn mtime(file: &Path) -> Timestamp {
    Timestamp::try_from(std::fs::metadata(file).unwrap().modified().unwrap()).unwrap()
}

pub fn all_time() -> QueryFilter {
    QueryFilter::new(
        TimeRange::new(
            "2000-01-01T00:00:00Z".parse().unwrap(),
            "2100-01-01T00:00:00Z".parse().unwrap(),
        ),
        BucketSize::Hour,
    )
}

pub struct World {
    pub env: Env,
    pub clock: Arc<AtomicI64>,
}

impl World {
    pub fn new() -> World {
        World::with_db(None)
    }

    pub fn with_db(db: Option<&Path>) -> World {
        let mut sets: Vec<&str> = BUILTIN_SETS.iter().map(|(_, s)| *s).collect();
        sets.push("generic_jsonl");
        let env = Env::with_fixtures(&sets);
        age_files(&env.roots, Duration::from_secs(3 * 86_400));
        std::fs::create_dir_all(&env.data).unwrap();
        if let Some(db) = db {
            std::fs::copy(db, tbt_store::paths::db_path(&env.data)).unwrap();
        }
        for adapter in builtin_adapters(&CollectorConfig::default()) {
            let set = BUILTIN_SETS
                .iter()
                .find(|(id, _)| *id == adapter.id())
                .unwrap()
                .1;
            env.set_roots(&adapter.id(), adapter.display_name(), &[env.root(set)]);
        }
        let clock = Arc::new(AtomicI64::new(
            (Timestamp::now() + jiff::SignedDuration::from_hours(24)).as_millisecond(),
        ));
        World { env, clock }
    }

    pub fn config(&self) -> CollectorConfig {
        let text =
            std::fs::read_to_string(fixtures_root().join("generic_jsonl/config.toml")).unwrap();
        let mut doc: toml::Table = text.parse().unwrap();
        let Some(toml::Value::Array(entries)) = doc.remove("generic_jsonl") else {
            panic!("no [[generic_jsonl]] in the example config");
        };
        let generic_jsonl = entries
            .into_iter()
            .map(|mut entry| {
                let table = entry.as_table_mut().unwrap();
                let id = table["id"].as_str().unwrap().to_owned();
                let set = GENERIC_SETS.iter().find(|(g, _)| *g == id).unwrap().1;
                let root = self.env.root(set).to_string_lossy().into_owned();
                table.insert(
                    "roots".into(),
                    toml::Value::Array(vec![toml::Value::String(root)]),
                );
                entry
            })
            .collect();
        CollectorConfig {
            generic_jsonl,
            ..CollectorConfig::default()
        }
    }

    pub fn builtin(&self) -> Collector {
        Collector::builtin(self.env.data.clone(), self.config())
    }

    pub fn clocked(&self) -> Collector {
        let config = self.config();
        let clock = Arc::clone(&self.clock);
        let gemini: Arc<dyn HarnessAdapter> = Arc::new(GeminiCliAdapter::with_clock(
            DEFAULT_FINALIZE_GRACE,
            move || Timestamp::from_millisecond(clock.load(Ordering::SeqCst)).unwrap(),
        ));
        let adapters = builtin_adapters(&config)
            .into_iter()
            .map(|a| {
                if a.id() == HarnessId::GEMINI_CLI {
                    Arc::clone(&gemini)
                } else {
                    a
                }
            })
            .collect();
        Collector::new(self.env.data.clone(), Registry::new(adapters), config)
    }

    pub fn set_clock(&self, at: Timestamp) {
        self.clock.store(at.as_millisecond(), Ordering::SeqCst);
    }

    pub fn root(&self, set: &str) -> PathBuf {
        self.env.root(set)
    }

    pub fn db(&self) -> Connection {
        self.env.db()
    }

    pub fn per_harness(&self) -> BTreeMap<String, i64> {
        let db = self.db();
        let mut stmt = db
            .prepare("SELECT harness_id, count(*) FROM usage_event GROUP BY harness_id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    pub fn session_row(&self, harness: &str, ext_id: &str) -> SessionSnapshot {
        self.db()
            .query_row(
                "SELECT events, input_tokens, output_tokens, cache_write_tokens,
                        cache_read_tokens, cost_micros, unpriced_events, started_at, ended_at
                 FROM session WHERE harness_id = ?1 AND ext_id = ?2",
                [harness, ext_id],
                |r| {
                    Ok(SessionSnapshot {
                        sums: [
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                        ],
                        started_at: r.get(7)?,
                        ended_at: r.get(8)?,
                    })
                },
            )
            .unwrap()
    }

    pub fn assert_consistent(&self) {
        let db = self.db();
        assert_session_totals_consistent(&db);
        let t = totals(&db);
        let mut hourly_sums = [0i64; 7];
        for v in t.hourly.values() {
            for (acc, x) in hourly_sums.iter_mut().zip(v) {
                *acc += x;
            }
        }
        assert_eq!(hourly_sums, t.event_sums, "hourly_usage sums != event sums");
        assert_eq!(hourly_from_events(&db), hourly_rows(&db));

        let kpi = self.env.store().kpi_summary(&all_time()).unwrap();
        assert_eq!(kpi.events as i64, t.event_sums[0]);
        assert_eq!(kpi.tokens.input as i64, t.event_sums[1]);
        assert_eq!(kpi.tokens.output as i64, t.event_sums[2]);
        assert_eq!(kpi.tokens.cache_write as i64, t.event_sums[3]);
        assert_eq!(kpi.tokens.cache_read as i64, t.event_sums[4]);
        assert_eq!(kpi.cost, Micros(t.event_sums[5]));
        assert_eq!(kpi.unpriced_events as i64, t.event_sums[6]);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSnapshot {
    pub sums: [i64; 7],
    pub started_at: String,
    pub ended_at: Option<String>,
}

type HourKey = (String, String, String, i64);

fn hourly_rows(db: &Connection) -> BTreeMap<HourKey, [i64; 7]> {
    grouped(
        db,
        "SELECT substr(hour, 1, 13), harness_id, model, project_id, events, input_tokens,
                output_tokens, cache_write_tokens, cache_read_tokens, cost_micros, unpriced_events
         FROM hourly_usage WHERE events > 0",
    )
}

fn hourly_from_events(db: &Connection) -> BTreeMap<HourKey, [i64; 7]> {
    grouped(
        db,
        "SELECT substr(ts, 1, 13), harness_id, model, project_id, count(*), sum(input_tokens),
                sum(output_tokens), sum(cache_write_5m_tokens + cache_write_1h_tokens),
                sum(cache_read_tokens), sum(cost_micros), sum(price_id IS NULL)
         FROM usage_event GROUP BY 1, 2, 3, 4",
    )
}

fn grouped(db: &Connection, sql: &str) -> BTreeMap<HourKey, [i64; 7]> {
    let mut stmt = db.prepare(sql).unwrap();
    stmt.query_map([], |r| {
        let mut v = [0i64; 7];
        for (i, x) in v.iter_mut().enumerate() {
            *x = r.get(4 + i)?;
        }
        Ok(((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?), v))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

pub fn run_as(collector: &Collector, trigger: RunTrigger) -> RunReport {
    match collector.ingest_run(trigger).unwrap() {
        RunOutcome::Finished(r) => r,
        RunOutcome::Busy => panic!("collector unexpectedly busy"),
    }
}

pub fn assert_quiet_rerun(report: &RunReport) {
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.events_new, 0);
    assert_eq!(report.messages_new, 0);
    assert_eq!(report.sources_scanned, SOURCES);
    assert_eq!(report.sources_unchanged, SOURCES);
    assert_eq!(report.sources_read, 0);
    assert_eq!(report.sources_missing, 0);
}
