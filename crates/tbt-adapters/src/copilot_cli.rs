use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tbt_core::jsonl::{FileCursor, JsonlLine, JsonlReader, ReadStart};
use tbt_core::{
    Cursor, HarnessAdapter, HarnessId, MessageKind, MessageRecord, MessageRole, PREVIEW_MAX_CHARS,
    ParseWarning, ReadBatch, Result, SessionUpsert, SourceKind, SourceRef, Timestamp, TokenCounts,
    UsageEvent, truncate_preview,
};

use crate::discover;
use crate::models::known_provider;

const DEFAULT_PROVIDER: &str = "github";
const MAX_OPEN_CALLS: usize = 256;

#[derive(Debug, Clone, Copy, Default)]
pub struct CopilotCliAdapter;

impl CopilotCliAdapter {
    pub fn new() -> CopilotCliAdapter {
        CopilotCliAdapter
    }
}

pub fn default_roots_from(copilot_home: Option<OsString>, home: Option<PathBuf>) -> Vec<PathBuf> {
    match copilot_home.filter(|d| !d.is_empty()) {
        Some(dir) => vec![PathBuf::from(dir).join("session-state")],
        None => home
            .map(|h| h.join(".copilot").join("session-state"))
            .into_iter()
            .collect(),
    }
}

impl HarnessAdapter for CopilotCliAdapter {
    fn id(&self) -> HarnessId {
        HarnessId::COPILOT_CLI
    }

    fn display_name(&self) -> &str {
        "GitHub Copilot CLI"
    }

    fn default_roots(&self) -> Vec<PathBuf> {
        default_roots_from(std::env::var_os("COPILOT_HOME"), std::env::home_dir())
    }

    fn discover(&self, roots: &[PathBuf]) -> Result<Vec<SourceRef>> {
        Ok(discover::files_with_extension(roots, "jsonl")
            .into_iter()
            .filter(|p| roots.contains(p) || is_event_log(p))
            .map(|p| SourceRef::new(p, SourceKind::JsonlFile))
            .collect())
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

        let mut batch = Builder::new(src, state);
        batch.warnings = warnings;
        for line in reader.by_ref() {
            match line {
                Ok(line) => batch.line(&line),
                Err(e) => {
                    batch.warnings.push(warning(src, e.to_string()));
                    break;
                }
            }
        }
        batch.finish(reader.into_cursor())
    }
}

fn is_event_log(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "events.jsonl")
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct State {
    #[serde(flatten)]
    file: FileCursor,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    totals: BTreeMap<String, TokenCounts>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    open_calls: Vec<(String, String)>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    reasoning_tokens: u64,
}

impl Usage {
    fn totals(&self) -> TokenCounts {
        TokenCounts {
            input: self.input_tokens,
            output: self.output_tokens,
            cache_write_5m: self.cache_write_tokens,
            cache_write_1h: 0,
            cache_read: self.cache_read_tokens,
            reasoning: self.reasoning_tokens,
        }
    }
}

fn without_cache(mut t: TokenCounts) -> TokenCounts {
    let cached = t.cache_read + t.cache_write();
    if t.input >= cached {
        t.input -= cached;
    }
    t
}

struct Builder<'a> {
    src: &'a SourceRef,
    state: State,
    dir_session: String,
    session: Option<SessionUpsert>,
    events: Vec<UsageEvent>,
    messages: Vec<MessageRecord>,
    warnings: Vec<ParseWarning>,
    emitted: HashSet<String>,
}

