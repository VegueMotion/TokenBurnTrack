#![cfg(feature = "gemini_cli")]

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use common::*;
use tbt_adapters::gemini_cli::GeminiCliAdapter;
use tbt_core::{
    Cursor, HarnessAdapter, HarnessId, MessageKind, MessageRole, ReadBatch, SourceKind, SourceRef,
    Timestamp,
};

const LEGACY_ROOT: &str = "gemini_cli/0.20.0/tmp";
const LEGACY: &str = "gemini_cli/0.20.0/tmp/a3827afff0e2c3d3dca297252932c975b5331f52833903f3935f73eb01cefb92/chats/session-2026-10-02T09-30-7a1b2c3d.json";
const AMBER: &str = "gemini_cli/0.71.0-nightly/tmp/amber-acorn-00";
const MAIN: &str =
    "gemini_cli/0.71.0-nightly/tmp/amber-acorn-00/chats/session-2026-11-20T10-15-5a1e2d3c.jsonl";
const BIRCH: &str = "gemini_cli/0.71.0-nightly/tmp/bold-birch-00";

const MAIN_ID: &str = "5a1e2d3c-6b7a-4f8e-9d0c-1b2a3f4e5d6c";
const SUB_ID: &str = "f4c2b3a1-6f7e-4d8c-9b0a-1f2e3d4c5b6a";
const LEGACY_ID: &str = "7a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d";
const REWOUND: &str = "c18f9e0d-3c4b-4a5f-8e7d-8c9b0a1f2e3d";

const GRACE: Duration = Duration::from_secs(600);

fn settled() -> GeminiCliAdapter {
    GeminiCliAdapter::with_clock(Duration::ZERO, || Timestamp::MAX)
}

#[derive(Clone)]
struct Clock(Arc<AtomicI64>);

impl Clock {
    fn new() -> Clock {
        Clock(Arc::new(AtomicI64::new(0)))
    }

    fn adapter(&self) -> GeminiCliAdapter {
        let ms = self.0.clone();
        GeminiCliAdapter::with_clock(GRACE, move || {
            Timestamp::from_millisecond(ms.load(Ordering::SeqCst)).unwrap()
        })
    }

    fn at_mtime(&self, path: &Path, offset: Duration) {
        let mtime: Timestamp = std::fs::metadata(path)
            .unwrap()
            .modified()
            .unwrap()
            .try_into()
            .unwrap();
        let ms = mtime.as_millisecond() + i64::try_from(offset.as_millis()).unwrap();
        self.0.store(ms, Ordering::SeqCst);
    }
}

fn jsonl(path: impl Into<PathBuf>) -> SourceRef {
    SourceRef::new(path, SourceKind::JsonlFile)
}

fn json(path: impl Into<PathBuf>) -> SourceRef {
    SourceRef::new(path, SourceKind::JsonFile)
}

fn golden(name: &str, roots: &[&str]) -> ReadBatch {
    let roots: Vec<PathBuf> = roots.iter().map(|r| fixture(r)).collect();
    let batch = read_all(&settled(), &roots);
    insta::assert_yaml_snapshot!(name, Projection::of(&batch));
    batch
}

fn chats_dir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let chats = dir.path().join("proj").join("chats");
    std::fs::create_dir_all(&chats).unwrap();
    std::fs::write(dir.path().join("proj/.project_root"), "/w/proj\n").unwrap();
    (dir, chats)
}

fn key_set(batch: &ReadBatch) -> BTreeSet<String> {
    dedup_keys(batch).into_iter().collect()
}

fn touch(path: &Path, secs: u64) {
    let file = std::fs::File::options().write(true).open(path).unwrap();
    file.set_modified(std::time::SystemTime::now() + Duration::from_secs(secs))
        .unwrap();
}

