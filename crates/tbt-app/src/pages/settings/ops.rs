use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use jiff::Timestamp;
use tbt_collector::recompute::{RecomputeReport, recompute_for_price, recompute_unpriced};
use tbt_collector::{CollectorLock, LockAttempt};
use tbt_core::{Decimal, ModelPrice, PriceId};
use tbt_pricing::{PriceCatalog, PriceChange};
use tbt_store::Store;

use super::price_form::PricePlan;

pub const LITELLM_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
pub const MAX_PRICE_FILE_BYTES: u64 = 20 * 1024 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum WriteError {
    Busy,
    Failed(anyhow::Error),
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WriteError::Busy => {
                f.write_str("A sync is writing to the database right now. Try again in a moment.")
            }
            WriteError::Failed(e) => write!(f, "{e:#}"),
        }
    }
}

impl From<anyhow::Error> for WriteError {
    fn from(e: anyhow::Error) -> Self {
        WriteError::Failed(e)
    }
}

pub fn with_writable_store<T>(
    db: &Path,
    job: impl FnOnce(&mut Store) -> anyhow::Result<T>,
) -> Result<T, WriteError> {
    let dir = db
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .context("database path has no parent directory")?;
    let _lock = match CollectorLock::try_acquire(dir).context("cannot take the collector lock")? {
        LockAttempt::Acquired(lock) => lock,
        LockAttempt::Busy => return Err(WriteError::Busy),
    };
    let mut store = Store::open(db).with_context(|| format!("cannot open {}", db.display()))?;
    job(&mut store).map_err(WriteError::Failed)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    pub events_changed: usize,
    pub sessions_rebuilt: usize,
    pub rows_written: usize,
}

impl Outcome {
    fn add(&mut self, r: RecomputeReport) {
        self.events_changed += r.events_changed;
        self.sessions_rebuilt += r.sessions_rebuilt;
    }

    pub fn summary(&self) -> String {
        let events = match self.events_changed {
            1 => "1 event re-priced".to_owned(),
            n => format!("{n} events re-priced"),
        };
        if self.sessions_rebuilt > 0 {
            format!("{events} in {} sessions", self.sessions_rebuilt)
        } else {
            events
        }
    }
}

pub fn apply_plan(
    store: &mut Store,
    plan: &PricePlan,
    unpriced: Option<(&str, &str)>,
) -> anyhow::Result<Outcome> {
    let mut out = Outcome::default();
    let touched = match plan {
        PricePlan::Insert(p) => vec![store.insert_price(p)?],
        PricePlan::Supersede {
            close,
            effective_to,
            open,
        } => {
            store.close_price(*close, *effective_to)?;
            vec![*close, store.insert_price(open)?]
        }
        PricePlan::Fix(p) => {
            store.update_price(p)?;
            vec![p.id.context("fixed row has no id")?]
        }
    };
    out.rows_written = touched.len();
    for id in touched {
        out.add(recompute_for_price(store, id)?);
    }
    if let Some((provider, model)) = unpriced {
        out.add(recompute_unpriced(store, Some(provider), Some(model))?);
    }
    Ok(out)
}

pub fn close_row(store: &mut Store, id: PriceId, at: Timestamp) -> anyhow::Result<Outcome> {
    store.close_price(id, at)?;
    let mut out = Outcome {
        rows_written: 1,
        ..Outcome::default()
    };
    out.add(recompute_for_price(store, id)?);
    Ok(out)
}

pub fn fetch_litellm() -> anyhow::Result<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(FETCH_TIMEOUT))
        .http_status_as_error(true)
        .build()
        .into();
    let mut response = agent
        .get(LITELLM_URL)
        .call()
        .with_context(|| format!("could not download {LITELLM_URL}"))?;
    response
        .body_mut()
        .with_config()
        .limit(MAX_PRICE_FILE_BYTES)
        .read_to_string()
        .context("could not read the LiteLLM price list (larger than 20 MB or interrupted)")
}

