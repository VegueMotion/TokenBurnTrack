use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use gpui::{AsyncApp, Context, Entity, Subscription, Task, WeakEntity};
use jiff::{SignedDuration, Timestamp};
use notify::event::{EventKind, ModifyKind};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tbt_collector::scheduler::{MAX_SLEEP_CHUNK, next_tick};
use tbt_collector::{Collector, CollectorConfig, ConfigError, IngestFinished, RunOutcome};
use tbt_core::HarnessId;
use tbt_store::{HarnessConfig, HarnessStatus, IngestRunSummary};

pub use tbt_collector::RunTrigger;

use crate::app_state::{AppState, AppStateEvent, HarnessOption, LastSync};
use crate::data::DataSource;

pub const WATCH_ENV: &str = "TBT_WATCH";
pub const STATUS_REFRESH: Duration = Duration::from_secs(30);
pub const DAEMON_POLL: Duration = Duration::from_secs(3);
pub const DAEMON_WAIT_MAX: Duration = Duration::from_secs(10 * 60);
pub const DAEMON_FINISH_GRACE: SignedDuration = SignedDuration::from_secs(10);
pub const WATCH_POLL: Duration = Duration::from_secs(5);
pub const WATCH_QUIET: Duration = Duration::from_secs(30);
pub const WATCH_MIN_INTERVAL: Duration = Duration::from_secs(120);
pub const WATCH_MAX_DELAY: Duration = Duration::from_secs(5 * 60);
pub const RECENT_RUNS: usize = 5;

#[derive(Debug, Clone)]
pub enum SyncSource {
    Mock,
    Unavailable(String),
    Collector(Arc<Collector>),
}

pub fn resolve_sync(
    data_dir_arg: Option<PathBuf>,
    config: Result<CollectorConfig, ConfigError>,
) -> (Option<PathBuf>, SyncSource) {
    match config {
        Ok(config) => {
            let dir = data_dir_arg.or_else(|| config.resolve_data_dir());
            let source = match &dir {
                Some(dir) => {
                    SyncSource::Collector(Arc::new(Collector::builtin(dir.clone(), config)))
                }
                None => SyncSource::Unavailable(format!(
                    "no data directory (pass --data-dir or set {})",
                    tbt_store::paths::DATA_DIR_ENV
                )),
            };
            (dir, source)
        }
        Err(e) => {
            let dir = data_dir_arg.or_else(tbt_store::paths::default_data_dir);
            (dir, SyncSource::Unavailable(e.to_string()))
        }
    }
}

