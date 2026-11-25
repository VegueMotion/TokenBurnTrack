#![cfg(feature = "zed")]

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use rusqlite::{Connection, params};
use tbt_adapters::zed::ZedAdapter;
use tbt_core::{Cursor, HarnessAdapter, ReadBatch, SourceKind, SourceRef, Timestamp};

const ROOT: &str = "zed/1.29.2/threads";
const MAIN: &str = "4f6c1d2a-8b3e-4a7f-9c1d-2e3f4a5b6c7d";
const SUB: &str = "7a8b9c0d-1e2f-4a3b-8c4d-5e6f7a8b9c0d";
const LOCAL: &str = "b1c2d3e4-f5a6-4b7c-8d9e-0f1a2b3c4d5e";
const JSON: &str = "c9d8e7f6-a5b4-4c3d-9e2f-1a0b9c8d7e6f";
const EMPTY: &str = "d0e1f2a3-b4c5-4d6e-8f7a-9b0c1d2e3f4a";

fn db() -> PathBuf {
    fixture(&format!("{ROOT}/threads.db"))
}

fn read(adapter: &ZedAdapter, db: &Path, cursor: &Cursor) -> ReadBatch {
    adapter
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

fn thread_json(input: u64, output: u64, model: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "title": "Lorem ipsum refactor",
        "messages": [],
        "updated_at": "2026-11-24T10:00:00Z",
        "cumulative_token_usage": {"input_tokens": input, "output_tokens": output},
        "model": {"provider": "zed.dev", "model": model},
        "version": "0.3.0",
    }))
    .unwrap()
}

fn save(db: &Path, id: &str, updated_at: &str, json: &[u8]) {
    let blob = ruzstd::encoding::compress_to_vec(json, ruzstd::encoding::CompressionLevel::Fastest);
    Connection::open(db)
        .unwrap()
        .execute(
            "UPDATE threads SET updated_at = ?2, data_type = 'zstd', data = ?3 WHERE id = ?1",
            params![id, updated_at, blob],
        )
        .unwrap();
}

#[test]
fn discovers_threads_db_in_a_root_or_as_a_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("threads.db"), "").unwrap();
    std::fs::write(dir.path().join("other.db"), "").unwrap();
    let adapter = ZedAdapter::new();
    let found = adapter
        .discover(&[dir.path().to_path_buf(), dir.path().join("missing")])
        .unwrap();
    assert_eq!(found.len(), 1);
    assert!(found[0].path.ends_with("threads.db"));
    assert_eq!(found[0].kind, SourceKind::Sqlite);
    assert_eq!(adapter.discover(&[db()]).unwrap().len(), 1);
}