#[test]
fn golden_legacy_json() {
    let batch = golden("legacy", &[LEGACY_ROOT]);
    assert_unique_dedup_keys(&batch);
    assert!(batch.warnings.is_empty());
    assert_eq!(batch.events.len(), 3);
    let s = &batch.sessions[0];
    assert_eq!(s.ext_id, LEGACY_ID);
    assert_eq!(s.project_path, None);
    let e = &batch.events[1];
    assert_eq!(e.dedup_key, "2d8b6e3a-3f4c-4b5d-9e0f-9a8b7c6d5e43");
    assert_eq!(e.harness, HarnessId::GEMINI_CLI);
    assert_eq!(e.provider, "google");
    assert_eq!(e.model, "gemini-2.5-pro");
    assert_eq!(e.session_ext_id, LEGACY_ID);
    assert_eq!(
        (
            e.input_tokens,
            e.output_tokens,
            e.cache_write_5m_tokens,
            e.cache_write_1h_tokens,
            e.cache_read_tokens,
            e.reasoning_tokens,
        ),
        (928, 528, 0, 0, 8192, 140)
    );
    assert_eq!(e.tokens().total(), 9648);
    assert!(e.raw.as_ref().unwrap().get().contains("\"thoughts\":140"));
    assert!(e.reported_cost.is_none());
}

#[test]
fn golden_main_session_with_subagent() {
    let batch = golden("main_with_subagent", &[AMBER]);
    assert_unique_dedup_keys(&batch);
    assert!(batch.warnings.is_empty(), "{:?}", batch.warnings);

    let ids: Vec<_> = batch.sessions.iter().map(|s| s.ext_id.as_str()).collect();
    assert_eq!(ids, [SUB_ID, MAIN_ID]);
    let sub = &batch.sessions[0];
    assert_eq!(sub.parent_ext_id.as_deref(), Some(MAIN_ID));
    assert_eq!(batch.sessions[1].parent_ext_id, None);
    for s in &batch.sessions {
        assert_eq!(
            s.project_path.as_deref(),
            Some("/u/user/projects/amber-acorn-00")
        );
    }
    assert_eq!(
        batch.sessions[1].title.as_deref(),
        Some("lorem ipsum dolor sit amet")
    );

    let count = |s: &str| {
        batch
            .events
            .iter()
            .filter(|e| e.session_ext_id == s)
            .count()
    };
    assert_eq!((count(MAIN_ID), count(SUB_ID)), (6, 2));
    assert!(batch.events.iter().any(|e| e.dedup_key == REWOUND));

    let tool_call = batch
        .events
        .iter()
        .find(|e| e.dedup_key == "7c3a4f5e-8d9c-4b0a-9f2e-3d4c5b6a7f8e")
        .unwrap();
    assert_eq!(
        (
            tool_call.input_tokens,
            tool_call.output_tokens,
            tool_call.reasoning_tokens
        ),
        (10240, 508, 412)
    );
    let result = batch
        .messages
        .iter()
        .find(|m| m.kind == Some(MessageKind::ToolResult) && m.session_ext_id == MAIN_ID)
        .unwrap();
    assert_eq!(result.role, MessageRole::Tool);
    assert_eq!(result.tool_name.as_deref(), Some("run_shell_command"));
}

#[test]
fn golden_legacy_resumed_into_jsonl() {
    let batch = golden("resumed_legacy", &[BIRCH]);
    assert!(batch.warnings.is_empty(), "{:?}", batch.warnings);
    assert_eq!(batch.sessions.len(), 2);
    for s in &batch.sessions {
        assert_eq!(s.ext_id, LEGACY_ID);
        assert_eq!(s.resumed_from_ext_id, None);
        assert_eq!(
            s.project_path.as_deref(),
            Some("/u/user/projects/bold-birch-00")
        );
    }

    let legacy = read_file(
        &settled(),
        &fixture(&format!(
            "{BIRCH}/chats/session-2026-10-02T09-30-7a1b2c3d.json"
        )),
    );
    let resumed = read_file(
        &settled(),
        &fixture(&format!(
            "{BIRCH}/chats/session-2026-10-02T09-30-7a1b2c3d.jsonl"
        )),
    );
    assert_eq!(legacy.events.len(), 3);
    assert_eq!(resumed.events.len(), 4);
    let (a, b) = (key_set(&legacy), key_set(&resumed));
    assert_eq!(a.intersection(&b).count(), 3);
    assert_eq!(a.union(&b).count(), 4);
}