pub fn read_price_file(path: &Path) -> anyhow::Result<String> {
    let file =
        std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut text = String::new();
    file.take(MAX_PRICE_FILE_BYTES + 1)
        .read_to_string(&mut text)
        .with_context(|| format!("cannot read {} as UTF-8 text", path.display()))?;
    anyhow::ensure!(
        text.len() as u64 <= MAX_PRICE_FILE_BYTES,
        "{} is larger than 20 MB",
        path.display()
    );
    Ok(text)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewLine {
    pub provider: String,
    pub model: String,
    pub old: Option<(Decimal, Decimal)>,
    pub new: (Decimal, Decimal),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportPreview {
    pub changed: usize,
    pub new: usize,
    pub unchanged: usize,
    pub lines: Vec<PreviewLine>,
}

impl ImportPreview {
    pub fn summary(&self) -> String {
        format!(
            "{} changed, {} new, {} unchanged",
            self.changed, self.new, self.unchanged
        )
    }

    pub fn is_empty(&self) -> bool {
        self.changed == 0 && self.new == 0
    }
}

const PREVIEW_LINES: usize = 12;

pub fn preview_import(
    current: Vec<ModelPrice>,
    json: &str,
    now: Timestamp,
) -> anyhow::Result<ImportPreview> {
    let total = PriceCatalog::load(Vec::new())?
        .import_litellm(json, now)
        .context("not a LiteLLM price list")?
        .len();
    let catalog = PriceCatalog::load(current)?;
    let changes = catalog.import_litellm(json, now)?;
    let changed = changes
        .iter()
        .filter(|c| matches!(c, PriceChange::Close { .. }))
        .count();
    let opened = changes.len() - changed;
    let mut lines = Vec::new();
    let mut closed: Option<&ModelPrice> = None;
    for change in &changes {
        match change {
            PriceChange::Close { id, .. } => {
                closed = catalog.prices().iter().find(|p| p.id == Some(*id));
            }
            PriceChange::Open(p) => {
                let old = closed.take().map(|o| (o.input_per_mtok, o.output_per_mtok));
                lines.push(PreviewLine {
                    provider: p.provider.clone(),
                    model: p.model_pattern.clone(),
                    old,
                    new: (p.input_per_mtok, p.output_per_mtok),
                });
            }
        }
    }
    lines.sort_by_key(|l| l.old.is_none());
    lines.truncate(PREVIEW_LINES);
    Ok(ImportPreview {
        changed,
        new: opened - changed,
        unchanged: total.saturating_sub(opened),
        lines,
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportOutcome {
    pub closed: usize,
    pub opened: usize,
    pub recompute: Outcome,
}

pub fn apply_import(
    store: &mut Store,
    json: &str,
    now: Timestamp,
) -> anyhow::Result<ImportOutcome> {
    let catalog = PriceCatalog::load(store.model_prices()?)?;
    let changes = catalog.import_litellm(json, now)?;
    let mut out = ImportOutcome::default();
    let mut touched = Vec::new();
    for change in &changes {
        match change {
            PriceChange::Close { id, effective_to } => {
                store.close_price(*id, *effective_to)?;
                touched.push(*id);
                out.closed += 1;
            }
            PriceChange::Open(p) => {
                touched.push(store.insert_price(p)?);
                out.opened += 1;
            }
        }
    }
    for id in touched {
        out.recompute.add(recompute_for_price(store, id)?);
    }
    out.recompute.rows_written = out.closed + out.opened;
    Ok(out)
}

pub fn load_prices(db: Option<&Path>) -> anyhow::Result<Vec<ModelPrice>> {
    match db {
        Some(db) if db.exists() => Ok(Store::open_read_only(db)?.model_prices()?),
        Some(_) => Ok(Vec::new()),
        None => Ok(tbt_pricing::bundled_prices()?),
    }
}

pub fn data_dir_of(db: &Path) -> Option<PathBuf> {
    db.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tbt_core::PriceSource;

    const FIXTURE: &str = r#"{
        "sample_spec": {"litellm_provider": "openai", "mode": "chat"},
        "gpt-test": {
            "litellm_provider": "openai", "mode": "chat",
            "input_cost_per_token": 1.25e-6, "output_cost_per_token": 0.00001
        },
        "gpt-same": {
            "litellm_provider": "openai", "mode": "chat",
            "input_cost_per_token": 2e-6, "output_cost_per_token": 8e-6
        },
        "claude-test": {
            "litellm_provider": "anthropic", "mode": "chat",
            "input_cost_per_token": 3e-6, "output_cost_per_token": 1.5e-5,
            "cache_read_input_token_cost": 3e-7
        },
        "brand-new": {
            "litellm_provider": "anthropic", "mode": "chat",
            "input_cost_per_token": 1e-6, "output_cost_per_token": 5e-6
        },
        "embedder": {
            "litellm_provider": "openai", "mode": "embedding",
            "input_cost_per_token": 1e-7, "output_cost_per_token": 0
        },
        "some-other-cloud/x": {
            "litellm_provider": "bedrock", "mode": "chat",
            "input_cost_per_token": 1e-6, "output_cost_per_token": 1e-6
        }
    }"#;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn imported(id: i64, provider: &str, model: &str, input: &str, output: &str) -> ModelPrice {
        ModelPrice {
            id: Some(PriceId(id)),
            provider: provider.into(),
            model_pattern: model.into(),
            effective_from: ts("2026-01-01T00:00:00Z"),
            effective_to: None,
            input_per_mtok: input.parse().unwrap(),
            output_per_mtok: output.parse().unwrap(),
            cache_write_5m_per_mtok: None,
            cache_write_1h_per_mtok: None,
            cache_read_per_mtok: None,
            long_context: None,
            source: PriceSource::Imported,
        }
    }

    fn current() -> Vec<ModelPrice> {
        let mut claude = imported(3, "anthropic", "claude-test", "3", "15");
        claude.cache_read_per_mtok = Some("0.3".parse().unwrap());
        vec![
            imported(1, "openai", "gpt-test", "1", "10"),
            imported(2, "openai", "gpt-same", "2", "8"),
            claude,
        ]
    }

    #[test]
    fn preview_counts_changed_new_unchanged() {
        let now = ts("2026-11-22T20:00:00Z");
        let p = preview_import(current(), FIXTURE, now).unwrap();
        assert_eq!((p.changed, p.new, p.unchanged), (1, 1, 2));
        assert_eq!(p.summary(), "1 changed, 1 new, 2 unchanged");
        assert_eq!(p.lines.len(), 2);
        assert_eq!(p.lines[0].model, "gpt-test");
        assert_eq!(
            p.lines[0].old,
            Some(("1".parse().unwrap(), "10".parse().unwrap()))
        );
        assert_eq!(p.lines[0].new.0.to_string(), "1.25");
        assert_eq!(p.lines[1].model, "brand-new");
        assert_eq!(p.lines[1].old, None);
    }

    #[test]
    fn preview_from_empty_catalog_is_all_new() {
        let p = preview_import(Vec::new(), FIXTURE, ts("2026-11-22T20:00:00Z")).unwrap();
        assert_eq!((p.changed, p.new, p.unchanged), (0, 4, 0));
    }

    #[test]
    fn preview_rejects_garbage() {
        assert!(preview_import(Vec::new(), "not json", ts("2026-11-22T20:00:00Z")).is_err());
        assert!(preview_import(Vec::new(), "[1,2]", ts("2026-11-22T20:00:00Z")).is_err());
    }

    #[test]
    fn import_applies_close_and_open() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("tbt.sqlite");
        let now = ts("2026-11-22T20:00:00Z");
        let out = with_writable_store(&db, |store| {
            for p in current() {
                store.insert_price(&p)?;
            }
            apply_import(store, FIXTURE, now)
        })
        .unwrap();
        assert_eq!((out.closed, out.opened), (1, 2));
        let prices = load_prices(Some(&db)).unwrap();
        assert_eq!(prices.len(), 5);
        let gpt: Vec<_> = prices
            .iter()
            .filter(|p| p.model_pattern == "gpt-test")
            .collect();
        assert_eq!(gpt.len(), 2);
        assert!(gpt.iter().any(|p| p.effective_to == Some(now)));
        assert!(
            gpt.iter()
                .any(|p| p.effective_from == now && p.effective_to.is_none())
        );

        let again = with_writable_store(&db, |s| apply_import(s, FIXTURE, now)).unwrap();
        assert_eq!((again.closed, again.opened), (0, 0));
    }

    #[test]
    fn busy_lock_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("tbt.sqlite");
        let LockAttempt::Acquired(_held) = CollectorLock::try_acquire(dir.path()).unwrap() else {
            panic!("lock should be free");
        };
        let err = with_writable_store(&db, |_| Ok(())).unwrap_err();
        assert!(matches!(err, WriteError::Busy));
        assert!(err.to_string().contains("Try again"));
    }

    #[test]
    fn price_file_size_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prices.json");
        std::fs::write(&path, FIXTURE).unwrap();
        assert_eq!(read_price_file(&path).unwrap(), FIXTURE);
        assert!(read_price_file(&dir.path().join("missing.json")).is_err());
    }

    #[test]
    fn mock_data_uses_bundled_prices() {
        let prices = load_prices(None).unwrap();
        assert!(!prices.is_empty());
        assert!(prices.iter().all(|p| p.source == PriceSource::Bundled));
        let dir = tempfile::tempdir().unwrap();
        assert!(
            load_prices(Some(&dir.path().join("none.sqlite")))
                .unwrap()
                .is_empty()
        );
    }
}
