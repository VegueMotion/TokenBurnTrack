mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::*;
use serde::{Deserialize, Serialize};
use tbt_collector::CollectorConfig;
use tbt_core::{
    BaselineHint, Cursor, HarnessAdapter, HarnessId, ReadBatch, Result, SessionUpsert, SourceKind,
    SourceRef, TokenCounts, UsageEvent,
};

const COUNTER: HarnessId = HarnessId::from_static("counter");

struct CounterAdapter;

#[derive(Deserialize)]
struct Line {
    session: String,
    #[serde(default)]
    resumed_from: Option<String>,
    key: String,
    ts: tbt_core::Timestamp,
    input: u64,
    output: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    lines: usize,
    len: u64,
    last: Option<TokenCounts>,
}

impl HarnessAdapter for CounterAdapter {
    fn id(&self) -> HarnessId {
        COUNTER
    }
    fn display_name(&self) -> &str {
        "Counter"
    }
    fn default_roots(&self) -> Vec<PathBuf> {
        Vec::new()
    }
    fn discover(&self, roots: &[PathBuf]) -> Result<Vec<SourceRef>> {
        Ok(tbt_adapters::discover::sources_with_extension(
            roots,
            "cum",
            SourceKind::JsonlFile,
        ))
    }
    fn read(&self, src: &SourceRef, cursor: &Cursor) -> Result<ReadBatch> {
        let mut state: State = cursor.state_or_default()?;
        let text =
            std::fs::read_to_string(&src.path).map_err(|e| tbt_core::Error::io(&src.path, e))?;
        if text.len() as u64 == state.len {
            return Ok(ReadBatch::unchanged(cursor.clone()));
        }
        let mut batch = ReadBatch::default();
        let mut first_event_key = None;
        let mut session: Option<SessionUpsert> = None;
        for raw in text.lines().skip(state.lines) {
            let line: Line = serde_json::from_str(raw)?;
            let total = TokenCounts {
                input: line.input,
                output: line.output,
                ..TokenCounts::default()
            };
            let delta = match &state.last {
                Some(prev) => total.delta_since(prev),
                None => {
                    first_event_key = Some(line.key.clone());
                    total
                }
            };
            state.last = Some(total);
            state.lines += 1;
            let s = session.get_or_insert_with(|| SessionUpsert::new(COUNTER, &line.session));
            s.resumed_from_ext_id = line.resumed_from.clone().or(s.resumed_from_ext_id.take());
            s.started_at = Some(s.started_at.map_or(line.ts, |t| t.min(line.ts)));
            s.ended_at = Some(s.ended_at.map_or(line.ts, |t| t.max(line.ts)));
            let mut e = UsageEvent {
                harness: COUNTER,
                dedup_key: line.key,
                session_ext_id: line.session,
                ts: line.ts,
                provider: "openai".into(),
                model: "gpt-test".into(),
                input_tokens: 0,
                output_tokens: 0,
                cache_write_5m_tokens: 0,
                cache_write_1h_tokens: 0,
                cache_read_tokens: 0,
                reasoning_tokens: 0,
                reported_cost: None,
                project_path: Some("/w".into()),
                git_branch: None,
                raw: None,
            };
            e.set_tokens(delta);
            batch.events.push(e);
        }
        if let Some(mut s) = session {
            s.baseline_hint = state.last.map(|last_total| BaselineHint {
                first_event_key,
                last_total,
            });
            batch.sessions.push(s);
        }
        state.len = text.len() as u64;
        batch.next_cursor = Cursor::from_state(&state)?;
        Ok(batch)
    }
}

