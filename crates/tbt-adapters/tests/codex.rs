#![cfg(feature = "codex")]

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use common::*;
use serde_json::Value;
use tbt_adapters::codex::CodexAdapter;
use tbt_core::{
    Cursor, HarnessAdapter, HarnessId, MessageKind, MessageRole, ReadBatch, SourceKind, SourceRef,
    TokenCounts,
};

const SHORT: &str = "codex/0.135.0/sessions/2026/05/31/rollout-2026-05-31T19-42-32-019e7f21-575c-7721-8bee-e12a2dcde581.jsonl";
const TOOL_HEAVY: &str = "codex/0.139.0/sessions/2026/06/11/rollout-2026-06-11T23-38-02-019eb89e-e981-75ab-83fb-4ce7015e2fb7.jsonl";
const RESUME_BASE: &str = "codex/0.155.0/sessions/2026/09/18/rollout-2026-09-18T13-35-05-01a0b44c-76aa-76b8-86dd-e11f3ffd813c.jsonl";
const RESUME_NEXT: &str = "codex/0.155.0/sessions/2026/09/18/rollout-2026-09-18T15-31-24-01a0b44c-76aa-76b8-86dd-e11f3ffd813c_01a0b4b6-f515-7c6f-8a5f-1faf9245c651.jsonl";
const LONG_COMPACTED: &str = "codex/0.163.0/sessions/2026/10/20/rollout-2026-10-20T10-49-13-01a15880-1cd3-78c2-8cc0-d25d5501e60c.jsonl";
const MODEL_SWITCH: &str = "codex/0.163.0/sessions/2026/10/20/rollout-2026-10-20T13-05-40-01a158fd-0b21-7550-aaf7-d75b42c0c0de.jsonl";
const FORK_PARENT: &str = "codex/0.165.0/sessions/2026/11/03/rollout-2026-11-03T13-25-20-01a1a15f-00cc-7ca3-b42e-51fcf7d71147.jsonl";
const FORK_CHILD: &str = "codex/0.165.0/sessions/2026/11/03/rollout-2026-11-03T20-23-44-01a1a2de-0e3c-709f-85bd-85d08a98fcd0.jsonl";
const NO_USAGE: &str = "codex/0.171.0/sessions/2026/11/22/rollout-2026-11-22T01-31-59-01a200aa-befa-7c90-9386-8e9f0ba1cc9f.jsonl";

fn adapter() -> CodexAdapter {
    CodexAdapter::new()
}

fn src(path: impl Into<PathBuf>) -> SourceRef {
    SourceRef::new(path, SourceKind::JsonlFile)
}

fn golden(name: &str, rel: &str) -> ReadBatch {
    let batch = read_all(&adapter(), &[fixture(rel)]);
    assert_unique_dedup_keys(&batch);
    insta::assert_yaml_snapshot!(name, Projection::of(&batch));
    batch
}

fn sum(batch: &ReadBatch) -> TokenCounts {
    batch.events.iter().map(|e| e.tokens()).sum()
}

fn tc(input: u64, output: u64, cache_read: u64, reasoning: u64) -> TokenCounts {
    TokenCounts {
        input,
        output,
        cache_read,
        reasoning,
        ..Default::default()
    }
}

