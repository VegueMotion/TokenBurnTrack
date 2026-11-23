#![cfg(feature = "claude_code")]

mod common;

use std::collections::BTreeSet;
use std::path::PathBuf;

use common::*;
use tbt_adapters::ClaudeCodeAdapter;
use tbt_core::{
    Cursor, HarnessAdapter, HarnessId, MessageKind, MessageRole, SourceKind, SourceRef,
};

const SHORT: &str = "claude_code/2.1.330/projects/-mnt-bold-juniper-8a-projects-pale-fjord-8a/35395b06-00f0-4c8b-9098-93d35ca8f399.jsonl";
const LONG: &str = "claude_code/2.1.330/projects/-mnt-bold-juniper-8a-projects-pale-quartz-b9/981f8c38-df8d-441c-bba0-9385343213aa.jsonl";
const SUBAGENT_MAIN: &str = "claude_code/2.1.330/projects/-mnt-bold-juniper-8a-projects-pale-fjord-8a/64ed9da4-d663-42b6-bd5f-db9652067ec7.jsonl";
const SUBAGENT_DIR: &str = "claude_code/2.1.330/projects/-mnt-bold-juniper-8a-projects-pale-fjord-8a/64ed9da4-d663-42b6-bd5f-db9652067ec7";
const RESUMED_ORIGINAL: &str = "claude_code/2.1.329/projects/-mnt-bold-juniper-8a-projects-pale-fjord-8a/3fc87bac-5bef-406a-bb3b-6447d69a519d.jsonl";
const RESUMED_CONTINUATION: &str = "claude_code/2.1.329/projects/-mnt-bold-juniper-8a-projects-pale-fjord-8a/d1774c7e-944b-47aa-8282-44b01089b283.jsonl";
const TOOL_HEAVY: &str = "claude_code/2.1.325/projects/-mnt-bold-juniper-8a-projects-keen-lagoon-45/f001a22b-ee17-44c8-ab5f-c1ab7bd4b32b.jsonl";
const MULTI_MODEL: &str = "claude_code/2.1.319/projects/-mnt-bold-juniper-8a-projects-noble-orchid-25/ee7bdb9e-c647-49d2-837d-71810ebd64c7.jsonl";

fn adapter() -> ClaudeCodeAdapter {
    ClaudeCodeAdapter::new()
}

fn src(path: impl Into<PathBuf>) -> SourceRef {
    SourceRef::new(path, SourceKind::JsonlFile)
}

fn golden(name: &str, roots: &[&str]) -> tbt_core::ReadBatch {
    let roots: Vec<PathBuf> = roots.iter().map(|r| fixture(r)).collect();
    let batch = read_all(&adapter(), &roots);
    assert_unique_dedup_keys(&batch);
    insta::assert_yaml_snapshot!(name, Projection::of(&batch));
    batch
}

#[test]
fn golden_short() {
    let batch = golden("short", &[SHORT]);
    assert_eq!(batch.events.len(), 1);
    let e = &batch.events[0];
    assert_eq!(
        e.dedup_key,
        "msg_647UkLvz2JUpXMI8QpmSZlzK:req_814WhIrt06rB1hjv3tw0tm5T"
    );
    assert_eq!(e.harness, HarnessId::CLAUDE_CODE);
    assert_eq!(e.provider, "anthropic");
    assert_eq!(e.model, "claude-sonnet-5-5");
    assert_eq!(
        (
            e.input_tokens,
            e.output_tokens,
            e.cache_write_5m_tokens,
            e.cache_write_1h_tokens,
            e.cache_read_tokens,
            e.reasoning_tokens,
        ),
        (2, 538, 0, 16609, 21566, 97)
    );
    assert_eq!(e.ts.to_string(), "2026-11-21T22:13:09.328Z");
    assert!(e.raw.as_ref().unwrap().get().contains("\"iterations\""));
    assert!(e.reported_cost.is_none());
}

