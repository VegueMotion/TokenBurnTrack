#![cfg(feature = "antigravity")]

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use rusqlite::{Connection, params};
use tbt_adapters::antigravity::AntigravityAdapter;
use tbt_core::{Cursor, HarnessAdapter, ReadBatch, SourceKind, SourceRef, Timestamp};

const ROOT: &str = "antigravity/1.1.17";
const APP: &str = "5c1e9a7b-3d2f-4e6a-8b0c-1f2e3d4c5b6a";
const CLI: &str = "8e2d4c6b-1a3f-4b5e-9c7d-0e1f2a3b4c5d";
const T0: i64 = 1_795_168_800;

fn app_db() -> PathBuf {
    fixture(&format!("{ROOT}/antigravity/conversations/{APP}.db"))
}

fn tokens(batch: &ReadBatch, i: usize) -> (u64, u64, u64, u64, u64) {
    let e = &batch.events[i];
    (
        e.input_tokens,
        e.output_tokens,
        e.cache_write_5m_tokens,
        e.cache_read_tokens,
        e.reasoning_tokens,
    )
}

fn varint(mut n: u64, out: &mut Vec<u8>) {
    while n >= 0x80 {
        out.push((n as u8) | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
}

fn field_varint(no: u64, n: u64, out: &mut Vec<u8>) {
    varint(no << 3, out);
    varint(n, out);
}

fn field_bytes(no: u64, b: &[u8], out: &mut Vec<u8>) {
    varint((no << 3) | 2, out);
    varint(b.len() as u64, out);
    out.extend_from_slice(b);
}

fn generation(label: &str, input: u64, output: u64, response_id: &str, ts: i64) -> Vec<u8> {
    let mut usage = Vec::new();
    field_varint(2, input, &mut usage);
    field_varint(3, output, &mut usage);
    field_varint(10, output, &mut usage);
    if !response_id.is_empty() {
        field_bytes(11, response_id.as_bytes(), &mut usage);
    }
    let mut chat = Vec::new();
    field_bytes(21, label.as_bytes(), &mut chat);
    field_bytes(4, &usage, &mut chat);
    let mut stamp = Vec::new();
    field_varint(1, ts as u64, &mut stamp);
    let mut meta = Vec::new();
    field_bytes(4, &stamp, &mut meta);
    let mut data = Vec::new();
    field_bytes(1, &chat, &mut data);
    field_bytes(9, &meta, &mut data);
    data
}

fn insert(db: &Path, idx: i64, data: &[u8]) {
    let conn = Connection::open(db).unwrap();
    conn.execute(
        "INSERT INTO gen_metadata VALUES (?1, ?2, ?3)",
        params![idx, data, data.len() as i64],
    )
    .unwrap();
}

fn update(db: &Path, idx: i64, data: &[u8]) {
    let conn = Connection::open(db).unwrap();
    conn.execute(
        "UPDATE gen_metadata SET data = ?2 WHERE idx = ?1",
        params![idx, data],
    )
    .unwrap();
}

fn read(adapter: &AntigravityAdapter, db: &Path, cursor: &Cursor) -> ReadBatch {
    adapter
        .read(&SourceRef::new(db, SourceKind::Sqlite), cursor)
        .unwrap()
}

#[test]
fn discovers_conversation_databases_only() {
    let dir = tempfile::tempdir().unwrap();
    let conv = dir.path().join("conversations");
    std::fs::create_dir_all(&conv).unwrap();
    for f in ["a.db", "a.db-wal", "a.db-shm", "a.pb"] {
        std::fs::write(conv.join(f), "").unwrap();
    }
    let found = AntigravityAdapter::new()
        .discover(&[conv.clone(), dir.path().join("missing")])
        .unwrap();
    assert_eq!(found.len(), 1);
    assert!(found[0].path.ends_with("conversations/a.db"));
    assert_eq!(found[0].kind, SourceKind::Sqlite);
}

#[test]
fn golden_fixture() {
    let batch = read_all(&AntigravityAdapter::new(), &[fixture(ROOT)]);
    assert_unique_dedup_keys(&batch);
    assert!(batch.warnings.is_empty(), "{:?}", batch.warnings);
    assert!(batch.messages.is_empty());

    let keys: Vec<&str> = batch.events.iter().map(|e| e.dedup_key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "resp_9Kp4yD4",
            "resp_9Kp4yE5",
            "resp_7Qm2xA1",
            "resp_7Qm2xB2",
            "resp_7Qm2xC3"
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
            ("google", "gemini-3.1-pro-preview"),
            ("google", "gemini-3.1-pro-preview"),
            ("google", "gemini-3.6-flash"),
            ("google", "gemini-3.6-flash"),
            ("anthropic", "claude-sonnet-4-5"),
        ]
    );
    assert_eq!(tokens(&batch, 0), (4000, 1000, 0, 2500, 700));
    assert_eq!(tokens(&batch, 1), (250, 80, 0, 4000, 0));
    assert_eq!(tokens(&batch, 2), (1200, 450, 0, 0, 300));
    assert_eq!(tokens(&batch, 3), (300, 200, 0, 1100, 50));
    assert_eq!(tokens(&batch, 4), (2000, 600, 1500, 0, 200));

    let ts: Vec<Timestamp> = batch.events.iter().map(|e| e.ts).collect();
    let at = |s: i64| Timestamp::from_second(s).unwrap();
    assert_eq!(ts[0], at(T0 + 86_400 + 20));
    assert_eq!(ts[1], at(T0 + 86_400));
    assert_eq!(ts[2], Timestamp::new(T0 + 10, 500_000_000).unwrap());
    assert_eq!(ts[3], at(T0 + 40));
    assert_eq!(ts[4], at(T0 + 95));

    for e in &batch.events {
        let want = if e.session_ext_id == APP {
            "/u/user/pale fjord-8a"
        } else {
            assert_eq!(e.session_ext_id, CLI);
            "/u/user/keen-lagoon-45"
        };
        assert_eq!(e.project_path.as_deref(), Some(want));
    }
    let mut sessions: Vec<(&str, Option<Timestamp>)> = batch
        .sessions
        .iter()
        .map(|s| (s.ext_id.as_str(), s.started_at))
        .collect();
    sessions.sort();
    assert_eq!(
        sessions,
        [(APP, Some(at(T0))), (CLI, Some(at(T0 + 86_400)))]
    );
}

#[test]
fn rereading_returns_nothing_new() {
    let adapter = AntigravityAdapter::new();
    let first = read(&adapter, &app_db(), &Cursor::empty());
    assert_eq!(first.events.len(), 3);
    let again = read(&adapter, &app_db(), &first.next_cursor);
    assert!(again.events.is_empty());
    assert!(again.sessions.is_empty());
    assert_eq!(again.next_cursor, first.next_cursor);
}

#[test]
fn new_generations_are_read_incrementally() {
    let (_dir, db) = copy_to_temp(&app_db());
    let adapter = AntigravityAdapter::new();
    let first = read(&adapter, &db, &Cursor::empty());
    insert(
        &db,
        4,
        &generation("Gemini 3.6 Flash (Low)", 10, 5, "resp_new", T0 + 200),
    );
    let next = read(&adapter, &db, &first.next_cursor);
    assert_eq!(dedup_keys(&next), ["resp_new"]);
    assert_eq!(next.sessions.len(), 1);
}

#[test]
fn fresh_empty_generation_is_held_until_its_tokens_arrive() {
    let (_dir, db) = copy_to_temp(&app_db());
    let now = Timestamp::now().as_second();
    insert(
        &db,
        4,
        &generation("Gemini 3.6 Flash (High)", 0, 0, "", now),
    );
    insert(
        &db,
        5,
        &generation("Gemini 3.6 Flash (High)", 7, 3, "resp_after", now),
    );
    let adapter = AntigravityAdapter::new();
    let first = read(&adapter, &db, &Cursor::empty());
    assert_eq!(first.events.len(), 4);
    assert_eq!(
        first.next_cursor.as_json(),
        &serde_json::json!({"next_idx": 4, "done": [5]})
    );

    update(
        &db,
        4,
        &generation("Gemini 3.6 Flash (High)", 40, 9, "resp_late", now),
    );
    let second = read(&adapter, &db, &first.next_cursor);
    assert_eq!(dedup_keys(&second), ["resp_late"]);
    assert_eq!(
        second.next_cursor.as_json(),
        &serde_json::json!({"next_idx": 6})
    );
}

#[test]
fn stale_empty_generation_is_not_held() {
    let (_dir, db) = copy_to_temp(&app_db());
    let now = Timestamp::now().as_second();
    insert(
        &db,
        4,
        &generation("Gemini 3.6 Flash (High)", 0, 0, "", now - 60),
    );
    let adapter = AntigravityAdapter::new().with_stale_after(Duration::from_secs(30));
    let batch = read(&adapter, &db, &Cursor::empty());
    assert_eq!(
        batch.next_cursor.as_json(),
        &serde_json::json!({"next_idx": 5})
    );
}

#[test]
fn replaced_database_is_read_from_the_start() {
    let (_dir, db) = copy_to_temp(&app_db());
    let adapter = AntigravityAdapter::new();
    let stale = Cursor::from_json(serde_json::json!({"next_idx": 40}));
    assert_eq!(read(&adapter, &db, &stale).events.len(), 3);
}

#[test]
fn tokens_without_response_id_or_model_are_skipped_with_a_warning() {
    let (_dir, db) = copy_to_temp(&app_db());
    insert(&db, 4, &generation("Gemini 3.6 Flash (High)", 5, 5, "", T0));
    insert(&db, 5, &generation("", 5, 5, "resp_nomodel", T0));
    let batch = read(&AntigravityAdapter::new(), &db, &Cursor::empty());
    assert_eq!(batch.events.len(), 3);
    assert_eq!(batch.warnings.len(), 2, "{:?}", batch.warnings);
    assert!(batch.warnings[0].message.contains("no response id"));
    assert!(batch.warnings[1].message.contains("no model name"));
}

#[test]
fn database_without_tables_reads_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("x.db");
    Connection::open(&db)
        .unwrap()
        .execute_batch("CREATE TABLE other (a);")
        .unwrap();
    let batch = read(&AntigravityAdapter::new(), &db, &Cursor::empty());
    assert!(batch.events.is_empty() && batch.warnings.is_empty());
}

#[test]
fn locked_database_is_retried_later() {
    let (_dir, db) = copy_to_temp(&app_db());
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch("PRAGMA locking_mode = EXCLUSIVE; BEGIN EXCLUSIVE;")
        .unwrap();
    let adapter = AntigravityAdapter::new().with_busy_timeout(Duration::from_millis(10));
    let batch = read(&adapter, &db, &Cursor::empty());
    assert!(batch.events.is_empty());
    assert_eq!(batch.next_cursor, Cursor::empty());
    assert!(batch.warnings[0].message.contains("locked"));
}
