#![cfg(feature = "opencode")]

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use rusqlite::{Connection, params};
use tbt_adapters::opencode::OpenCodeAdapter;
use tbt_core::{
    Cursor, HarnessAdapter, HarnessId, MessageKind, MessageRole, ReadBatch, SourceKind, SourceRef,
    Timestamp,
};

const DB: &str = "opencode/1.19.5/opencode.db";
const PARENT_SESSION: &str = "ses_1859d49beffej0mYxVImsxvVj8";
const SUBAGENT_SESSION: &str = "ses_1859d3a5bffemujK39r99FD3lm";
const TASK_MESSAGE: &str = "msg_e7a62b66f001bm5eXk4J2eZzbJ";
const FIXTURE_EVENTS: usize = 33;

fn adapter() -> OpenCodeAdapter {
    OpenCodeAdapter::new()
}

fn src(path: &Path) -> SourceRef {
    SourceRef::new(path, SourceKind::Sqlite)
}

fn temp_db() -> (tempfile::TempDir, PathBuf) {
    copy_to_temp(&fixture(DB))
}

fn writer(path: &Path) -> Connection {
    Connection::open(path).unwrap()
}

fn now_ms() -> i64 {
    Timestamp::now().as_millisecond()
}

fn read(a: &OpenCodeAdapter, path: &Path, cursor: &Cursor) -> ReadBatch {
    a.read(&src(path), cursor).unwrap()
}

fn keys(batch: &ReadBatch) -> Vec<String> {
    dedup_keys(batch)
}

fn message_data(conn: &Connection, id: &str) -> String {
    conn.query_row("SELECT data FROM message WHERE id = ?1", [id], |r| r.get(0))
        .unwrap()
}

fn part_count(conn: &Connection, message_id: &str) -> usize {
    conn.query_row(
        "SELECT count(*) FROM part WHERE message_id = ?1 \
         AND json_extract(data, '$.type') IN ('text', 'reasoning', 'tool')",
        [message_id],
        |r| r.get::<_, i64>(0),
    )
    .unwrap() as usize
}

fn insert_assistant(conn: &Connection, session: &str, id: &str, ts: i64, completed: bool) {
    let time = if completed {
        serde_json::json!({"created": ts, "completed": ts + 5})
    } else {
        serde_json::json!({"created": ts})
    };
    let data = serde_json::json!({
        "role": "assistant", "providerID": "anthropic", "modelID": "claude-sonnet-4-5",
        "path": {"cwd": "/w/new", "root": "/w/new"}, "cost": 0.0123,
        "tokens": {"total": 1111, "input": 100, "output": 10, "reasoning": 1,
                   "cache": {"write": 200, "read": 800}},
        "time": time, "finish": "stop",
    });
    conn.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, session, ts, ts + 5, data.to_string()],
    )
    .unwrap();
    let parts = [
        (
            format!("{id}_p1"),
            serde_json::json!({"type": "step-start"}),
        ),
        (
            format!("{id}_p2"),
            serde_json::json!({"type": "text", "text": "hello there"}),
        ),
        (
            format!("{id}_p3"),
            serde_json::json!({"type": "tool", "tool": "bash", "callID": "c1",
                               "state": {"status": "completed", "input": {"command": "ls"}}}),
        ),
        (
            format!("{id}_p4"),
            serde_json::json!({"type": "step-finish", "tokens": data["tokens"], "cost": 0.0123}),
        ),
    ];
    for (i, (pid, pdata)) in parts.iter().enumerate() {
        let t = ts + i as i64;
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![pid, id, session, t, t, pdata.to_string()],
        )
        .unwrap();
    }
}

