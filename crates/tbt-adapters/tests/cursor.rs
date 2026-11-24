#![cfg(feature = "cursor")]

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tbt_adapters::cursor::CursorAdapter;
use tbt_core::{
    Cursor, HarnessAdapter, MessageKind, MessageRole, ReadBatch, SourceKind, SourceRef, Timestamp,
};

const COMPOSER: &str = "6f1c2a9e-0b7d-4e5a-9c3f-2d8e1b4a7c60";
const OTHER_COMPOSER: &str = "9a8b7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d";
const WORKSPACE: &str = "1785802725457";
const T0: i64 = 1_795_000_000_000;

struct Fixture {
    _dir: tempfile::TempDir,
    db: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("User");
        let global = user.join("globalStorage");
        let ws = user.join("workspaceStorage").join(WORKSPACE);
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(
            ws.join("workspace.json"),
            r#"{"folder": "file:///home/u/projects/keen%20lagoon"}"#,
        )
        .unwrap();
        let db = global.join("state.vscdb");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
             CREATE TABLE cursorDiskKV (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
             CREATE TABLE composerHeaders (composerId TEXT PRIMARY KEY, workspaceId TEXT,
               createdAt INTEGER, lastUpdatedAt INTEGER, isArchived INTEGER, isSubagent INTEGER,
               recency INTEGER, checkpointAt INTEGER, value TEXT);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO composerHeaders (composerId, workspaceId, createdAt) VALUES (?1, ?2, ?3)",
            params![COMPOSER, WORKSPACE, T0],
        )
        .unwrap();
        let f = Fixture { _dir: dir, db };
        f.put("composerVirtualRowHeights:_recentIds", &json!({}));
        f.composer(COMPOSER, "Fix the flaky test", &[]);
        f.bubble(
            COMPOSER,
            "b-user-1",
            &user_bubble("why does it fail?", 1_000),
        );
        f.bubble(
            COMPOSER,
            "b-asst-1",
            &assistant_bubble("claude-4.5-sonnet", 1200, 300, 2_000),
        );
        f.bubble(
            COMPOSER,
            "b-tool-1",
            &json!({
                "_v": 3, "type": 2, "createdAt": iso(3_000), "text": "",
                "toolFormerData": {"tool": 15, "name": "read_file",
                                   "params": "{\"targetFile\":\"src/lib.rs\"}"},
                "tokenCount": {"inputTokens": 0, "outputTokens": 0},
            }),
        );
        f.bubble(
            COMPOSER,
            "b-asst-2",
            &json!({
                "_v": 3, "type": 2, "createdAt": iso(4_000), "text": "",
                "thinking": {"text": "consider the race"},
                "tokenCount": {"inputTokens": 800, "outputTokens": 90},
            }),
        );
        f
    }

    fn conn(&self) -> Connection {
        Connection::open(&self.db).unwrap()
    }

    fn put(&self, key: &str, value: &Value) {
        self.conn()
            .execute(
                "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
                params![key, value.to_string()],
            )
            .unwrap();
    }

    fn composer(&self, id: &str, name: &str, generating: &[&str]) {
        self.put(
            &format!("composerData:{id}"),
            &json!({
                "_v": 17, "composerId": id, "name": name, "createdAt": T0,
                "lastUpdatedAt": T0 + 5_000, "status": "completed",
                "generatingBubbleIds": generating,
                "modelConfig": {"modelName": "gpt-5", "maxMode": false},
                "fullConversationHeadersOnly": [],
            }),
        );
    }

    fn bubble(&self, composer: &str, id: &str, value: &Value) {
        self.put(&format!("bubbleId:{composer}:{id}"), value);
    }

    fn src(&self) -> SourceRef {
        SourceRef::new(&self.db, SourceKind::Sqlite)
    }
}

fn iso(offset_ms: i64) -> String {
    Timestamp::from_millisecond(T0 + offset_ms)
        .unwrap()
        .to_string()
}