pub fn watch_enabled() -> bool {
    std::env::var(WATCH_ENV).is_ok_and(|v| v == "1")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncResult {
    Finished(IngestFinished),
    Busy,
    Failed(String),
    Skipped,
}

pub struct SyncCore {
    collector: Arc<Collector>,
    running: AtomicBool,
}

impl SyncCore {
    pub fn new(collector: Arc<Collector>) -> SyncCore {
        SyncCore {
            collector,
            running: AtomicBool::new(false),
        }
    }

    pub fn collector(&self) -> &Arc<Collector> {
        &self.collector
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    pub fn run(&self, trigger: RunTrigger) -> SyncResult {
        if self.running.swap(true, Ordering::AcqRel) {
            return SyncResult::Skipped;
        }
        let outcome = catch_unwind(AssertUnwindSafe(|| self.collector.ingest_run(trigger)));
        self.running.store(false, Ordering::Release);
        match outcome {
            Ok(Ok(RunOutcome::Finished(report))) => SyncResult::Finished(report.finished_event()),
            Ok(Ok(RunOutcome::Busy)) => SyncResult::Busy,
            Ok(Err(e)) => SyncResult::Failed(format!("{e:#}")),
            Err(panic) => SyncResult::Failed(format!("collector crashed: {}", panic_text(&*panic))),
        }
    }

    pub fn adapter_defaults(&self) -> Vec<(HarnessId, Vec<PathBuf>)> {
        self.collector
            .registry()
            .adapters()
            .iter()
            .map(|a| (a.id(), a.default_roots()))
            .collect()
    }
}

fn panic_text(panic: &(dyn Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_owned())
}

pub fn sleep_chunk(now: Timestamp, deadline: Timestamp) -> Option<Duration> {
    if now >= deadline {
        return None;
    }
    let remaining = Duration::try_from(deadline.duration_since(now)).unwrap_or_default();
    Some(remaining.min(MAX_SLEEP_CHUNK))
}

pub fn daemon_finished(busy_at: Timestamp, runs: &[IngestRunSummary]) -> bool {
    runs.first()
        .and_then(|r| r.finished_at)
        .is_some_and(|f| f + DAEMON_FINISH_GRACE >= busy_at)
}

pub fn harness_options(status: &[HarnessStatus]) -> Vec<HarnessOption> {
    status
        .iter()
        .filter(|h| h.config.enabled)
        .map(|h| HarnessOption {
            id: h.config.id.clone(),
            name: h.config.display_name.clone(),
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct Debouncer {
    quiet: Duration,
    min_interval: Duration,
    max_delay: Duration,
    first_event: Option<Instant>,
    last_event: Option<Instant>,
    last_run: Option<Instant>,
}

impl Default for Debouncer {
    fn default() -> Debouncer {
        Debouncer::new(WATCH_QUIET, WATCH_MIN_INTERVAL, WATCH_MAX_DELAY)
    }
}

impl Debouncer {
    pub fn new(quiet: Duration, min_interval: Duration, max_delay: Duration) -> Debouncer {
        Debouncer {
            quiet,
            min_interval,
            max_delay,
            first_event: None,
            last_event: None,
            last_run: None,
        }
    }

    pub fn event(&mut self, now: Instant) {
        self.first_event.get_or_insert(now);
        self.last_event = Some(now);
    }

    pub fn record_run(&mut self, now: Instant) {
        self.last_run = Some(now);
        self.first_event = None;
        self.last_event = None;
    }

    pub fn due_at(&self) -> Option<Instant> {
        let settled = (self.last_event? + self.quiet).min(self.first_event? + self.max_delay);
        Some(match self.last_run {
            Some(run) => settled.max(run + self.min_interval),
            None => settled,
        })
    }

    pub fn poll(&mut self, now: Instant) -> bool {
        let due = self.due_at().is_some_and(|d| now >= d);
        if due {
            self.record_run(now);
        }
        due
    }
}

fn lock(debouncer: &Mutex<Debouncer>) -> MutexGuard<'_, Debouncer> {
    debouncer.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn is_relevant_change(kind: &EventKind) -> bool {
    match kind {
        EventKind::Access(_) => false,
        EventKind::Modify(ModifyKind::Metadata(_)) => false,
        EventKind::Any
        | EventKind::Create(_)
        | EventKind::Modify(_)
        | EventKind::Remove(_)
        | EventKind::Other => true,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchTarget {
    pub path: PathBuf,
    pub recursive: bool,
}

pub fn watch_targets(
    defaults: &[(HarnessId, Vec<PathBuf>)],
    configs: &[HarnessConfig],
) -> Vec<WatchTarget> {
    let mut out: Vec<WatchTarget> = Vec::new();
    for (id, default_roots) in defaults {
        let config = configs.iter().find(|c| c.id == *id);
        if config.is_some_and(|c| !c.enabled) {
            continue;
        }
        let roots = config
            .and_then(|c| c.roots.clone())
            .unwrap_or_else(|| default_roots.clone());
        let recursive = *id != HarnessId::OPENCODE;
        for path in roots {
            if !out.iter().any(|t| t.path == path) {
                out.push(WatchTarget { path, recursive });
            }
        }
    }
    out
}

fn build_watcher(
    targets: &[WatchTarget],
    debouncer: Arc<Mutex<Debouncer>>,
) -> Option<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok_and(|e| is_relevant_change(&e.kind)) {
            lock(&debouncer).event(Instant::now());
        }
    })
    .ok()?;
    let mut watching = 0;
    for t in targets {
        let mode = if t.recursive && t.path.is_dir() {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        if t.path.exists() && watcher.watch(&t.path, mode).is_ok() {
            watching += 1;
        }
    }
    (watching > 0).then_some(watcher)
}

struct Watch {
    debouncer: Arc<Mutex<Debouncer>>,
    targets: Option<Vec<WatchTarget>>,
    watcher: Option<RecommendedWatcher>,
    task: Option<Task<()>>,
}

type Refreshed = (
    tbt_store::Result<Vec<HarnessStatus>>,
    tbt_store::Result<Vec<IngestRunSummary>>,
);

pub struct CollectorBridge {
    core: Option<Arc<SyncCore>>,
    app_state: Entity<AppState>,
    data: Arc<dyn DataSource>,
    watch: Option<Watch>,
    run_task: Option<Task<()>>,
    refresh_task: Option<Task<()>>,
    daemon_task: Option<Task<()>>,
    _tasks: Vec<Task<()>>,
    _subscription: Subscription,
}

impl CollectorBridge {
    pub fn new(
        source: SyncSource,
        app_state: Entity<AppState>,
        data: Arc<dyn DataSource>,
        cx: &mut Context<Self>,
    ) -> CollectorBridge {
        let (core, unavailable) = match source {
            SyncSource::Collector(c) => (Some(Arc::new(SyncCore::new(c))), None),
            SyncSource::Unavailable(msg) => (None, Some(msg)),
            SyncSource::Mock => (None, None),
        };
        app_state.update(cx, |s, cx| {
            s.sync_available = core.is_some();
            s.sync_error = unavailable;
            cx.notify();
        });
        let watch = (core.is_some() && watch_enabled()).then(|| Watch {
            debouncer: Arc::new(Mutex::new(Debouncer::default())),
            targets: None,
            watcher: None,
            task: None,
        });
        let subscription = cx.subscribe(&app_state, |b, _, ev: &AppStateEvent, cx| {
            if *ev == AppStateEvent::DataChanged {
                b.refresh(cx);
            }
        });
        let mut bridge = CollectorBridge {
            core,
            app_state,
            data,
            watch,
            run_task: None,
            refresh_task: None,
            daemon_task: None,
            _tasks: Vec::new(),
            _subscription: subscription,
        };
        bridge.refresh(cx);
        bridge._tasks.push(Self::spawn_status_ticker(cx));
        if let Some(core) = &bridge.core {
            let interval = core.collector().config().sync_interval_minutes;
            bridge._tasks.push(Self::spawn_scheduler(interval, cx));
        }
        if bridge.watch.is_some() {
            bridge._tasks.push(Self::spawn_watch_poll(cx));
        }
        bridge
    }

    pub fn sync_now(&mut self, cx: &mut Context<Self>) {
        self.spawn_run(RunTrigger::Manual, cx);
    }

    fn spawn_status_ticker(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(STATUS_REFRESH).await;
                let alive = this.update(cx, |b, cx| b.app_state.update(cx, |_, cx| cx.notify()));
                if alive.is_err() {
                    return;
                }
            }
        })
    }

    fn spawn_scheduler(interval: u32, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            if !run_inline(&this, RunTrigger::Startup, cx).await {
                return;
            }
            loop {
                let next = next_tick(Timestamp::now(), interval);
                while let Some(chunk) = sleep_chunk(Timestamp::now(), next) {
                    cx.background_executor().timer(chunk).await;
                }
                if !run_inline(&this, RunTrigger::Hourly, cx).await {
                    return;
                }
            }
        })
    }

    fn spawn_watch_poll(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(WATCH_POLL).await;
                if this.update(cx, |b, cx| b.poll_watch(cx)).is_err() {
                    return;
                }
            }
        })
    }

    fn begin_run(&mut self, cx: &mut Context<Self>) -> Option<Arc<SyncCore>> {
        let core = self.core.clone()?;
        if self.app_state.read(cx).syncing {
            return None;
        }
        if let Some(w) = &self.watch {
            lock(&w.debouncer).record_run(Instant::now());
        }
        self.app_state.update(cx, |s, cx| {
            s.syncing = true;
            cx.notify();
        });
        Some(core)
    }

    fn spawn_run(&mut self, trigger: RunTrigger, cx: &mut Context<Self>) {
        if let Some(core) = self.begin_run(cx) {
            self.run_task = Some(cx.spawn(async move |this, cx| {
                execute(&this, core, trigger, cx).await;
            }));
        }
    }

    fn finish_run(&mut self, result: SyncResult, cx: &mut Context<Self>) {
        self.app_state.update(cx, |s, cx| {
            if s.apply_sync_result(&result) {
                cx.emit(AppStateEvent::DataChanged);
            }
            cx.notify();
        });
        match result {
            SyncResult::Finished(_) => self.daemon_task = None,
            SyncResult::Busy => self.wait_for_daemon(cx),
            SyncResult::Failed(_) | SyncResult::Skipped => {}
        }
    }

    fn wait_for_daemon(&mut self, cx: &mut Context<Self>) {
        let data = self.data.clone();
        let busy_at = Timestamp::now();
        let started = Instant::now();
        self.daemon_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(DAEMON_POLL).await;
                let data = data.clone();
                let runs = cx
                    .background_executor()
                    .spawn(async move { data.ingest_runs(1).unwrap_or_default() })
                    .await;
                if daemon_finished(busy_at, &runs) || started.elapsed() >= DAEMON_WAIT_MAX {
                    break;
                }
            }
            this.update(cx, |b, cx| {
                b.daemon_task = None;
                b.app_state.update(cx, |s, cx| {
                    s.daemon_busy = false;
                    cx.emit(AppStateEvent::DataChanged);
                    cx.notify();
                });
            })
            .ok();
        }));
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let data = self.data.clone();
        let defaults = self
            .core
            .as_ref()
            .filter(|_| self.watch.is_some())
            .map(|c| c.adapter_defaults());
        self.refresh_task = Some(cx.spawn(async move |this, cx| {
            let loaded: Refreshed = cx
                .background_executor()
                .spawn(async move { (data.harness_status(), data.ingest_runs(RECENT_RUNS)) })
                .await;
            this.update(cx, |b, cx| b.apply_refresh(loaded, defaults, cx))
                .ok();
        }));
    }

    fn apply_refresh(
        &mut self,
        (status, runs): Refreshed,
        defaults: Option<Vec<(HarnessId, Vec<PathBuf>)>>,
        cx: &mut Context<Self>,
    ) {
        self.app_state.update(cx, |s, cx| {
            if let Ok(status) = &status
                && s.set_harnesses(harness_options(status))
            {
                cx.emit(AppStateEvent::FiltersChanged);
            }
            if let Some(last) = runs.iter().flatten().find_map(LastSync::from_run) {
                s.last_sync = Some(last);
            }
            cx.notify();
        });
        if let (Ok(status), Some(defaults)) = (status, defaults) {
            let configs: Vec<HarnessConfig> = status.into_iter().map(|h| h.config).collect();
            self.rewatch(watch_targets(&defaults, &configs), cx);
        }
    }

    fn rewatch(&mut self, targets: Vec<WatchTarget>, cx: &mut Context<Self>) {
        let Some(w) = &mut self.watch else {
            return;
        };
        if w.targets.as_ref() == Some(&targets) {
            return;
        }
        w.targets = Some(targets.clone());
        let debouncer = w.debouncer.clone();
        w.task = Some(cx.spawn(async move |this, cx| {
            let watcher = cx
                .background_executor()
                .spawn(async move { build_watcher(&targets, debouncer) })
                .await;
            this.update(cx, |b, _| {
                if let Some(w) = &mut b.watch {
                    w.watcher = watcher;
                    w.task = None;
                }
            })
            .ok();
        }));
    }

    fn poll_watch(&mut self, cx: &mut Context<Self>) {
        let Some(w) = &self.watch else {
            return;
        };
        if w.watcher.is_none() || self.app_state.read(cx).syncing {
            return;
        }
        if lock(&w.debouncer).poll(Instant::now()) {
            self.spawn_run(RunTrigger::Watch, cx);
        }
    }
}