#[test]
fn golden_fixture() {
    let batch = read_file(&adapter(), &fixture(DB));
    assert_unique_dedup_keys(&batch);
    insta::assert_yaml_snapshot!("fixture", Projection::of(&batch));

    assert!(batch.warnings.is_empty(), "{:?}", batch.warnings);
    assert_eq!(batch.events.len(), FIXTURE_EVENTS);
    assert_eq!(batch.sessions.len(), 6);
    assert!(
        batch
            .events
            .iter()
            .all(|e| e.harness == HarnessId::OPENCODE)
    );
    assert!(batch.events.iter().all(|e| e.reported_cost.is_none()));

    let e = batch
        .events
        .iter()
        .find(|e| e.dedup_key == TASK_MESSAGE)
        .unwrap();
    assert_eq!(
        (
            e.provider.as_str(),
            e.model.as_str(),
            e.session_ext_id.as_str()
        ),
        ("opencode", "deepseek-v4-flash-free", PARENT_SESSION)
    );
    assert_eq!(
        (
            e.input_tokens,
            e.output_tokens,
            e.cache_write_5m_tokens,
            e.cache_write_1h_tokens,
            e.cache_read_tokens,
            e.reasoning_tokens,
        ),
        (8445, 293, 0, 0, 0, 95)
    );
    assert_eq!(e.tokens().total(), 8738);
    assert_eq!(e.ts.as_millisecond(), 1780169796910);
    assert_eq!(
        e.project_path.as_deref(),
        Some("/mnt/bold-juniper-8a/projects/lunar-birch-b1")
    );
    assert!(e.raw.as_ref().unwrap().get().contains("\"reasoning\""));

    let sub = batch
        .sessions
        .iter()
        .find(|s| s.ext_id == SUBAGENT_SESSION)
        .unwrap();
    assert_eq!(sub.parent_ext_id.as_deref(), Some(PARENT_SESSION));
    assert_eq!(sub.harness_version.as_deref(), Some("local"));

    let providers: BTreeSet<_> = batch.events.iter().map(|e| e.provider.as_str()).collect();
    assert_eq!(providers, BTreeSet::from(["ollama", "opencode"]));

    let task = batch
        .messages
        .iter()
        .find(|m| m.tool_name.as_deref() == Some("task"))
        .unwrap();
    assert_eq!(task.role, MessageRole::Assistant);
    assert_eq!(task.kind, Some(MessageKind::ToolUse));
    assert_eq!(task.event_dedup_key.as_deref(), Some(TASK_MESSAGE));
    assert!(task.key.starts_with("prt_"));
    assert!(
        batch
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::User)
            .all(|m| m.event_dedup_key.is_none())
    );
}

#[test]
fn totals_match_session_denormalized_counters() {
    let batch = read_file(&adapter(), &fixture(DB));
    let conn = Connection::open_with_flags(fixture(DB), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT id, tokens_input, tokens_output + tokens_reasoning, tokens_reasoning, \
             tokens_cache_read, tokens_cache_write FROM session",
        )
        .unwrap();
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                [1, 2, 3, 4, 5].map(|i| r.get::<_, i64>(i).unwrap() as u64),
            ))
        })
        .unwrap();
    for row in rows {
        let (session, expected) = row.unwrap();
        let mut got = [0u64; 5];
        for e in batch.events.iter().filter(|e| e.session_ext_id == session) {
            got[0] += e.input_tokens;
            got[1] += e.output_tokens;
            got[2] += e.reasoning_tokens;
            got[3] += e.cache_read_tokens;
            got[4] += e.cache_write_5m_tokens;
        }
        assert_eq!(got, expected, "{session}");
    }
}

#[test]
fn discover_finds_db_in_root_dir_or_as_file() {
    let a = adapter();
    let dir = fixture("opencode/1.19.5");
    let found = a
        .discover(&[
            dir.clone(),
            fixture(DB),
            fixtures_root().join("nope"),
            fixture("opencode"),
        ])
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].kind, SourceKind::Sqlite);
    assert!(found[0].path.ends_with("opencode/1.19.5/opencode.db"));
}

#[test]
fn reread_from_cursor_yields_nothing_new() {
    let a = adapter();
    let path = fixture(DB);
    let first = read(&a, &path, &Cursor::empty());
    assert_eq!(first.events.len(), FIXTURE_EVENTS);
    let again = read(&a, &path, &first.next_cursor);
    assert!(again.is_empty(), "{again:?}");
    assert_eq!(again.next_cursor, first.next_cursor);
}

#[test]
fn reading_does_not_modify_the_database() {
    let (dir, path) = temp_db();
    let before = std::fs::read(&path).unwrap();
    let a = adapter();
    let first = read(&a, &path, &Cursor::empty());
    read(&a, &path, &first.next_cursor);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["opencode.db"]);
}

