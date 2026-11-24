#![cfg(feature = "generic_jsonl")]

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use common::*;
use serde_json::Value;
use tbt_adapters::generic_jsonl::{
    ConfigError, GenericJsonlAdapter, GenericJsonlConfig, TimestampFormat,
};
use tbt_core::{Cursor, HarnessAdapter, HarnessId, ReadBatch, SourceKind, SourceRef};

const PROXY_DAY: &str = "generic_jsonl/openai_proxy/2026-11-22.jsonl";
const ANTHROPIC_S1: &str = "generic_jsonl/anthropic_style/projects/pale-fjord/s-0001.jsonl";

fn example_configs() -> Vec<GenericJsonlConfig> {
    let text = std::fs::read_to_string(fixture("generic_jsonl/config.toml")).unwrap();
    #[derive(serde::Deserialize)]
    struct Doc {
        generic_jsonl: Vec<GenericJsonlConfig>,
    }
    toml::from_str::<Doc>(&text).unwrap().generic_jsonl
}

fn config(id: &str) -> GenericJsonlConfig {
    example_configs().into_iter().find(|c| c.id == id).unwrap()
}

fn proxy() -> GenericJsonlAdapter {
    GenericJsonlAdapter::new(config("llm_proxy")).unwrap()
}

fn anthropic() -> GenericJsonlAdapter {
    GenericJsonlAdapter::new(config("in_house_agent")).unwrap()
}

fn src(path: impl Into<PathBuf>) -> SourceRef {
    SourceRef::new(path, SourceKind::JsonlFile)
}

fn golden(name: &str, adapter: &GenericJsonlAdapter, root: &str) -> ReadBatch {
    let batch = read_all(adapter, &[fixture(root)]);
    assert_unique_dedup_keys(&batch);
    insta::assert_yaml_snapshot!(name, Projection::of(&batch));
    batch
}

