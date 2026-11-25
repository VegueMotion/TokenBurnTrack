#![cfg(feature = "goose")]

mod common;

use std::path::{Path, PathBuf};

use common::*;
use rusqlite::{Connection, params};
use tbt_adapters::goose::GooseAdapter;
use tbt_core::{Cursor, HarnessAdapter, ReadBatch, SourceKind, SourceRef, Timestamp};

const ROOT: &str = "goose/1.61.0/sessions";
const MAIN: &str = "20261120_1";
const SUB: &str = "20261120_2";
const LEGACY: &str = "20261018_3";
const UPGRADED: &str = "20261021_1";
const RELAY: &str = "20261122_2";
const ROUTED: &str = "20261123_1";

type SessionView<'a> = (&'a str, Option<&'a str>, Option<&'a str>, Option<&'a str>);

fn db() -> PathBuf {
    fixture(&format!("{ROOT}/sessions.db"))
}

fn read(db: &Path, cursor: &Cursor) -> ReadBatch {
    GooseAdapter::new()
        .read(&SourceRef::new(db, SourceKind::Sqlite), cursor)
        .unwrap()
}

fn tokens(batch: &ReadBatch, i: usize) -> (u64, u64, u64, u64) {
    let e = &batch.events[i];
    (
        e.input_tokens,
        e.output_tokens,
        e.cache_write_5m_tokens,
        e.cache_read_tokens,
    )
}

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn add_ledger(db: &Path, session: &str, model: Option<&str>, usage: [i64; 4], source: &str) {
    Connection::open(db)
        .unwrap()
        .execute(
            "INSERT INTO usage_ledger (session_id, created_timestamp, model, input_tokens,
             output_tokens, cache_read_tokens, cache_write_tokens, cost_source)
             VALUES (?1, 1795600000, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                session, model, usage[0], usage[1], usage[2], usage[3], source
            ],
        )
        .unwrap();
}

fn set_accumulated(db: &Path, session: &str, usage: [i64; 4]) {
    Connection::open(db)
        .unwrap()
        .execute(
            "UPDATE sessions SET accumulated_input_tokens = ?2, accumulated_output_tokens = ?3,
             accumulated_cache_read_tokens = ?4, accumulated_cache_write_tokens = ?5,
             updated_at = '2026-11-25 07:00:00' WHERE id = ?1",
            params![session, usage[0], usage[1], usage[2], usage[3]],
        )
        .unwrap();
}

#[test]
fn discovers_sessions_db_in_a_root_or_as_a_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("sessions.db"), "").unwrap();
    std::fs::write(dir.path().join("other.db"), "").unwrap();
    let adapter = GooseAdapter::new();
    let found = adapter
        .discover(&[dir.path().to_path_buf(), dir.path().join("missing")])
        .unwrap();
    assert_eq!(found.len(), 1);
    assert!(found[0].path.ends_with("sessions.db"));
    assert_eq!(adapter.discover(&[db()]).unwrap().len(), 1);
}

