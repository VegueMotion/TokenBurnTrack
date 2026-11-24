mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::*;
use tbt_collector::{CollectorConfig, CollectorLock, LockAttempt, RunOutcome, RunTrigger};
use tbt_core::{Cursor, HarnessAdapter, HarnessId, PrivacyLevel, ReadBatch, Result, SourceRef};

#[test]
fn full_run_matches_adapter_golden_counts() {
    let env = Env::with_fixtures(&[CLAUDE_FIXTURES]);
    let collector = env.claude_collector();
    let rx = collector.subscribe();
    let report = run(&collector);

    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.sources_scanned, 8);
    assert_eq!(report.sources_read, 8);
    assert_eq!(report.events_new as i64, claude_golden_total());
    assert_eq!(env.count("SELECT count(*) FROM usage_event"), 204);

    let expected: std::collections::BTreeMap<String, i64> = CLAUDE_GOLDEN_EVENTS
        .iter()
        .map(|(s, n)| (s.to_string(), *n))
        .collect();
    assert_eq!(env.session_events(), expected);
    assert_session_totals_consistent(&env.db());
    assert_eq!(
        env.count("SELECT count(*) FROM usage_event WHERE price_id IS NOT NULL")
            + env.count("SELECT count(*) FROM usage_event WHERE price_id IS NULL"),
        204
    );
    assert_eq!(
        env.count("SELECT coalesce(sum(events),0) FROM hourly_usage"),
        204
    );
    assert_eq!(
        env.count("SELECT count(*) FROM message WHERE preview IS NOT NULL"),
        0
    );
    assert!(env.count("SELECT count(*) FROM message") > 0);

    let runs = env.store().ingest_runs(5).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].stats.events_new, 204);
    assert_eq!(runs[0].stats.sources_scanned, 8);
    assert!(runs[0].finished_at.is_some() && runs[0].error.is_none());

    let finished = rx.try_recv().unwrap();
    assert_eq!(finished.run_id, report.run_id);
    assert_eq!(finished.events_new, 204);
    assert_eq!(finished.trigger, RunTrigger::Manual);
    assert!(rx.try_recv().is_err());
}

#[test]
fn second_run_skips_unchanged_sources() {
    let env = Env::with_fixtures(&[CLAUDE_FIXTURES]);
    let collector = env.claude_collector();
    run(&collector);
    let before = totals(&env.db());

    let again = run(&collector);
    assert_eq!(again.events_new, 0);
    assert_eq!(again.messages_new, 0);
    assert_eq!(again.sources_scanned, 8);
    assert_eq!(again.sources_unchanged, 8);
    assert_eq!(again.sources_read, 0);
    assert_eq!(totals(&env.db()), before);
    assert_eq!(env.store().ingest_runs(5).unwrap().len(), 2);
}