#[test]
fn discover_finds_session_files_only() {
    let found = settled()
        .discover(&[fixture("gemini_cli"), fixtures_root().join("nope")])
        .unwrap();
    let kinds: Vec<_> = found
        .iter()
        .map(|s| {
            let name = s.path.file_name().unwrap().to_string_lossy().into_owned();
            (name, s.kind)
        })
        .collect();
    assert_eq!(kinds.len(), 5, "{kinds:?}");
    assert!(found.is_sorted_by(|a, b| a.path.as_os_str() < b.path.as_os_str()));
    assert_eq!(
        kinds
            .iter()
            .filter(|(_, k)| *k == SourceKind::JsonFile)
            .count(),
        2
    );
    assert!(
        found
            .iter()
            .any(|s| s.path.ends_with(format!("chats/{MAIN_ID}/{SUB_ID}.jsonl")))
    );

    let (dir, chats) = chats_dir();
    let proj = chats.parent().unwrap();
    std::fs::write(proj.join("logs.json"), "[]").unwrap();
    std::fs::write(proj.join("checkpoint-x.json"), "[]").unwrap();
    std::fs::write(chats.join("notes.json"), "{}").unwrap();
    std::fs::create_dir_all(proj.join("tool-outputs")).unwrap();
    std::fs::write(proj.join("tool-outputs/x.jsonl"), "").unwrap();
    std::fs::write(chats.join("session-a.json"), "{}").unwrap();
    std::fs::write(chats.join("session-a.jsonl"), "").unwrap();
    let found = settled().discover(&[dir.path().to_path_buf()]).unwrap();
    let names: Vec<_> = found
        .iter()
        .map(|s| s.path.strip_prefix(dir.path()).unwrap().to_owned())
        .collect();
    assert_eq!(
        names,
        [
            PathBuf::from("proj/chats/session-a.json"),
            PathBuf::from("proj/chats/session-a.jsonl"),
        ]
    );
}

#[test]
fn unchanged_file_returns_unchanged_batch() {
    let a = settled();
    for s in a.discover(&[fixture("gemini_cli")]).unwrap() {
        let first = a.read(&s, &Cursor::empty()).unwrap();
        assert!(!first.events.is_empty());
        let again = a.read(&s, &first.next_cursor).unwrap();
        assert!(again.is_empty(), "{}", s.path.display());
        assert_eq!(again.next_cursor, first.next_cursor);
    }
}

#[test]
fn appended_lines_yield_each_event_once() {
    let full = read_file(&settled(), &fixture(MAIN));
    let lines = read_lines(&fixture(MAIN));

    for split in 1..lines.len() {
        let (_dir, chats) = chats_dir();
        let path = chats.join("session-main.jsonl");
        write_lines(&path, &lines[..split]);
        let clock = Clock::new();
        let a = clock.adapter();
        let s = jsonl(&path);
        clock.at_mtime(&path, Duration::ZERO);
        let head = a.read(&s, &Cursor::empty()).unwrap();

        append(&path, &(lines[split..].join("\n") + "\n"));
        clock.at_mtime(&path, GRACE);
        let tail = a.read(&s, &head.next_cursor).unwrap();

        let mut keys = dedup_keys(&head);
        keys.extend(dedup_keys(&tail));
        assert_eq!(keys, dedup_keys(&full), "split at line {split}");
        let mut events = head.events.clone();
        events.extend(tail.events.clone());
        for (got, want) in events.iter().zip(&full.events) {
            assert_eq!(got.tokens(), want.tokens(), "split at line {split}");
            assert_eq!(got.session_ext_id, MAIN_ID);
        }
        assert_eq!(
            head.messages.len() + tail.messages.len(),
            full.messages.len(),
            "split at line {split}"
        );
    }
}