#[test]
fn golden_fixture() {
    let batch = read_all(&GooseAdapter::new(), &[fixture(ROOT)]);
    assert_unique_dedup_keys(&batch);
    assert!(batch.warnings.is_empty(), "{:?}", batch.warnings);

    assert_eq!(
        dedup_keys(&batch),
        [
            format!("{LEGACY}:total:12900"),
            "ledger:1".to_owned(),
            "ledger:2".to_owned(),
            "ledger:3".to_owned(),
            "ledger:4".to_owned(),
            "ledger:5".to_owned(),
            "ledger:7".to_owned(),
        ]
    );
    let models: Vec<(&str, &str, &str)> = batch
        .events
        .iter()
        .map(|e| {
            (
                e.session_ext_id.as_str(),
                e.provider.as_str(),
                e.model.as_str(),
            )
        })
        .collect();
    assert_eq!(
        models,
        [
            (LEGACY, "openai", "gpt-4.1"),
            (MAIN, "anthropic", "claude-sonnet-4-5-20250929"),
            (SUB, "ollama", "qwen3-coder:30b"),
            (MAIN, "anthropic", "claude-sonnet-4-5-20250929"),
            (UPGRADED, "openai", "gpt-4.1"),
            (UPGRADED, "openai", "gpt-4.1-2025-04-14"),
            (ROUTED, "openrouter", "z-ai/glm-4.6"),
        ]
    );
    assert_eq!(tokens(&batch, 0), (7904, 900, 0, 4096));
    assert_eq!(tokens(&batch, 1), (4700, 1200, 9600, 15700));
    assert_eq!(tokens(&batch, 3), (7900, 1700, 0, 26100));
    assert_eq!(tokens(&batch, 4), (3952, 500, 0, 2048));
    assert_eq!(batch.events[0].ts, ts("2026-10-18T14:05:09Z"));
    assert_eq!(batch.events[1].ts, ts("2026-11-20T09:20:05Z"));

    let costs: Vec<Option<String>> = batch
        .events
        .iter()
        .map(|e| e.reported_cost.map(|c| c.to_string()))
        .collect();
    assert_eq!(
        costs,
        [
            None,
            None,
            None,
            None,
            None,
            None,
            Some("0.00231".to_owned())
        ]
    );
    assert!(!batch.events.iter().any(|e| e.session_ext_id == RELAY));

    let sessions: Vec<SessionView> = batch
        .sessions
        .iter()
        .map(|s| {
            (
                s.ext_id.as_str(),
                s.parent_ext_id.as_deref(),
                s.title.as_deref(),
                s.project_path.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        sessions,
        [
            (
                LEGACY,
                None,
                Some("Duis aute irure"),
                Some("/home/user/projects/dolor")
            ),
            (
                UPGRADED,
                None,
                Some("Excepteur sint"),
                Some("/home/user/projects/sit")
            ),
            (
                MAIN,
                None,
                Some("Lorem ipsum refactor"),
                Some("/home/user/projects/lorem")
            ),
            (
                SUB,
                Some(MAIN),
                Some("Ut enim ad minim"),
                Some("/home/user/projects/lorem")
            ),
            (
                ROUTED,
                None,
                Some("Sunt in culpa"),
                Some("/home/user/projects/amet")
            ),
        ]
    );
    assert_eq!(
        batch.sessions[2].started_at,
        Some(ts("2026-11-20T09:15:00Z"))
    );
}

#[test]
fn rereading_returns_nothing_new() {
    let first = read(&db(), &Cursor::empty());
    assert_eq!(first.events.len(), 7);
    let again = read(&db(), &first.next_cursor);
    assert!(again.events.is_empty());
    assert!(again.sessions.is_empty());
    assert_eq!(again.next_cursor, first.next_cursor);
}

#[test]
fn new_ledger_rows_are_read_once() {
    let (_dir, db) = copy_to_temp(&db());
    let first = read(&db, &Cursor::empty());
    add_ledger(
        &db,
        MAIN,
        Some("claude-opus-4-5"),
        [500, 50, 0, 0],
        "estimated",
    );
    let second = read(&db, &first.next_cursor);
    assert_eq!(dedup_keys(&second), ["ledger:8"]);
    assert_eq!(tokens(&second, 0), (500, 50, 0, 0));
    assert_eq!(second.events[0].model, "claude-opus-4-5");
    assert_eq!(second.sessions.len(), 1);
    assert!(read(&db, &second.next_cursor).events.is_empty());
}

#[test]
fn legacy_session_counts_growth_and_is_not_recounted_once_upgraded() {
    let (_dir, db) = copy_to_temp(&db());
    let first = read(&db, &Cursor::empty());

    set_accumulated(&db, LEGACY, [15000, 1000, 5000, 0]);
    let second = read(&db, &first.next_cursor);
    assert_eq!(dedup_keys(&second), [format!("{LEGACY}:total:16000")]);
    assert_eq!(tokens(&second, 0), (2096, 100, 0, 904));

    add_ledger(&db, LEGACY, None, [16000, 1100, 5000, 0], "carried_forward");
    add_ledger(&db, LEGACY, Some("gpt-4.1"), [700, 30, 0, 0], "estimated");
    let third = read(&db, &second.next_cursor);
    assert_eq!(dedup_keys(&third), ["ledger:8", "ledger:9"]);
    assert_eq!(tokens(&third, 0), (1000, 100, 0, 0));
    assert_eq!(tokens(&third, 1), (700, 30, 0, 0));
    assert!(third.next_cursor.as_json()["totals"].get(LEGACY).is_none());
    assert!(read(&db, &third.next_cursor).events.is_empty());
}

#[test]
fn database_without_usage_ledger_reads_running_totals() {
    let (_dir, db) = copy_to_temp(&db());
    Connection::open(&db)
        .unwrap()
        .execute_batch("DROP TABLE usage_ledger;")
        .unwrap();
    let batch = read(&db, &Cursor::empty());
    let keys = dedup_keys(&batch);
    assert_eq!(
        keys,
        [
            format!("{LEGACY}:total:12900"),
            format!("{UPGRADED}:total:9700"),
            format!("{MAIN}:total:66900"),
            format!("{SUB}:total:2510"),
            format!("{ROUTED}:total:3600"),
        ]
    );
    assert_eq!(batch.events[2].ts, ts("2026-11-20T09:42:17Z"));
    assert_eq!(batch.events[2].model, "claude-sonnet-4-5");
}

#[test]
fn database_without_sessions_table_reads_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("sessions.db");
    Connection::open(&db)
        .unwrap()
        .execute_batch("CREATE TABLE other (a);")
        .unwrap();
    let batch = read(&db, &Cursor::empty());
    assert!(batch.events.is_empty() && batch.warnings.is_empty());
}
