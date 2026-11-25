use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tbt_core::jsonl::{FileCursor, JsonlLine, JsonlReader, ReadStart};
use tbt_core::{
    Cursor, Decimal, HarnessAdapter, HarnessId, ParseWarning, ReadBatch, Result, SessionUpsert,
    SourceKind, SourceRef, Timestamp, TokenCounts, UsageEvent,
};

use crate::discover;

const UNKNOWN: &str = "unknown";

#[derive(Debug, Clone, Copy, Default)]
pub struct PiAdapter;

impl PiAdapter {
    pub fn new() -> PiAdapter {
        PiAdapter
    }
}

pub fn default_roots_from(
    session_dir: Option<OsString>,
    agent_dir: Option<OsString>,
    home: Option<PathBuf>,
) -> Vec<PathBuf> {
    if let Some(dir) = session_dir.filter(|d| !d.is_empty()) {
        return vec![PathBuf::from(dir)];
    }
    match agent_dir.filter(|d| !d.is_empty()) {
        Some(dir) => vec![PathBuf::from(dir).join("sessions")],
        None => home
            .map(|h| h.join(".pi").join("agent").join("sessions"))
            .into_iter()
            .collect(),
    }
}

impl HarnessAdapter for PiAdapter {
    fn id(&self) -> HarnessId {
        HarnessId::PI
    }

    fn display_name(&self) -> &str {
        "pi"
    }

    fn default_roots(&self) -> Vec<PathBuf> {
        default_roots_from(
            std::env::var_os("PI_CODING_AGENT_SESSION_DIR"),
            std::env::var_os("PI_CODING_AGENT_DIR"),
            std::env::home_dir(),
        )
    }

    fn discover(&self, roots: &[PathBuf]) -> Result<Vec<SourceRef>> {
        Ok(discover::sources_with_extension(
            roots,
            "jsonl",
            SourceKind::JsonlFile,
        ))
    }

