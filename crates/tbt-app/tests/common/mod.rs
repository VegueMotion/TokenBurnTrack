#![allow(dead_code)]

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use tbt_adapters::ClaudeCodeAdapter;
use tbt_core::{Cursor, HarnessAdapter, ReadBatch, SourceRef};
use tbt_pricing::{PriceCatalog, bundled_prices};
use tbt_store::{RunStats, RunTrigger, Store};

pub fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

pub fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
    } else {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(from, to)?;
    }
    Ok(())
}

pub fn copy_claude_code_fixtures(to: &Path) -> std::io::Result<Vec<PathBuf>> {
    let dest = to.join("claude_code");
    copy_tree(&fixtures_root().join("claude_code"), &dest)?;
    let mut roots: Vec<PathBuf> = std::fs::read_dir(&dest)?
        .map(|e| e.map(|e| e.path().join("projects")))
        .collect::<std::io::Result<_>>()?;
    roots.retain(|r| r.is_dir());
    roots.sort();
    Ok(roots)
}

fn read_to_end(adapter: &dyn HarnessAdapter, src: &SourceRef) -> anyhow::Result<ReadBatch> {
    let mut all = ReadBatch::unchanged(Cursor::empty());
    for _ in 0..1000 {
        let batch = adapter.read(src, &all.next_cursor)?;
        let done = batch.is_empty() && batch.next_cursor == all.next_cursor;
        all.sessions.extend(batch.sessions);
        all.events.extend(batch.events);
        all.messages.extend(batch.messages);
        all.warnings.extend(batch.warnings);
        all.next_cursor = batch.next_cursor;
        if done {
            return Ok(all);
        }
    }
    anyhow::bail!("{} did not reach a fixed point", src.path.display())
}

pub fn build_db(
    db: &Path,
    roots: &[PathBuf],
    unpriced_models: &[&str],
    now: Timestamp,
) -> anyhow::Result<usize> {
    let adapter = ClaudeCodeAdapter::new();
    let harness = adapter.id();
    let mut store = Store::open(db)?;
    store.register_harness(&harness, adapter.display_name())?;
    let prices: Vec<_> = bundled_prices()?
        .into_iter()
        .filter(|p| !unpriced_models.contains(&p.model_pattern.as_str()))
        .collect();
    store.seed_prices_if_empty(&prices)?;
    let catalog = PriceCatalog::load(store.model_prices()?)?;

    let run = store.start_run(RunTrigger::Manual, now)?;
    let sources = adapter.discover(roots)?;
    let mut warnings = Vec::new();
    let mut events_new = 0;
    for src in &sources {
        let batch = read_to_end(&adapter, src)?;
        let mut tx = store.begin_source_tx(&harness, &src.path)?;
        let mut ids = Vec::new();
        for s in &batch.sessions {
            let project = tx.upsert_project(s.project_path.as_deref())?;
            ids.push(tx.upsert_session(s, project)?);
        }
        let priced: Vec<_> = batch.events.into_iter().map(|e| catalog.apply(e)).collect();
        events_new += tx.insert_events(&priced)?;
        for (s, id) in batch.sessions.iter().zip(ids) {
            if let Some(parent) = &s.parent_ext_id
                && let Some(parent) = tx.session_id(&harness, parent)?
            {
                tx.set_parent_session(id, parent)?;
            }
            if let Some(original) = &s.resumed_from_ext_id
                && let Some(original) = tx.session_id(&harness, original)?
            {
                tx.link_resumed_session(id, original)?;
            }
        }
        tx.save_cursor(&batch.next_cursor, now)?;
        tx.commit()?;
        warnings.extend(batch.warnings);
    }
    let stats = RunStats {
        sources_scanned: sources.len() as u64,
        events_new: events_new as u64,
    };
    store.finish_run(run, now, &stats, &warnings, None)?;
    Ok(events_new)
}
