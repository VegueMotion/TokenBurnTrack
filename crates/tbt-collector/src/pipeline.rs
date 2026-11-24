use std::collections::HashSet;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use crossbeam_channel::{Receiver, Sender};
use jiff::Timestamp;
use tbt_core::{
    HarnessId, ParseWarning, PricedEvent, PrivacyLevel, ReadBatch, RunId, SessionId, SessionUpsert,
    SourceRef,
};
use tbt_pricing::PriceCatalog;
use tbt_store::{RunStats, RunTrigger, SourceTx, Store, paths};

use crate::config::CollectorConfig;
use crate::lock::{CollectorLock, LockAttempt};
use crate::price_seed::{self, SeedReport};
use crate::registry::{Registry, ResolvedAdapter};

pub type PriceSeeder = fn(&mut Store) -> anyhow::Result<SeedReport>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestFinished {
    pub run_id: RunId,
    pub trigger: RunTrigger,
    pub finished_at: Timestamp,
    pub sources_scanned: u64,
    pub sources_read: u64,
    pub sources_failed: u64,
    pub events_new: u64,
    pub messages_new: u64,
    pub warnings: usize,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunError {
    pub harness: Option<HarnessId>,
    pub source: Option<PathBuf>,
    pub message: String,
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(h) = &self.harness {
            write!(f, "[{h}] ")?;
        }
        if let Some(s) = &self.source {
            write!(f, "{}: ", s.display())?;
        }
        f.write_str(&self.message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReport {
    pub run_id: RunId,
    pub trigger: RunTrigger,
    pub started_at: Timestamp,
    pub finished_at: Timestamp,
    pub sources_scanned: u64,
    pub sources_read: u64,
    pub sources_unchanged: u64,
    pub sources_failed: u64,
    pub sources_missing: u64,
    pub events_new: u64,
    pub messages_new: u64,
    pub raw_json_dropped: u64,
    pub warnings: Vec<ParseWarning>,
    pub errors: Vec<RunError>,
}

impl RunReport {
    pub fn error_text(&self) -> Option<String> {
        (!self.errors.is_empty()).then(|| {
            self.errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        })
    }

    pub fn finished_event(&self) -> IngestFinished {
        IngestFinished {
            run_id: self.run_id,
            trigger: self.trigger,
            finished_at: self.finished_at,
            sources_scanned: self.sources_scanned,
            sources_read: self.sources_read,
            sources_failed: self.sources_failed,
            events_new: self.events_new,
            messages_new: self.messages_new,
            warnings: self.warnings.len(),
            error: self.error_text(),
        }
    }

    pub fn summary(&self) -> String {
        let secs = self
            .finished_at
            .duration_since(self.started_at)
            .as_secs_f64();
        format!(
            "run {} ({}): {} sources scanned, {} read, {} unchanged, {} failed, {} missing; \
             {} new events, {} new messages, {} warnings in {secs:.1}s",
            self.run_id,
            trigger_name(self.trigger),
            self.sources_scanned,
            self.sources_read,
            self.sources_unchanged,
            self.sources_failed,
            self.sources_missing,
            self.events_new,
            self.messages_new,
            self.warnings.len(),
        )
    }
}

pub fn trigger_name(t: RunTrigger) -> &'static str {
    match t {
        RunTrigger::Hourly => "hourly",
        RunTrigger::Startup => "startup",
        RunTrigger::Manual => "manual",
        RunTrigger::Watch => "watch",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    Finished(RunReport),
    Busy,
}

impl RunOutcome {
    pub fn report(&self) -> Option<&RunReport> {
        match self {
            RunOutcome::Finished(r) => Some(r),
            RunOutcome::Busy => None,
        }
    }

    pub fn is_busy(&self) -> bool {
        matches!(self, RunOutcome::Busy)
    }
}

pub struct Collector {
    data_dir: PathBuf,
    registry: Registry,
    config: CollectorConfig,
    seeder: PriceSeeder,
    listeners: Mutex<Vec<Sender<IngestFinished>>>,
}

impl fmt::Debug for Collector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Collector")
            .field("data_dir", &self.data_dir)
            .field("registry", &self.registry)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Collector {
    pub fn new(data_dir: PathBuf, registry: Registry, config: CollectorConfig) -> Collector {
        Collector {
            data_dir,
            registry,
            config,
            seeder: price_seed::sync_bundled_prices,
            listeners: Mutex::new(Vec::new()),
        }
    }

    pub fn builtin(data_dir: PathBuf, config: CollectorConfig) -> Collector {
        let registry = Registry::builtin(&config);
        Collector::new(data_dir, registry, config)
    }

    pub fn with_price_seeder(mut self, seeder: PriceSeeder) -> Collector {
        self.seeder = seeder;
        self
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn db_path(&self) -> PathBuf {
        paths::db_path(&self.data_dir)
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub fn config(&self) -> &CollectorConfig {
        &self.config
    }

    pub fn subscribe(&self) -> Receiver<IngestFinished> {
        let (tx, rx) = crossbeam_channel::unbounded();
        self.listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tx);
        rx
    }

    fn notify(&self, event: &IngestFinished) {
        let mut listeners = self.listeners.lock().unwrap_or_else(|e| e.into_inner());
        listeners.retain(|tx| tx.send(event.clone()).is_ok());
    }

    pub fn ingest_run(&self, trigger: RunTrigger) -> anyhow::Result<RunOutcome> {
        let lock = match CollectorLock::try_acquire(&self.data_dir)? {
            LockAttempt::Acquired(lock) => lock,
            LockAttempt::Busy => {
                tracing::info!(data_dir = %self.data_dir.display(), "collector lock busy; skipping run");
                return Ok(RunOutcome::Busy);
            }
        };
        let report = self.run_locked(trigger)?;
        drop(lock);
        self.notify(&report.finished_event());
        Ok(RunOutcome::Finished(report))
    }

    fn run_locked(&self, trigger: RunTrigger) -> anyhow::Result<RunReport> {
        let started_at = Timestamp::now();
        let db_path = self.db_path();
        let mut store = Store::open(&db_path)?;
        let mut errors = Vec::new();

        match (self.seeder)(&mut store) {
            Ok(seed) => tracing::debug!(?seed, "price seed"),
            Err(e) => errors.push(RunError {
                harness: None,
                source: None,
                message: format!("price seed failed: {e:#}"),
            }),
        }
        let catalog = match store
            .model_prices()
            .map_err(anyhow::Error::from)
            .and_then(|p| PriceCatalog::load(p).map_err(anyhow::Error::from))
        {
            Ok(c) => c,
            Err(e) => {
                errors.push(RunError {
                    harness: None,
                    source: None,
                    message: format!("price catalog unusable, events stay unpriced: {e:#}"),
                });
                PriceCatalog::load(Vec::new())?
            }
        };

        let resolved = self.registry.resolve(&mut store)?;
        let run_id = store.start_run(trigger, started_at)?;
        tracing::info!(run = %run_id, trigger = trigger_name(trigger), "ingest run started");

        let mut writer = Writer {
            store: &mut store,
            catalog: &catalog,
            privacy: self.config.privacy,
            report: RunReport {
                run_id,
                trigger,
                started_at,
                finished_at: started_at,
                sources_scanned: 0,
                sources_read: 0,
                sources_unchanged: 0,
                sources_failed: 0,
                sources_missing: 0,
                events_new: 0,
                messages_new: 0,
                raw_json_dropped: 0,
                warnings: Vec::new(),
                errors,
            },
        };
        let enabled: Vec<&ResolvedAdapter> = resolved.iter().filter(|r| r.enabled).collect();
        let (tx, rx) = crossbeam_channel::bounded::<Msg>(enabled.len().max(1) * 2);
        std::thread::scope(|scope| {
            for r in &enabled {
                let tx = tx.clone();
                let db_path = &db_path;
                scope.spawn(move || read_adapter(r, db_path, &tx));
            }
            drop(tx);
            for msg in rx {
                writer.handle(msg);
            }
        });

        let mut report = writer.report;
        if let Some(days) = self.config.raw_json_retention_days {
            match store.drop_raw_json_older_than(days, Timestamp::now()) {
                Ok(n) => report.raw_json_dropped = n as u64,
                Err(e) => report.errors.push(RunError {
                    harness: None,
                    source: None,
                    message: format!("dropping old raw_json failed: {e}"),
                }),
            }
        }

        report.finished_at = Timestamp::now();
        let mut stored_warnings = report.warnings.clone();
        stored_warnings.extend(report.errors.iter().filter_map(|e| {
            Some(ParseWarning {
                harness: e.harness.clone()?,
                source: e
                    .source
                    .as_ref()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                line: None,
                byte_offset: None,
                message: format!("error: {}", e.message),
            })
        }));
        store.finish_run(
            run_id,
            report.finished_at,
            &RunStats {
                sources_scanned: report.sources_scanned,
                events_new: report.events_new,
            },
            &stored_warnings,
            report.error_text().as_deref(),
        )?;
        tracing::info!("{}", report.summary());
        for e in &report.errors {
            tracing::warn!("{e}");
        }
        Ok(report)
    }
}

enum Msg {
    Scanned(usize),
    Missing {
        harness: HarnessId,
        paths: Vec<PathBuf>,
    },
    Unchanged,
    Batch {
        harness: HarnessId,
        source: SourceRef,
        batch: ReadBatch,
    },
    Failed(RunError),
}

fn read_adapter(r: &ResolvedAdapter, db_path: &Path, tx: &Sender<Msg>) {
    let fail = |source: Option<PathBuf>, message: String| {
        let _ = tx.send(Msg::Failed(RunError {
            harness: Some(r.id.clone()),
            source,
            message,
        }));
    };
    let store = match Store::open_read_only(db_path) {
        Ok(s) => s,
        Err(e) => return fail(None, format!("cannot open the store: {e}")),
    };
    let discovered = match guarded(|| r.adapter.discover(&r.roots)) {
        Ok(s) => s,
        Err(e) => return fail(None, format!("discover failed: {e}")),
    };
    let known: HashSet<PathBuf> = match store.known_sources(&r.id) {
        Ok(k) => k.into_iter().collect(),
        Err(e) => return fail(None, format!("cannot list known sources: {e}")),
    };
    let _ = tx.send(Msg::Scanned(discovered.len()));
    let found: HashSet<&Path> = discovered.iter().map(|s| s.path.as_path()).collect();
    let mut missing: Vec<PathBuf> = known
        .iter()
        .filter(|p| !found.contains(p.as_path()))
        .cloned()
        .collect();
    if !missing.is_empty() {
        missing.sort();
        let _ = tx.send(Msg::Missing {
            harness: r.id.clone(),
            paths: missing,
        });
    }

    let mut sources: Vec<(SystemTime, SourceRef)> = discovered
        .into_iter()
        .map(|s| (mtime(&s.path), s))
        .collect();
    sources.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.path.cmp(&b.1.path)));

    for (_, source) in sources {
        let cursor = match store.cursor_for(&r.id, &source.path) {
            Ok(c) => c.unwrap_or_default(),
            Err(e) => {
                fail(Some(source.path), format!("cannot load cursor: {e}"));
                continue;
            }
        };
        let batch = match guarded(|| r.adapter.read(&source, &cursor)) {
            Ok(b) => b,
            Err(e) => {
                fail(Some(source.path), format!("read failed: {e}"));
                continue;
            }
        };
        let msg = if batch.is_empty() && batch.next_cursor == cursor && known.contains(&source.path)
        {
            Msg::Unchanged
        } else {
            Msg::Batch {
                harness: r.id.clone(),
                source,
                batch,
            }
        };
        if tx.send(msg).is_err() {
            return;
        }
    }
}

fn guarded<T>(f: impl FnOnce() -> tbt_core::Result<T>) -> Result<T, String> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(e.to_string()),
        Err(panic) => {
            let text = panic
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_owned());
            Err(format!("adapter panicked: {text}"))
        }
    }
}