#[test]
fn golden_fixture() {
    let batch = read_all(&ZedAdapter::new(), &[fixture(ROOT)]);
    assert_unique_dedup_keys(&batch);
    assert!(batch.warnings.is_empty(), "{:?}", batch.warnings);
    assert!(batch.messages.is_empty());

    let keys: Vec<&str> = batch.events.iter().map(|e| e.dedup_key.as_str()).collect();
    assert_eq!(
        keys,
        [
            format!("{SUB}:4880"),
            format!("{MAIN}:71790"),
            format!("{LOCAL}:2270"),
            format!("{JSON}:7848"),
        ]
    );
    let models: Vec<(&str, &str)> = batch
        .events
        .iter()
        .map(|e| (e.provider.as_str(), e.model.as_str()))
        .collect();
    assert_eq!(
        models,
        [
            ("anthropic", "claude-haiku-4-5"),
            ("anthropic", "claude-sonnet-4-5"),
            ("lmstudio", "deepseek-r1-distill-qwen-7b"),
            ("openai", "gpt-5"),
        ]
    );
    assert_eq!(tokens(&batch, 0), (3200, 480, 0, 1200));
    assert_eq!(tokens(&batch, 1), (18250, 2140, 9600, 41800));
    assert_eq!(tokens(&batch, 2), (950, 1320, 0, 0));
    assert_eq!(tokens(&batch, 3), (5000, 800, 0, 2048));
    assert_eq!(
        batch.events[1].ts,
        "2026-11-20T09:42:17.123456789Z"
            .parse::<Timestamp>()
            .unwrap()
    );

    let paths: Vec<Option<&str>> = batch
        .events
        .iter()
        .map(|e| e.project_path.as_deref())
        .collect();
    assert_eq!(
        paths,
        [
            Some("/home/user/projects/lorem"),
            Some("/home/user/projects/lorem"),
            Some("/home/user/projects/sit"),
            None,
        ]
    );

    let sessions: Vec<(&str, Option<&str>, Option<&str>)> = batch
        .sessions
        .iter()
        .map(|s| {
            (
                s.ext_id.as_str(),
                s.parent_ext_id.as_deref(),
                s.title.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        sessions,
        [
            (SUB, Some(MAIN), Some("Ut enim ad minim")),
            (MAIN, None, Some("Lorem ipsum refactor")),
            (LOCAL, None, Some("Duis aute irure")),
            (JSON, None, Some("Excepteur sint")),
        ]
    );
    assert_eq!(
        batch.sessions[1].started_at,
        Some("2026-11-20T09:15:00Z".parse().unwrap())
    );
    assert!(!batch.sessions.iter().any(|s| s.ext_id == EMPTY));
}

#[test]
fn rereading_returns_nothing_new() {
    let adapter = ZedAdapter::new();
    let first = read(&adapter, &db(), &Cursor::empty());
    assert_eq!(first.events.len(), 4);
    let again = read(&adapter, &db(), &first.next_cursor);
    assert!(again.events.is_empty());
    assert!(again.sessions.is_empty());
    assert_eq!(again.next_cursor, first.next_cursor);
}

#[test]
fn growth_of_a_thread_is_read_as_a_delta() {
    let (_dir, db) = copy_to_temp(&db());
    let adapter = ZedAdapter::new();
    let first = read(&adapter, &db, &Cursor::empty());

    save(
        &db,
        LOCAL,
        "2026-11-24T10:00:00+00:00",
        &thread_json(1500, 1700, "claude-opus-4-5"),
    );
    let second = read(&adapter, &db, &first.next_cursor);
    assert_eq!(dedup_keys(&second), [format!("{LOCAL}:3200")]);
    assert_eq!(tokens(&second, 0), (550, 380, 0, 0));
    assert_eq!(second.events[0].provider, "anthropic");
    assert_eq!(second.events[0].model, "claude-opus-4-5");
    assert_eq!(second.sessions.len(), 1);

    save(
        &db,
        LOCAL,
        "2026-11-24T10:05:00+00:00",
        &thread_json(1500, 1700, "claude-opus-4-5"),
    );
    let third = read(&adapter, &db, &second.next_cursor);
    assert!(third.events.is_empty());
    assert_eq!(third.next_cursor, second.next_cursor);

    save(
        &db,
        LOCAL,
        "2026-11-24T10:05:00+00:00",
        &thread_json(2100, 1700, "claude-opus-4-5"),
    );
    let fourth = read(&adapter, &db, &third.next_cursor);
    assert_eq!(dedup_keys(&fourth), [format!("{LOCAL}:3800")]);
    assert_eq!(tokens(&fourth, 0), (600, 0, 0, 0));
}

#[test]
fn deleted_threads_leave_the_cursor() {
    let (_dir, db) = copy_to_temp(&db());
    let adapter = ZedAdapter::new();
    let first = read(&adapter, &db, &Cursor::empty());
    Connection::open(&db)
        .unwrap()
        .execute("DELETE FROM threads WHERE id = ?1", [JSON])
        .unwrap();
    let second = read(&adapter, &db, &first.next_cursor);
    assert!(second.events.is_empty());
    let threads = second.next_cursor.as_json()["threads"]
        .as_object()
        .unwrap()
        .clone();
    assert_eq!(threads.len(), 4);
    assert!(!threads.contains_key(JSON));
}

#[test]
fn unreadable_thread_is_skipped_with_a_warning() {
    let (_dir, db) = copy_to_temp(&db());
    Connection::open(&db)
        .unwrap()
        .execute(
            "UPDATE threads SET data = X'00010203' WHERE id = ?1",
            [MAIN],
        )
        .unwrap();
    let batch = read(&ZedAdapter::new(), &db, &Cursor::empty());
    assert_eq!(batch.events.len(), 3);
    assert_eq!(batch.warnings.len(), 1, "{:?}", batch.warnings);
    assert!(batch.warnings[0].message.contains(MAIN));
}

#[test]
fn usage_without_a_model_is_kept_as_unknown() {
    let (_dir, db) = copy_to_temp(&db());
    let json = serde_json::to_vec(&serde_json::json!({
        "cumulative_token_usage": {"input_tokens": 10, "output_tokens": 5},
    }))
    .unwrap();
    save(&db, EMPTY, "2026-11-23T08:01:00+00:00", &json);
    let batch = read(&ZedAdapter::new(), &db, &Cursor::empty());
    let e = batch
        .events
        .iter()
        .find(|e| e.session_ext_id == EMPTY)
        .unwrap();
    assert_eq!((e.provider.as_str(), e.model.as_str()), ("zed", "unknown"));
    assert_eq!(batch.warnings.len(), 1);
}

#[test]
fn database_without_threads_table_reads_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("threads.db");
    Connection::open(&db)
        .unwrap()
        .execute_batch("CREATE TABLE other (a);")
        .unwrap();
    let batch = read(&ZedAdapter::new(), &db, &Cursor::empty());
    assert!(batch.events.is_empty() && batch.warnings.is_empty());
}

#[test]
fn old_schema_without_added_columns_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("threads.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE threads (id TEXT PRIMARY KEY, summary TEXT NOT NULL,
         updated_at TEXT NOT NULL, data_type TEXT NOT NULL, data BLOB NOT NULL);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO threads VALUES ('t', 'x', '2026-01-02T03:04:05+00:00', 'json', ?1)",
        [thread_json(7, 3, "gpt-5")],
    )
    .unwrap();
    let batch = read(&ZedAdapter::new(), &db, &Cursor::empty());
    assert_eq!(dedup_keys(&batch), ["t:10"]);
    assert_eq!(batch.events[0].project_path, None);
}

#[test]
fn locked_database_is_retried_later() {
    let (_dir, db) = copy_to_temp(&db());
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch("PRAGMA locking_mode = EXCLUSIVE; BEGIN EXCLUSIVE;")
        .unwrap();
    let adapter = ZedAdapter::new().with_busy_timeout(Duration::from_millis(10));
    let batch = read(&adapter, &db, &Cursor::empty());
    assert!(batch.events.is_empty());
    assert_eq!(batch.next_cursor, Cursor::empty());
    assert!(batch.warnings[0].message.contains("locked"));
}