fn user_bubble(text: &str, offset_ms: i64) -> Value {
    json!({"_v": 3, "type": 1, "text": text, "createdAt": iso(offset_ms)})
}

fn assistant_bubble(model: &str, input: u64, output: u64, offset_ms: i64) -> Value {
    json!({
        "_v": 3, "type": 2, "text": "the fixture races", "createdAt": iso(offset_ms),
        "modelInfo": {"modelName": model},
        "tokenCount": {"inputTokens": input, "outputTokens": output},
    })
}

fn read(f: &Fixture, cursor: &Cursor) -> ReadBatch {
    CursorAdapter::new().read(&f.src(), cursor).unwrap()
}

#[test]
fn reads_sessions_events_and_messages() {
    let f = Fixture::new();
    let batch = read(&f, &Cursor::empty());
    assert!(batch.warnings.is_empty(), "{:?}", batch.warnings);

    assert_eq!(batch.sessions.len(), 1);
    let s = &batch.sessions[0];
    assert_eq!(s.ext_id, COMPOSER);
    assert_eq!(s.title.as_deref(), Some("Fix the flaky test"));
    assert_eq!(
        s.project_path.as_deref(),
        Some("/home/u/projects/keen lagoon")
    );
    assert_eq!(s.started_at, Timestamp::from_millisecond(T0).ok());

    assert_eq!(dedup_keys(&batch), ["b-asst-1", "b-asst-2"]);
    let e = &batch.events[0];
    assert_eq!(
        (e.provider.as_str(), e.model.as_str()),
        ("anthropic", "claude-sonnet-4-5")
    );
    assert_eq!((e.input_tokens, e.output_tokens), (1200, 300));
    assert_eq!(e.ts.as_millisecond(), T0 + 2_000);
    assert_eq!(
        e.project_path.as_deref(),
        Some("/home/u/projects/keen lagoon")
    );
    let e = &batch.events[1];
    assert_eq!((e.provider.as_str(), e.model.as_str()), ("openai", "gpt-5"));

    let kinds: Vec<_> = batch
        .messages
        .iter()
        .map(|m| (m.key.as_str(), m.role, m.kind, m.tool_name.as_deref()))
        .collect();
    assert_eq!(
        kinds,
        [
            ("b-user-1", MessageRole::User, Some(MessageKind::Text), None),
            (
                "b-asst-1",
                MessageRole::Assistant,
                Some(MessageKind::Text),
                None
            ),
            (
                "b-tool-1",
                MessageRole::Assistant,
                Some(MessageKind::ToolUse),
                Some("read_file")
            ),
            (
                "b-asst-2",
                MessageRole::Assistant,
                Some(MessageKind::Thinking),
                None
            ),
        ]
    );
    assert_eq!(
        batch.messages[1].event_dedup_key.as_deref(),
        Some("b-asst-1")
    );
    assert_eq!(batch.messages[2].event_dedup_key, None);
}

