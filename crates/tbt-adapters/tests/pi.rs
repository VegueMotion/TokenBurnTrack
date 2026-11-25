#![cfg(feature = "pi")]

mod common;

use std::path::Path;

use common::*;
use tbt_adapters::PiAdapter;
use tbt_core::{Cursor, SourceKind, SourceRef};

const ROOT: &str = "pi/v3/sessions";
const LOREM: &str = "--home-user-projects-lorem--";
const MAIN: &str = "3f1c2a9e-5b7d-4e21-9c0a-1d2e3f4a5b01";
const MAIN_FILE: &str = "2026-11-20T09-00-00-000Z_3f1c2a9e-5b7d-4e21-9c0a-1d2e3f4a5b01.jsonl";
const FORK_FILE: &str = "2026-11-21T14-30-00-000Z_7a8b9c0d-1e2f-4a3b-8c4d-5e6f7a8b9c02.jsonl";

fn source(root: &Path, file: &str) -> SourceRef {
    SourceRef::new(root.join(LOREM).join(file), SourceKind::JsonlFile)
}

#[test]
fn golden() {
    let batch = read_all(&PiAdapter::new(), &[fixture(ROOT)]);
    insta::assert_yaml_snapshot!("fixture", Projection::of(&batch));
}

#[test]
fn fork_skips_entries_copied_from_the_parent() {
    let root = fixture(ROOT);
    let adapter = PiAdapter::new();
    let main = read_to_end(&adapter, &source(&root, MAIN_FILE), &Cursor::empty());
    let fork = read_to_end(&adapter, &source(&root, FORK_FILE), &Cursor::empty());

    assert_eq!(main.events.len(), 3);
    let [only] = fork.events.as_slice() else {
        panic!("expected one fork event: {:?}", fork.events);
    };
    assert_eq!(only.model, "gpt-5.5");
    assert_eq!(only.provider, "openai");
    assert_eq!(fork.sessions[0].resumed_from_ext_id.as_deref(), Some(MAIN));
}

#[test]
fn appended_turns_are_read_incrementally() {
    let (_dir, root) = copy_to_temp(&fixture(ROOT));
    let adapter = PiAdapter::new();
    let src = source(&root, MAIN_FILE);
    let first = read_to_end(&adapter, &src, &Cursor::empty());
    assert_eq!(first.events.len(), 3);

    append(
        &src.path,
        concat!(
            r#"{"type":"message","id":"a100000c","parentId":"a100000b","timestamp":"2026-11-20T09:06:00.000Z","#,
            r#""message":{"role":"assistant","content":[],"provider":"anthropic","model":"claude-sonnet-4-5","#,
            r#""usage":{"input":10,"output":20,"cacheRead":300,"cacheWrite":0,"totalTokens":330,"cost":{"total":0.001}},"#,
            r#""stopReason":"stop","timestamp":1795165560000}}"#,
            "\n"
        ),
    );
    let next = read_to_end(&adapter, &src, &first.next_cursor);
    let [event] = next.events.as_slice() else {
        panic!("expected one new event: {:?}", next.events);
    };
    assert_eq!(event.session_ext_id, MAIN);
    assert_eq!(event.cache_read_tokens, 300);
    assert_eq!(
        event.project_path.as_deref(),
        Some("/home/user/projects/lorem")
    );
    assert_eq!(
        next.sessions[0].title.as_deref(),
        Some("Consectetur adipiscing")
    );
}