fn rel_paths(found: &[SourceRef], root: &Path) -> Vec<String> {
    found
        .iter()
        .map(|s| {
            s.path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect()
}

#[test]
fn example_toml_config_parses_and_validates() {
    let configs = example_configs();
    assert_eq!(configs.len(), 2);
    let proxy = &configs[0];
    assert_eq!(proxy.id, "llm_proxy");
    assert_eq!(proxy.timestamp_format, TimestampFormat::UnixMillis);
    assert!(proxy.input_includes_cache_read);
    assert_eq!(
        proxy.git_branch.as_deref(),
        Some(r#"$.metadata["git branch"]"#)
    );
    assert_eq!(configs[1].filter.as_deref(), Some(r#"$.type == "usage""#));
    for c in &configs {
        assert_eq!(c.validate().unwrap(), [], "{}", c.id);
    }

    let a = GenericJsonlAdapter::new(proxy.clone()).unwrap();
    assert_eq!(a.id(), HarnessId::new("llm_proxy"));
    assert_eq!(a.display_name(), "LLM proxy");
    assert!(a.config_warnings().is_empty());
    assert_eq!(a.config(), proxy);
    if let Some(home) = std::env::home_dir() {
        assert_eq!(
            a.default_roots(),
            [home.join(".local/share/llm-proxy/logs")]
        );
    }
}

#[test]
fn new_rejects_invalid_config() {
    let mut c = config("llm_proxy");
    c.id = "codex".into();
    assert!(matches!(
        GenericJsonlAdapter::new(c),
        Err(ConfigError::InvalidId { .. })
    ));
    let mut c = config("llm_proxy");
    c.dedup_key = "{$.id".into();
    assert_eq!(
        GenericJsonlAdapter::new(c).unwrap_err().to_string(),
        "dedup_key `{$.id`: expected `}` at offset 5"
    );
}

#[test]
fn display_name_falls_back_to_id() {
    let mut c = config("llm_proxy");
    c.display_name = " ".into();
    assert_eq!(
        GenericJsonlAdapter::new(c).unwrap().display_name(),
        "llm_proxy"
    );
}

#[test]
fn golden_openai_proxy() {
    let batch = golden("openai_proxy", &proxy(), "generic_jsonl/openai_proxy");
    let id = HarnessId::new("llm_proxy");
    assert!(batch.events.iter().all(|e| e.harness == id));
    assert!(batch.warnings.iter().all(|w| w.harness == id));

    let a1 = batch
        .events
        .iter()
        .find(|e| e.dedup_key == "chatcmpl-a1")
        .unwrap();
    assert_eq!(
        (
            a1.input_tokens,
            a1.cache_read_tokens,
            a1.output_tokens,
            a1.reasoning_tokens
        ),
        (200, 1000, 300, 120)
    );
    assert_eq!(a1.ts.to_string(), "2026-11-22T09:00:00.123Z");
    assert_eq!(a1.reported_cost, Some("0.00345".parse().unwrap()));
    assert_eq!(
        a1.project_path.as_deref(),
        Some("/u/user/projects/pale-fjord")
    );
    assert_eq!(a1.git_branch.as_deref(), Some("main"));
    let raw: Value = serde_json::from_str(a1.raw.as_ref().unwrap().get()).unwrap();
    assert_eq!(raw["input_tokens"], 1200);
    assert_eq!(raw["cache_read_tokens"], 1000);
    assert!(raw.get("metadata").is_none());

    let b1 = batch
        .events
        .iter()
        .find(|e| e.dedup_key == "chatcmpl-b1")
        .unwrap();
    assert_eq!(b1.provider, "azure");

    let z1 = batch
        .events
        .iter()
        .find(|e| e.dedup_key == "chatcmpl-z1")
        .unwrap();
    assert_eq!(z1.reported_cost, Some("0.015".parse().unwrap()));

    let d1 = batch
        .events
        .iter()
        .find(|e| e.dedup_key == "chatcmpl-d1")
        .unwrap();
    assert_eq!(d1.session_ext_id, "2026-11-22");
}

#[test]
fn golden_anthropic_style() {
    let batch = golden(
        "anthropic_style",
        &anthropic(),
        "generic_jsonl/anthropic_style",
    );
    assert_eq!(
        dedup_keys(&batch),
        [
            "msg_10:req_10",
            "msg_01:req_01",
            "msg_02:req_02",
            "msg_06:req_06"
        ]
    );
    let e = &batch.events[1];
    assert_eq!(e.provider, "anthropic");
    assert_eq!(
        (
            e.input_tokens,
            e.output_tokens,
            e.cache_write_5m_tokens,
            e.cache_write_1h_tokens,
            e.cache_read_tokens,
            e.reasoning_tokens,
        ),
        (12, 340, 2000, 0, 15000, 80)
    );
    assert_eq!(batch.events[2].ts.to_string(), "2026-11-22T08:00:35Z");
    let s1 = batch
        .sessions
        .iter()
        .find(|s| s.ext_id == "s-0001")
        .unwrap();
    assert_eq!(s1.git_branch.as_deref(), Some("branch-a1b2c3"));
    assert_eq!(
        s1.started_at.unwrap().to_string(),
        "2026-11-22T08:00:05.25Z"
    );
    assert_eq!(s1.ended_at.unwrap().to_string(), "2026-11-22T08:02:00Z");
    assert!(batch.messages.is_empty());
}

#[test]
fn discover_follows_glob() {
    let root = fixture("generic_jsonl/openai_proxy");
    let found = proxy()
        .discover(&[root.clone(), root.join("missing")])
        .unwrap();
    assert_eq!(
        rel_paths(&found, &root),
        ["2026-11-22.jsonl", "archive/2026-11-21.jsonl"]
    );

    let root = fixture("generic_jsonl");
    let found = anthropic()
        .discover(&[root.join("anthropic_style")])
        .unwrap();
    assert_eq!(
        rel_paths(&found, &root),
        [
            "anthropic_style/projects/keen-lagoon/s-0002.jsonl",
            "anthropic_style/projects/pale-fjord/s-0001.jsonl",
        ]
    );
    assert!(anthropic().discover(&[root]).unwrap().is_empty());
}

#[test]
fn reread_from_cursor_yields_nothing_new() {
    for (a, root) in [
        (proxy(), "generic_jsonl/openai_proxy"),
        (anthropic(), "generic_jsonl/anthropic_style"),
    ] {
        for s in a.discover(&[fixture(root)]).unwrap() {
            let first = a.read(&s, &Cursor::empty()).unwrap();
            assert!(!first.events.is_empty());
            let again = a.read(&s, &first.next_cursor).unwrap();
            assert!(again.is_empty(), "{}", s.path.display());
            assert_eq!(again.next_cursor, first.next_cursor);
        }
    }
}

#[test]
fn appended_lines_yield_only_new_events() {
    for (a, rel) in [(proxy(), PROXY_DAY), (anthropic(), ANTHROPIC_S1)] {
        let full = read_file(&a, &fixture(rel));
        let lines = read_lines(&fixture(rel));
        for split in 1..lines.len() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(Path::new(rel).file_name().unwrap());
            write_lines(&path, &lines[..split]);
            let s = src(&path);
            let head = a.read(&s, &Cursor::empty()).unwrap();

            append(&path, &(lines[split..].join("\n") + "\n"));
            let tail = a.read(&s, &head.next_cursor).unwrap();

            let mut all = head.clone();
            merge(&mut all, tail);
            assert_eq!(
                Projection::of(&all).warnings,
                Projection::of(&full).warnings,
                "{rel} split at {split}"
            );
            let keys: BTreeSet<_> = dedup_keys(&all).into_iter().collect();
            let expected: BTreeSet<_> = dedup_keys(&full).into_iter().collect();
            assert_eq!(keys, expected, "{rel} split at {split}");
        }
    }
}

#[test]
fn repeat_across_reads_is_left_to_store_dedup() {
    let lines = read_lines(&fixture(PROXY_DAY));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("p.jsonl");
    write_lines(&path, &lines[..3]);
    let a = proxy();
    let s = src(&path);
    let head = a.read(&s, &Cursor::empty()).unwrap();
    append(&path, &(lines[3].clone() + "\n"));
    let tail = a.read(&s, &head.next_cursor).unwrap();
    assert_eq!(dedup_keys(&head), ["chatcmpl-a1", "chatcmpl-a2"]);
    assert_eq!(dedup_keys(&tail), ["chatcmpl-a1"]);
}

#[test]
fn truncated_file_restarts_from_zero() {
    let (_dir, path) = copy_to_temp(&fixture(ANTHROPIC_S1));
    let a = anthropic();
    let s = src(&path);
    let first = a.read(&s, &Cursor::empty()).unwrap();

    let lines = read_lines(&path);
    write_lines(&path, &lines[..3]);
    let expected = read_file(&a, &path);
    assert_eq!(dedup_keys(&expected), ["msg_01:req_01"]);
    assert!(expected.events.len() < first.events.len());

    let after = a.read(&s, &first.next_cursor).unwrap();
    assert_eq!(dedup_keys(&after), dedup_keys(&expected));
    assert_eq!(after.next_cursor, expected.next_cursor);
    assert_eq!(after.sessions, expected.sessions);
}

#[test]
fn partial_trailing_line_is_not_consumed() {
    let lines = read_lines(&fixture(ANTHROPIC_S1));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    let head = lines[..2].join("\n") + "\n";
    let (cut_a, cut_b) = lines[2].split_at(lines[2].len() / 2);
    std::fs::write(&path, format!("{head}{cut_a}")).unwrap();

    let a = anthropic();
    let s = src(&path);
    let first = a.read(&s, &Cursor::empty()).unwrap();
    assert!(first.events.is_empty() && first.warnings.is_empty());
    assert_eq!(
        first.next_cursor.as_json()["byte_offset"],
        head.len() as u64
    );

    append(&path, &format!("{cut_b}\n"));
    let second = a.read(&s, &first.next_cursor).unwrap();
    assert_eq!(dedup_keys(&second), ["msg_01:req_01"]);
    assert!(second.warnings.is_empty());
}

#[test]
fn invalid_cursor_rereads_with_warning() {
    let a = anthropic();
    let s = src(fixture(ANTHROPIC_S1));
    let bogus = Cursor::from_json(serde_json::json!({"byte_offset": "x"}));
    let batch = a.read(&s, &bogus).unwrap();
    assert_eq!(batch.events.len(), 3);
    assert!(batch.warnings[0].message.contains("invalid cursor"));
}

#[test]
fn constant_session_and_missing_optional_fields() {
    let mut c = config("in_house_agent");
    c.session_id = Some("one-session".into());
    c.project_path = None;
    c.git_branch = Some("trunk".into());
    c.reported_cost = None;
    c.reasoning_tokens = None;
    let batch = read_file(
        &GenericJsonlAdapter::new(c).unwrap(),
        &fixture(ANTHROPIC_S1),
    );
    assert_eq!(batch.sessions.len(), 1);
    assert_eq!(batch.sessions[0].ext_id, "one-session");
    assert_eq!(batch.sessions[0].project_path, None);
    assert!(batch.events.iter().all(|e| {
        e.git_branch.as_deref() == Some("trunk")
            && e.project_path.is_none()
            && e.reported_cost.is_none()
            && e.reasoning_tokens == 0
    }));
}