impl<'a> Builder<'a> {
    fn new(src: &'a SourceRef, state: State) -> Builder<'a> {
        Builder {
            src,
            state,
            dir_session: dir_session_id(&src.path),
            session: None,
            events: Vec::new(),
            messages: Vec::new(),
            warnings: Vec::new(),
            emitted: HashSet::new(),
        }
    }

    fn warn(&mut self, line: &JsonlLine, message: impl Into<String>) {
        self.warnings
            .push(warning(self.src, message).at(line.line_no, line.offset));
    }

    fn session_id(&self) -> String {
        self.state
            .session_id
            .clone()
            .unwrap_or_else(|| self.dir_session.clone())
    }

    fn session(&mut self) -> &mut SessionUpsert {
        let ext_id = self.session_id();
        let s = self
            .session
            .get_or_insert_with(|| SessionUpsert::new(HarnessId::COPILOT_CLI, ext_id.clone()));
        s.ext_id = ext_id;
        s
    }

    fn line(&mut self, line: &JsonlLine) {
        let value: Value = match serde_json::from_str(&line.text) {
            Ok(v) => v,
            Err(e) => return self.warn(line, format!("invalid JSON: {e}")),
        };
        let Some(obj) = value.as_object() else {
            return self.warn(line, "line is not a JSON object");
        };
        let Some(kind) = str_field(obj, "type") else {
            return self.warn(line, "line has no `type`");
        };
        let ts = match obj.get("timestamp") {
            None | Some(Value::Null) => None,
            Some(v) => {
                let parsed = v.as_str().and_then(|s| s.parse::<Timestamp>().ok());
                if parsed.is_none() {
                    self.warn(line, format!("invalid timestamp {v}"));
                }
                parsed
            }
        };
        let empty = Map::new();
        let data = obj.get("data").and_then(Value::as_object).unwrap_or(&empty);

        if kind == "session.start" {
            set_if_present(&mut self.state.session_id, data, "sessionId");
            set_if_present(&mut self.state.version, data, "copilotVersion");
        }
        if matches!(kind, "session.start" | "session.resume")
            && let Some(context) = data.get("context").and_then(Value::as_object)
        {
            set_if_present(&mut self.state.cwd, context, "cwd");
            set_if_present(&mut self.state.git_branch, context, "branch");
        }
        let s = self.session();
        if let Some(ts) = ts {
            s.started_at = Some(s.started_at.map_or(ts, |t| t.min(ts)));
            s.ended_at = Some(s.ended_at.map_or(ts, |t| t.max(ts)));
        }

        let envelope_id = str_field(obj, "id");
        match kind {
            "session.usage_checkpoint" => {
                let metrics = data
                    .get("accountingSnapshot")
                    .and_then(Value::as_object)
                    .and_then(|a| a.get("modelMetrics"));
                if let Some(metrics) = metrics {
                    self.snapshot(line, ts, metrics);
                }
            }
            "session.shutdown" => match data.get("modelMetrics") {
                Some(metrics) => self.snapshot(line, ts, metrics),
                None => self.warn(line, "session.shutdown has no `modelMetrics`"),
            },
            "user.message" => {
                let preview = str_field(data, "content").map(str::to_owned);
                self.message(
                    line,
                    ts,
                    envelope_id,
                    MessageRole::User,
                    MessageKind::Text,
                    None,
                    preview,
                );
            }
            "assistant.message" => self.assistant_message(line, ts, envelope_id, data),
            "assistant.reasoning" => {
                let preview = str_field(data, "content").map(str::to_owned);
                self.message(
                    line,
                    ts,
                    envelope_id,
                    MessageRole::Assistant,
                    MessageKind::Thinking,
                    None,
                    preview,
                );
            }
            "tool.execution_complete" => {
                let Some(call_id) = str_field(data, "toolCallId") else {
                    return;
                };
                let name = self.close_call(call_id);
                let preview = data
                    .get("result")
                    .and_then(Value::as_object)
                    .and_then(|r| str_field(r, "content"))
                    .or_else(|| {
                        data.get("error")
                            .and_then(Value::as_object)
                            .and_then(|e| str_field(e, "message"))
                    })
                    .map(str::to_owned);
                let key = format!("{call_id}:result");
                self.message(
                    line,
                    ts,
                    Some(&key),
                    MessageRole::Tool,
                    MessageKind::ToolResult,
                    name,
                    preview,
                );
            }
            _ => {}
        }
    }

    fn assistant_message(
        &mut self,
        line: &JsonlLine,
        ts: Option<Timestamp>,
        envelope_id: Option<&str>,
        data: &Map<String, Value>,
    ) {
        if let Some(content) = str_field(data, "content") {
            self.message(
                line,
                ts,
                envelope_id,
                MessageRole::Assistant,
                MessageKind::Text,
                None,
                Some(content.to_owned()),
            );
        }
        let requests = data.get("toolRequests").and_then(Value::as_array);
        for req in requests.into_iter().flatten().filter_map(Value::as_object) {
            let Some(call_id) = str_field(req, "toolCallId") else {
                continue;
            };
            let name = str_field(req, "name").map(str::to_owned);
            if let Some(name) = &name {
                self.open_call(call_id, name);
            }
            let preview = req.get("arguments").map(Value::to_string);
            self.message(
                line,
                ts,
                Some(call_id),
                MessageRole::Assistant,
                MessageKind::ToolUse,
                name,
                preview,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn message(
        &mut self,
        line: &JsonlLine,
        ts: Option<Timestamp>,
        key: Option<&str>,
        role: MessageRole,
        kind: MessageKind,
        tool_name: Option<String>,
        preview: Option<String>,
    ) {
        let Some(ts) = ts else {
            return self.warn(line, "message has no valid `timestamp`");
        };
        let key = key
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{}#{}", self.session_id(), line.line_no));
        self.messages.push(MessageRecord {
            session_ext_id: self.session_id(),
            key,
            ts,
            role,
            kind: Some(kind),
            tool_name,
            preview: preview.and_then(|p| non_empty_preview(&p)),
            event_dedup_key: None,
        });
    }

    fn snapshot(&mut self, line: &JsonlLine, ts: Option<Timestamp>, metrics: &Value) {
        let Some(metrics) = metrics.as_object() else {
            return self.warn(line, "`modelMetrics` is not an object");
        };
        let Some(ts) = ts else {
            return self.warn(line, "usage line has no valid `timestamp`");
        };
        for (model, entry) in metrics {
            let Some(usage) = entry.get("usage").filter(|u| !u.is_null()) else {
                self.warn(line, format!("`modelMetrics.{model}` has no `usage`"));
                continue;
            };
            let tokens = match Usage::deserialize(usage) {
                Ok(u) => u.totals(),
                Err(e) => {
                    self.warn(line, format!("invalid `modelMetrics.{model}.usage`: {e}"));
                    continue;
                }
            };
            let prev = self.state.totals.insert(model.clone(), tokens);
            let delta = without_cache(tokens.delta_since(&prev.unwrap_or_default()));
            if delta.is_zero() {
                continue;
            }
            let key = format!("{}:{model}:{}", self.session_id(), tokens.total());
            if !self.emitted.insert(key.clone()) {
                continue;
            }
            let mut event = UsageEvent {
                harness: HarnessId::COPILOT_CLI,
                dedup_key: key,
                session_ext_id: self.session_id(),
                ts,
                provider: known_provider(model).unwrap_or(DEFAULT_PROVIDER).to_owned(),
                model: canonical_model(model),
                input_tokens: 0,
                output_tokens: 0,
                cache_write_5m_tokens: 0,
                cache_write_1h_tokens: 0,
                cache_read_tokens: 0,
                reasoning_tokens: 0,
                reported_cost: None,
                project_path: self.state.cwd.clone(),
                git_branch: self.state.git_branch.clone(),
                raw: serde_json::value::to_raw_value(entry).ok(),
            };
            event.set_tokens(delta);
            self.events.push(event);
        }
    }

    fn open_call(&mut self, id: &str, name: &str) {
        let open = &mut self.state.open_calls;
        if open.len() >= MAX_OPEN_CALLS {
            open.remove(0);
        }
        open.push((id.to_owned(), name.to_owned()));
    }

    fn close_call(&mut self, id: &str) -> Option<String> {
        let open = &mut self.state.open_calls;
        let idx = open.iter().position(|(open_id, _)| open_id == id)?;
        Some(open.remove(idx).1)
    }

    fn finish(mut self, file: FileCursor) -> Result<ReadBatch> {
        self.state.file = file;
        let sessions = match self.session.take() {
            Some(mut s) => {
                s.project_path = self.state.cwd.clone();
                s.git_branch = self.state.git_branch.clone();
                s.harness_version = self.state.version.clone();
                vec![s]
            }
            None => Vec::new(),
        };
        Ok(ReadBatch {
            sessions,
            events: self.events,
            messages: self.messages,
            next_cursor: Cursor::from_state(&self.state)?,
            warnings: self.warnings,
        })
    }
}

pub fn canonical_model(model: &str) -> String {
    if model.starts_with("claude-") {
        model.replace('.', "-")
    } else {
        model.to_owned()
    }
}

fn warning(src: &SourceRef, message: impl Into<String>) -> ParseWarning {
    ParseWarning::new(HarnessId::COPILOT_CLI, src, message)
}

fn str_field<'v>(obj: &'v Map<String, Value>, field: &str) -> Option<&'v str> {
    obj.get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn set_if_present(target: &mut Option<String>, obj: &Map<String, Value>, field: &str) {
    if let Some(v) = str_field(obj, field) {
        *target = Some(v.to_owned());
    }
}

fn dir_session_id(path: &Path) -> String {
    path.parent()
        .and_then(Path::file_name)
        .or_else(|| path.file_stem())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn non_empty_preview(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| truncate_preview(trimmed, PREVIEW_MAX_CHARS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_roots_respect_copilot_home() {
        let home = Some(PathBuf::from("/home/u"));
        assert_eq!(
            default_roots_from(None, home.clone()),
            [PathBuf::from("/home/u/.copilot/session-state")]
        );
        assert_eq!(
            default_roots_from(Some("".into()), home.clone()),
            [PathBuf::from("/home/u/.copilot/session-state")]
        );
        assert_eq!(
            default_roots_from(Some("/cfg/copilot".into()), home),
            [PathBuf::from("/cfg/copilot/session-state")]
        );
        assert!(default_roots_from(None, None).is_empty());
    }

    #[test]
    fn cache_is_subtracted_only_when_input_includes_it() {
        let inclusive = Usage {
            input_tokens: 1000,
            output_tokens: 50,
            cache_read_tokens: 600,
            cache_write_tokens: 100,
            reasoning_tokens: 20,
        };
        assert_eq!(
            without_cache(inclusive.totals()),
            TokenCounts {
                input: 300,
                output: 50,
                cache_write_5m: 100,
                cache_write_1h: 0,
                cache_read: 600,
                reasoning: 20,
            }
        );
        let exclusive = Usage {
            input_tokens: 12,
            cache_read_tokens: 600,
            ..Usage::default()
        };
        assert_eq!(without_cache(exclusive.totals()).input, 12);
    }

    #[test]
    fn claude_versions_use_hyphens() {
        assert_eq!(canonical_model("claude-sonnet-4.5"), "claude-sonnet-4-5");
        assert_eq!(canonical_model("claude-opus-4.1"), "claude-opus-4-1");
        assert_eq!(canonical_model("gpt-5.1"), "gpt-5.1");
    }
}
