#![cfg(feature = "deepseek_harness")]

mod common;

use std::path::{Path, PathBuf};

use common::*;
use tbt_adapters::DeepSeekHarnessAdapter;
use tbt_core::{Cursor, HarnessAdapter, SourceKind, SourceRef};

const ROOT: &str = "deepseek_harness/v4/sessions";
const LOREM: &str = "--home-user-projects-lorem--";

fn log(root: &Path, project: &str, id: &str, file: &str) -> SourceRef {
    SourceRef::new(
        root.join(project).join(id).join(file),
        SourceKind::JsonlFile,
    )
}

fn main_log(root: &Path) -> SourceRef {
    log(root, LOREM, "ds-main", "session.v4.jsonl.zstd")
}

fn zstd_frame(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    ruzstd::encoding::compress(
        text.as_bytes(),
        &mut out,
        ruzstd::encoding::CompressionLevel::Fastest,
    );
    out
}

fn untorn_copy() -> (tempfile::TempDir, PathBuf) {
    let (dir, root) = copy_to_temp(&fixture(ROOT));
    let path = main_log(&root).path;
    let bytes = std::fs::read(&path).unwrap();
    let mut keep = bytes.len();
    for cut in (0..bytes.len()).rev() {
        if bytes[cut..].starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
            keep = cut;
            break;
        }
    }
    std::fs::write(&path, &bytes[..keep]).unwrap();
    (dir, root)
}

#[test]
fn golden() {
    let batch = read_all(&DeepSeekHarnessAdapter::new(), &[fixture(ROOT)]);
    insta::assert_yaml_snapshot!("fixture", Projection::of(&batch));
}