#[test]
fn golden_long() {
    assert_eq!(golden("long", &[LONG]).events.len(), 64);
}

#[test]
fn golden_tool_heavy() {
    let batch = golden("tool_heavy", &[TOOL_HEAVY]);
    assert_eq!(batch.events.len(), 53);
    let results = batch
        .messages
        .iter()
        .filter(|m| m.kind == Some(MessageKind::ToolResult));
    for m in results {
        assert_eq!(m.role, MessageRole::Tool);
        assert!(
            m.tool_name.is_some(),
            "tool result {} has no tool name",
            m.key
        );
    }
}

#[test]
fn golden_multi_model_skips_synthetic() {
    let batch = golden("multi_model", &[MULTI_MODEL]);
    assert_eq!(batch.events.len(), 6);
    assert!(batch.events.iter().all(|e| e.model != "<synthetic>"));
    assert!(batch.warnings.is_empty());
    let models: BTreeSet<_> = batch.events.iter().map(|e| e.model.as_str()).collect();
    assert!(models.len() >= 2, "{models:?}");
}

#[test]
fn golden_subagents() {
    let batch = golden("subagents", &[SUBAGENT_MAIN, SUBAGENT_DIR]);
    let main = "64ed9da4-d663-42b6-bd5f-db9652067ec7";
    let sub = "64ed9da4-d663-42b6-bd5f-db9652067ec7/agent-a919fc890880d1075";
    assert_eq!(batch.sessions.len(), 2);
    assert_eq!(batch.sessions[0].ext_id, main);
    assert_eq!(batch.sessions[0].parent_ext_id, None);
    assert_eq!(batch.sessions[1].ext_id, sub);
    assert_eq!(batch.sessions[1].parent_ext_id.as_deref(), Some(main));
    let count = |s: &str| {
        batch
            .events
            .iter()
            .filter(|e| e.session_ext_id == s)
            .count()
    };
    assert_eq!((count(main), count(sub)), (19, 6));
}

#[test]
fn golden_resumed_pair_shares_50_dedup_keys() {
    let original = golden("resumed_original", &[RESUMED_ORIGINAL]);
    let continuation = golden("resumed_continuation", &[RESUMED_CONTINUATION]);
    assert_eq!(original.events.len(), 50);
    assert_eq!(continuation.events.len(), 55);

    let a: BTreeSet<_> = dedup_keys(&original).into_iter().collect();
    let b: BTreeSet<_> = dedup_keys(&continuation).into_iter().collect();
    assert_eq!(a.intersection(&b).count(), 50);

    let s = &continuation.sessions[0];
    assert_eq!(s.ext_id, "d1774c7e-944b-47aa-8282-44b01089b283");
    assert_eq!(
        s.resumed_from_ext_id.as_deref(),
        Some("3fc87bac-5bef-406a-bb3b-6447d69a519d")
    );
    assert_eq!(original.sessions[0].resumed_from_ext_id, None);
}

#[test]
fn discover_finds_all_fixtures_including_subagents_in_order() {
    let found = adapter()
        .discover(&[fixture("claude_code"), fixtures_root().join("nope")])
        .unwrap();
    assert_eq!(found.len(), 8);
    assert!(found.iter().all(|s| s.kind == SourceKind::JsonlFile));
    assert!(found.is_sorted_by(|a, b| a.path.as_os_str() < b.path.as_os_str()));
    assert!(
        found
            .iter()
            .any(|s| s.path.ends_with("subagents/agent-a919fc890880d1075.jsonl"))
    );
}

#[test]
fn reread_from_cursor_yields_nothing_new() {
    let a = adapter();
    for s in a.discover(&[fixture("claude_code")]).unwrap() {
        let first = a.read(&s, &Cursor::empty()).unwrap();
        assert!(!first.events.is_empty());
        let again = a.read(&s, &first.next_cursor).unwrap();
        assert!(again.is_empty(), "{}", s.path.display());
        assert_eq!(again.next_cursor, first.next_cursor);
    }
}