fn json_lines(path: &Path) -> Vec<Value> {
    read_lines(path)
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn thread_counter_span(path: &Path) -> u64 {
    let lines = json_lines(path);
    let records: Vec<&Value> = lines
        .iter()
        .filter(|l| l["type"] == "token_usage_record")
        .map(|l| &l["payload"])
        .collect();
    if let (Some(first), Some(last)) = (records.first(), records.last()) {
        let start = first["thread_token_usage"]["total_tokens"]
            .as_u64()
            .unwrap()
            - first["usage"]["total_tokens"].as_u64().unwrap();
        return last["thread_token_usage"]["total_tokens"].as_u64().unwrap() - start;
    }
    lines
        .iter()
        .filter_map(|l| l["payload"]["info"]["total_token_usage"]["total_tokens"].as_u64())
        .max()
        .unwrap_or(0)
}

fn assert_matches_counter(batch: &ReadBatch, rel: &str) {
    assert_eq!(
        sum(batch).total(),
        thread_counter_span(&fixture(rel)),
        "{rel}"
    );
}

#[test]
fn golden_short() {
    let batch = golden("short", SHORT);
    assert_eq!(batch.events.len(), 2);
    assert_eq!(sum(&batch), tc(2456, 61, 20736, 24));
    assert_matches_counter(&batch, SHORT);
    let e = &batch.events[0];
    assert_eq!(e.harness, HarnessId::CODEX);
    assert_eq!(e.dedup_key, "019e7f21-575c-7721-8bee-e12a2dcde581:11599");
    assert_eq!(
        (e.provider.as_str(), e.model.as_str()),
        ("openai", "gpt-5.5")
    );
    assert_eq!(
        (
            e.input_tokens,
            e.cache_read_tokens,
            e.output_tokens,
            e.reasoning_tokens
        ),
        (1959, 9600, 40, 24)
    );
    assert_eq!(e.ts.to_string(), "2026-05-31T17:42:34.731Z");
    assert_eq!(
        e.project_path.as_deref(),
        Some("/mnt/bold-juniper-8a/projects")
    );
    assert!(e.raw.as_ref().unwrap().get().contains("last_token_usage"));
    assert!(e.reported_cost.is_none());
    let s = &batch.sessions[0];
    assert_eq!(s.ext_id, "019e7f21-575c-7721-8bee-e12a2dcde581");
    assert_eq!(s.harness_version.as_deref(), Some("0.135.0"));
    assert!(s.baseline_hint.is_none());
}

#[test]
fn golden_tool_heavy_skips_repeated_total() {
    let batch = golden("tool_heavy", TOOL_HEAVY);
    assert_eq!(batch.events.len(), 77);
    assert_eq!(sum(&batch), tc(324_611, 29_156, 6_684_544, 10_181));
    assert_matches_counter(&batch, TOOL_HEAVY);
    assert_eq!(batch.sessions[0].git_branch.as_deref(), Some("master"));
    let results = batch
        .messages
        .iter()
        .filter(|m| m.kind == Some(MessageKind::ToolResult));
    let mut n = 0;
    for m in results {
        n += 1;
        assert_eq!(m.role, MessageRole::Tool);
        assert!(m.tool_name.is_some(), "tool result {} has no tool", m.key);
    }
    assert_eq!(n, 125);
}

#[test]
fn golden_long_with_compaction_counts_every_response() {
    let batch = golden("long_compaction", LONG_COMPACTED);
    assert_eq!(batch.events.len(), 126);
    assert_eq!(sum(&batch), tc(211_997, 70_275, 15_188_736, 22_994));
    assert_matches_counter(&batch, LONG_COMPACTED);
    assert!(
        batch
            .events
            .iter()
            .all(|e| e.dedup_key.starts_with("resp_"))
    );
    assert!(
        batch
            .events
            .iter()
            .any(|e| e.dedup_key == "resp_363b4b0222c888dba8301704e3664210fc8b4a909aa062a7db")
    );
}

#[test]
fn golden_model_switch_without_usage() {
    let batch = golden("model_switch", MODEL_SWITCH);
    assert!(batch.events.is_empty());
    assert!(batch.warnings.is_empty());
    assert_eq!(batch.sessions.len(), 1);
}

#[test]
fn golden_no_usage() {
    let batch = golden("no_usage", NO_USAGE);
    assert!(batch.events.is_empty());
    assert!(batch.warnings.is_empty());
    let s = &batch.sessions[0];
    assert_eq!(s.ext_id, "01a200aa-befa-7c90-9386-8e9f0ba1cc9f");
    assert_eq!(s.harness_version.as_deref(), Some("0.171.0"));
    assert_eq!(s.git_branch.as_deref(), Some("master"));
}

#[test]
fn resumed_thread_shares_session_and_does_not_double_count() {
    let base = golden("resumed_base", RESUME_BASE);
    let next = golden("resumed_continuation", RESUME_NEXT);
    assert_eq!((base.events.len(), next.events.len()), (21, 24));
    assert_eq!(sum(&base), tc(24_286, 4_586, 506_624, 245));
    assert_eq!(sum(&next), tc(26_700, 4_741, 677_120, 138));
    assert_matches_counter(&base, RESUME_BASE);
    assert_matches_counter(&next, RESUME_NEXT);

    let thread = "01a0b44c-76aa-76b8-86dd-e11f3ffd813c";
    assert_eq!(base.sessions[0].ext_id, thread);
    assert_eq!(next.sessions[0].ext_id, thread);
    assert_eq!(next.sessions[0].resumed_from_ext_id, None);
    assert!(next.events.iter().all(|e| e.session_ext_id == thread));

    let a: BTreeSet<_> = dedup_keys(&base).into_iter().collect();
    let b: BTreeSet<_> = dedup_keys(&next).into_iter().collect();
    assert!(a.is_disjoint(&b));
    let keys: BTreeSet<_> = base
        .messages
        .iter()
        .chain(&next.messages)
        .map(|m| m.key.as_str())
        .collect();
    assert_eq!(keys.len(), base.messages.len() + next.messages.len());

    let both = (sum(&base) + sum(&next)).total();
    assert_eq!(both, 535_496 + (833_664 - 125_103));
}

#[test]
fn fork_links_parent_and_does_not_double_count() {
    let parent = golden("fork_parent", FORK_PARENT);
    let child = golden("fork_child", FORK_CHILD);
    assert_eq!((parent.events.len(), child.events.len()), (124, 7));
    assert_eq!(sum(&parent), tc(166_344, 40_236, 15_377_664, 15_408));
    assert_eq!(sum(&child), tc(197_979, 3_988, 1_266_048, 699));
    assert_matches_counter(&parent, FORK_PARENT);
    assert_matches_counter(&child, FORK_CHILD);

    let p = "01a1a15f-00cc-7ca3-b42e-51fcf7d71147";
    let c = &child.sessions[0];
    assert_eq!(c.ext_id, "01a1a2de-0e3c-709f-85bd-85d08a98fcd0");
    assert_eq!(c.resumed_from_ext_id.as_deref(), Some(p));
    assert_eq!(c.parent_ext_id, None);
    assert_eq!(parent.sessions[0].resumed_from_ext_id, None);

    let a: BTreeSet<_> = dedup_keys(&parent).into_iter().collect();
    let b: BTreeSet<_> = dedup_keys(&child).into_iter().collect();
    assert!(a.is_disjoint(&b));
    assert_eq!((sum(&parent) + sum(&child)).total(), 17_052_259);
}

#[test]
fn discover_finds_all_fixtures_in_order() {
    let found = adapter()
        .discover(&[fixture("codex"), fixtures_root().join("nope")])
        .unwrap();
    assert_eq!(found.len(), 9);
    assert!(found.iter().all(|s| s.kind == SourceKind::JsonlFile));
    assert!(found.is_sorted_by(|a, b| a.path.as_os_str() < b.path.as_os_str()));
}

#[test]
fn all_fixtures_read_without_warnings() {
    let batch = read_all(&adapter(), &[fixture("codex")]);
    assert!(batch.warnings.is_empty(), "{:?}", batch.warnings);
    assert_unique_dedup_keys(&batch);
    assert_eq!(batch.events.len(), 2 + 77 + 21 + 24 + 126 + 124 + 7);
}

#[test]
fn reread_from_cursor_yields_nothing_new() {
    let a = adapter();
    for s in a.discover(&[fixture("codex")]).unwrap() {
        let first = a.read(&s, &Cursor::empty()).unwrap();
        assert!(!first.sessions.is_empty());
        let again = a.read(&s, &first.next_cursor).unwrap();
        assert!(again.is_empty(), "{}", s.path.display());
        assert_eq!(again.next_cursor, first.next_cursor);
    }
}

#[test]
fn touched_file_without_new_lines_yields_nothing() {
    let (_dir, path) = copy_to_temp(&fixture(TOOL_HEAVY));
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

fn assert_split_reads_match(rel: &str) {
    let full = read_file(&adapter(), &fixture(rel));
    let lines = read_lines(&fixture(rel));
    let name = Path::new(rel).file_name().unwrap();
    for split in [1, lines.len() / 3, lines.len() / 2, lines.len() - 1] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        write_lines(&path, &lines[..split]);
        let a = adapter();
        let s = src(&path);
        let head = a.read(&s, &Cursor::empty()).unwrap();

        append(&path, &(lines[split..].join("\n") + "\n"));
        let tail = a.read(&s, &head.next_cursor).unwrap();

        let mut keys = dedup_keys(&head);
        keys.extend(dedup_keys(&tail));
        assert_eq!(keys, dedup_keys(&full), "{rel} split at line {split}");
        assert_eq!(sum(&head) + sum(&tail), sum(&full));
        let models: Vec<_> = head
            .events
            .iter()
            .chain(&tail.events)
            .map(|e| &e.model)
            .collect();
        let full_models: Vec<_> = full.events.iter().map(|e| &e.model).collect();
        assert_eq!(models, full_models);
        let mut msg_keys: Vec<_> = head.messages.iter().map(|m| &m.key).collect();
        msg_keys.extend(tail.messages.iter().map(|m| &m.key));
        let full_keys: Vec<_> = full.messages.iter().map(|m| &m.key).collect();
        assert_eq!(msg_keys, full_keys);
        let tools: Vec<_> = head
            .messages
            .iter()
            .chain(&tail.messages)
            .map(|m| &m.tool_name)
            .collect();
        let full_tools: Vec<_> = full.messages.iter().map(|m| &m.tool_name).collect();
        assert_eq!(tools, full_tools);
        assert_eq!(tail.sessions[0].ext_id, full.sessions[0].ext_id);
        assert_eq!(tail.sessions[0].project_path, full.sessions[0].project_path);
        assert!(head.warnings.is_empty() && tail.warnings.is_empty());
    }
}

#[test]
fn appended_lines_yield_only_new_events_token_count_format() {
    assert_split_reads_match(TOOL_HEAVY);
}

#[test]
fn appended_lines_yield_only_new_events_record_format() {
    assert_split_reads_match(RESUME_NEXT);
    assert_split_reads_match(LONG_COMPACTED);
}

#[test]
fn truncated_file_restarts_from_zero() {
    let (_dir, path) = copy_to_temp(&fixture(TOOL_HEAVY));
    let a = adapter();
    let s = src(&path);
    let first = a.read(&s, &Cursor::empty()).unwrap();

    let lines = read_lines(&path);
    write_lines(&path, &lines[..lines.len() / 4]);
    let expected = read_file(&a, &path);
    assert!(!expected.events.is_empty() && expected.events.len() < first.events.len());

    let after = a.read(&s, &first.next_cursor).unwrap();
    assert_eq!(dedup_keys(&after), dedup_keys(&expected));
    assert_eq!(after.messages.len(), expected.messages.len());
    assert_eq!(after.next_cursor, expected.next_cursor);
}

#[test]
fn partial_trailing_line_is_not_consumed() {
    let lines = read_lines(&fixture(RESUME_BASE));
    let usage_line = lines
        .iter()
        .position(|l| l.contains("\"token_usage_record\""))
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-p.jsonl");
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
    assert_eq!(second.events.len(), 21);
    assert_eq!(
        second.events[0].session_ext_id,
        "01a0b44c-76aa-76b8-86dd-e11f3ffd813c"
    );
    assert_eq!(second.events[0].model, "gpt-6-astra");
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
    let usage_line = lines
        .iter()
        .position(|l| l.contains("\"token_count\""))
        .unwrap();
    let mut broken = lines[..usage_line].to_vec();
    broken.push("{not json".into());
    broken.push("[1,2]".into());
    broken.push(r#"{"no_type":true}"#.into());
    broken.push(r#"{"timestamp":"2026-05-31T17:42:34Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":"x"}}}}"#.into());
    broken.extend_from_slice(&lines[usage_line..]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.jsonl");
    write_lines(&path, &broken);

    let batch = read_file(&adapter(), &path);
    assert_eq!(batch.events.len(), 2);
    let at: Vec<_> = batch
        .warnings
        .iter()
        .map(|w| (w.line, w.harness.clone()))
        .collect();
    let n = usage_line as u64;
    assert_eq!(
        at,
        (1..=4)
            .map(|i| (Some(n + i), HarnessId::CODEX))
            .collect::<Vec<_>>()
    );
    let offset: usize = broken[..usage_line].iter().map(|l| l.len() + 1).sum();
    assert_eq!(batch.warnings[0].byte_offset, Some(offset as u64));
    assert!(batch.warnings[0].message.starts_with("invalid JSON"));
    assert!(batch.warnings[3].message.contains("total_token_usage"));
}

#[test]
fn invalid_cursor_rereads_with_warning() {
    let a = adapter();
    let s = src(fixture(SHORT));
    let bogus = Cursor::from_json(serde_json::json!({"byte_offset": "x"}));
    let batch = a.read(&s, &bogus).unwrap();
    assert_eq!(batch.events.len(), 2);
    assert_eq!(batch.warnings.len(), 1);
    assert!(batch.warnings[0].message.contains("invalid cursor"));
}

fn usage(input: u64, cached: u64, output: u64) -> String {
    format!(
        r#"{{"input_tokens":{input},"cached_input_tokens":{cached},"output_tokens":{output},"reasoning_output_tokens":0,"total_tokens":{}}}"#,
        input + output
    )
}

fn token_count(ts: &str, total: &str, last: &str) -> String {
    format!(
        r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{total},"last_token_usage":{last}}}}}}}"#
    )
}