#[test]
fn discovery_keeps_the_newest_generation_per_session() {
    let root = fixture(ROOT);
    let found = DeepSeekHarnessAdapter::new()
        .discover(std::slice::from_ref(&root))
        .unwrap();
    let rel: Vec<_> = found
        .iter()
        .map(|s| {
            s.path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        rel,
        [
            "--home-user-projects-ipsum--/ds-migrated/session.v4.jsonl.zstd",
            "--home-user-projects-lorem--/ds-fork/session.v4.jsonl.zstd",
            "--home-user-projects-lorem--/ds-main/session.v4.jsonl.zstd",
            "--home-user-projects-lorem--/ds-sub/session.v4.jsonl.zstd",
            "_no-cwd/ds-plain/session.v4.jsonl",
        ]
    );
}

#[test]
fn main_session_counts_messages_failed_attempts_and_compaction() {
    let root = fixture(ROOT);
    let batch = read_to_end(
        &DeepSeekHarnessAdapter::new(),
        &main_log(&root),
        &Cursor::empty(),
    );
    assert!(batch.warnings.is_empty(), "{:?}", batch.warnings);
    let view: Vec<_> = batch
        .events
        .iter()
        .map(|e| {
            (
                e.dedup_key.as_str(),
                e.provider.as_str(),
                e.model.as_str(),
                e.input_tokens,
                e.output_tokens,
                e.cache_read_tokens,
                e.reasoning_tokens,
            )
        })
        .collect();
    assert_eq!(
        view,
        [
            (
                "ds-main:1:1",
                "deepseek",
                "deepseek-v4-flash",
                8082,
                235,
                384,
                75
            ),
            (
                "ds-main:1:2:attempt:1",
                "deepseek",
                "deepseek-v4-flash",
                150,
                12,
                8320,
                0
            ),
            (
                "ds-main:1:2",
                "deepseek",
                "deepseek-v4-flash",
                199,
                84,
                9216,
                81
            ),
            (
                "ds-main:2:1",
                "deepseek",
                "deepseek-v4-pro",
                1316,
                249,
                7552,
                72
            ),
            (
                "ds-main:compaction:cmp-1",
                "deepseek",
                "deepseek-v4-flash",
                9000,
                300,
                0,
                0
            ),
        ]
    );
    let s = &batch.sessions[0];
    assert_eq!(s.title.as_deref(), Some("Fix the flaky lorem test"));
    assert_eq!(s.project_path.as_deref(), Some("/home/user/projects/lorem"));
}

#[test]
fn fork_skips_inherited_rows_and_subagent_links_its_parent() {
    let root = fixture(ROOT);
    let adapter = DeepSeekHarnessAdapter::new();
    let fork = read_to_end(
        &adapter,
        &log(&root, LOREM, "ds-fork", "session.v4.jsonl.zstd"),
        &Cursor::empty(),
    );
    let [only] = fork.events.as_slice() else {
        panic!("expected one fork event: {:?}", fork.events);
    };
    assert_eq!(
        (only.provider.as_str(), only.model.as_str()),
        ("moonshotai", "kimi-k2.6")
    );
    assert_eq!(
        fork.sessions[0].resumed_from_ext_id.as_deref(),
        Some("ds-main")
    );
    assert_eq!(fork.sessions[0].parent_ext_id, None);

    let sub = read_to_end(
        &adapter,
        &log(&root, LOREM, "ds-sub", "session.v4.jsonl.zstd"),
        &Cursor::empty(),
    );
    assert_eq!(sub.events.len(), 1);
    assert_eq!(sub.events[0].provider, "deepseek");
    assert_eq!(sub.sessions[0].parent_ext_id.as_deref(), Some("ds-main"));
}

#[test]
fn plain_log_ignores_a_torn_last_line() {
    let root = fixture(ROOT);
    let batch = read_to_end(
        &DeepSeekHarnessAdapter::new(),
        &log(&root, "_no-cwd", "ds-plain", "session.v4.jsonl"),
        &Cursor::empty(),
    );
    assert!(batch.warnings.is_empty(), "{:?}", batch.warnings);
    let [e] = batch.events.as_slice() else {
        panic!("expected one event: {:?}", batch.events);
    };
    assert_eq!(
        (e.provider.as_str(), e.cache_write_5m_tokens),
        ("anthropic", 4000)
    );
    assert_eq!(e.project_path, None);
}

#[test]
fn appended_frames_are_read_and_unchanged_logs_are_skipped() {
    let (_dir, root) = untorn_copy();
    let adapter = DeepSeekHarnessAdapter::new();
    let src = main_log(&root);
    let first = read_to_end(&adapter, &src, &Cursor::empty());
    assert_eq!(first.events.len(), 5);

    let again = adapter.read(&src, &first.next_cursor).unwrap();
    assert!(again.events.is_empty() && again.sessions.is_empty());

    let mut bytes = std::fs::read(&src.path).unwrap();
    bytes.extend(zstd_frame(concat!(
        r#"{"type":"turn/start","seq":26,"time":1795165400000,"data":{"turn":3}}"#,
        "\n",
        r#"{"type":"assistant/message","seq":27,"time":1795165404000,"data":{"turn":3,"step":1,"#,
        r#""message":{"role":"assistant","content":[],"source":{"kind":"model","provider":"deepseek-official","model":"deepseek-v4-pro"},"id":"m27"},"#,
        r#""stream":[],"usage":{"inputTokens":64,"outputTokens":32,"cacheReadTokens":9600}}}"#,
        "\n",
    )));
    std::fs::write(&src.path, bytes).unwrap();

    let next = read_to_end(&adapter, &src, &first.next_cursor);
    assert!(next.warnings.is_empty(), "{:?}", next.warnings);
    let added: Vec<_> = next
        .events
        .iter()
        .filter(|e| !first.events.iter().any(|f| f.dedup_key == e.dedup_key))
        .map(|e| (e.dedup_key.as_str(), e.model.as_str(), e.cache_read_tokens))
        .collect();
    assert_eq!(added, [("ds-main:3:1", "deepseek-v4-pro", 9600)]);
}