#[test]
fn new_message_and_parts_are_read_alone() {
    let (_dir, path) = temp_db();
    let a = adapter();
    let first = read(&a, &path, &Cursor::empty());

    let ts = now_ms();
    insert_assistant(&writer(&path), PARENT_SESSION, "msg_new_1", ts, true);
    let next = read(&a, &path, &first.next_cursor);
    assert!(next.warnings.is_empty(), "{:?}", next.warnings);
    assert_eq!(keys(&next), ["msg_new_1"]);
    let e = &next.events[0];
    assert_eq!(e.provider, "anthropic");
    assert_eq!(
        (
            e.input_tokens,
            e.output_tokens,
            e.cache_write_5m_tokens,
            e.cache_read_tokens,
            e.reasoning_tokens
        ),
        (100, 11, 200, 800, 1)
    );
    assert_eq!(e.reported_cost, Some("0.0123".parse().unwrap()));
    assert_eq!(e.project_path.as_deref(), Some("/w/new"));
    assert_eq!(e.ts.as_millisecond(), ts + 5);

    let msg_keys: Vec<_> = next.messages.iter().map(|m| m.key.as_str()).collect();
    assert_eq!(msg_keys, ["msg_new_1_p2", "msg_new_1_p3"]);
    assert!(
        next.messages
            .iter()
            .all(|m| m.event_dedup_key.as_deref() == Some("msg_new_1"))
    );
    assert_eq!(next.messages[1].tool_name.as_deref(), Some("bash"));
    assert_eq!(next.sessions.len(), 1);
    assert_eq!(next.sessions[0].ext_id, PARENT_SESSION);

    assert!(read(&a, &path, &next.next_cursor).is_empty());
}

#[test]
fn updated_row_is_reread_once() {
    let (_dir, path) = temp_db();
    let a = adapter();
    let first = read(&a, &path, &Cursor::empty());

    let conn = writer(&path);
    conn.execute(
        "UPDATE message SET time_updated = ?1, data = json_set(data, '$.summary', json('true')) \
         WHERE id = ?2",
        params![now_ms(), TASK_MESSAGE],
    )
    .unwrap();
    let next = read(&a, &path, &first.next_cursor);
    assert_eq!(keys(&next), [TASK_MESSAGE]);
    assert!(next.messages.is_empty());
    let before = first
        .events
        .iter()
        .find(|e| e.dedup_key == TASK_MESSAGE)
        .unwrap();
    assert_eq!(next.events[0].tokens(), before.tokens());
    assert!(read(&a, &path, &next.next_cursor).is_empty());
}