fn mtime(path: &Path) -> SystemTime {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

struct Writer<'a> {
    store: &'a mut Store,
    catalog: &'a PriceCatalog,
    privacy: PrivacyLevel,
    report: RunReport,
}

impl Writer<'_> {
    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Scanned(n) => self.report.sources_scanned += n as u64,
            Msg::Unchanged => self.report.sources_unchanged += 1,
            Msg::Failed(e) => {
                if e.source.is_some() {
                    self.report.sources_failed += 1;
                }
                self.report.errors.push(e);
            }
            Msg::Missing { harness, paths } => {
                for path in paths {
                    match self.store.mark_source_missing(&harness, &path) {
                        Ok(()) => self.report.sources_missing += 1,
                        Err(e) => self.report.errors.push(RunError {
                            harness: Some(harness.clone()),
                            source: Some(path),
                            message: format!("cannot mark missing: {e}"),
                        }),
                    }
                }
            }
            Msg::Batch {
                harness,
                source,
                mut batch,
            } => {
                let warnings = std::mem::take(&mut batch.warnings);
                self.report.warnings.extend(warnings);
                match write_batch(
                    self.store,
                    self.catalog,
                    self.privacy,
                    &harness,
                    &source,
                    batch,
                ) {
                    Ok((events, messages)) => {
                        self.report.sources_read += 1;
                        self.report.events_new += events as u64;
                        self.report.messages_new += messages as u64;
                        tracing::debug!(
                            harness = %harness,
                            source = %source.path.display(),
                            events,
                            messages,
                            "source ingested"
                        );
                    }
                    Err(e) => {
                        self.report.sources_failed += 1;
                        self.report.errors.push(RunError {
                            harness: Some(harness),
                            source: Some(source.path),
                            message: format!("write failed: {e:#}"),
                        });
                    }
                }
            }
        }
    }
}