#[test]
fn touched_file_without_new_lines_yields_nothing() {
    let (_dir, path) = copy_to_temp(&fixture(LONG));
    let a = adapter();
    let s = src(&path);
    let first = a.read(&s, &Cursor::empty()).unwrap();
    let file = std::fs::File::options().write(true).open(&path).unwrap();
    file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5))
        .unwrap();
    let again = a.read(&s, &first.next_cursor).unwrap();
    assert!(again.events.is_empty() && again.messages.is_empty());
    assert!(again.warnings.is_empty());
}

#[test]
fn appended_lines_yield_only_new_events() {
    let full = read_file(&adapter(), &fixture(SUBAGENT_MAIN));
    let lines = read_lines(&fixture(SUBAGENT_MAIN));

    for split in [lines.len() / 3, lines.len() / 2, lines.len() - 1] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("64ed9da4-d663-42b6-bd5f-db9652067ec7.jsonl");
        write_lines(&path, &lines[..split]);
        let a = adapter();
        let s = src(&path);
        let head = a.read(&s, &Cursor::empty()).unwrap();

        append(&path, &(lines[split..].join("\n") + "\n"));
        let tail = a.read(&s, &head.next_cursor).unwrap();

        let mut keys = dedup_keys(&head);
        keys.extend(dedup_keys(&tail));
        assert_eq!(keys, dedup_keys(&full), "split at line {split}");
        let messages = head.messages.len() + tail.messages.len();
        assert_eq!(messages, full.messages.len());
        assert!(
            tail.events
                .iter()
                .all(|e| !dedup_keys(&head).contains(&e.dedup_key))
        );
    }
}

#[test]
fn content_blocks_split_across_reads_emit_one_event() {
    let lines = read_lines(&fixture(SHORT));
    let first_block = lines
        .iter()
        .position(|l| l.contains("\"apiBlockIndex\":0"))
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    write_lines(&path, &lines[..=first_block]);
    let a = adapter();
    let s = src(&path);
    let head = a.read(&s, &Cursor::empty()).unwrap();
    assert_eq!(head.events.len(), 1);

    append(&path, &(lines[first_block + 1..].join("\n") + "\n"));
    let tail = a.read(&s, &head.next_cursor).unwrap();
    assert!(tail.events.is_empty());
    let linked: Vec<_> = tail
        .messages
        .iter()
        .filter_map(|m| m.event_dedup_key.as_deref())
        .collect();
    assert_eq!(linked, [head.events[0].dedup_key.as_str()]);
}

#[test]
fn truncated_file_restarts_from_zero() {
    let (_dir, path) = copy_to_temp(&fixture(LONG));
    let a = adapter();
    let s = src(&path);
    let first = a.read(&s, &Cursor::empty()).unwrap();

    let lines = read_lines(&path);
    let keep = lines.len() / 4;
    write_lines(&path, &lines[..keep]);
    let expected = read_file(&a, &path);
    assert!(!expected.events.is_empty() && expected.events.len() < first.events.len());

    let after = a.read(&s, &first.next_cursor).unwrap();
    assert_eq!(dedup_keys(&after), dedup_keys(&expected));
    assert_eq!(after.messages.len(), expected.messages.len());
    assert_eq!(after.next_cursor, expected.next_cursor);
}

