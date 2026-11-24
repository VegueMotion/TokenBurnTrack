use std::collections::HashSet;
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

const DEFAULT_PROVIDER: &str = "openai";
const UNKNOWN_MODEL: &str = "unknown";
const MAX_OPEN_CALLS: usize = 256;

#[derive(Debug, Clone, Copy, Default)]
pub struct CodexAdapter;

impl CodexAdapter {
    pub fn new() -> CodexAdapter {
        CodexAdapter
    }
}

pub fn default_roots_from(codex_home: Option<OsString>, home: Option<PathBuf>) -> Vec<PathBuf> {
    match codex_home.filter(|d| !d.is_empty()) {
        Some(dir) => vec![PathBuf::from(dir).join("sessions")],
        None => home
            .map(|h| h.join(".codex").join("sessions"))
            .into_iter()
            .collect(),
    }
}

impl HarnessAdapter for CodexAdapter {
    fn id(&self) -> HarnessId {
        HarnessId::CODEX
    }

    fn display_name(&self) -> &str {
        "Codex CLI"
    }

    fn default_roots(&self) -> Vec<PathBuf> {
        default_roots_from(std::env::var_os("CODEX_HOME"), std::env::home_dir())
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct State {
    #[serde(flatten)]
    file: FileCursor,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    settings_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cli_version: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    records_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_total: Option<TokenCounts>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    open_calls: Vec<(String, String)>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Usage {
    input_tokens: Option<u64>,
    cached_input_tokens: Option<u64>,
    cache_write_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    reasoning_output_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

impl Usage {
    fn tokens(&self) -> TokenCounts {
        let cache_read = self.cached_input_tokens.unwrap_or(0);
        let cache_write = self.cache_write_input_tokens.unwrap_or(0);
        TokenCounts {
            input: self
                .input_tokens
                .unwrap_or(0)
                .saturating_sub(cache_read + cache_write),
            output: self.output_tokens.unwrap_or(0),
            cache_write_5m: cache_write,
            cache_write_1h: 0,
            cache_read,
            reasoning: self.reasoning_output_tokens.unwrap_or(0),
        }
    }

    fn running_total(&self) -> u64 {
        self.total_tokens.unwrap_or_else(|| self.tokens().total())
    }
}

struct Builder<'a> {
    src: &'a SourceRef,
    state: State,
    path_thread: String,
    session: Option<SessionUpsert>,
    events: Vec<UsageEvent>,
    messages: Vec<MessageRecord>,
    warnings: Vec<ParseWarning>,
    emitted: HashSet<String>,
    unlinked: Vec<usize>,
}

struct Ctx<'l> {
    line: &'l JsonlLine,
    raw_ts: Option<&'l str>,
    ts: Option<Timestamp>,
}

impl<'a> Builder<'a> {
    fn new(src: &'a SourceRef, state: State) -> Builder<'a> {
        Builder {
            src,
            state,
            path_thread: path_thread_id(&src.path),
            session: None,
            events: Vec::new(),
            messages: Vec::new(),
            warnings: Vec::new(),
            emitted: HashSet::new(),
            unlinked: Vec::new(),
        }
    }

    fn warn(&mut self, line: &JsonlLine, message: impl Into<String>) {
        self.warnings
            .push(warning(self.src, message).at(line.line_no, line.offset));
    }

    fn thread_id(&self) -> String {
        self.state
            .thread_id
            .clone()
            .unwrap_or_else(|| self.path_thread.clone())
    }

    fn session(&mut self) -> &mut SessionUpsert {
        let ext_id = self.thread_id();
        let s = self
            .session
            .get_or_insert_with(|| SessionUpsert::new(HarnessId::CODEX, ext_id.clone()));
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
        let raw_ts = str_field(obj, "timestamp");
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
        let payload = obj.get("payload").and_then(Value::as_object);

        if kind == "session_meta" {
            if let Some(p) = payload {
                self.session_meta(p);
            } else {
                self.warn(line, "session_meta has no `payload`");
            }
        }
        let s = self.session();
        if let Some(ts) = ts {
            s.started_at = Some(s.started_at.map_or(ts, |t| t.min(ts)));
            s.ended_at = Some(s.ended_at.map_or(ts, |t| t.max(ts)));
        }

        let Some(payload) = payload else {
            return;
        };
        let ctx = Ctx { line, raw_ts, ts };
        match kind {
            "turn_context" => self.turn_context(payload),
            "token_usage_record" => self.usage_record(&ctx, payload),
            "response_item" => self.response_item(&ctx, payload),
            "event_msg" => match str_field(payload, "type") {
                Some("token_count") => self.token_count(&ctx, payload),
                Some("thread_settings_applied") => self.thread_settings(payload),
                _ => {}
            },
            _ => {}
        }
    }

    fn session_meta(&mut self, p: &Map<String, Value>) {
        if let Some(id) = str_field(p, "id") {
            self.state.thread_id = Some(id.to_owned());
        }
        set_if_present(&mut self.state.cwd, p, "cwd");
        set_if_present(&mut self.state.cli_version, p, "cli_version");
        set_if_present(&mut self.state.provider, p, "model_provider");
        set_if_present(&mut self.state.model, p, "model");
        if let Some(git) = p.get("git").and_then(Value::as_object) {
            set_if_present(&mut self.state.git_branch, git, "branch");
        }
        if str_field(p, "history_mode") == Some("paginated") || p.contains_key("history_base") {
            self.state.records_only = true;
        }
        let own = self.state.thread_id.clone();
        let base = p
            .get("history_base")
            .and_then(Value::as_object)
            .and_then(|b| str_field(b, "thread_id"));
        let original = str_field(p, "forked_from_id")
            .or(base)
            .filter(|id| Some(*id) != own.as_deref())
            .map(str::to_owned);
        if original.is_some() {
            self.session().resumed_from_ext_id = original;
        }
    }

    fn turn_context(&mut self, p: &Map<String, Value>) {
        set_if_present(&mut self.state.model, p, "model");
        set_if_present(&mut self.state.cwd, p, "cwd");
    }

    fn thread_settings(&mut self, p: &Map<String, Value>) {
        let Some(settings) = p.get("thread_settings").and_then(Value::as_object) else {
            return;
        };
        set_if_present(&mut self.state.settings_model, settings, "model");
        if self.state.provider.is_none() {
            set_if_present(&mut self.state.provider, settings, "model_provider_id");
        }
    }

    fn usage_record(&mut self, ctx: &Ctx<'_>, p: &Map<String, Value>) {
        self.state.records_only = true;
        let Some(usage) = self.usage(ctx, p, "usage") else {
            if !p.contains_key("usage") {
                self.warn(ctx.line, "token_usage_record has no `usage`");
            }
            return;
        };
        let key = match str_field(p, "response_id") {
            Some(id) => id.to_owned(),
            None => match self.usage(ctx, p, "thread_token_usage") {
                Some(thread) => self.counter_key(&thread),
                None => {
                    return self.warn(
                        ctx.line,
                        "token_usage_record has neither `response_id` nor `thread_token_usage`",
                    );
                }
            },
        };
        let raw = serde_json::value::to_raw_value(p).ok();
        self.emit(ctx, key, usage.tokens(), raw);
    }

    fn token_count(&mut self, ctx: &Ctx<'_>, p: &Map<String, Value>) {
        if self.state.records_only {
            return;
        }
        let Some(info) = p.get("info").and_then(Value::as_object) else {
            return;
        };
        let Some(total) = self.usage(ctx, info, "total_token_usage") else {
            if !info.contains_key("total_token_usage") {
                self.warn(ctx.line, "token_count has no `info.total_token_usage`");
            }
            return;
        };
        let total_tokens = total.tokens();
        let prev = self.state.last_total.replace(total_tokens);
        if prev == Some(total_tokens) {
            return;
        }
        let tokens = match self.usage(ctx, info, "last_token_usage") {
            Some(last) => last.tokens(),
            None => total_tokens.delta_since(&prev.unwrap_or_default()),
        };
        let key = self.counter_key(&total);
        let raw = serde_json::value::to_raw_value(info).ok();
        self.emit(ctx, key, tokens, raw);
    }

    fn usage(&mut self, ctx: &Ctx<'_>, obj: &Map<String, Value>, field: &str) -> Option<Usage> {
        let value = obj.get(field).filter(|v| !v.is_null())?;
        match Usage::deserialize(value) {
            Ok(u) => Some(u),
            Err(e) => {
                self.warn(ctx.line, format!("invalid `{field}`: {e}"));
                None
            }
        }
    }

    fn counter_key(&self, total: &Usage) -> String {
        format!("{}:{}", self.thread_id(), total.running_total())
    }

    fn emit(
        &mut self,
        ctx: &Ctx<'_>,
        key: String,
        tokens: TokenCounts,
        raw: Option<Box<tbt_core::RawValue>>,
    ) {
        if tokens.is_zero() {
            return;
        }
        let Some(ts) = ctx.ts else {
            return self.warn(ctx.line, "usage line has no valid `timestamp`");
        };
        if !self.emitted.insert(key.clone()) {
            return;
        }
        let model = match self
            .state
            .model
            .clone()
            .or_else(|| self.state.settings_model.clone())
        {
            Some(m) => m,
            None => {
                self.warn(ctx.line, "no model known for usage line");
                UNKNOWN_MODEL.to_owned()
            }
        };
        let mut event = UsageEvent {
            harness: HarnessId::CODEX,
            dedup_key: key.clone(),
            session_ext_id: self.thread_id(),
            ts,
            provider: self
                .state
                .provider
                .clone()
                .unwrap_or_else(|| DEFAULT_PROVIDER.to_owned()),
            model,
            input_tokens: 0,
            output_tokens: 0,
            cache_write_5m_tokens: 0,
            cache_write_1h_tokens: 0,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
            reported_cost: None,
            project_path: self.state.cwd.clone(),
            git_branch: self.state.git_branch.clone(),
            raw,
        };
        event.set_tokens(tokens);
        self.events.push(event);
        for idx in self.unlinked.drain(..) {
            self.messages[idx].event_dedup_key = Some(key.clone());
        }
    }

    fn response_item(&mut self, ctx: &Ctx<'_>, p: &Map<String, Value>) {
        let text = |field: &str| str_field(p, field).map(str::to_owned);
        let call_id = str_field(p, "call_id");
        let (role, kind, tool_name, preview, fallback_key) =
            match str_field(p, "type").unwrap_or_default() {
                "message" => {
                    let role = match str_field(p, "role") {
                        Some("assistant") => MessageRole::Assistant,
                        Some("user") => MessageRole::User,
                        Some("developer" | "system") => MessageRole::System,
                        _ => return,
                    };
                    let preview = p.get("content").map(content_text);
                    (role, Some(MessageKind::Text), None, preview, None)
                }
                "reasoning" => {
                    let summary = p.get("summary").map(content_text).filter(|s| !s.is_empty());
                    let preview = summary.or_else(|| p.get("content").map(content_text));
                    (
                        MessageRole::Assistant,
                        Some(MessageKind::Thinking),
                        None,
                        preview,
                        None,
                    )
                }
                kind @ ("function_call" | "custom_tool_call" | "local_shell_call") => {
                    let name = match kind {
                        "local_shell_call" => Some("local_shell".to_owned()),
                        _ => text("name"),
                    };
                    if let (Some(id), Some(name)) = (call_id, &name) {
                        self.open_call(id, name);
                    }
                    let preview = text("arguments")
                        .or_else(|| text("input"))
                        .or_else(|| p.get("action").map(Value::to_string));
                    (
                        MessageRole::Assistant,
                        Some(MessageKind::ToolUse),
                        name,
                        preview,
                        call_id.map(str::to_owned),
                    )
                }
                "function_call_output" | "custom_tool_call_output" => {
                    let name = call_id.and_then(|id| self.close_call(id));
                    let preview = p.get("output").map(output_text);
                    (
                        MessageRole::Tool,
                        Some(MessageKind::ToolResult),
                        name,
                        preview,
                        call_id.map(|id| format!("{id}:output")),
                    )
                }
                "web_search_call" => {
                    let action = p.get("action").and_then(Value::as_object);
                    let preview = action
                        .and_then(|a| str_field(a, "query"))
                        .map(str::to_owned)
                        .or_else(|| p.get("action").map(Value::to_string));
                    (
                        MessageRole::Assistant,
                        Some(MessageKind::ToolUse),
                        Some("web_search".to_owned()),
                        preview,
                        None,
                    )
                }
                _ => return,
            };

        let Some(ts) = ctx.ts else {
            return self.warn(ctx.line, "response_item has no valid `timestamp`");
        };
        let key = str_field(p, "id")
            .map(str::to_owned)
            .or(fallback_key)
            .unwrap_or_else(|| format!("{}#{}", ctx.raw_ts.unwrap_or_default(), ctx.line.line_no));
        if role == MessageRole::User {
            self.unlinked.clear();
        }
        if role == MessageRole::Assistant {
            self.unlinked.push(self.messages.len());
        }
        self.messages.push(MessageRecord {
            session_ext_id: self.thread_id(),
            key,
            ts,
            role,
            kind,
            tool_name,
            preview: preview.and_then(|p| non_empty_preview(&p)),
            event_dedup_key: None,
        });
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
                s.harness_version = self.state.cli_version.clone();
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

fn warning(src: &SourceRef, message: impl Into<String>) -> ParseWarning {
    ParseWarning::new(HarnessId::CODEX, src, message)
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

fn path_thread_id(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let thread = stem
        .strip_prefix("rollout-")
        .and_then(|rest| rest.get("YYYY-MM-DDThh-mm-ss-".len()..))
        .map(|rest| rest.split('_').next().unwrap_or(rest))
        .filter(|id| !id.is_empty());
    thread.map(str::to_owned).unwrap_or(stem)
}

fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| match p {
                Value::String(s) => Some(s.as_str()),
                Value::Object(o) => str_field(o, "text"),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn output_text(output: &Value) -> String {
    match output {
        Value::Object(o) => o.get("content").map(content_text).unwrap_or_default(),
        other => content_text(other),
    }
}

fn non_empty_preview(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| truncate_preview(trimmed, PREVIEW_MAX_CHARS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_roots_respect_codex_home() {
        let home = Some(PathBuf::from("/home/u"));
        assert_eq!(
            default_roots_from(None, home.clone()),
            [PathBuf::from("/home/u/.codex/sessions")]
        );
        assert_eq!(
            default_roots_from(Some("".into()), home.clone()),
            [PathBuf::from("/home/u/.codex/sessions")]
        );
        assert_eq!(
            default_roots_from(Some("/cfg/codex".into()), home),
            [PathBuf::from("/cfg/codex/sessions")]
        );
        assert!(default_roots_from(None, None).is_empty());
    }

    #[test]
    fn thread_id_from_file_name() {
        assert_eq!(
            path_thread_id(Path::new(
                "/s/rollout-2026-05-31T19-42-32-019e7f21-575c-7721-8bee-e12a2dcde581.jsonl"
            )),
            "019e7f21-575c-7721-8bee-e12a2dcde581"
        );
        assert_eq!(
            path_thread_id(Path::new(
                "/s/rollout-2026-09-18T15-31-24-01a0b44c-76aa-76b8-86dd-e11f3ffd813c_01a0b4b6-f515-7c6f-8a5f-1faf9245c651.jsonl"
            )),
            "01a0b44c-76aa-76b8-86dd-e11f3ffd813c"
        );
        assert_eq!(path_thread_id(Path::new("/s/other.jsonl")), "other");
    }

    #[test]
    fn cached_and_cache_write_are_subtracted_from_input() {
        let usage: Usage = serde_json::from_value(serde_json::json!({
            "input_tokens": 1000, "cached_input_tokens": 600, "cache_write_input_tokens": 100,
            "output_tokens": 50, "reasoning_output_tokens": 20, "total_tokens": 1050
        }))
        .unwrap();
        assert_eq!(
            usage.tokens(),
            TokenCounts {
                input: 300,
                output: 50,
                cache_write_5m: 100,
                cache_write_1h: 0,
                cache_read: 600,
                reasoning: 20,
            }
        );
        assert_eq!(usage.tokens().total(), 1050);
        let legacy: Usage = serde_json::from_value(serde_json::json!({
            "input_tokens": 10, "cached_input_tokens": null, "output_tokens": 2
        }))
        .unwrap();
        assert_eq!(legacy.tokens().input, 10);
        assert_eq!(legacy.running_total(), 12);
    }
}