#[test]
fn discover_finds_db_in_root_dir_or_as_file() {
    let f = Fixture::new();
    let a = CursorAdapter::new();
    let dir = f.db.parent().unwrap().to_path_buf();
    assert_eq!(a.discover(&[dir]).unwrap(), [f.src()]);
    assert_eq!(a.discover(std::slice::from_ref(&f.db)).unwrap(), [f.src()]);
    assert!(
        a.discover(&[PathBuf::from("/nonexistent")])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn reread_from_cursor_yields_only_new_rows() {
    let f = Fixture::new();
    let first = read(&f, &Cursor::empty());
    let again = read(&f, &first.next_cursor);
    assert!(again.is_empty());

    f.bubble(
        COMPOSER,
        "b-asst-3",
        &assistant_bubble("claude-4.5-sonnet", 10, 5, 6_000),
    );
    let next = read(&f, &first.next_cursor);
    assert_eq!(dedup_keys(&next), ["b-asst-3"]);
    assert_eq!(next.sessions.len(), 1);
    assert_eq!(
        next.sessions[0].project_path.as_deref(),
        Some("/home/u/projects/keen lagoon")
    );
}

#[test]
fn generating_bubble_is_held_back_until_finished() {
    let f = Fixture::new();
    let first = read(&f, &Cursor::empty());

    f.bubble(COMPOSER, "b-live", &assistant_bubble("gpt-5", 50, 1, 7_000));
    f.composer(COMPOSER, "Fix the flaky test", &["b-live"]);
    f.bubble(
        OTHER_COMPOSER,
        "b-other",
        &assistant_bubble("gpt-5", 7, 7, 8_000),
    );
    f.composer(OTHER_COMPOSER, "Other", &[]);

    let a = CursorAdapter::new().with_stale_after(Duration::from_secs(3650 * 24 * 3600));
    let held = a.read(&f.src(), &first.next_cursor).unwrap();
    assert_eq!(dedup_keys(&held), ["b-other"]);

    let still = a.read(&f.src(), &held.next_cursor).unwrap();
    assert!(still.events.is_empty());

    f.bubble(
        COMPOSER,
        "b-live",
        &assistant_bubble("gpt-5", 500, 40, 7_000),
    );
    f.composer(COMPOSER, "Fix the flaky test", &[]);
    let done = a.read(&f.src(), &still.next_cursor).unwrap();
    assert_eq!(dedup_keys(&done), ["b-live"]);
    assert_eq!(done.events[0].input_tokens, 500);

    assert!(
        a.read(&f.src(), &done.next_cursor)
            .unwrap()
            .events
            .is_empty()
    );
}

#[test]
fn stale_generating_bubble_is_counted_with_a_warning() {
    let f = Fixture::new();
    f.bubble(COMPOSER, "b-live", &assistant_bubble("gpt-5", 50, 1, 7_000));
    f.composer(COMPOSER, "Fix the flaky test", &["b-live"]);
    let now = Timestamp::now().as_millisecond();
    assert!(now > T0, "fixture timestamps must be in the past");
    let batch = CursorAdapter::new()
        .with_stale_after(Duration::from_secs(1))
        .read(&f.src(), &Cursor::empty())
        .unwrap();
    assert!(dedup_keys(&batch).contains(&"b-live".to_owned()));
    assert!(batch.warnings.iter().any(|w| w.message.contains("b-live")));
}

#[test]
fn rowids_below_the_cursor_restart_the_read() {
    let f = Fixture::new();
    let stale = Cursor::from_json(json!({"last_rowid": 1_000_000}));
    let batch = read(&f, &stale);
    assert_eq!(dedup_keys(&batch), ["b-asst-1", "b-asst-2"]);
}

#[test]
fn reading_does_not_modify_the_database() {
    let f = Fixture::new();
    let before = std::fs::read(&f.db).unwrap();
    read(&f, &Cursor::empty());
    assert_eq!(std::fs::read(&f.db).unwrap(), before);
}

#[test]
fn invalid_json_warns_and_continues() {
    let f = Fixture::new();
    f.conn()
        .execute(
            "INSERT INTO cursorDiskKV (key, value) VALUES (?1, 'not json')",
            [format!("bubbleId:{COMPOSER}:b-bad")],
        )
        .unwrap();
    let batch = read(&f, &Cursor::empty());
    assert_eq!(dedup_keys(&batch), ["b-asst-1", "b-asst-2"]);
    assert!(batch.warnings.iter().any(|w| w.message.contains("b-bad")));
}

#[test]
fn missing_database_is_an_error() {
    let src = SourceRef::new(Path::new("/nonexistent/state.vscdb"), SourceKind::Sqlite);
    assert!(CursorAdapter::new().read(&src, &Cursor::empty()).is_err());
}