fn line(
    session: &str,
    resumed_from: Option<&str>,
    key: &str,
    minute: u32,
    input: u64,
    output: u64,
) -> String {
    let resumed = resumed_from
        .map(|r| format!(r#","resumed_from":"{r}""#))
        .unwrap_or_default();
    format!(
        r#"{{"session":"{session}"{resumed},"key":"{key}","ts":"2026-11-23T10:{minute:02}:00Z","input":{input},"output":{output}}}"#
    ) + "\n"
}

fn write(path: &Path, lines: &[String]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, lines.concat()).unwrap();
}

fn original(env: &Env) -> PathBuf {
    let path = env.roots.join("a.cum");
    write(
        &path,
        &[
            line("A", None, "k1", 1, 100, 10),
            line("A", None, "k2", 2, 250, 20),
            line("A", None, "k3", 3, 400, 40),
        ],
    );
    path
}

fn collector(env: &Env, roots: Vec<PathBuf>) -> tbt_collector::Collector {
    env.collector_with(
        vec![(Arc::new(CounterAdapter) as Arc<dyn HarnessAdapter>, roots)],
        CollectorConfig::default(),
    )
}

fn tokens(env: &Env, key: &str) -> (i64, i64) {
    env.db()
        .query_row(
            "SELECT input_tokens, output_tokens FROM usage_event WHERE dedup_key = ?1",
            [key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

fn session_input(env: &Env, ext: &str) -> i64 {
    env.db()
        .query_row(
            "SELECT input_tokens FROM session WHERE ext_id = ?1",
            [ext],
            |r| r.get(0),
        )
        .unwrap()
}

fn cumulative(env: &Env, ext: &str) -> Option<TokenCounts> {
    let json: Option<String> = env
        .db()
        .query_row(
            "SELECT cumulative_json FROM session WHERE ext_id = ?1",
            [ext],
            |r| r.get(0),
        )
        .unwrap();
    json.map(|j| serde_json::from_str(&j).unwrap())
}

#[test]
fn resumed_file_is_rebased_on_the_original_total() {
    let env = Env::new();
    let a = original(&env);
    run(&collector(&env, vec![a.clone()]));
    assert_eq!(cumulative(&env, "A").unwrap().input, 400);

    let b = env.roots.join("b.cum");
    write(
        &b,
        &[
            line("B", Some("A"), "k4", 10, 450, 45),
            line("B", Some("A"), "k5", 11, 500, 60),
        ],
    );
    let c = collector(&env, vec![a, b.clone()]);
    run(&c);
    assert_eq!(tokens(&env, "k4"), (50, 5));
    assert_eq!(tokens(&env, "k5"), (50, 15));
    assert_eq!(session_input(&env, "A") + session_input(&env, "B"), 500);
    assert_eq!(
        env.session_link("B", "resumed_from_session_id").as_deref(),
        Some("A")
    );
    assert_eq!(cumulative(&env, "B").unwrap().input, 500);
    assert_session_totals_consistent(&env.db());

    append(&b, &line("B", Some("A"), "k6", 12, 600, 70));
    run(&c);
    assert_eq!(tokens(&env, "k6"), (100, 10));
    assert_eq!(cumulative(&env, "B").unwrap().input, 600);
}

#[test]
fn counter_reset_in_resumed_file_counts_in_full() {
    let env = Env::new();
    let a = original(&env);
    run(&collector(&env, vec![a.clone()]));
    let b = env.roots.join("b.cum");
    write(&b, &[line("B", Some("A"), "k4", 10, 30, 50)]);
    run(&collector(&env, vec![a, b]));
    assert_eq!(tokens(&env, "k4"), (30, 50));
}

#[test]
fn copied_history_links_by_dedup_key_without_explicit_id() {
    let env = Env::new();
    let a = original(&env);
    run(&collector(&env, vec![a.clone()]));
    let b = env.roots.join("b.cum");
    write(
        &b,
        &[
            line("B", None, "k3", 3, 400, 40),
            line("B", None, "k4", 10, 450, 45),
        ],
    );
    let report = run(&collector(&env, vec![a, b]));
    assert_eq!(report.events_new, 1);
    assert_eq!(
        env.session_link("B", "resumed_from_session_id").as_deref(),
        Some("A")
    );
    assert_eq!(tokens(&env, "k4"), (50, 5));
    assert_eq!(tokens(&env, "k3"), (150, 20));
    let started: String = env
        .db()
        .query_row(
            "SELECT started_at FROM session WHERE ext_id = 'B'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(started.starts_with("2026-11-23T10:10:00"), "{started}");
}

#[test]
fn resumed_file_before_its_original_is_not_rebased() {
    let env = Env::new();
    let b = env.roots.join("b.cum");
    write(&b, &[line("B", Some("A"), "k4", 10, 450, 45)]);
    run(&collector(&env, vec![b.clone()]));
    assert_eq!(tokens(&env, "k4"), (450, 45));
    assert_eq!(
        env.session_link("B", "resumed_from_session_id").as_deref(),
        Some("A")
    );

    let a = original(&env);
    run(&collector(&env, vec![a, b]));
    assert_eq!(session_input(&env, "A"), 400);
    assert_eq!(env.session_link("A", "resumed_from_session_id"), None);
    assert_eq!(cumulative(&env, "A").unwrap().input, 400);
}