#[test]
fn re_appended_message_is_emitted_with_its_final_tokens() {
    let msg = |tokens: &str| {
        format!(
            r#"{{"id":"m1","timestamp":"2026-01-01T00:00:01Z","type":"gemini","content":"","model":"gemini-2.5-pro"{tokens}}}"#
        )
    };
    let (_dir, chats) = chats_dir();
    let path = chats.join("session-x.jsonl");
    write_lines(
        &path,
        &[
            r#"{"sessionId":"s1","projectHash":"h","startTime":"2026-01-01T00:00:00Z"}"#.into(),
            msg(""),
        ],
    );
    let clock = Clock::new();
    let a = clock.adapter();
    let s = jsonl(&path);
    clock.at_mtime(&path, Duration::ZERO);
    let first = a.read(&s, &Cursor::empty()).unwrap();
    assert!(first.events.is_empty() && first.messages.is_empty());

    append(
        &path,
        &format!(
            "{}\n",
            msg(
                r#","tokens":{"input":100,"output":10,"cached":0,"thoughts":0,"tool":0,"total":110}"#
            )
        ),
    );
    clock.at_mtime(&path, Duration::ZERO);
    let second = a.read(&s, &first.next_cursor).unwrap();
    assert!(second.events.is_empty(), "emitted before final");

    append(
        &path,
        &format!(
            "{}\n",
            msg(
                r#","tokens":{"input":100,"output":25,"cached":40,"thoughts":5,"tool":0,"total":130}"#
            )
        ),
    );
    clock.at_mtime(&path, Duration::ZERO);
    let third = a.read(&s, &second.next_cursor).unwrap();
    assert!(third.events.is_empty());

    append(
        &path,
        "{\"id\":\"u2\",\"timestamp\":\"2026-01-01T00:00:09Z\",\"type\":\"user\",\"content\":[{\"text\":\"next\"}]}\n",
    );
    clock.at_mtime(&path, Duration::ZERO);
    let fourth = a.read(&s, &third.next_cursor).unwrap();
    assert_eq!(dedup_keys(&fourth), ["m1"]);
    let e = &fourth.events[0];
    assert_eq!(
        (
            e.input_tokens,
            e.cache_read_tokens,
            e.output_tokens,
            e.reasoning_tokens
        ),
        (60, 40, 30, 5)
    );
    assert_eq!(e.session_ext_id, "s1");
    assert_eq!(e.project_path.as_deref(), Some("/w/proj"));
    assert!(fourth.messages.iter().all(|m| m.key != "u2:text"));
}

#[test]
fn pending_message_is_finalized_after_grace_period() {
    let lines = read_lines(&fixture(MAIN));
    let (_dir, chats) = chats_dir();
    let path = chats.join("session-main.jsonl");
    write_lines(&path, &lines);
    let clock = Clock::new();
    let a = clock.adapter();
    let s = jsonl(&path);

    clock.at_mtime(&path, Duration::ZERO);
    let first = a.read(&s, &Cursor::empty()).unwrap();
    assert_eq!(first.events.len(), 5);
    assert!(first.next_cursor.as_json()["pending"].is_object());

    clock.at_mtime(&path, GRACE - Duration::from_secs(1));
    let early = a.read(&s, &first.next_cursor).unwrap();
    assert!(early.is_empty());
    assert_eq!(early.next_cursor, first.next_cursor);

    clock.at_mtime(&path, GRACE);
    let late = a.read(&s, &first.next_cursor).unwrap();
    assert_eq!(dedup_keys(&late), ["a5d3c4b2-7a8f-4e9d-8c1b-2a3f4e5d6c7b"]);
    assert_eq!(late.sessions.len(), 1);
    assert!(late.next_cursor.as_json().get("pending").is_none());

    let again = a.read(&s, &late.next_cursor).unwrap();
    assert!(again.is_empty());
    assert_eq!(again.next_cursor, late.next_cursor);
}

#[test]
fn rewind_finalizes_the_rewound_message_immediately() {
    let lines = read_lines(&fixture(MAIN));
    let rewind = lines.iter().position(|l| l.contains("$rewindTo")).unwrap();
    let (_dir, chats) = chats_dir();
    let path = chats.join("session-main.jsonl");
    write_lines(&path, &lines[..=rewind]);
    let clock = Clock::new();
    clock.at_mtime(&path, Duration::ZERO);
    let batch = clock
        .adapter()
        .read(&jsonl(&path), &Cursor::empty())
        .unwrap();
    assert_eq!(dedup_keys(&batch).last().map(String::as_str), Some(REWOUND));
}