#[test]
fn legacy_token_counts_and_messages() {
    let lines = [
        r#"{"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{"id":"t1","cwd":"/w","cli_version":"0.100.0","model_provider":"azure","git":{"branch":"dev"}}}"#.to_owned(),
        r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":null}}"#.to_owned(),
        r#"{"timestamp":"2026-01-01T00:00:01Z","type":"turn_context","payload":{"model":"gpt-a","cwd":"/w/sub"}}"#.to_owned(),
        r#"{"timestamp":"2026-01-01T00:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}"#.to_owned(),
        r#"{"timestamp":"2026-01-01T00:00:02Z","type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"think"}],"content":null}}"#.to_owned(),
        r#"{"timestamp":"2026-01-01T00:00:02Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"cmd\":\"ls\"}","call_id":"call_1"}}"#.to_owned(),
        r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_1","output":"a.txt"}}"#.to_owned(),
        token_count("2026-01-01T00:00:03Z", &usage(100, 60, 10), &usage(100, 60, 10)),
        token_count("2026-01-01T00:00:04Z", &usage(100, 60, 10), &usage(100, 60, 10)),
        r#"{"timestamp":"2026-01-01T00:00:05Z","type":"turn_context","payload":{"model":"gpt-b"}}"#.to_owned(),
        r#"{"timestamp":"2026-01-01T00:00:05Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}}"#.to_owned(),
        token_count("2026-01-01T00:00:06Z", &usage(250, 150, 30), &usage(150, 90, 20)),
        r#"{"timestamp":"2026-01-01T00:00:07Z","type":"something_new","payload":{}}"#.to_owned(),
    ];
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-2026-01-01T00-00-00-t1.jsonl");
    write_lines(&path, &lines);
    let batch = read_file(&adapter(), &path);

    assert!(batch.warnings.is_empty(), "{:?}", batch.warnings);
    assert_eq!(dedup_keys(&batch), ["t1:110", "t1:280"]);
    let e: Vec<_> = batch
        .events
        .iter()
        .map(|e| (e.provider.as_str(), e.model.as_str(), e.tokens()))
        .collect();
    assert_eq!(
        e,
        [
            ("azure", "gpt-a", tc(40, 10, 60, 0)),
            ("azure", "gpt-b", tc(60, 20, 90, 0)),
        ]
    );
    assert_eq!(batch.events[0].project_path.as_deref(), Some("/w/sub"));
    assert_eq!(batch.events[0].git_branch.as_deref(), Some("dev"));

    let s = &batch.sessions[0];
    assert_eq!(batch.sessions.len(), 1);
    assert_eq!(s.ext_id, "t1");
    assert_eq!(s.harness_version.as_deref(), Some("0.100.0"));
    assert_eq!(s.started_at.unwrap().to_string(), "2026-01-01T00:00:00Z");
    assert_eq!(s.ended_at.unwrap().to_string(), "2026-01-01T00:00:07Z");

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
                "2026-01-01T00:00:01Z#4",
                MessageRole::User,
                Some(MessageKind::Text),
                None,
                Some("hi"),
                None
            ),
            (
                "2026-01-01T00:00:02Z#5",
                MessageRole::Assistant,
                Some(MessageKind::Thinking),
                None,
                Some("think"),
                Some("t1:110")
            ),
            (
                "call_1",
                MessageRole::Assistant,
                Some(MessageKind::ToolUse),
                Some("shell"),
                Some(r#"{"cmd":"ls"}"#),
                Some("t1:110")
            ),
            (
                "call_1:output",
                MessageRole::Tool,
                Some(MessageKind::ToolResult),
                Some("shell"),
                Some("a.txt"),
                None
            ),
            (
                "2026-01-01T00:00:05Z#11",
                MessageRole::Assistant,
                Some(MessageKind::Text),
                None,
                Some("done"),
                Some("t1:280")
            ),
        ]
    );
}

