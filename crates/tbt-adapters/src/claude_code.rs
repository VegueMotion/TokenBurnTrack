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

const PROVIDER: &str = "anthropic";
const SYNTHETIC_MODEL: &str = "<synthetic>";
const MAX_OPEN_TOOL_USES: usize = 256;

const TITLE_SUMMARY: u8 = 1;
const TITLE_AI: u8 = 2;
const TITLE_CUSTOM: u8 = 3;

#[derive(Debug, Clone, Copy, Default)]
pub struct ClaudeCodeAdapter;

impl ClaudeCodeAdapter {
    pub fn new() -> ClaudeCodeAdapter {
        ClaudeCodeAdapter
    }
}

pub fn default_roots_from(config_dir: Option<OsString>, home: Option<PathBuf>) -> Vec<PathBuf> {
    match config_dir.filter(|d| !d.is_empty()) {
        Some(dir) => vec![PathBuf::from(dir).join("projects")],
        None => home
            .map(|h| h.join(".claude").join("projects"))
            .into_iter()
            .collect(),
    }
}

impl HarnessAdapter for ClaudeCodeAdapter {
    fn id(&self) -> HarnessId {
        HarnessId::CLAUDE_CODE
    }

    fn display_name(&self) -> &str {
        "Claude Code"
    }