fn write_batch(
    store: &mut Store,
    catalog: &PriceCatalog,
    privacy: PrivacyLevel,
    harness: &HarnessId,
    source: &SourceRef,
    batch: ReadBatch,
) -> anyhow::Result<(usize, usize)> {
    let ReadBatch {
        sessions,
        mut events,
        messages,
        next_cursor,
        warnings: _,
    } = batch;
    let mut tx = store.begin_source_tx(harness, &source.path)?;
    let mut hinted: Vec<(SessionId, tbt_core::TokenCounts)> = Vec::new();

    for s in &sessions {
        let project = tx.upsert_project(s.project_path.as_deref())?;
        let existing = tx.session_id(harness, &s.ext_id)?;
        let first_key = events
            .iter()
            .find(|e| e.session_ext_id == s.ext_id)
            .map(|e| e.dedup_key.as_str());
        let original = find_original(&tx, harness, s, existing, first_key)?;

        let mut upsert = s.clone();
        if original.is_some() {
            upsert.started_at = if existing.is_none() { s.ended_at } else { None };
        }
        let id = tx.upsert_session(&upsert, project)?;

        if let Some(orig) = original {
            let orig = match orig {
                Original::Stored(id) => id,
                Original::Missing(ext) => {
                    tx.upsert_session(&SessionUpsert::new(harness.clone(), ext), project)?
                }
            };
            tx.link_resumed_session(id, orig)?;
            if let Some(hint) = &s.baseline_hint
                && let Some(key) = &hint.first_event_key
                && let Some(base) = tx.last_cumulative_total(orig)?
                && let Some(e) = events
                    .iter_mut()
                    .find(|e| e.dedup_key == *key && e.session_ext_id == s.ext_id)
            {
                let rebased = e.tokens().delta_since(&base);
                e.set_tokens(rebased);
            }
        }

        if let Some(parent) = &s.parent_ext_id
            && *parent != s.ext_id
        {
            let pid = match tx.session_id(harness, parent)? {
                Some(pid) => pid,
                None => tx.upsert_session(
                    &SessionUpsert::new(harness.clone(), parent.clone()),
                    project,
                )?,
            };
            tx.set_parent_session(id, pid)?;
        }

        if let Some(hint) = &s.baseline_hint {
            hinted.push((id, hint.last_total));
        }
    }

    let priced: Vec<PricedEvent> = events.into_iter().map(|e| catalog.apply(e)).collect();
    let events_new = tx.insert_events(&priced)?;
    for (id, total) in &hinted {
        tx.save_cumulative_total(*id, total)?;
    }
    let messages = privacy.apply(messages);
    let messages_new = if messages.is_empty() {
        0
    } else {
        tx.insert_messages(&messages)?
    };
    tx.save_cursor(&next_cursor, Timestamp::now())?;
    tx.commit()?;
    Ok((events_new, messages_new))
}

enum Original {
    Stored(SessionId),
    Missing(String),
}

fn find_original(
    tx: &SourceTx<'_>,
    harness: &HarnessId,
    s: &SessionUpsert,
    existing: Option<SessionId>,
    first_key: Option<&str>,
) -> anyhow::Result<Option<Original>> {
    if let Some(ext) = s.resumed_from_ext_id.as_ref().filter(|e| **e != s.ext_id) {
        return Ok(Some(match tx.session_id(harness, ext)? {
            Some(id) => Original::Stored(id),
            None => Original::Missing(ext.clone()),
        }));
    }
    if existing.is_some() {
        return Ok(None);
    }
    let Some(key) = first_key else {
        return Ok(None);
    };
    Ok(tx
        .session_holding_dedup_key(harness, key)?
        .map(Original::Stored))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collector_is_send_sync() {
        fn check<T: Send + Sync>() {}
        check::<Collector>();
    }
}