#[test]
fn records_take_precedence_over_token_counts() {
    let record = |ts: &str, id: &str, u: &str, thread: &str| {
        format!(
            r#"{{"timestamp":"{ts}","type":"token_usage_record","payload":{{"thread_id":"t2","response_id":"{id}","usage":{u},"thread_token_usage":{thread}}}}}"#
        )
    };
    let lines = [
        r#"{"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{"id":"t2","history_mode":"paginated","history_base":{"thread_id":"t2","end_ordinal_exclusive":5}}}"#.to_owned(),
        token_count("2026-01-01T00:00:00Z", &usage(1000, 0, 50), &usage(500, 0, 20)),
        r#"{"timestamp":"2026-01-01T00:00:01Z","type":"turn_context","payload":{"model":"gpt-c"}}"#.to_owned(),
        record("2026-01-01T00:00:02Z", "resp_a", &usage(100, 40, 5), &usage(1100, 40, 55)),
        token_count("2026-01-01T00:00:02Z", &usage(1100, 40, 55), &usage(100, 40, 5)),
        record("2026-01-01T00:00:03Z", "resp_b", &usage(200, 100, 7), &usage(1300, 140, 62)),
        record("2026-01-01T00:00:03Z", "resp_b", &usage(200, 100, 7), &usage(1300, 140, 62)),
        token_count("2026-01-01T00:00:04Z", &usage(1300, 140, 62), &usage(200, 100, 7)),
    ];
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("r.jsonl");
    write_lines(&path, &lines);
    let batch = read_file(&adapter(), &path);

    assert!(batch.warnings.is_empty(), "{:?}", batch.warnings);
    assert_eq!(dedup_keys(&batch), ["resp_a", "resp_b"]);
    assert_eq!(sum(&batch), tc(160, 12, 140, 0));
    assert!(
        batch
            .events
            .iter()
            .all(|e| e.model == "gpt-c" && e.provider == "openai")
    );
    assert_eq!(batch.sessions[0].resumed_from_ext_id, None);
    assert!(batch.sessions[0].baseline_hint.is_none());
}

#[test]
fn usage_without_model_is_kept_with_warning() {
    let lines = [
        r#"{"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{"id":"t3"}}"#
            .to_owned(),
        token_count("2026-01-01T00:00:01Z", &usage(10, 0, 1), &usage(10, 0, 1)),
    ];
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("u.jsonl");
    write_lines(&path, &lines);
    let batch = read_file(&adapter(), &path);
    assert_eq!(batch.events.len(), 1);
    assert_eq!(batch.events[0].model, "unknown");
    assert_eq!(batch.warnings.len(), 1);
    assert_eq!(batch.warnings[0].line, Some(2));
}
