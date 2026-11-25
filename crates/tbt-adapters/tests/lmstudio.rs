#![cfg(feature = "lmstudio")]

mod common;

use std::path::PathBuf;

use common::*;
use tbt_adapters::lmstudio::LmStudioAdapter;
use tbt_core::{Cursor, HarnessAdapter, MessageKind, MessageRole, SourceKind, Timestamp};

const ROOT: &str = "lmstudio/0.4.20/conversations";
const SYNTHETIC: &str = "lorem-ipsum/1766930000000.conversation.json";

fn conversation(rel: &str) -> PathBuf {
    fixture(&format!("{ROOT}/{rel}"))
}

#[test]
fn discovers_conversations_in_folders() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("folder")).unwrap();
    for f in [
        "1.conversation.json",
        "folder/2.conversation.json",
        "settings.json",
    ] {
        std::fs::write(dir.path().join(f), "{}").unwrap();
    }
    let found = LmStudioAdapter::new()
        .discover(&[dir.path().to_path_buf()])
        .unwrap();
    assert_eq!(found.len(), 2);
    assert!(found[1].path.ends_with("folder/2.conversation.json"));
    assert_eq!(found[0].kind, SourceKind::JsonFile);
}

#[test]
fn golden_fixture() {
    let batch = read_all(&LmStudioAdapter::new(), &[fixture(ROOT)]);
    assert_unique_dedup_keys(&batch);
    assert!(batch.warnings.is_empty(), "{:?}", batch.warnings);
    let rows: Vec<(&str, &str, &str, u64, u64, u64)> = batch
        .events
        .iter()
        .map(|e| {
            (
                e.session_ext_id.as_str(),
                e.provider.as_str(),
                e.model.as_str(),
                e.input_tokens,
                e.output_tokens,
                e.reasoning_tokens,
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (
                "1766924503826",
                "lmstudio",
                "openai/gpt-oss-20b",
                86,
                1757,
                57
            ),
            (
                "1766925535102",
                "lmstudio",
                "openai/gpt-oss-20b",
                86,
                2220,
                11
            ),
            (
                "1766930000000",
                "lmstudio",
                "openai/gpt-oss-20b",
                30,
                252,
                40
            ),
            ("1766930000000", "lmstudio", "qwen/qwen3-8b", 30, 182, 25),
        ]
    );
    assert_eq!(
        batch.events[0].ts,
        Timestamp::from_millisecond(1_766_925_515_887).unwrap()
    );
    assert_eq!(batch.messages.len(), 10);
    assert_eq!(batch.sessions.len(), 3);
}

#[test]
fn conversation_maps_versions_messages_and_session() {
    let batch = read_file(&LmStudioAdapter::new(), &conversation(SYNTHETIC));
    let s = &batch.sessions[0];
    assert_eq!(s.ext_id, "1766930000000");
    assert_eq!(s.title.as_deref(), Some("lorem ipsum dolor sit amet"));
    assert_eq!(
        s.started_at,
        Some(Timestamp::from_millisecond(1_766_930_000_000).unwrap())
    );
    assert_eq!(
        s.ended_at,
        Some(Timestamp::from_millisecond(1_766_930_120_000).unwrap())
    );

    let msgs: Vec<(MessageRole, Option<MessageKind>, i64, bool)> = batch
        .messages
        .iter()
        .map(|m| {
            (
                m.role,
                m.kind,
                m.ts.as_millisecond() - 1_766_930_000_000,
                m.event_dedup_key.is_some(),
            )
        })
        .collect();
    assert_eq!(
        msgs,
        [
            (MessageRole::User, Some(MessageKind::Text), 2_000, false),
            (
                MessageRole::Assistant,
                Some(MessageKind::Thinking),
                60_000,
                false
            ),
            (
                MessageRole::Assistant,
                Some(MessageKind::Text),
                61_000,
                true
            ),
            (MessageRole::User, Some(MessageKind::Text), 121_000, false),
        ]
    );
    assert_eq!(
        batch.messages[2].event_dedup_key.as_deref(),
        Some(batch.events[1].dedup_key.as_str())
    );
    assert_eq!(
        batch.messages[2].preview.as_deref(),
        Some("ullamco laboris nisi ut")
    );
}

#[test]
fn unchanged_file_is_skipped_and_a_rewrite_is_read_again() {
    let (_dir, path) = copy_to_temp(&conversation(SYNTHETIC));
    let adapter = LmStudioAdapter::new();
    let src = tbt_core::SourceRef::new(&path, SourceKind::JsonFile);
    let first = adapter.read(&src, &Cursor::empty()).unwrap();
    assert_eq!(first.events.len(), 2);
    let again = adapter.read(&src, &first.next_cursor).unwrap();
    assert!(again.is_empty());
    assert_eq!(again.next_cursor, first.next_cursor);

    let mut conv: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    conv["messages"][3]["versions"][0]["steps"][0]["genInfo"] = serde_json::json!({
        "identifier": "qwen/qwen3-8b",
        "stats": {"promptTokensCount": 300, "predictedTokensCount": 90}
    });
    let text = serde_json::to_string_pretty(&conv).unwrap();
    std::fs::write(&path, text).unwrap();
    let third = adapter.read(&src, &again.next_cursor).unwrap();
    assert_eq!(third.events.len(), 3, "{:?}", third.warnings);
    assert_eq!(third.events[2].input_tokens, 300);
}

#[test]
fn invalid_json_is_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("1.conversation.json");
    std::fs::write(&path, "{ nope").unwrap();
    let batch = read_file(&LmStudioAdapter::new(), &path);
    assert!(batch.events.is_empty());
    assert!(batch.warnings[0].message.contains("invalid JSON"));
}