    fn read(&self, src: &SourceRef, cursor: &Cursor) -> Result<ReadBatch> {
        let mut warnings = Vec::new();
        let mut state = match cursor.state::<State>() {
            Ok(state) => state.unwrap_or_default(),
            Err(e) => {
                warnings.push(warning(src, format!("{e}; reading from the start")));
                State::default()
            }
        };

        let mut reader = JsonlReader::open(&src.path, &state.file)?;
        match reader.start() {
            ReadStart::Unchanged => return Ok(ReadBatch::unchanged(cursor.clone())),
            ReadStart::Continue => {}
            ReadStart::Fresh | ReadStart::Restarted(_) => state = State::default(),
        }

        let mut b = Builder {
            src,
            state,
            batch: ReadBatch {
                warnings,
                ..ReadBatch::default()
            },
            touched: false,
        };
        for line in reader.by_ref() {
            match line {
                Ok(line) => b.line(&line),
                Err(e) => {
                    b.batch.warnings.push(warning(src, e.to_string()));
                    break;
                }
            }
        }
        b.finish(reader.into_cursor())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct State {
    #[serde(flatten)]
    file: FileCursor,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<Header>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<(String, String)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_ts: Option<Timestamp>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Header {
    id: String,
    timestamp: Option<Timestamp>,
    cwd: Option<String>,
    #[serde(rename = "parentSession", skip_serializing_if = "Option::is_none")]
    parent_session: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Entry {
    #[serde(rename = "type")]
    kind: String,
    id: Option<String>,
    timestamp: Option<String>,
    message: Option<Message>,
    provider: Option<String>,
    model: Option<String>,
    #[serde(rename = "modelId")]
    model_id: Option<String>,
    usage: Option<Usage>,
    name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Message {
    role: String,
    provider: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
    timestamp: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Usage {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    cache_write_1h: u64,
    reasoning: u64,
    cost: Option<Cost>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Cost {
    total: Option<f64>,
}

impl Usage {
    fn tokens(&self) -> TokenCounts {
        let cache_write_1h = self.cache_write_1h.min(self.cache_write);
        TokenCounts {
            input: self.input,
            output: self.output,
            cache_write_5m: self.cache_write - cache_write_1h,
            cache_write_1h,
            cache_read: self.cache_read,
            reasoning: self.reasoning.min(self.output),
        }
    }
}

struct Builder<'a> {
    src: &'a SourceRef,
    state: State,
    batch: ReadBatch,
    touched: bool,
}

impl Builder<'_> {
    fn warn(&mut self, line: &JsonlLine, message: impl Into<String>) {
        self.batch
            .warnings
            .push(warning(self.src, message).at(line.line_no, line.offset));
    }

    fn line(&mut self, line: &JsonlLine) {
        let value: Value = match serde_json::from_str(&line.text) {
            Ok(v) => v,
            Err(e) => return self.warn(line, format!("invalid JSON: {e}")),
        };
        if value.get("type").and_then(Value::as_str) == Some("session") {
            match Header::deserialize(&value) {
                Ok(h) if !h.id.is_empty() => {
                    self.state.session = Some(h);
                    self.touched = true;
                }
                Ok(_) => self.warn(line, "session header has no `id`"),
                Err(e) => self.warn(line, format!("session header: {e}")),
            }
            return;
        }
        let entry = match Entry::deserialize(&value) {
            Ok(e) => e,
            Err(e) => return self.warn(line, e.to_string()),
        };
        if self.state.session.is_none() {
            return self.warn(line, "entry before the session header");
        }
        let ts = entry.timestamp.as_deref().and_then(|t| t.parse().ok());

        match entry.kind.as_str() {
            "session_info" => {
                self.state.title = entry.name.clone().filter(|n| !n.is_empty());
                self.touched = true;
            }
            "model_change" => {
                if let (Some(p), Some(m)) = (&entry.provider, &entry.model_id) {
                    self.state.model = Some((p.clone(), m.clone()));
                }
            }
            "message" => {
                let Some(msg) = &entry.message else { return };
                if msg.role != "assistant" {
                    return;
                }
                if let (Some(p), Some(m)) = (&msg.provider, &msg.model) {
                    self.state.model = Some((p.clone(), m.clone()));
                }
                let ts = msg
                    .timestamp
                    .and_then(|ms| Timestamp::from_millisecond(ms).ok())
                    .or(ts);
                self.usage(
                    line,
                    &entry,
                    msg.usage.as_ref(),
                    value.pointer("/message/usage"),
                    ts,
                );
            }
            "usage" => {
                if let (Some(p), Some(m)) = (&entry.provider, &entry.model) {
                    let model = Some((p.clone(), m.clone()));
                    let current = std::mem::replace(&mut self.state.model, model);
                    self.usage(line, &entry, entry.usage.as_ref(), value.get("usage"), ts);
                    self.state.model = current;
                } else {
                    self.usage(line, &entry, entry.usage.as_ref(), value.get("usage"), ts);
                }
            }
            "compaction" | "branch_summary" => {
                self.usage(line, &entry, entry.usage.as_ref(), value.get("usage"), ts);
            }
            _ => {}
        }
    }

    fn usage(
        &mut self,
        line: &JsonlLine,
        entry: &Entry,
        usage: Option<&Usage>,
        raw: Option<&Value>,
        ts: Option<Timestamp>,
    ) {
        let Some(usage) = usage else { return };
        let tokens = usage.tokens();
        if tokens.is_zero() {
            return;
        }
        let Some(ts) = ts else {
            return self.warn(line, format!("{} entry has no valid timestamp", entry.kind));
        };
        let Some(header) = self.state.session.as_ref() else {
            return;
        };
        if header.parent_session.is_some() && header.timestamp.is_some_and(|h| ts <= h) {
            return;
        }
        let session_ext_id = header.id.clone();
        let project_path = header.cwd.clone();
        let Some(id) = entry.id.as_deref() else {
            return self.warn(line, format!("{} entry has no `id`", entry.kind));
        };
        let (provider, model) = self
            .state
            .model
            .clone()
            .unwrap_or_else(|| (UNKNOWN.to_owned(), UNKNOWN.to_owned()));

        self.state.last_ts = Some(self.state.last_ts.map_or(ts, |t| t.max(ts)));
        self.touched = true;
        self.batch.events.push(UsageEvent {
            harness: HarnessId::PI,
            dedup_key: format!("{id}:{}", ts.as_millisecond()),
            session_ext_id,
            ts,
            provider,
            model,
            input_tokens: tokens.input,
            output_tokens: tokens.output,
            cache_write_5m_tokens: tokens.cache_write_5m,
            cache_write_1h_tokens: tokens.cache_write_1h,
            cache_read_tokens: tokens.cache_read,
            reasoning_tokens: tokens.reasoning,
            reported_cost: usage
                .cost
                .as_ref()
                .and_then(|c| c.total)
                .and_then(cost_decimal),
            project_path,
            git_branch: None,
            raw: raw.and_then(|r| serde_json::value::to_raw_value(r).ok()),
        });
    }

    fn finish(mut self, file: FileCursor) -> Result<ReadBatch> {
        if self.touched
            && let Some(h) = &self.state.session
        {
            let mut s = SessionUpsert::new(HarnessId::PI, h.id.clone());
            s.project_path = h.cwd.clone();
            s.title = self.state.title.clone();
            s.started_at = h.timestamp;
            s.ended_at = self.state.last_ts.or(h.timestamp);
            s.resumed_from_ext_id = h
                .parent_session
                .as_deref()
                .and_then(parent_ext_id)
                .filter(|p| *p != h.id);
            self.batch.sessions.push(s);
        }
        self.state.file = file;
        self.batch.next_cursor = Cursor::from_state(&self.state)?;
        Ok(self.batch)
    }
}

fn parent_ext_id(path: &str) -> Option<String> {
    let stem = Path::new(path).file_stem()?.to_str()?;
    let (_, id) = stem.split_once('_')?;
    (!id.is_empty()).then(|| id.to_owned())
}

fn cost_decimal(cost: f64) -> Option<Decimal> {
    if !cost.is_finite() || cost <= 0.0 {
        return None;
    }
    cost.to_string().parse().ok()
}

fn warning(src: &SourceRef, message: impl Into<String>) -> ParseWarning {
    ParseWarning::new(HarnessId::PI, src, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_roots_follow_pi_env() {
        assert_eq!(
            default_roots_from(Some("/s".into()), Some("/a".into()), Some("/h".into())),
            [PathBuf::from("/s")]
        );
        assert_eq!(
            default_roots_from(None, Some("/a".into()), Some("/h".into())),
            [PathBuf::from("/a/sessions")]
        );
        assert_eq!(
            default_roots_from(Some("".into()), Some("".into()), Some("/h".into())),
            [PathBuf::from("/h/.pi/agent/sessions")]
        );
        assert!(default_roots_from(None, None, None).is_empty());
    }

    #[test]
    fn parent_id_comes_from_the_file_name() {
        assert_eq!(
            parent_ext_id("/x/--home-u-p--/2026-11-20T10-00-00-000Z_0b8f.jsonl").as_deref(),
            Some("0b8f")
        );
        assert_eq!(parent_ext_id("/x/plain.jsonl"), None);
    }
}
