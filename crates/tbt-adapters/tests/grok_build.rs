#![cfg(feature = "grok_build")]

mod common;

use std::path::Path;

use common::*;
use serde_json::Value;
use tbt_adapters::GrokBuildAdapter;
use tbt_core::{Cursor, HarnessAdapter, SourceKind, SourceRef};

const ROOT: &str = "grok_build/1.0.45/sessions";
const MAIN: &str = "019a6b21-3c4d-7e5f-8a9b-0c1d2e3f4a01";
const FORK: &str = "019a6b4e-7f80-7a91-8b2c-3d4e5f6a7b02";
const SUB: &str = "019a6b25-1a2b-7c3d-9e4f-5a6b7c8d9e03";
const LOREM: &str = "%2Fhome%2Fuser%2Fprojects%2Florem";

fn usage_file(root: &Path, session: &str) -> SourceRef {
    SourceRef::new(
        root.join(LOREM).join(session).join("usage.json"),
        SourceKind::JsonFile,
    )
}

#[test]
fn golden() {
    let batch = read_all(&GrokBuildAdapter::new(), &[fixture(ROOT)]);
    insta::assert_yaml_snapshot!("fixture", Projection::of(&batch));
}

#[test]
fn discovers_only_usage_files() {
    let found = GrokBuildAdapter::new().discover(&[fixture(ROOT)]).unwrap();
    assert_eq!(found.len(), 4);
    assert!(
        found
            .iter()
            .all(|s| s.kind == SourceKind::JsonFile && s.path.file_name().unwrap() == "usage.json")
    );
}

#[test]
fn subagent_sessions_are_skipped() {
    let root = fixture(ROOT);
    let batch = read_to_end(
        &GrokBuildAdapter::new(),
        &usage_file(&root, SUB),
        &Cursor::empty(),
    );
    assert!(batch.events.is_empty() && batch.sessions.is_empty());
}

#[test]
fn fork_emits_only_turns_after_the_fork() {
    let root = fixture(ROOT);
    let adapter = GrokBuildAdapter::new();
    let fork = read_to_end(&adapter, &usage_file(&root, FORK), &Cursor::empty());
    let main = read_to_end(&adapter, &usage_file(&root, MAIN), &Cursor::empty());

    assert_eq!(main.events.len(), 4);
    let [only] = fork.events.as_slice() else {
        panic!("expected one fork event: {:?}", fork.events);
    };
    assert_eq!(only.input_tokens, 30500);
    assert!(main.events.iter().all(|e| e.dedup_key != only.dedup_key));
    assert_eq!(fork.sessions[0].resumed_from_ext_id.as_deref(), Some(MAIN));
}

#[test]
fn summary_change_alone_updates_the_session() {
    let (_dir, root) = copy_to_temp(&fixture(ROOT));
    let adapter = GrokBuildAdapter::new();
    let src = usage_file(&root, MAIN);
    let first = adapter.read(&src, &Cursor::empty()).unwrap();

    let path = src.path.with_file_name("summary.json");
    let mut summary: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    summary["generated_title"] = "Sit amet".into();
    std::fs::write(&path, summary.to_string()).unwrap();
    let file = std::fs::File::options().write(true).open(&path).unwrap();
    file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5))
        .unwrap();

    let second = adapter.read(&src, &first.next_cursor).unwrap();
    assert!(second.events.is_empty());
    assert_eq!(second.sessions[0].title.as_deref(), Some("Sit amet"));
}

#[test]
fn unchanged_file_reads_nothing() {
    let root = fixture(ROOT);
    let adapter = GrokBuildAdapter::new();
    let src = usage_file(&root, MAIN);
    let first = adapter.read(&src, &Cursor::empty()).unwrap();
    let again = adapter.read(&src, &first.next_cursor).unwrap();
    assert!(again.is_empty());
    assert_eq!(again.next_cursor, first.next_cursor);
}

#[test]
fn new_turns_and_late_fold_emit_only_the_difference() {
    let (_dir, root) = copy_to_temp(&fixture(ROOT));
    let adapter = GrokBuildAdapter::new();
    let src = usage_file(&root, MAIN);
    let first = adapter.read(&src, &Cursor::empty()).unwrap();
    assert_eq!(first.events.len(), 4);

    let mut file: Value =
        serde_json::from_str(&std::fs::read_to_string(&src.path).unwrap()).unwrap();
    let turns = file["turns"].as_array_mut().unwrap();
    let last = turns.last_mut().unwrap();
    last["endedAt"] = "2026-09-20T10:09:41.000000001+00:00".into();
    let row = &mut last["modelUsage"]["grok-4.5"];
    row["inputTokens"] = 40100.into();
    row["outputTokens"] = 1600.into();
    row["cachedReadTokens"] = 37000.into();
    row["costUsdTicks"] = 800000000_i64.into();
    let mut t4 = last.clone();
    t4["turnNumber"] = 4.into();
    t4["endedAt"] = "2026-09-20T10:12:00.5+00:00".into();
    turns.push(t4);
    std::fs::write(&src.path, serde_json::to_string_pretty(&file).unwrap()).unwrap();

    let second = adapter.read(&src, &first.next_cursor).unwrap();
    let got: Vec<_> = second
        .events
        .iter()
        .map(|e| {
            (
                e.dedup_key.clone(),
                e.input_tokens,
                e.cache_read_tokens,
                e.output_tokens,
                e.reported_cost.map(|c| c.to_string()),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                "2026-09-20T10:09:41.000000001+00:00:3:grok-4.5".to_owned(),
                900,
                3100,
                120,
                Some("0.0056000000".to_owned()),
            ),
            (
                "2026-09-20T10:12:00.5+00:00:4:grok-4.5".to_owned(),
                3100,
                37000,
                1600,
                Some("0.0800000000".to_owned()),
            ),
        ]
    );
    let third = adapter.read(&src, &second.next_cursor).unwrap();
    assert!(third.is_empty());
}