async fn execute(
    this: &WeakEntity<CollectorBridge>,
    core: Arc<SyncCore>,
    trigger: RunTrigger,
    cx: &mut AsyncApp,
) {
    let result = cx
        .background_executor()
        .spawn(async move { core.run(trigger) })
        .await;
    this.update(cx, |b, cx| b.finish_run(result, cx)).ok();
}

async fn run_inline(
    this: &WeakEntity<CollectorBridge>,
    trigger: RunTrigger,
    cx: &mut AsyncApp,
) -> bool {
    let Ok(core) = this.update(cx, |b, cx| b.begin_run(cx)) else {
        return false;
    };
    if let Some(core) = core {
        execute(this, core, trigger, cx).await;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, CreateKind, DataChange, MetadataKind, RemoveKind};
    use tbt_core::RunId;
    use tbt_store::RunStats;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn sleep_chunks_follow_the_wall_clock() {
        let now = ts("2026-11-23T10:17:42Z");
        let next = next_tick(now, 60);
        assert_eq!(next, ts("2026-11-23T11:00:00Z"));
        assert_eq!(sleep_chunk(now, next), Some(MAX_SLEEP_CHUNK));
        assert_eq!(
            sleep_chunk(ts("2026-11-23T10:59:50Z"), next),
            Some(secs(10))
        );
        assert_eq!(sleep_chunk(next, next), None);
        let after_suspend = ts("2026-11-23T13:05:00Z");
        assert_eq!(sleep_chunk(after_suspend, next), None);
        assert_eq!(next_tick(after_suspend, 60), ts("2026-11-23T14:00:00Z"));
    }

    #[test]
    fn next_tick_after_a_slow_run_does_not_drift() {
        let tick = ts("2026-11-23T11:00:00Z");
        let run_end = ts("2026-11-23T11:00:37Z");
        assert_eq!(next_tick(run_end, 60), ts("2026-11-23T12:00:00Z"));
        assert_eq!(next_tick(tick, 15), ts("2026-11-23T11:15:00Z"));
    }

    #[test]
    fn debouncer_waits_for_quiet_period() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(secs(30), secs(120), secs(300));
        assert_eq!(d.due_at(), None);
        assert!(!d.poll(t0));
        d.event(t0);
        d.event(t0 + secs(10));
        assert_eq!(d.due_at(), Some(t0 + secs(40)));
        assert!(!d.poll(t0 + secs(39)));
        assert!(d.poll(t0 + secs(40)));
        assert_eq!(d.due_at(), None);
        assert!(!d.poll(t0 + secs(41)));
    }

    #[test]
    fn debouncer_rate_limits_runs() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(secs(30), secs(120), secs(300));
        d.event(t0);
        assert!(d.poll(t0 + secs(30)));
        d.event(t0 + secs(35));
        assert_eq!(d.due_at(), Some(t0 + secs(150)));
        assert!(!d.poll(t0 + secs(100)));
        assert!(d.poll(t0 + secs(150)));
    }

    #[test]
    fn debouncer_fires_during_constant_activity() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(secs(30), secs(120), secs(300));
        let mut fired = None;
        for i in 0..100 {
            let now = t0 + secs(i * 5);
            d.event(now);
            if d.poll(now) {
                fired = Some(i * 5);
                break;
            }
        }
        assert_eq!(fired, Some(300));
    }

    #[test]
    fn other_runs_reset_the_debouncer() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(secs(30), secs(120), secs(300));
        d.event(t0);
        d.record_run(t0 + secs(5));
        assert_eq!(d.due_at(), None);
        d.event(t0 + secs(10));
        assert_eq!(d.due_at(), Some(t0 + secs(125)));
    }

    #[test]
    fn only_content_changes_count() {
        assert!(is_relevant_change(&EventKind::Create(CreateKind::File)));
        assert!(is_relevant_change(&EventKind::Modify(ModifyKind::Data(
            DataChange::Content
        ))));
        assert!(is_relevant_change(&EventKind::Remove(RemoveKind::File)));
        assert!(is_relevant_change(&EventKind::Any));
        assert!(!is_relevant_change(&EventKind::Access(AccessKind::Read)));
        assert!(!is_relevant_change(&EventKind::Modify(
            ModifyKind::Metadata(MetadataKind::AccessTime)
        )));
    }

    fn config(id: HarnessId, enabled: bool, roots: Option<Vec<&str>>) -> HarnessConfig {
        HarnessConfig {
            display_name: id.to_string(),
            id,
            enabled,
            roots: roots.map(|r| r.into_iter().map(PathBuf::from).collect()),
        }
    }

    #[test]
    fn watch_targets_follow_enabled_harnesses() {
        let defaults = vec![
            (
                HarnessId::CLAUDE_CODE,
                vec![PathBuf::from("/h/.claude/projects")],
            ),
            (HarnessId::CODEX, vec![PathBuf::from("/h/.codex/sessions")]),
            (
                HarnessId::OPENCODE,
                vec![PathBuf::from("/h/.local/share/opencode")],
            ),
            (HarnessId::GEMINI_CLI, vec![PathBuf::from("/h/.gemini/tmp")]),
        ];
        let configs = vec![
            config(HarnessId::CLAUDE_CODE, true, Some(vec!["/custom/claude"])),
            config(HarnessId::CODEX, false, None),
            config(HarnessId::OPENCODE, true, None),
        ];
        assert_eq!(
            watch_targets(&defaults, &configs),
            vec![
                WatchTarget {
                    path: PathBuf::from("/custom/claude"),
                    recursive: true
                },
                WatchTarget {
                    path: PathBuf::from("/h/.local/share/opencode"),
                    recursive: false
                },
                WatchTarget {
                    path: PathBuf::from("/h/.gemini/tmp"),
                    recursive: true
                },
            ]
        );
    }

    fn run(id: i64, finished: Option<&str>) -> IngestRunSummary {
        IngestRunSummary {
            id: RunId(id),
            trigger: RunTrigger::Hourly,
            started_at: ts("2026-11-23T10:00:00Z"),
            finished_at: finished.map(ts),
            stats: RunStats::default(),
            warnings: vec![],
            error: None,
        }
    }

    #[test]
    fn daemon_is_done_when_the_newest_run_finished_after_busy() {
        let busy_at = ts("2026-11-23T11:00:02Z");
        assert!(!daemon_finished(busy_at, &[]));
        assert!(!daemon_finished(
            busy_at,
            &[run(1, Some("2026-11-23T10:00:05Z"))]
        ));
        assert!(!daemon_finished(
            busy_at,
            &[run(2, None), run(1, Some("2026-11-23T11:00:01Z"))]
        ));
        assert!(daemon_finished(
            busy_at,
            &[run(2, Some("2026-11-23T11:00:20Z"))]
        ));
        assert!(daemon_finished(
            busy_at,
            &[run(2, Some("2026-11-23T10:59:55Z"))]
        ));
    }

    #[test]
    fn resolve_sync_prefers_the_flag_and_reports_bad_config() {
        let dir = PathBuf::from("/tmp/tbt-flag");
        let (resolved, source) = resolve_sync(Some(dir.clone()), Ok(CollectorConfig::default()));
        assert_eq!(resolved.as_deref(), Some(dir.as_path()));
        match source {
            SyncSource::Collector(c) => assert_eq!(c.data_dir(), dir),
            other => panic!("expected a collector, got {other:?}"),
        }

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "privacy = 3").unwrap();
        let (resolved, source) = resolve_sync(Some(dir.clone()), CollectorConfig::load(&path));
        assert_eq!(resolved, Some(dir));
        match source {
            SyncSource::Unavailable(msg) => assert!(msg.contains("config.toml"), "{msg}"),
            other => panic!("expected unavailable, got {other:?}"),
        }
    }

    #[test]
    fn panics_become_text() {
        let p: Box<dyn Any + Send> = Box::new("boom");
        assert_eq!(panic_text(&*p), "boom");
        let p: Box<dyn Any + Send> = Box::new(String::from("bang"));
        assert_eq!(panic_text(&*p), "bang");
        let p: Box<dyn Any + Send> = Box::new(7_u8);
        assert_eq!(panic_text(&*p), "unknown panic");
    }
}