#[test]
fn patch_updates_pending_message_preview() {
    let (_dir, chats) = chats_dir();
    let path = chats.join("session-p.jsonl");
    write_lines(
        &path,
        &[
            r#"{"sessionId":"s1","projectHash":"h"}"#.into(),
            r#"{"id":"m1","timestamp":"2026-01-01T00:00:01Z","type":"gemini","content":"old","model":"m","tokens":{"input":1,"output":1,"cached":0,"total":2},"toolCalls":[{"id":"t1","name":"glob","args":{},"status":"executing","timestamp":"2026-01-01T00:00:01Z"}]}"#.into(),
            r#"{"$patch":{"id":"m1","content":[{"text":"new"}],"toolCalls":[{"id":"t1","result":[{"functionResponse":{"response":{"output":"found"}}}]}]}}"#.into(),
            r#"{"$patch":{"id":"other","content":"ignored"}}"#.into(),
            r#"{"$future":{"x":1}}"#.into(),
            r#"{"something":"else"}"#.into(),
        ],
    );
    let batch = read_file(&settled(), &path);
    assert!(batch.warnings.is_empty(), "{:?}", batch.warnings);
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
                "m1:text",
                MessageRole::Assistant,
                Some(MessageKind::Text),
                None,
                Some("new"),
                Some("m1")
            ),
            (
                "m1:tool:t1",
                MessageRole::Assistant,
                Some(MessageKind::ToolUse),
                Some("glob"),
                Some("{}"),
                Some("m1")
            ),
            (
                "m1:result:t1",
                MessageRole::Tool,
                Some(MessageKind::ToolResult),
                Some("glob"),
                Some("found"),
                None
            ),
        ]
    );
}

#[test]
fn truncated_file_restarts_from_zero() {
    let lines = read_lines(&fixture(MAIN));
    let (_dir, chats) = chats_dir();
    let path = chats.join("session-main.jsonl");
    write_lines(&path, &lines);
    let a = settled();
    let s = jsonl(&path);
    let first = a.read(&s, &Cursor::empty()).unwrap();

    write_lines(&path, &lines[..lines.len() / 3]);
    let expected = read_file(&a, &path);
    assert!(!expected.events.is_empty() && expected.events.len() < first.events.len());

    let after = a.read(&s, &first.next_cursor).unwrap();
    assert_eq!(dedup_keys(&after), dedup_keys(&expected));
    assert_eq!(after.messages.len(), expected.messages.len());
    assert_eq!(after.next_cursor, expected.next_cursor);
}

#[test]
fn legacy_json_rewrites_are_read_without_duplicates() {
    let full: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixture(LEGACY)).unwrap()).unwrap();
    let with_messages = |n: usize| {
        let mut v = full.clone();
        v["messages"].as_array_mut().unwrap().truncate(n);
        serde_json::to_string_pretty(&v).unwrap()
    };
    let (_dir, chats) = chats_dir();
    let path = chats.join("session-legacy.json");
    let clock = Clock::new();
    let a = clock.adapter();
    let s = json(&path);

    std::fs::write(&path, with_messages(3)).unwrap();
    clock.at_mtime(&path, Duration::ZERO);
    let first = a.read(&s, &Cursor::empty()).unwrap();
    assert_eq!(dedup_keys(&first), ["1c7a5d2f-2e3b-4a4c-8d9e-8f7a6b5c4d32"]);

    std::fs::write(&path, with_messages(5)).unwrap();
    touch(&path, 2);
    clock.at_mtime(&path, Duration::ZERO);
    let second = a.read(&s, &first.next_cursor).unwrap();
    assert_eq!(
        dedup_keys(&second),
        ["2d8b6e3a-3f4c-4b5d-9e0f-9a8b7c6d5e43"]
    );

    let unchanged = a.read(&s, &second.next_cursor).unwrap();
    assert!(unchanged.is_empty());

    clock.at_mtime(&path, GRACE);
    let third = a.read(&s, &second.next_cursor).unwrap();
    assert_eq!(dedup_keys(&third), ["4f0d8a5c-5b6e-4d7f-9a2b-1c0d9e8f7a65"]);

    touch(&path, 4);
    clock.at_mtime(&path, GRACE);
    let touched = a.read(&s, &third.next_cursor).unwrap();
    assert!(touched.events.is_empty() && touched.messages.is_empty());

    let mut all = first;
    for b in [second, third, touched] {
        merge(&mut all, b);
    }
    assert_unique_dedup_keys(&all);
    assert_eq!(all.events.len(), 3);
    let mut msg_keys: Vec<_> = all.messages.iter().map(|m| m.key.clone()).collect();
    let n = msg_keys.len();
    msg_keys.sort();
    msg_keys.dedup();
    assert_eq!(msg_keys.len(), n);
    let full_messages = read_file(&settled(), &fixture(LEGACY)).messages.len();
    assert_eq!(n, full_messages);
}