#[test]
fn incomplete_message_is_held_back_until_completed() {
    let (_dir, path) = temp_db();
    let conn = writer(&path);
    let original = message_data(&conn, TASK_MESSAGE);
    let parts = part_count(&conn, TASK_MESSAGE);
    assert!(parts > 0);
    conn.execute(
        "UPDATE message SET time_updated = ?1, data = json_set(json_remove(data, \
         '$.time.completed', '$.finish'), '$.tokens', json('{\"input\":5,\"output\":0,\
         \"reasoning\":0,\"cache\":{\"read\":0,\"write\":0}}')) WHERE id = ?2",
        params![now_ms(), TASK_MESSAGE],
    )
    .unwrap();

    let a = adapter();
    let first = read(&a, &path, &Cursor::empty());
    assert_eq!(first.events.len(), FIXTURE_EVENTS - 1);
    assert!(!keys(&first).contains(&TASK_MESSAGE.to_owned()));
    assert!(first.warnings.is_empty());
    let held = read(&a, &path, &first.next_cursor);
    assert!(held.events.is_empty() && held.messages.is_empty());

    conn.execute(
        "UPDATE message SET time_updated = ?1, data = ?2 WHERE id = ?3",
        params![now_ms() + 1, original, TASK_MESSAGE],
    )
    .unwrap();
    let second = read(&a, &path, &held.next_cursor);
    assert_eq!(keys(&second), [TASK_MESSAGE]);
    assert_eq!(
        (
            second.events[0].input_tokens,
            second.events[0].output_tokens,
            second.events[0].reasoning_tokens
        ),
        (8445, 293, 95)
    );
    assert_eq!(second.messages.len(), parts);
    assert!(
        second
            .messages
            .iter()
            .all(|m| m.event_dedup_key.as_deref() == Some(TASK_MESSAGE))
    );
    assert!(read(&a, &path, &second.next_cursor).is_empty());

    let mut all = first;
    merge(&mut all, second);
    assert_unique_dedup_keys(&all);
    assert_eq!(all.events.len(), FIXTURE_EVENTS);
    let full = read_file(&a, &fixture(DB));
    let mut got: Vec<_> = all.messages.iter().map(|m| m.key.clone()).collect();
    let mut want: Vec<_> = full.messages.iter().map(|m| m.key.clone()).collect();
    got.sort();
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn stale_incomplete_message_is_counted_with_a_warning() {
    let (_dir, path) = temp_db();
    writer(&path)
        .execute(
            "UPDATE message SET data = json_remove(data, '$.time.completed') WHERE id = ?1",
            [TASK_MESSAGE],
        )
        .unwrap();
    let a = adapter().with_stale_after(Duration::from_secs(3600));
    let batch = read(&a, &path, &Cursor::empty());
    assert_eq!(batch.events.len(), FIXTURE_EVENTS);
    assert_eq!(batch.warnings.len(), 1);
    assert!(batch.warnings[0].message.contains("never completed"));
    assert!(read(&a, &path, &batch.next_cursor).is_empty());
}

#[test]
fn unknown_schema_version_warns_once_and_still_reads() {
    let (_dir, path) = temp_db();
    writer(&path)
        .execute(
            "INSERT INTO migration (id, time_completed) VALUES ('29990101000000_future', 1)",
            [],
        )
        .unwrap();
    let a = adapter();
    let first = read(&a, &path, &Cursor::empty());
    assert_eq!(first.events.len(), FIXTURE_EVENTS);
    assert_eq!(first.warnings.len(), 1);
    assert!(
        first.warnings[0]
            .message
            .contains("unknown schema version 29990101000000_future")
    );
    assert!(read(&a, &path, &first.next_cursor).is_empty());
}

#[test]
fn missing_migration_tables_warn() {
    let (_dir, path) = temp_db();
    writer(&path)
        .execute_batch("DROP TABLE migration; DROP TABLE __drizzle_migrations;")
        .unwrap();
    let batch = read(&adapter(), &path, &Cursor::empty());
    assert_eq!(batch.events.len(), FIXTURE_EVENTS);
    assert_eq!(batch.warnings.len(), 1);
    assert!(batch.warnings[0].message.contains("no schema version"));
}

#[test]
fn reads_wal_database_while_another_connection_writes() {
    let (dir, path) = temp_db();
    let owner = writer(&path);
    let mode: String = owner
        .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    owner.execute_batch("BEGIN IMMEDIATE").unwrap();
    insert_assistant(&owner, PARENT_SESSION, "msg_wal_1", now_ms(), true);

    let a = adapter().with_busy_timeout(Duration::from_millis(200));
    let first = read(&a, &path, &Cursor::empty());
    assert!(first.warnings.is_empty(), "{:?}", first.warnings);
    assert_eq!(first.events.len(), FIXTURE_EVENTS);

    owner.execute_batch("COMMIT").unwrap();
    let next = read(&a, &path, &first.next_cursor);
    assert_eq!(keys(&next), ["msg_wal_1"]);

    insert_assistant(&owner, PARENT_SESSION, "msg_wal_2", now_ms() + 10, true);
    let check: String = owner
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(check, "ok");
    assert_eq!(keys(&read(&a, &path, &next.next_cursor)), ["msg_wal_2"]);
    drop(owner);
    assert!(dir.path().join("opencode.db").exists());
}

#[test]
fn exclusive_lock_yields_a_warning_and_keeps_the_cursor() {
    let (_dir, path) = temp_db();
    let a = adapter().with_busy_timeout(Duration::from_millis(50));
    let first = read(&a, &path, &Cursor::empty());

    let owner = writer(&path);
    owner.execute_batch("BEGIN EXCLUSIVE").unwrap();
    insert_assistant(&owner, PARENT_SESSION, "msg_locked", now_ms(), true);

    for cursor in [Cursor::empty(), first.next_cursor.clone()] {
        let batch = read(&a, &path, &cursor);
        assert!(batch.events.is_empty() && batch.sessions.is_empty());
        assert_eq!(batch.warnings.len(), 1);
        assert!(batch.warnings[0].message.contains("locked"));
        assert_eq!(batch.next_cursor, cursor);
    }

    owner.execute_batch("COMMIT").unwrap();
    let next = read(&a, &path, &first.next_cursor);
    assert_eq!(keys(&next), ["msg_locked"]);
}

#[test]
fn invalid_cursor_restarts_with_a_warning() {
    let a = adapter();
    let bad = Cursor::from_json(serde_json::json!({"messages": "x"}));
    let batch = read(&a, &fixture(DB), &bad);
    assert_eq!(batch.events.len(), FIXTURE_EVENTS);
    assert_eq!(batch.warnings.len(), 1);
    assert!(batch.warnings[0].message.contains("invalid cursor"));
}

#[test]
fn missing_database_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("opencode.db");
    assert!(adapter().read(&src(&path), &Cursor::empty()).is_err());
    assert!(!path.exists());
}