    fn default_roots(&self) -> Vec<PathBuf> {
        default_roots_from(std::env::var_os("CLAUDE_CONFIG_DIR"), std::env::home_dir())
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
    last_event_key: Option<String>,
    #[serde(skip_serializing_if = "is_zero")]
    title_rank: u8,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    open_tool_uses: Vec<(String, String)>,
}

fn is_zero(n: &u8) -> bool {
    *n == 0
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Usage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation: Option<CacheCreation>,
    output_tokens_details: Option<OutputDetails>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CacheCreation {
    ephemeral_5m_input_tokens: Option<u64>,
    ephemeral_1h_input_tokens: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct OutputDetails {
    thinking_tokens: Option<u64>,
}

impl Usage {
    fn tokens(&self) -> TokenCounts {
        let cache_write = self.cache_creation_input_tokens.unwrap_or(0);
        let split = self.cache_creation.as_ref();
        let split_5m = split.and_then(|c| c.ephemeral_5m_input_tokens).unwrap_or(0);
        let split_1h = split.and_then(|c| c.ephemeral_1h_input_tokens).unwrap_or(0);
        TokenCounts {
            input: self.input_tokens.unwrap_or(0),
            output: self.output_tokens.unwrap_or(0),
            cache_write_5m: split_5m.max(cache_write.saturating_sub(split_1h)),
            cache_write_1h: split_1h,
            cache_read: self.cache_read_input_tokens.unwrap_or(0),
            reasoning: self
                .output_tokens_details
                .as_ref()
                .and_then(|d| d.thinking_tokens)
                .unwrap_or(0),
        }
    }
}

struct Builder<'a> {
    src: &'a SourceRef,
    state: State,
    subagent_file: bool,
    path_session: (String, Option<String>),
    current: Option<usize>,
    sessions: Vec<SessionUpsert>,
    events: Vec<UsageEvent>,
    messages: Vec<MessageRecord>,
    warnings: Vec<ParseWarning>,
    emitted: HashSet<String>,
}

struct Ctx<'l> {
    line: &'l JsonlLine,
    obj: &'l Map<String, Value>,
    session: usize,
    ts: Option<Timestamp>,
}

impl<'a> Builder<'a> {
    fn new(src: &'a SourceRef, state: State) -> Builder<'a> {
        let (path_session, subagent_file) = path_identity(&src.path);
        Builder {
            src,
            state,
            subagent_file,
            path_session,
            current: None,
            sessions: Vec::new(),
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

        let session = self.session_for(obj);
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
        self.update_session(session, obj, ts);

        let ctx = Ctx {
            line,
            obj,
            session,
            ts,
        };
        match kind {
            "assistant" => self.assistant(&ctx),
            "user" => self.user(&ctx),
            "custom-title" => self.title(session, obj, "customTitle", TITLE_CUSTOM),
            "ai-title" => self.title(session, obj, "aiTitle", TITLE_AI),
            "summary" => self.title(session, obj, "summary", TITLE_SUMMARY),
            "derived-from" => self.derived_from(session, obj),
            _ => {}
        }
    }

    fn session_for(&mut self, obj: &Map<String, Value>) -> usize {
        let sidechain = obj.get("isSidechain").and_then(Value::as_bool) == Some(true);
        let identity = match (str_field(obj, "sessionId"), str_field(obj, "agentId")) {
            (Some(sid), Some(agent)) if sidechain || self.subagent_file => {
                Some((subagent_ext_id(sid, agent), Some(sid.to_owned())))
            }
            (Some(sid), _) if !self.subagent_file => Some((sid.to_owned(), None)),
            _ => None,
        };
        let idx = match identity {
            Some((ext_id, parent)) => self.session_index(ext_id, parent),
            None => match self.current {
                Some(idx) => idx,
                None => {
                    let (ext_id, parent) = self.path_session.clone();
                    self.session_index(ext_id, parent)
                }
            },
        };
        self.current = Some(idx);
        idx
    }

    fn session_index(&mut self, ext_id: String, parent: Option<String>) -> usize {
        if let Some(idx) = self.sessions.iter().position(|s| s.ext_id == ext_id) {
            return idx;
        }
        let mut s = SessionUpsert::new(HarnessId::CLAUDE_CODE, ext_id);
        s.parent_ext_id = parent;
        self.sessions.push(s);
        self.sessions.len() - 1
    }

    fn update_session(&mut self, idx: usize, obj: &Map<String, Value>, ts: Option<Timestamp>) {
        let s = &mut self.sessions[idx];
        if let Some(ts) = ts {
            s.started_at = Some(s.started_at.map_or(ts, |t| t.min(ts)));
            s.ended_at = Some(s.ended_at.map_or(ts, |t| t.max(ts)));
        }
        set_if_present(&mut s.project_path, obj, "cwd");
        set_if_present(&mut s.git_branch, obj, "gitBranch");
        set_if_present(&mut s.harness_version, obj, "version");
    }

    fn title(&mut self, session: usize, obj: &Map<String, Value>, field: &str, rank: u8) {
        let Some(title) = str_field(obj, field) else {
            return;
        };
        if rank >= self.state.title_rank {
            self.state.title_rank = rank;
            self.sessions[session].title = Some(title.to_owned());
        }
    }

    fn derived_from(&mut self, session: usize, obj: &Map<String, Value>) {
        let original = obj
            .get("sessions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_object)
            .filter(|s| str_field(s, "type") == Some("claude_code_session"))
            .filter_map(|s| str_field(s, "id"))
            .find(|id| *id != self.sessions[session].ext_id);
        if let Some(id) = original {
            self.sessions[session].resumed_from_ext_id = Some(id.to_owned());
        }
    }

    fn assistant(&mut self, ctx: &Ctx<'_>) {
        let Some(message) = ctx.obj.get("message").and_then(Value::as_object) else {
            return self.warn(ctx.line, "assistant line has no `message`");
        };
        let event_key = self.event(ctx, message);
        self.content_messages(
            ctx,
            message.get("content"),
            MessageRole::Assistant,
            event_key,
        );
    }

    fn user(&mut self, ctx: &Ctx<'_>) {
        let content = ctx
            .obj
            .get("message")
            .and_then(Value::as_object)
            .and_then(|m| m.get("content"));
        self.content_messages(ctx, content, MessageRole::User, None);
    }

    fn event(&mut self, ctx: &Ctx<'_>, message: &Map<String, Value>) -> Option<String> {
        let usage_value = message.get("usage").filter(|u| !u.is_null())?;
        let model = str_field(message, "model").unwrap_or_default();
        let api_error = ctx.obj.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true);
        if model == SYNTHETIC_MODEL || api_error {
            return None;
        }
        let usage = match Usage::deserialize(usage_value) {
            Ok(u) => u,
            Err(e) => {
                self.warn(ctx.line, format!("invalid `message.usage`: {e}"));
                return None;
            }
        };
        let tokens = usage.tokens();
        if tokens.is_zero() {
            return None;
        }
        let Some(message_id) = str_field(message, "id") else {
            self.warn(ctx.line, "usage line has no `message.id`");
            return None;
        };
        let Some(ts) = ctx.ts else {
            self.warn(ctx.line, "usage line has no valid `timestamp`");
            return None;
        };
        if model.is_empty() {
            self.warn(ctx.line, "usage line has no `message.model`");
            return None;
        }
        let key = match str_field(ctx.obj, "requestId") {
            Some(request_id) => format!("{message_id}:{request_id}"),
            None => message_id.to_owned(),
        };

        let repeat = self.state.last_event_key.as_deref() == Some(key.as_str())
            || !self.emitted.insert(key.clone());
        self.state.last_event_key = Some(key.clone());
        if repeat {
            return Some(key);
        }

        let session = &self.sessions[ctx.session];
        let mut event = UsageEvent {
            harness: HarnessId::CLAUDE_CODE,
            dedup_key: key.clone(),
            session_ext_id: session.ext_id.clone(),
            ts,
            provider: PROVIDER.to_owned(),
            model: model.to_owned(),
            input_tokens: 0,
            output_tokens: 0,
            cache_write_5m_tokens: 0,
            cache_write_1h_tokens: 0,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
            reported_cost: None,
            project_path: session.project_path.clone(),
            git_branch: session.git_branch.clone(),
            raw: serde_json::value::to_raw_value(usage_value).ok(),
        };
        event.set_tokens(tokens);
        self.events.push(event);
        Some(key)
    }

    fn content_messages(
        &mut self,
        ctx: &Ctx<'_>,
        content: Option<&Value>,
        role: MessageRole,
        event_key: Option<String>,
    ) {
        let blocks: Vec<&Value> = match content {
            Some(text @ Value::String(_)) => vec![text],
            Some(Value::Array(blocks)) => blocks.iter().collect(),
            _ => return,
        };
        if blocks.is_empty() {
            return;
        }
        let Some(uuid) = str_field(ctx.obj, "uuid") else {
            return self.warn(ctx.line, "message line has no `uuid`");
        };
        let Some(ts) = ctx.ts else {
            return self.warn(ctx.line, "message line has no valid `timestamp`");
        };
        let session_ext_id = self.sessions[ctx.session].ext_id.clone();
        for (i, block) in blocks.into_iter().enumerate() {
            let (role, kind, tool_name, preview) = self.block(role, block);
            self.messages.push(MessageRecord {
                session_ext_id: session_ext_id.clone(),
                key: format!("{uuid}:{i}"),
                ts,
                role,
                kind,
                tool_name,
                preview: preview.and_then(|p| non_empty_preview(&p)),
                event_dedup_key: event_key.clone(),
            });
        }
    }

    fn block(
        &mut self,
        role: MessageRole,
        block: &Value,
    ) -> (
        MessageRole,
        Option<MessageKind>,
        Option<String>,
        Option<String>,
    ) {
        if let Value::String(text) = block {
            return (role, Some(MessageKind::Text), None, Some(text.clone()));
        }
        let Some(b) = block.as_object() else {
            return (role, None, None, None);
        };
        let text = |field: &str| str_field(b, field).map(str::to_owned);
        match str_field(b, "type").unwrap_or_default() {
            "text" => (role, Some(MessageKind::Text), None, text("text")),
            "thinking" => (role, Some(MessageKind::Thinking), None, text("thinking")),
            "redacted_thinking" => (role, Some(MessageKind::Thinking), None, None),
            "tool_use" | "server_tool_use" => {
                let name = text("name");
                if let (Some(id), Some(name)) = (str_field(b, "id"), &name) {
                    self.open_tool_use(id, name);
                }
                let input = b.get("input").map(Value::to_string);
                (role, Some(MessageKind::ToolUse), name, input)
            }
            "tool_result" => {
                let name = str_field(b, "tool_use_id").and_then(|id| self.close_tool_use(id));
                let preview = b.get("content").map(result_text);
                (
                    MessageRole::Tool,
                    Some(MessageKind::ToolResult),
                    name,
                    preview,
                )
            }
            _ => (role, None, None, None),
        }
    }

    fn open_tool_use(&mut self, id: &str, name: &str) {
        let open = &mut self.state.open_tool_uses;
        if open.len() >= MAX_OPEN_TOOL_USES {
            open.remove(0);
        }
        open.push((id.to_owned(), name.to_owned()));
    }

    fn close_tool_use(&mut self, id: &str) -> Option<String> {
        let open = &mut self.state.open_tool_uses;
        let idx = open.iter().position(|(open_id, _)| open_id == id)?;
        Some(open.remove(idx).1)
    }

    fn finish(mut self, file: FileCursor) -> Result<ReadBatch> {
        self.state.file = file;
        Ok(ReadBatch {
            sessions: self.sessions,
            events: self.events,
            messages: self.messages,
            next_cursor: Cursor::from_state(&self.state)?,
            warnings: self.warnings,
        })
    }
}

fn warning(src: &SourceRef, message: impl Into<String>) -> ParseWarning {
    ParseWarning::new(HarnessId::CLAUDE_CODE, src, message)
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

fn subagent_ext_id(session_id: &str, agent_id: &str) -> String {
    format!("{session_id}/agent-{agent_id}")
}

fn path_identity(path: &Path) -> ((String, Option<String>), bool) {
    let name = |p: Option<&Path>| {
        p.and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
    };
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let subagents_dir = path.parent();
    let session_dir = subagents_dir.and_then(Path::parent);
    if let (Some(agent), Some("subagents"), Some(sid)) = (
        stem.strip_prefix("agent-"),
        name(subagents_dir).as_deref(),
        name(session_dir),
    ) {
        return ((subagent_ext_id(&sid, agent), Some(sid)), true);
    }
    ((stem, None), false)
}

fn result_text(content: &Value) -> String {
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

fn non_empty_preview(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| truncate_preview(trimmed, PREVIEW_MAX_CHARS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_roots_respect_config_dir() {
        let home = Some(PathBuf::from("/home/u"));
        assert_eq!(
            default_roots_from(None, home.clone()),
            [PathBuf::from("/home/u/.claude/projects")]
        );
        assert_eq!(
            default_roots_from(Some("".into()), home.clone()),
            [PathBuf::from("/home/u/.claude/projects")]
        );
        assert_eq!(
            default_roots_from(Some("/cfg/claude".into()), home),
            [PathBuf::from("/cfg/claude/projects")]
        );
        assert!(default_roots_from(None, None).is_empty());
    }

    #[test]
    fn path_identity_of_main_and_subagent_files() {
        assert_eq!(
            path_identity(Path::new("/p/slug/abc.jsonl")),
            (("abc".to_owned(), None), false)
        );
        assert_eq!(
            path_identity(Path::new("/p/slug/abc/subagents/agent-a1.jsonl")),
            (("abc/agent-a1".to_owned(), Some("abc".to_owned())), true)
        );
    }

    #[test]
    fn cache_write_split_and_fallback() {
        let usage = |v: Value| Usage::deserialize(&v).unwrap().tokens();
        let split = usage(serde_json::json!({
            "input_tokens": 3, "output_tokens": 50, "cache_read_input_tokens": 7,
            "cache_creation_input_tokens": 30,
            "cache_creation": {"ephemeral_5m_input_tokens": 10, "ephemeral_1h_input_tokens": 20},
            "output_tokens_details": {"thinking_tokens": 9}
        }));
        assert_eq!(
            split,
            TokenCounts {
                input: 3,
                output: 50,
                cache_write_5m: 10,
                cache_write_1h: 20,
                cache_read: 7,
                reasoning: 9,
            }
        );
        let legacy = usage(serde_json::json!({
            "input_tokens": 1, "output_tokens": 2, "cache_creation_input_tokens": 40,
            "output_tokens_details": null
        }));
        assert_eq!((legacy.cache_write_5m, legacy.cache_write_1h), (40, 0));
        assert_eq!(legacy.reasoning, 0);
    }
}
