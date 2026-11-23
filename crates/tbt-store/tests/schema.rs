mod common;

use std::path::Path;

use common::*;
use tbt_core::{HarnessId, ParseWarning, SourceKind, SourceRef};
use tbt_store::{RunStats, RunTrigger, Store, StoreError};

fn tables(conn: &rusqlite::Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn migrates_empty_db_to_latest() {
    let db = Db::new();
    let raw = db.raw();
    let version: i64 = raw
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 1);
    assert_eq!(
        tables(&raw),
        [
            "harness",
            "hourly_usage",
            "ingest_run",
            "message",
            "model_price",
            "project",
            "session",
            "session_model",
            "source_file",
            "usage_event",
        ]
    );
    let none: (i64, String) = raw
        .query_row("SELECT id, path FROM project", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(none, (0, String::new()));
    let mode: String = raw
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
}

#[test]
fn reopen_is_idempotent_and_keeps_data() {
    let mut db = Db::new();
    db.ingest(
        "/a.jsonl",
        &[priced(event("k1", "s1", "2026-01-01T10:00:00Z"), 5, None)],
    );
    drop(db.store);
    let store = Store::open(&db.path).unwrap();
    drop(store);
    let _store = Store::open(&db.path).unwrap();
    let raw = rusqlite::Connection::open(&db.path).unwrap();
    assert_eq!(count(&raw, "project"), 1);
    assert_eq!(count(&raw, "usage_event"), 1);
    let fk: i64 = raw
        .query_row("PRAGMA foreign_key_check", [], |r| r.get(0))
        .unwrap_or(0);
    assert_eq!(fk, 0);
}

#[test]
fn foreign_keys_are_enforced() {
    let mut store = Store::open_in_memory().unwrap();
    let mut tx = store.begin_source_tx(&CC, Path::new("/a")).unwrap();
    let s = tx
        .upsert_session(
            &tbt_core::SessionUpsert::new(CC, "s"),
            tbt_core::ProjectId::NONE,
        )
        .unwrap();
    assert!(
        tx.link_resumed_session(s, tbt_core::SessionId(999))
            .is_err()
    );
}

#[test]
fn read_only_cannot_write_but_reads() {
    let mut db = Db::new();
    db.ingest(
        "/a.jsonl",
        &[priced(event("k1", "s1", "2026-01-01T10:00:00Z"), 5, None)],
    );
    drop(db.store);

    let mut ro = Store::open_read_only(&db.path).unwrap();
    assert_eq!(ro.unpriced_models().unwrap().len(), 1);
    assert!(ro.harnesses().unwrap().iter().any(|h| h.id == CC));

    assert!(matches!(
        ro.begin_source_tx(&CC, Path::new("/b")),
        Err(StoreError::ReadOnly)
    ));
    assert!(matches!(
        ro.register_harness(&HarnessId::CODEX, "Codex"),
        Err(StoreError::ReadOnly)
    ));
    assert!(matches!(
        ro.start_run(RunTrigger::Manual, ts("2026-01-01T00:00:00Z")),
        Err(StoreError::ReadOnly)
    ));
    assert!(matches!(
        ro.insert_price(&price("x", "1", None)),
        Err(StoreError::ReadOnly)
    ));
    assert!(matches!(ro.begin_recompute_tx(), Err(StoreError::ReadOnly)));
    assert!(matches!(
        ro.drop_raw_json_older_than(1, ts("2026-01-01T00:00:00Z")),
        Err(StoreError::ReadOnly)
    ));
}

#[test]
fn read_only_never_migrates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.sqlite");
    assert!(Store::open_read_only(&path).is_err());
    assert!(!path.exists());
}

#[test]
fn read_only_sees_writer_commits() {
    let mut db = Db::new();
    let ro = Store::open_read_only(&db.path).unwrap();
    assert!(ro.unpriced_models().unwrap().is_empty());
    db.ingest(
        "/a.jsonl",
        &[priced(event("k1", "s1", "2026-01-01T10:00:00Z"), 5, None)],
    );
    assert_eq!(ro.unpriced_models().unwrap().len(), 1);
}

#[test]
fn runs_and_harness_config() {
    let mut store = Store::open_in_memory().unwrap();
    store.register_harness(&CC, "Claude Code").unwrap();
    store.set_harness_enabled(&CC, false).unwrap();
    let roots = vec![std::path::PathBuf::from("/x/projects")];
    store.set_harness_roots(&CC, Some(&roots)).unwrap();
    store.register_harness(&CC, "Claude Code 2").unwrap();
    let h = &store.harnesses().unwrap()[0];
    assert_eq!(h.display_name, "Claude Code 2");
    assert!(!h.enabled);
    assert_eq!(h.roots.as_deref(), Some(roots.as_slice()));
    store.set_harness_roots(&CC, None).unwrap();
    assert_eq!(store.harnesses().unwrap()[0].roots, None);
    assert!(matches!(
        store.set_harness_enabled(&HarnessId::CODEX, true),
        Err(StoreError::NotFound(_))
    ));

    let run = store
        .start_run(RunTrigger::Hourly, ts("2026-01-01T10:00:00Z"))
        .unwrap();
    let w = ParseWarning::new(
        CC,
        &SourceRef::new("/a.jsonl", SourceKind::JsonlFile),
        "bad line",
    )
    .at(3, 10);
    let stats = RunStats {
        sources_scanned: 4,
        events_new: 7,
    };
    store
        .finish_run(
            run,
            ts("2026-01-01T10:00:05Z"),
            &stats,
            std::slice::from_ref(&w),
            None,
        )
        .unwrap();
    let running = store
        .start_run(RunTrigger::Manual, ts("2026-01-01T11:00:00Z"))
        .unwrap();
    let runs = store.ingest_runs(10).unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].id, running);
    assert_eq!(runs[0].finished_at, None);
    assert_eq!(runs[1].stats, stats);
    assert_eq!(runs[1].warnings, vec![w.clone()]);
    assert_eq!(runs[1].trigger, RunTrigger::Hourly);

    let status = store.harness_status().unwrap();
    assert_eq!(status[0].last_warnings, vec![w]);
}
