mod common;

use std::path::PathBuf;
use std::sync::Arc;

use jiff::Timestamp;
use tbt_adapters::ClaudeCodeAdapter;
use tbt_app::app_state::AppState;
use tbt_app::collector_bridge::{
    SyncCore, SyncResult, daemon_finished, harness_options, resolve_sync,
};
use tbt_app::data::{DataSource, StoreData};
use tbt_collector::{Collector, CollectorConfig, CollectorLock, LockAttempt, Registry, RunTrigger};
use tbt_core::{HarnessAdapter, HarnessId};
use tbt_store::Store;
use tempfile::TempDir;

struct Sandbox {
    _dir: TempDir,
    data_dir: PathBuf,
}

fn sandbox() -> Sandbox {
    let dir = tempfile::tempdir().unwrap();
    let claude_dir = dir.path().join("home/.claude");
    let roots = common::copy_claude_code_fixtures(&claude_dir).unwrap();
    assert!(!roots.is_empty());
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let adapter = ClaudeCodeAdapter::new();
    let mut store = Store::open(&tbt_store::paths::db_path(&data_dir)).unwrap();
    store
        .register_harness(&HarnessId::CLAUDE_CODE, adapter.display_name())
        .unwrap();
    store
        .set_harness_roots(&HarnessId::CLAUDE_CODE, Some(&roots))
        .unwrap();
    Sandbox {
        _dir: dir,
        data_dir,
    }
}

fn collector(data_dir: &std::path::Path) -> Arc<Collector> {
    let registry = Registry::new(vec![Arc::new(ClaudeCodeAdapter::new())]);
    Arc::new(Collector::new(
        data_dir.to_owned(),
        registry,
        CollectorConfig::default(),
    ))
}

fn store_data(data_dir: &std::path::Path) -> StoreData {
    StoreData::from_data_dir(Some(data_dir.to_owned()))
}

#[test]
fn run_ingests_fixtures_and_signals_data_changed() {
    let sb = sandbox();
    let collector = collector(&sb.data_dir);
    let events = collector.subscribe();
    let core = SyncCore::new(collector);
    let data = store_data(&sb.data_dir);
    assert!(!data.has_data().unwrap());

    let result = core.run(RunTrigger::Startup);
    let SyncResult::Finished(run) = &result else {
        panic!("expected a finished run, got {result:?}");
    };
    assert!(run.events_new > 0);
    assert_eq!(run.error, None);
    assert_eq!(run.trigger, RunTrigger::Startup);
    assert_eq!(events.try_recv().unwrap(), *run);
    assert!(!core.is_running());

    assert!(data.has_data().unwrap());
    let harnesses = harness_options(&data.harness_status().unwrap());
    assert!(harnesses.iter().any(|h| h.id == HarnessId::CLAUDE_CODE));
    let runs = data.ingest_runs(1).unwrap();
    assert_eq!(runs[0].id, run.run_id);
    assert_eq!(runs[0].stats.events_new, run.events_new);

    let mut state = AppState::new("UTC".to_owned());
    state.syncing = true;
    assert!(state.apply_sync_result(&result));
    assert_eq!(
        state.sync_status(Timestamp::now()),
        format!("Synced just now · +{} events", run.events_new)
    );

    let again = core.run(RunTrigger::Manual);
    let SyncResult::Finished(second) = &again else {
        panic!("expected a finished run, got {again:?}");
    };
    assert_eq!(second.events_new, 0);
    assert!(state.apply_sync_result(&again));
    assert_eq!(
        state.sync_status(Timestamp::now()),
        "Synced just now · +0 events"
    );
}

#[test]
fn held_lock_reports_daemon_busy_until_its_run_lands() {
    let sb = sandbox();
    let collector = collector(&sb.data_dir);
    let events = collector.subscribe();
    let core = SyncCore::new(collector);
    let data = store_data(&sb.data_dir);

    let LockAttempt::Acquired(lock) = CollectorLock::try_acquire(&sb.data_dir).unwrap() else {
        panic!("lock should be free");
    };
    let busy_at = Timestamp::now();
    let result = core.run(RunTrigger::Hourly);
    assert_eq!(result, SyncResult::Busy);
    assert!(events.try_recv().is_err());

    let mut state = AppState::new("UTC".to_owned());
    state.syncing = true;
    assert!(!state.apply_sync_result(&result));
    assert!(state.daemon_busy && !state.syncing);
    assert_eq!(state.sync_status(Timestamp::now()), "Daemon is syncing…");
    assert!(!daemon_finished(busy_at, &data.ingest_runs(1).unwrap()));

    drop(lock);
    let daemon = self::collector(&sb.data_dir);
    assert!(!daemon.ingest_run(RunTrigger::Hourly).unwrap().is_busy());
    let runs = data.ingest_runs(1).unwrap();
    assert!(daemon_finished(busy_at, &runs));
    assert!(data.has_data().unwrap());
}