#[test]
fn partial_trailing_line_is_not_consumed() {
    let lines = read_lines(&fixture(SHORT));
    let usage_line = lines.iter().position(|l| l.contains("\"usage\"")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("p.jsonl");
    let head = lines[..usage_line].join("\n") + "\n";
    let (cut_a, cut_b) = lines[usage_line].split_at(lines[usage_line].len() / 2);
    std::fs::write(&path, format!("{head}{cut_a}")).unwrap();

    let a = adapter();
    let s = src(&path);
    let first = a.read(&s, &Cursor::empty()).unwrap();
    assert!(first.events.is_empty());
    assert!(first.warnings.is_empty());
    assert_eq!(
        first.next_cursor.as_json()["byte_offset"],
        head.len() as u64
    );

    let rest = lines[usage_line + 1..].join("\n");
    append(&path, &format!("{cut_b}\n{rest}\n"));
    let second = a.read(&s, &first.next_cursor).unwrap();
    assert_eq!(second.events.len(), 1);
    assert!(second.warnings.is_empty());
}

#[test]
fn unchanged_file_returns_unchanged_batch() {
    let a = adapter();
    let s = src(fixture(SHORT));
    let first = a.read(&s, &Cursor::empty()).unwrap();
    let again = a.read(&s, &first.next_cursor).unwrap();
    assert!(again.is_empty());
    assert_eq!(again.next_cursor, first.next_cursor);
}

#[test]
fn malformed_lines_warn_and_continue() {
    let lines = read_lines(&fixture(SHORT));
    let usage_line = lines.iter().position(|l| l.contains("\"usage\"")).unwrap();
    let mut broken = lines[..usage_line].to_vec();
    broken.push("{not json".into());
    broken.push("[1,2]".into());
    broken.push(r#"{"no_type":true}"#.into());
    broken.extend_from_slice(&lines[usage_line..]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.jsonl");
    write_lines(&path, &broken);

    let batch = read_file(&adapter(), &path);
    assert_eq!(batch.events.len(), 1);
    let at: Vec<_> = batch
        .warnings
        .iter()
        .map(|w| (w.line, w.harness.clone()))
        .collect();
    let n = usage_line as u64;
    assert_eq!(
        at,
        [
            (Some(n + 1), HarnessId::CLAUDE_CODE),
            (Some(n + 2), HarnessId::CLAUDE_CODE),
            (Some(n + 3), HarnessId::CLAUDE_CODE),
        ]
    );
    let offset: usize = broken[..usage_line].iter().map(|l| l.len() + 1).sum();
    assert_eq!(batch.warnings[0].byte_offset, Some(offset as u64));
    assert!(batch.warnings[0].message.starts_with("invalid JSON"));
}

#[test]
fn invalid_cursor_rereads_with_warning() {
    let a = adapter();
    let s = src(fixture(SHORT));
    let bogus = Cursor::from_json(serde_json::json!({"byte_offset": "x"}));
    let batch = a.read(&s, &bogus).unwrap();
    assert_eq!(batch.events.len(), 1);
    assert_eq!(batch.warnings.len(), 1);
    assert!(batch.warnings[0].message.contains("invalid cursor"));
}

#[test]
fn synthetic_and_legacy_lines() {
    let lines = [
        r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"2026-01-01T00:00:00Z","cwd":"/w","version":"1.0.0","message":{"role":"user","content":[{"type":"text","text":"hi"},{"type":"image","source":{}}]}}"#,
        r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"2026-01-01T00:00:01Z","requestId":"req_1","message":{"id":"msg_1","model":"claude-x","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}}],"usage":{"input_tokens":5,"output_tokens":7,"cache_creation_input_tokens":11,"cache_read_input_tokens":13}}}"#,
        r#"{"type":"user","uuid":"u2","sessionId":"s1","timestamp":"2026-01-01T00:00:02Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":[{"type":"text","text":"file.txt"}]}]}}"#,
        r#"{"type":"assistant","uuid":"a2","sessionId":"s1","timestamp":"2026-01-01T00:00:03Z","isApiErrorMessage":true,"message":{"id":"x","model":"<synthetic>","content":[{"type":"text","text":"error"}],"usage":{"input_tokens":0,"output_tokens":0}}}"#,
        r#"{"type":"assistant","uuid":"a3","sessionId":"s1","timestamp":"2026-01-01T00:00:04Z","message":{"id":"msg_2","model":"claude-x","content":[{"type":"text","text":"done"}],"usage":{"input_tokens":1,"output_tokens":2}}}"#,
        r#"{"type":"cost-state","sessionId":"s1","totalCostUSD":1.5}"#,
        r#"{"type":"summary","summary":"Old title","leafUuid":"a3"}"#,
        r#"{"type":"ai-title","aiTitle":"AI title","sessionId":"s1"}"#,
        r#"{"type":"something-new","sessionId":"s1"}"#,
    ];
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s1.jsonl");
    write_lines(&path, &lines.map(String::from));
    let batch = read_file(&adapter(), &path);

    assert!(batch.warnings.is_empty(), "{:?}", batch.warnings);
    assert_eq!(dedup_keys(&batch), ["msg_1:req_1", "msg_2"]);
    let e = &batch.events[0];
    assert_eq!((e.cache_write_5m_tokens, e.cache_write_1h_tokens), (11, 0));
    assert_eq!(e.project_path.as_deref(), Some("/w"));

    let s = &batch.sessions[0];
    assert_eq!(batch.sessions.len(), 1);
    assert_eq!(s.title.as_deref(), Some("AI title"));
    assert_eq!(s.harness_version.as_deref(), Some("1.0.0"));
    assert_eq!(s.started_at.unwrap().to_string(), "2026-01-01T00:00:00Z");
    assert_eq!(s.ended_at.unwrap().to_string(), "2026-01-01T00:00:04Z");

    let m: Vec<_> = batch
        .messages
        .iter()
        .map(|m| {
            (
                m.key.as_str(),
                m.role,
                m.kind,
                m.tool_name.as_deref(),
                m.preview.as_deref(),
                m.event_dedup_key.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        m,
        [
            (
                "u1:0",
                MessageRole::User,
                Some(MessageKind::Text),
                None,
                Some("hi"),
                None
            ),
            ("u1:1", MessageRole::User, None, None, None, None),
            (
                "a1:0",
                MessageRole::Assistant,
                Some(MessageKind::ToolUse),
                Some("Bash"),
                Some(r#"{"command":"ls"}"#),
                Some("msg_1:req_1")
            ),
            (
                "u2:0",
                MessageRole::Tool,
                Some(MessageKind::ToolResult),
                Some("Bash"),
                Some("file.txt"),
                None
            ),
            (
                "a2:0",
                MessageRole::Assistant,
                Some(MessageKind::Text),
                None,
                Some("error"),
                None
            ),
            (
                "a3:0",
                MessageRole::Assistant,
                Some(MessageKind::Text),
                None,
                Some("done"),
                Some("msg_2")
            ),
        ]
    );
}

#[test]
fn long_previews_are_truncated() {
    let text = "é".repeat(1000);
    let line = format!(
        r#"{{"type":"user","uuid":"u1","sessionId":"s","timestamp":"2026-01-01T00:00:00Z","message":{{"role":"user","content":"{text}"}}}}"#
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    write_lines(&path, &[line]);
    let batch = read_file(&adapter(), &path);
    let preview = batch.messages[0].preview.as_deref().unwrap();
    assert_eq!(preview.chars().count(), tbt_core::PREVIEW_MAX_CHARS);
    assert!(preview.ends_with('…'));
}

#[test]
fn custom_title_beats_later_ai_title_across_reads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    write_lines(
        &path,
        &[r#"{"type":"custom-title","customTitle":"Mine","sessionId":"s"}"#.into()],
    );
    let a = adapter();
    let s = src(&path);
    let first = a.read(&s, &Cursor::empty()).unwrap();
    assert_eq!(first.sessions[0].title.as_deref(), Some("Mine"));

    append(
        &path,
        "{\"type\":\"ai-title\",\"aiTitle\":\"Auto\",\"sessionId\":\"s\"}\n",
    );
    let second = a.read(&s, &first.next_cursor).unwrap();
    assert_eq!(second.sessions[0].title, None);
}
