#![cfg(feature = "copilot_cli")]

mod common;

use std::path::PathBuf;

use common::*;
use tbt_adapters::copilot_cli::CopilotCliAdapter;
use tbt_core::{
    Cursor, HarnessAdapter, HarnessId, MessageKind, MessageRole, ReadBatch, SourceKind,
};

const ROOT: &str = "copilot_cli/1.0.4/session-state";
const SWITCH: &str = "3b0f6c2e-8a41-4d7e-9c55-1f2a3b4c5d6e";
const RESUMED: &str = "7d9e1a2b-3c4d-4e5f-8a6b-9c0d1e2f3a4b";

fn events_file(session: &str) -> PathBuf {
    fixture(&format!("{ROOT}/{session}/events.jsonl"))
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

#[test]
fn discovers_only_event_logs() {
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("abc");
    std::fs::create_dir_all(session.join("checkpoints")).unwrap();
    std::fs::write(session.join("events.jsonl"), "").unwrap();
    std::fs::write(session.join("checkpoints/index.jsonl"), "").unwrap();
    let found = CopilotCliAdapter::new()
        .discover(&[dir.path().to_path_buf()])
        .unwrap();
    assert_eq!(found.len(), 1);
    assert!(found[0].path.ends_with("abc/events.jsonl"));
    assert_eq!(found[0].kind, SourceKind::JsonlFile);
}

#[test]
fn golden_fixture() {
    let batch = read_all(&CopilotCliAdapter::new(), &[fixture(ROOT)]);
    insta::assert_yaml_snapshot!("fixture", Projection::of(&batch));
    assert_unique_dedup_keys(&batch);
    assert!(batch.warnings.is_empty(), "{:?}", batch.warnings);
    assert_eq!(batch.events.len(), 6);
}

#[test]
fn model_switch_counts_each_model_from_its_snapshots() {
    let batch = read_file(&CopilotCliAdapter::new(), &events_file(SWITCH));
    let s = &batch.sessions[0];
    assert_eq!(s.ext_id, SWITCH);
    assert_eq!(
        s.project_path.as_deref(),
        Some("/u/user/projects/amber-acorn")
    );
    assert_eq!(s.git_branch.as_deref(), Some("main"));
    assert_eq!(s.harness_version.as_deref(), Some("1.0.4"));

    let models: Vec<_> = batch
        .events
        .iter()
        .map(|e| (e.provider.as_str(), e.model.as_str()))
        .collect();
    assert_eq!(
        models,
        [
            ("anthropic", "claude-sonnet-4-5"),
            ("openai", "gpt-5"),
            ("openai", "gpt-5"),
        ]
    );
    assert!(
        batch
            .events
            .iter()
            .all(|e| e.harness == HarnessId::COPILOT_CLI)
    );
    assert_eq!(tokens(&batch, 0), (500, 305, 6000, 24000, 0));
    assert_eq!(tokens(&batch, 1), (6000, 640, 0, 12000, 384));
    assert_eq!(tokens(&batch, 2), (1000, 120, 0, 18000, 64));
    assert_eq!(batch.events[2].dedup_key, format!("{SWITCH}:gpt-5:37760"));

    let tool_use = batch
        .messages
        .iter()
        .find(|m| m.kind == Some(MessageKind::ToolUse))
        .unwrap();
    assert_eq!(tool_use.key, "toolu_01");
    assert_eq!(tool_use.tool_name.as_deref(), Some("view"));
    let result = batch
        .messages
        .iter()
        .find(|m| m.key == "call_b2:result")
        .unwrap();
    assert_eq!(result.role, MessageRole::Tool);
    assert_eq!(result.tool_name.as_deref(), Some("bash"));
    assert_eq!(
        result.preview.as_deref(),
        Some("Excepteur sint occaecat: exit 101")
    );
}

#[test]
fn resume_continues_the_cumulative_totals() {
    let batch = read_file(&CopilotCliAdapter::new(), &events_file(RESUMED));
    assert_eq!(batch.sessions[0].git_branch.as_deref(), Some("main"));
    let models: Vec<_> = batch.events.iter().map(|e| e.model.as_str()).collect();
    assert_eq!(
        models,
        ["claude-haiku-4-5", "claude-haiku-4-5", "claude-opus-4-5"]
    );
    assert_eq!(tokens(&batch, 0), (500, 80, 8500, 0, 0));
    assert_eq!(tokens(&batch, 1), (1000, 120, 0, 5000, 0));
    assert_eq!(tokens(&batch, 2), (1000, 900, 9000, 30000, 0));
    assert_eq!(batch.events[0].git_branch.as_deref(), Some("branch-3f9a2c"));
    assert_eq!(batch.events[2].git_branch.as_deref(), Some("main"));
}

#[test]
fn appended_lines_continue_from_the_cursor() {
    let (_dir, root) = copy_to_temp(&fixture(ROOT));
    let path = root.join(SWITCH).join("events.jsonl");
    let lines = read_lines(&path);
    let adapter = CopilotCliAdapter::new();
    let src = adapter
        .discover(std::slice::from_ref(&path))
        .unwrap()
        .remove(0);

    write_lines(&path, &lines[..13]);
    let first = read_to_end(&adapter, &src, &Cursor::empty());
    assert_eq!(first.events.len(), 2);

    write_lines(&path, &lines);
    let second = read_to_end(&adapter, &src, &first.next_cursor);
    assert_eq!(dedup_keys(&second), [format!("{SWITCH}:gpt-5:37760")]);
    assert_eq!(tokens(&second, 0), (1000, 120, 0, 18000, 64));
    assert_eq!(second.messages.len(), 1);

    let full = read_file(&adapter, &path);
    let mut keys = dedup_keys(&first);
    keys.extend(dedup_keys(&second));
    assert_eq!(keys, dedup_keys(&full));
}

#[test]
fn lower_totals_after_a_restart_count_in_full() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s1").join("events.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let shutdown = |ts: &str, input: u64, output: u64| {
        format!(
            r#"{{"type":"session.shutdown","id":"{ts}","timestamp":"{ts}","data":{{"modelMetrics":{{"gpt-5":{{"usage":{{"inputTokens":{input},"outputTokens":{output},"cacheReadTokens":0,"cacheWriteTokens":0}}}}}}}}}}"#
        )
    };
    write_lines(
        &path,
        &[
            shutdown("2026-11-20T10:00:00Z", 1000, 100),
            shutdown("2026-11-20T11:00:00Z", 400, 50),
        ],
    );
    let batch = read_file(&CopilotCliAdapter::new(), &path);
    assert!(batch.warnings.is_empty(), "{:?}", batch.warnings);
    assert_eq!(batch.sessions[0].ext_id, "s1");
    assert_eq!(tokens(&batch, 0), (1000, 100, 0, 0, 0));
    assert_eq!(tokens(&batch, 1), (400, 50, 0, 0, 0));
}