#[test]
fn resumed_pair_links_and_counts_only_new_events() {
    let env = Env::with_fixtures(&[CLAUDE_FIXTURES]);
    run(&env.claude_collector());

    assert_eq!(
        env.session_link(RESUMED_CONTINUATION, "resumed_from_session_id")
            .as_deref(),
        Some(RESUMED_ORIGINAL)
    );
    assert_eq!(
        env.session_link(RESUMED_ORIGINAL, "resumed_from_session_id"),
        None
    );
    let events = env.session_events();
    assert_eq!(events[RESUMED_CONTINUATION], 5);
    assert_eq!(events[RESUMED_ORIGINAL], 50);

    let db = env.db();
    let (started, first_event, original_start): (String, String, String) = db
        .query_row(
            "SELECT s.started_at,
                    (SELECT min(ts) FROM usage_event WHERE session_id = s.id),
                    (SELECT started_at FROM session WHERE ext_id = ?2)
             FROM session s WHERE s.ext_id = ?1",
            [RESUMED_CONTINUATION, RESUMED_ORIGINAL],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(started, first_event);
    assert!(started > original_start);
    assert!(original_start.starts_with("2026-11-20T21:20:42.923"));
}

#[test]
fn resumed_continuation_ingested_alone_starts_at_its_first_inserted_event() {
    let env = Env::with_fixtures(&[CLAUDE_FIXTURES]);
    let continuation = env.find(&format!("{RESUMED_CONTINUATION}.jsonl"));
    let collector = env.collector_with(
        vec![(claude(), vec![continuation])],
        CollectorConfig::default(),
    );
    run(&collector);
    assert_eq!(env.session_events()[RESUMED_CONTINUATION], 55);
    assert_eq!(
        env.session_link(RESUMED_CONTINUATION, "resumed_from_session_id")
            .as_deref(),
        Some(RESUMED_ORIGINAL)
    );
    assert_session_totals_consistent(&env.db());
}

#[test]
fn subagent_is_linked_to_its_parent() {
    let env = Env::with_fixtures(&[CLAUDE_FIXTURES]);
    run(&env.claude_collector());
    assert_eq!(
        env.session_link(SUBAGENT_SESSION, "parent_session_id")
            .as_deref(),
        Some(MAIN_SESSION)
    );
    assert_eq!(env.session_link(MAIN_SESSION, "parent_session_id"), None);
}

#[test]
fn subagent_before_parent_creates_placeholder_that_is_filled_later() {
    let env = Env::with_fixtures(&[CLAUDE_FIXTURES]);
    let sub = env.find("agent-a919fc890880d1075.jsonl");
    let main = env.find(&format!("{MAIN_SESSION}.jsonl"));

    run(&env.collector_with(
        vec![(claude(), vec![sub.clone()])],
        CollectorConfig::default(),
    ));
    assert_eq!(
        env.session_link(SUBAGENT_SESSION, "parent_session_id")
            .as_deref(),
        Some(MAIN_SESSION)
    );
    assert_eq!(
        env.count(
            "SELECT events FROM session WHERE ext_id = '64ed9da4-d663-42b6-bd5f-db9652067ec7'"
        ),
        0
    );

    run(&env.collector_with(
        vec![(claude(), vec![sub, main])],
        CollectorConfig::default(),
    ));
    let db = env.db();
    let (title, started, events): (Option<String>, String, i64) = db
        .query_row(
            "SELECT title, started_at, events FROM session WHERE ext_id = ?1",
            [MAIN_SESSION],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert!(title.is_some());
    assert!(started.starts_with("2026-11-21T18:43:52.017"), "{started}");
    assert_eq!(events, 19);
    assert_eq!(env.count("SELECT count(*) FROM session"), 2);
}

#[test]
fn appended_lines_count_only_new_events() {
    let env = Env::new();
    let src = fixtures_root().join(CLAUDE_FIXTURES);
    let original = files_under(&src)
        .into_iter()
        .find(|p| p.ends_with(format!("{LONG_SESSION}.jsonl")))
        .unwrap();
    let text = std::fs::read_to_string(&original).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let path = env.roots.join("p/s.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let split = lines.len() / 2;
    std::fs::write(&path, lines[..split].join("\n") + "\n").unwrap();

    let collector = env.collector_with(
        vec![(claude(), vec![env.roots.clone()])],
        CollectorConfig::default(),
    );
    let first = run(&collector).events_new;
    assert!(first > 0 && first < 64, "{first}");

    append(&path, &(lines[split..].join("\n") + "\n"));
    let second = run(&collector);
    assert_eq!(second.sources_read, 1);
    assert_eq!(first + second.events_new, 64);
    assert_eq!(env.session_events()[LONG_SESSION], 64);
    assert_eq!(run(&collector).events_new, 0);
    assert_session_totals_consistent(&env.db());
}

#[test]
fn busy_lock_skips_the_run() {
    let env = Env::with_fixtures(&[CLAUDE_FIXTURES]);
    let collector = env.claude_collector();
    let rx = collector.subscribe();
    let LockAttempt::Acquired(lock) = CollectorLock::try_acquire(&env.data).unwrap() else {
        panic!("lock should be free");
    };
    assert_eq!(
        collector.ingest_run(RunTrigger::Hourly).unwrap(),
        RunOutcome::Busy
    );
    assert!(env.store().ingest_runs(5).unwrap().is_empty());
    assert!(rx.try_recv().is_err());
    drop(lock);
    assert_eq!(run(&collector).events_new, 204);
}

struct Flaky {
    inner: Arc<dyn HarnessAdapter>,
    fail: &'static str,
    panic: &'static str,
}

impl HarnessAdapter for Flaky {
    fn id(&self) -> HarnessId {
        self.inner.id()
    }
    fn display_name(&self) -> &str {
        self.inner.display_name()
    }
    fn default_roots(&self) -> Vec<PathBuf> {
        Vec::new()
    }
    fn discover(&self, roots: &[PathBuf]) -> Result<Vec<SourceRef>> {
        self.inner.discover(roots)
    }
    fn read(&self, src: &SourceRef, cursor: &Cursor) -> Result<ReadBatch> {
        let name = src.path.to_string_lossy();
        if name.contains(self.fail) {
            return Err(tbt_core::Error::other("simulated failure"));
        }
        if name.contains(self.panic) {
            panic!("simulated panic");
        }
        self.inner.read(src, cursor)
    }
}

#[test]
fn failing_sources_do_not_abort_the_run() {
    let env = Env::with_fixtures(&[CLAUDE_FIXTURES]);
    let flaky: Arc<dyn HarnessAdapter> = Arc::new(Flaky {
        inner: claude(),
        fail: SHORT_SESSION,
        panic: MULTI_MODEL_SESSION,
    });
    let collector = env.collector_with(
        vec![(flaky, vec![env.root(CLAUDE_FIXTURES)])],
        CollectorConfig::default(),
    );
    let report = run(&collector);
    assert_eq!(report.sources_failed, 2);
    assert_eq!(report.sources_read, 6);
    assert_eq!(report.events_new as i64, claude_golden_total() - 1 - 6);
    let text = report.error_text().unwrap();
    assert!(text.contains("simulated failure") && text.contains("simulated panic"));

    let runs = env.store().ingest_runs(1).unwrap();
    assert!(
        runs[0]
            .error
            .as_deref()
            .unwrap()
            .contains("simulated failure")
    );
    assert!(
        runs[0]
            .warnings
            .iter()
            .any(|w| w.message.contains("simulated panic"))
    );

    let healthy = env.claude_collector();
    let retry = run(&healthy);
    assert_eq!(retry.sources_read, 2);
    assert_eq!(retry.events_new, 7);
}

#[test]
fn removed_sources_are_marked_missing_and_keep_their_data() {
    let env = Env::with_fixtures(&[CLAUDE_FIXTURES]);
    let collector = env.claude_collector();
    run(&collector);
    let short = env.find(&format!("{SHORT_SESSION}.jsonl"));
    let kept = std::fs::read(&short).unwrap();
    std::fs::remove_file(&short).unwrap();

    let report = run(&collector);
    assert_eq!(report.sources_missing, 1);
    assert_eq!(report.sources_scanned, 7);
    assert_eq!(
        env.count("SELECT count(*) FROM source_file WHERE missing = 1"),
        1
    );
    assert_eq!(env.count("SELECT count(*) FROM usage_event"), 204);
    assert_eq!(run(&collector).sources_missing, 0);

    std::fs::write(&short, kept).unwrap();
    let back = run(&collector);
    assert_eq!(back.events_new, 0);
    assert_eq!(back.sources_read, 1);
    assert_eq!(
        env.count("SELECT count(*) FROM source_file WHERE missing = 1"),
        0
    );
}

#[test]
fn privacy_levels_are_applied() {
    let env = Env::with_fixtures(&[CLAUDE_FIXTURES]);
    let off = CollectorConfig {
        privacy: PrivacyLevel::Off,
        ..CollectorConfig::default()
    };
    let report = run(&env.collector_with(vec![(claude(), vec![env.root(CLAUDE_FIXTURES)])], off));
    assert_eq!(report.messages_new, 0);
    assert_eq!(env.count("SELECT count(*) FROM message"), 0);

    let env = Env::with_fixtures(&[CLAUDE_FIXTURES]);
    let preview = CollectorConfig {
        privacy: PrivacyLevel::TruncatedPreview,
        ..CollectorConfig::default()
    };
    run(&env.collector_with(vec![(claude(), vec![env.root(CLAUDE_FIXTURES)])], preview));
    assert!(env.count("SELECT count(*) FROM message WHERE preview IS NOT NULL") > 0);
    assert_eq!(
        env.count(&format!(
            "SELECT count(*) FROM message WHERE length(preview) > {}",
            tbt_core::PREVIEW_MAX_CHARS
        )),
        0
    );
}

#[test]
fn disabled_harness_is_not_read() {
    let env = Env::with_fixtures(&[CLAUDE_FIXTURES]);
    let collector = env.claude_collector();
    {
        let mut store = tbt_store::Store::open(&tbt_store::paths::db_path(&env.data)).unwrap();
        store
            .set_harness_enabled(&HarnessId::CLAUDE_CODE, false)
            .unwrap();
    }
    let report = run(&collector);
    assert_eq!((report.sources_scanned, report.events_new), (0, 0));
}

#[test]
fn raw_json_retention_drops_old_payloads() {
    let env = Env::with_fixtures(&[CLAUDE_FIXTURES]);
    let config = CollectorConfig {
        raw_json_retention_days: Some(0),
        ..CollectorConfig::default()
    };
    let report =
        run(&env.collector_with(vec![(claude(), vec![env.root(CLAUDE_FIXTURES)])], config));
    assert_eq!(report.raw_json_dropped, 204);
    assert_eq!(
        env.count("SELECT count(*) FROM usage_event WHERE raw_json IS NOT NULL"),
        0
    );
}

#[test]
fn collectors_on_separate_threads_never_ingest_twice() {
    let env = Env::with_fixtures(&[CLAUDE_FIXTURES]);
    let collector = Arc::new(env.claude_collector());
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let c = Arc::clone(&collector);
            std::thread::spawn(move || c.ingest_run(RunTrigger::Manual).unwrap())
        })
        .collect();
    let outcomes: Vec<RunOutcome> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let new: u64 = outcomes
        .iter()
        .filter_map(RunOutcome::report)
        .map(|r| r.events_new)
        .sum();
    assert_eq!(new, 204);
    assert_eq!(env.count("SELECT count(*) FROM usage_event"), 204);
}

struct HoldsBack;

impl HarnessAdapter for HoldsBack {
    fn id(&self) -> HarnessId {
        HarnessId::from_static("holds_back")
    }
    fn display_name(&self) -> &str {
        "Holds back"
    }
    fn default_roots(&self) -> Vec<PathBuf> {
        Vec::new()
    }
    fn discover(&self, roots: &[PathBuf]) -> Result<Vec<SourceRef>> {
        Ok(roots
            .iter()
            .map(|r| SourceRef::new(r, tbt_core::SourceKind::JsonFile))
            .collect())
    }
    fn read(&self, _src: &SourceRef, cursor: &Cursor) -> Result<ReadBatch> {
        let released = cursor.state::<bool>()?;
        if released == Some(true) {
            return Ok(ReadBatch::unchanged(cursor.clone()));
        }
        let mut batch = ReadBatch {
            next_cursor: Cursor::from_state(&released.is_some())?,
            ..ReadBatch::default()
        };
        if released == Some(false) {
            let mut e: tbt_core::UsageEvent = serde_json::from_value(serde_json::json!({
                "harness": "holds_back", "dedup_key": "held-1", "session_ext_id": "s",
                "ts": "2026-11-23T10:00:00Z", "provider": "google", "model": "gemini-test",
                "input_tokens": 0, "output_tokens": 0, "cache_write_5m_tokens": 0,
                "cache_write_1h_tokens": 0, "cache_read_tokens": 0, "reasoning_tokens": 0,
                "reported_cost": null, "project_path": null, "git_branch": null, "raw": null
            }))?;
            e.input_tokens = 7;
            batch.events.push(e);
        }
        Ok(batch)
    }
}

#[test]
fn unchanged_files_are_still_read_so_adapters_can_release_held_data() {
    let env = Env::new();
    let file = env.roots.join("session.json");
    std::fs::write(&file, "{}").unwrap();
    let meta = std::fs::metadata(&file).unwrap();
    let collector = env.collector_with(
        vec![(
            Arc::new(HoldsBack) as Arc<dyn HarnessAdapter>,
            vec![file.clone()],
        )],
        CollectorConfig::default(),
    );

    let first = run(&collector);
    assert_eq!((first.sources_read, first.events_new), (1, 0));
    let second = run(&collector);
    assert_eq!((second.sources_read, second.events_new), (1, 1));
    let third = run(&collector);
    assert_eq!((third.sources_unchanged, third.events_new), (1, 0));

    let after = std::fs::metadata(&file).unwrap();
    assert_eq!(after.len(), meta.len());
    assert_eq!(after.modified().unwrap(), meta.modified().unwrap());
    assert_eq!(env.count("SELECT input_tokens FROM usage_event"), 7);
}