#[test]
fn legacy_json_mid_write_warns_and_retries() {
    let (_dir, chats) = chats_dir();
    let path = chats.join("session-legacy.json");
    let text = std::fs::read_to_string(fixture(LEGACY)).unwrap();
    std::fs::write(&path, &text[..text.len() / 2]).unwrap();
    let a = settled();
    let s = json(&path);
    let first = a.read(&s, &Cursor::empty()).unwrap();
    assert!(first.events.is_empty());
    assert_eq!(first.warnings.len(), 1);
    assert!(first.warnings[0].message.starts_with("invalid JSON"));

    std::fs::write(&path, &text).unwrap();
    touch(&path, 2);
    let second = a.read(&s, &first.next_cursor).unwrap();
    assert_eq!(second.events.len(), 3);
    assert!(second.warnings.is_empty());
}

#[test]
fn malformed_lines_warn_and_continue() {
    let lines = read_lines(&fixture(MAIN));
    let mut broken = lines[..2].to_vec();
    broken.push("{not json".into());
    broken.push("[1,2]".into());
    broken.push(r#"{"id":"x1","type":"user"}"#.into());
    broken.push(r#"{"id":"x2","timestamp":"2026-01-01T00:00:00Z","type":"gemini","tokens":{"input":1,"output":1}}"#.into());
    broken.push(r#"{"id":"x3","timestamp":"2026-01-01T00:00:00Z","type":"gemini","model":"m","tokens":{"input":"many"}}"#.into());
    broken.extend_from_slice(&lines[2..]);
    let (_dir, chats) = chats_dir();
    let path = chats.join("session-m.jsonl");
    write_lines(&path, &broken);

    let batch = read_file(&settled(), &path);
    assert_eq!(batch.events.len(), 6);
    let at: Vec<_> = batch.warnings.iter().map(|w| w.line).collect();
    assert_eq!(at, [Some(3), Some(4), Some(5), Some(6), Some(7)]);
    assert!(
        batch
            .warnings
            .iter()
            .all(|w| w.harness == HarnessId::GEMINI_CLI)
    );
    let offset: usize = broken[..2].iter().map(|l| l.len() + 1).sum();
    assert_eq!(batch.warnings[0].byte_offset, Some(offset as u64));
    assert!(batch.warnings[0].message.starts_with("invalid JSON"));
}

#[test]
fn invalid_cursor_rereads_with_warning() {
    let a = settled();
    let bogus = Cursor::from_json(serde_json::json!({"byte_offset": "x"}));
    let batch = a.read(&jsonl(fixture(MAIN)), &bogus).unwrap();
    assert_eq!(batch.events.len(), 6);
    assert_eq!(batch.warnings.len(), 1);
    assert!(batch.warnings[0].message.contains("invalid cursor"));

    let bogus = Cursor::from_json(serde_json::json!({"size": "x"}));
    let batch = a.read(&json(fixture(LEGACY)), &bogus).unwrap();
    assert_eq!(batch.events.len(), 3);
    assert_eq!(batch.warnings.len(), 1);
}

#[test]
fn adapter_identity() {
    let a = GeminiCliAdapter::new();
    assert_eq!(a.id(), HarnessId::GEMINI_CLI);
    assert_eq!(a.display_name(), "Gemini CLI");
}