#[test]
fn collector_errors_become_status_text() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("not-a-dir");
    std::fs::write(&blocker, "x").unwrap();
    let core = SyncCore::new(collector(&blocker.join("data")));
    let result = core.run(RunTrigger::Manual);
    let SyncResult::Failed(msg) = &result else {
        panic!("expected a failure, got {result:?}");
    };
    assert!(!msg.is_empty());
    assert!(!core.is_running());

    let mut state = AppState::new("UTC".to_owned());
    state.syncing = true;
    assert!(!state.apply_sync_result(&result));
    assert!(state.sync_failed());
    assert!(
        state
            .sync_status(Timestamp::now())
            .starts_with("Sync failed: ")
    );
    assert_eq!(
        state.sync_tooltip(Timestamp::now()).as_deref(),
        Some(msg.as_str())
    );
}

#[test]
fn config_saved_while_running_applies_to_the_next_run() {
    let sb = sandbox();
    {
        let mut store = Store::open(&tbt_store::paths::db_path(&sb.data_dir)).unwrap();
        for id in [
            HarnessId::CODEX,
            HarnessId::OPENCODE,
            HarnessId::GEMINI_CLI,
            HarnessId::LMSTUDIO,
            HarnessId::ZED,
        ] {
            store.register_harness(&id, id.as_str()).unwrap();
            store.set_harness_enabled(&id, false).unwrap();
        }
    }
    let logs = sb.data_dir.parent().unwrap().join("proxy-logs");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(
        logs.join("a.jsonl"),
        "{\"ts\":\"2026-11-20T10:00:00Z\",\"id\":\"r1\",\"in\":10,\"out\":5}\n",
    )
    .unwrap();
    let edited = CollectorConfig::parse(&format!(
        r#"
privacy = "off"
[[generic_jsonl]]
id = "proxy"
roots = [{:?}]
glob = "*.jsonl"
timestamp = "$.ts"
provider = "openai"
model = "gpt-5"
input_tokens = "$.in"
output_tokens = "$.out"
dedup_key = "{{$.id}}"
"#,
        logs.display().to_string()
    ))
    .unwrap();

    let saved = Arc::new(std::sync::Mutex::new(CollectorConfig::default()));
    let config = saved.clone();
    let core = SyncCore::new(collector(&sb.data_dir))
        .reloading_config(move || Ok(config.lock().unwrap().clone()));
    let data = store_data(&sb.data_dir);
    let proxy = HarnessId::new("proxy");

    assert!(matches!(
        core.run(RunTrigger::Startup),
        SyncResult::Finished(_)
    ));
    assert!(
        data.harness_status()
            .unwrap()
            .iter()
            .all(|h| h.config.id != proxy)
    );

    *saved.lock().unwrap() = edited.clone();
    let SyncResult::Finished(run) = core.run(RunTrigger::Manual) else {
        panic!("expected a finished run");
    };
    assert_eq!(run.events_new, 1);
    assert_eq!(*core.collector().config(), edited);
    let status = data.harness_status().unwrap();
    assert_eq!(
        status
            .iter()
            .find(|h| h.config.id == proxy)
            .map(|h| h.events),
        Some(1)
    );
}

#[test]
fn app_and_collector_share_the_data_dir() {
    let sb = sandbox();
    let (dir, source) = resolve_sync(Some(sb.data_dir.clone()), Ok(CollectorConfig::default()));
    let tbt_app::collector_bridge::SyncSource::Collector(collector) = source else {
        panic!("expected a collector");
    };
    let data = StoreData::from_data_dir(dir);
    assert_eq!(data.db_path(), Some(collector.db_path().as_path()));
}
