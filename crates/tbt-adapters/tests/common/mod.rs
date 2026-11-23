#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tbt_core::{Cursor, HarnessAdapter, ReadBatch, SourceRef, TokenCounts};

pub fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

pub fn fixture(rel: &str) -> PathBuf {
    let path = fixtures_root().join(rel);
    assert!(path.exists(), "missing fixture {}", path.display());
    path
}

pub fn copy_tree(from: &Path, to: &Path) {
    if from.is_dir() {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            copy_tree(&entry.path(), &to.join(entry.file_name()));
        }
    } else {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::copy(from, to).unwrap();
    }
}

pub fn copy_to_temp(from: &Path) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let to = dir.path().join(from.file_name().unwrap());
    copy_tree(from, &to);
    (dir, to)
}

pub fn append(path: &Path, text: &str) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(text.as_bytes()).unwrap();
}

pub fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

pub fn write_lines(path: &Path, lines: &[String]) {
    let mut text = lines.join("\n");
    text.push('\n');
    std::fs::write(path, text).unwrap();
}

pub fn merge(into: &mut ReadBatch, batch: ReadBatch) {
    into.sessions.extend(batch.sessions);
    into.events.extend(batch.events);
    into.messages.extend(batch.messages);
    into.warnings.extend(batch.warnings);
    into.next_cursor = batch.next_cursor;
}

pub fn read_to_end(adapter: &dyn HarnessAdapter, src: &SourceRef, cursor: &Cursor) -> ReadBatch {
    let mut all = ReadBatch::unchanged(cursor.clone());
    for _ in 0..100 {
        let batch = adapter.read(src, &all.next_cursor).unwrap();
        let done = batch.is_empty() && batch.next_cursor == all.next_cursor;
        merge(&mut all, batch);
        if done {
            return all;
        }
    }
    panic!("{} did not reach a fixed point", src.path.display());
}

pub fn read_all(adapter: &dyn HarnessAdapter, roots: &[PathBuf]) -> ReadBatch {
    let mut all = ReadBatch::default();
    for src in adapter.discover(roots).unwrap() {
        merge(&mut all, read_to_end(adapter, &src, &Cursor::empty()));
    }
    all
}

pub fn read_file(adapter: &dyn HarnessAdapter, path: &Path) -> ReadBatch {
    let src = adapter
        .discover(&[path.to_path_buf()])
        .unwrap()
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("{} not discovered", path.display()));
    read_to_end(adapter, &src, &Cursor::empty())
}

#[derive(Debug, Serialize)]
pub struct Projection {
    pub sessions: Vec<SessionView>,
    pub event_count: usize,
    pub totals: BTreeMap<String, ModelTotals>,
    pub events: Vec<String>,
    pub message_count: usize,
    pub messages_by_kind: BTreeMap<String, usize>,
    pub messages_linked_to_events: usize,
    pub messages_with_preview: usize,
    pub tools: BTreeMap<String, usize>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct SessionView {
    pub ext_id: String,
    pub parent_ext_id: Option<String>,
    pub resumed_from_ext_id: Option<String>,
    pub project_path: Option<String>,
    pub title: Option<String>,
    pub git_branch: Option<String>,
    pub harness_version: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub has_baseline_hint: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct ModelTotals {
    pub calls: usize,
    pub input: u64,
    pub output: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
    pub reasoning: u64,
    pub total: u64,
}

impl ModelTotals {
    fn add(&mut self, t: TokenCounts) {
        self.calls += 1;
        self.input += t.input;
        self.output += t.output;
        self.cache_write_5m += t.cache_write_5m;
        self.cache_write_1h += t.cache_write_1h;
        self.cache_read += t.cache_read;
        self.reasoning += t.reasoning;
        self.total += t.total();
    }
}

impl Projection {
    pub fn of(batch: &ReadBatch) -> Projection {
        let sessions = batch
            .sessions
            .iter()
            .map(|s| SessionView {
                ext_id: s.ext_id.clone(),
                parent_ext_id: s.parent_ext_id.clone(),
                resumed_from_ext_id: s.resumed_from_ext_id.clone(),
                project_path: s.project_path.clone(),
                title: s.title.clone(),
                git_branch: s.git_branch.clone(),
                harness_version: s.harness_version.clone(),
                started_at: s.started_at.map(|t| t.to_string()),
                ended_at: s.ended_at.map(|t| t.to_string()),
                has_baseline_hint: s.baseline_hint.is_some(),
            })
            .collect();

        let mut totals: BTreeMap<String, ModelTotals> = BTreeMap::new();
        let mut all = ModelTotals::default();
        for e in &batch.events {
            totals
                .entry(format!("{}/{}", e.provider, e.model))
                .or_default()
                .add(e.tokens());
            all.add(e.tokens());
        }
        totals.insert("(all)".into(), all);

        let events = batch
            .events
            .iter()
            .map(|e| {
                let t = e.tokens();
                format!(
                    "{} {} {} in={} out={} cw5m={} cw1h={} cr={} rs={}{}",
                    e.ts,
                    e.dedup_key,
                    e.model,
                    t.input,
                    t.output,
                    t.cache_write_5m,
                    t.cache_write_1h,
                    t.cache_read,
                    t.reasoning,
                    e.reported_cost
                        .map(|c| format!(" cost={c}"))
                        .unwrap_or_default(),
                )
            })
            .collect();

        let mut messages_by_kind = BTreeMap::new();
        let mut tools = BTreeMap::new();
        for m in &batch.messages {
            let kind = m.kind.map_or("-", |k| k.as_str());
            *messages_by_kind
                .entry(format!("{}/{}", m.role, kind))
                .or_default() += 1;
            if let Some(tool) = &m.tool_name {
                *tools.entry(format!("{}/{tool}", kind)).or_default() += 1;
            }
        }

        Projection {
            sessions,
            event_count: batch.events.len(),
            totals,
            events,
            message_count: batch.messages.len(),
            messages_by_kind,
            messages_linked_to_events: batch
                .messages
                .iter()
                .filter(|m| m.event_dedup_key.is_some())
                .count(),
            messages_with_preview: batch
                .messages
                .iter()
                .filter(|m| m.preview.is_some())
                .count(),
            tools,
            warnings: batch
                .warnings
                .iter()
                .map(|w| {
                    format!(
                        "line {} @{}: {}",
                        w.line.map_or("-".into(), |l| l.to_string()),
                        w.byte_offset.map_or("-".into(), |o| o.to_string()),
                        w.message
                    )
                })
                .collect(),
        }
    }
}

pub fn dedup_keys(batch: &ReadBatch) -> Vec<String> {
    batch.events.iter().map(|e| e.dedup_key.clone()).collect()
}

pub fn assert_unique_dedup_keys(batch: &ReadBatch) {
    let mut keys = dedup_keys(batch);
    let n = keys.len();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), n, "duplicate dedup keys in batch");
}
