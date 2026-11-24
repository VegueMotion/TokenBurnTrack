use std::collections::HashSet;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tbt_core::jsonl::{FileCursor, JsonlLine, JsonlReader, ReadStart};
use tbt_core::{
    Cursor, Error, HarnessAdapter, HarnessId, MessageKind, MessageRecord, MessageRole,
    PREVIEW_MAX_CHARS, ParseWarning, RawValue, ReadBatch, Result, SessionUpsert, SourceKind,
    SourceRef, Timestamp, TokenCounts, UsageEvent, truncate_preview,
};

use crate::discover;

const PROVIDER: &str = "google";
const SESSION_FILE_PREFIX: &str = "session-";
const PROJECT_ROOT_FILE: &str = ".project_root";
const MAX_RECENT_EVENTS: usize = 64;

pub const DEFAULT_FINALIZE_GRACE: Duration = Duration::from_secs(10 * 60);

type Clock = Arc<dyn Fn() -> Timestamp + Send + Sync>;

#[derive(Clone)]
pub struct GeminiCliAdapter {
    grace: Duration,
    clock: Clock,
}

impl fmt::Debug for GeminiCliAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GeminiCliAdapter")
            .field("grace", &self.grace)
            .finish_non_exhaustive()
    }
}

impl Default for GeminiCliAdapter {
    fn default() -> GeminiCliAdapter {
        GeminiCliAdapter::new()
    }
}

impl GeminiCliAdapter {
    pub fn new() -> GeminiCliAdapter {
        GeminiCliAdapter::with_clock(DEFAULT_FINALIZE_GRACE, Timestamp::now)
    }

    pub fn with_clock(
        grace: Duration,
        clock: impl Fn() -> Timestamp + Send + Sync + 'static,
    ) -> GeminiCliAdapter {
        GeminiCliAdapter {
            grace,
            clock: Arc::new(clock),
        }
    }

    fn settled(&self, mtime: Option<Timestamp>) -> bool {
        let Some(mtime) = mtime else {
            return false;
        };
        let grace = i64::try_from(self.grace.as_millis()).unwrap_or(i64::MAX);
        let idle = (self.clock)()
            .as_millisecond()
            .saturating_sub(mtime.as_millisecond());
        idle >= grace
    }
}

pub fn default_roots_from(gemini_home: Option<OsString>, home: Option<PathBuf>) -> Vec<PathBuf> {
    let Some(base) = gemini_home
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or(home)
    else {
        return Vec::new();
    };
    let mut roots = vec![base.join(".gemini").join("tmp")];
    if cfg!(target_os = "macos") {
        roots.push(base.join(".cache").join(".gemini").join("tmp"));
    }
    roots
}

impl HarnessAdapter for GeminiCliAdapter {
    fn id(&self) -> HarnessId {
        HarnessId::GEMINI_CLI
    }

    fn display_name(&self) -> &str {
        "Gemini CLI"
    }

    fn default_roots(&self) -> Vec<PathBuf> {
        default_roots_from(std::env::var_os("GEMINI_CLI_HOME"), std::env::home_dir())
    }

    fn discover(&self, roots: &[PathBuf]) -> Result<Vec<SourceRef>> {
        let explicit = |p: &PathBuf| roots.contains(p);
        let mut out: Vec<SourceRef> = discover::files_with_extension(roots, "jsonl")
            .into_iter()
            .filter(|p| explicit(p) || chats_depth(p).is_some())
            .map(|p| SourceRef::new(p, SourceKind::JsonlFile))
            .collect();
        out.extend(
            discover::files_with_extension(roots, "json")
                .into_iter()
                .filter(|p| {
                    explicit(p)
                        || (chats_depth(p) == Some(1)
                            && file_name(p).starts_with(SESSION_FILE_PREFIX))
                })
                .map(|p| SourceRef::new(p, SourceKind::JsonFile)),
        );
        out.sort_by(|a, b| a.path.as_os_str().cmp(b.path.as_os_str()));
        Ok(out)
    }

    fn read(&self, src: &SourceRef, cursor: &Cursor) -> Result<ReadBatch> {
        match src.kind {
            SourceKind::JsonFile => self.read_json(src, cursor),
            _ => self.read_jsonl(src, cursor),
        }
    }
}

impl GeminiCliAdapter {
    fn read_jsonl(&self, src: &SourceRef, cursor: &Cursor) -> Result<ReadBatch> {
        let mut warnings = Vec::new();
        let mut state = match cursor.state::<JsonlState>() {
            Ok(state) => state.unwrap_or_default(),
            Err(e) => {
                warnings.push(warning(src, format!("{e}; reading from the start")));
                JsonlState::default()
            }
        };

        let mut reader = JsonlReader::open(&src.path, &state.file)?;
        match reader.start() {
            ReadStart::Unchanged => {
                if state.pending.is_none() || !self.settled(state.file.mtime) {
                    return Ok(ReadBatch::unchanged(cursor.clone()));
                }
            }
            ReadStart::Continue => {}
            ReadStart::Fresh | ReadStart::Restarted(_) => state = JsonlState::default(),
        }

        let mut out = Emitter::new(src, state.session_id.clone(), state.recent.clone());
        out.warnings = warnings;
        for line in reader.by_ref() {
            match line {
                Ok(line) => out.line(&mut state, &line),
                Err(e) => {
                    out.warnings.push(warning(src, e.to_string()));
                    break;
                }
            }
        }
        state.file = reader.into_cursor();
        if self.settled(state.file.mtime)
            && let Some(msg) = state.pending.take()
        {
            out.flush(msg);
        }
        state.session_id = out.explicit_session.then(|| out.session_id.clone());
        state.recent = std::mem::take(&mut out.recent);
        let next_cursor = Cursor::from_state(&state)?;
        Ok(out.finish(next_cursor))
    }

    fn read_json(&self, src: &SourceRef, cursor: &Cursor) -> Result<ReadBatch> {
        let mut warnings = Vec::new();
        let state = match cursor.state::<JsonState>() {
            Ok(state) => state.unwrap_or_default(),
            Err(e) => {
                warnings.push(warning(src, format!("{e}; reading from the start")));
                JsonState::default()
            }
        };

        let meta = std::fs::metadata(&src.path).map_err(|e| Error::io(&src.path, e))?;
        let size = meta.len();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| Timestamp::try_from(t).ok());
        let settled = self.settled(mtime);
        let unchanged = state.size == size && state.mtime.is_some() && state.mtime == mtime;
        if unchanged && !(state.pending && settled) && warnings.is_empty() {
            return Ok(ReadBatch::unchanged(cursor.clone()));
        }

        let bytes = std::fs::read(&src.path).map_err(|e| Error::io(&src.path, e))?;
        let mut next = JsonState {
            size,
            mtime,
            ..state.clone()
        };
        let mut out = Emitter::new(src, None, Vec::new());
        out.warnings = warnings;

        let record = match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(obj)) => obj,
            Ok(_) => {
                out.warnings
                    .push(warning(src, "session file is not a JSON object"));
                return Ok(out.finish(Cursor::from_state(&next)?));
            }
            Err(e) => {
                out.warnings
                    .push(warning(src, format!("invalid JSON: {e}")));
                return Ok(out.finish(Cursor::from_state(&next)?));
            }
        };

        out.metadata(&record);
        let messages: &[Value] = match record.get("messages") {
            Some(Value::Array(messages)) => messages,
            _ => {
                out.warnings
                    .push(warning(src, "session file has no `messages` array"));
                &[]
            }
        };

        let resumes_prefix = state.done > 0
            && messages
                .get(state.done - 1)
                .and_then(Value::as_object)
                .and_then(|m| str_field(m, "id"))
                == state.done_last_id.as_deref();
        let start = if resumes_prefix { state.done } else { 0 };

        let mut pending: Option<(usize, Msg)> = None;
        for (i, value) in messages.iter().enumerate().skip(start) {
            let parsed = match value.as_object() {
                Some(obj) => parse_message(obj),
                None => Err("message is not a JSON object".to_owned()),
            };
            match parsed {
                Ok(msg) => {
                    if let Some((_, prev)) = pending.take() {
                        out.flush(prev);
                    }
                    pending = Some((i, msg));
                }
                Err(e) => out
                    .warnings
                    .push(warning(src, format!("messages[{i}]: {e}"))),
            }
        }
        let mut done = messages.len();
        next.pending = false;
        if let Some((i, msg)) = pending {
            if settled {
                out.flush(msg);
            } else {
                next.pending = true;
                done = i;
            }
        }
        let last_id = done
            .checked_sub(1)
            .and_then(|i| messages.get(i))
            .and_then(Value::as_object)
            .and_then(|m| str_field(m, "id"))
            .map(str::to_owned);
        next.done = done;
        next.done_last_id = last_id;
        Ok(out.finish(Cursor::from_state(&next)?))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct JsonlState {
    #[serde(flatten)]
    file: FileCursor,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pending: Option<Msg>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    recent: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct JsonState {
    size: u64,
    mtime: Option<Timestamp>,
    done: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    done_last_id: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pending: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Msg {
    id: String,
    ts: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Usage {
    model: String,
    tokens: TokenCounts,
    raw: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Part {
    key: String,
    role: MessageRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<MessageKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool: Option<String>,
    #[serde(skip)]
    preview: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Tokens {
    input: Option<u64>,
    output: Option<u64>,
    cached: Option<u64>,
    thoughts: Option<u64>,
    tool: Option<u64>,
}

impl Tokens {
    fn counts(&self) -> TokenCounts {
        let prompt = self.input.unwrap_or(0);
        let cached = self.cached.unwrap_or(0).min(prompt);
        let thoughts = self.thoughts.unwrap_or(0);
        TokenCounts {
            input: prompt - cached + self.tool.unwrap_or(0),
            output: self.output.unwrap_or(0) + thoughts,
            cache_write_5m: 0,
            cache_write_1h: 0,
            cache_read: cached,
            reasoning: thoughts,
        }
    }
}

struct Emitter<'a> {
    src: &'a SourceRef,
    session_id: String,
    explicit_session: bool,
    session: SessionUpsert,
    touched: bool,
    recent: Vec<String>,
    events: Vec<UsageEvent>,
    messages: Vec<MessageRecord>,
    warnings: Vec<ParseWarning>,
    event_keys: HashSet<String>,
    message_keys: HashSet<String>,
}

impl<'a> Emitter<'a> {
    fn new(src: &'a SourceRef, session_id: Option<String>, recent: Vec<String>) -> Emitter<'a> {
        let identity = path_identity(&src.path);
        let explicit_session = session_id.is_some();
        let session_id = session_id.unwrap_or(identity.fallback_id);
        let mut session = SessionUpsert::new(HarnessId::GEMINI_CLI, session_id.clone());
        session.parent_ext_id = identity.parent;
        session.project_path = identity.project_dir.as_deref().and_then(project_root);
        Emitter {
            src,
            session_id,
            explicit_session,
            session,
            touched: false,
            recent,
            events: Vec::new(),
            messages: Vec::new(),
            warnings: Vec::new(),
            event_keys: HashSet::new(),
            message_keys: HashSet::new(),
        }
    }

    fn warn(&mut self, line: &JsonlLine, message: impl Into<String>) {
        self.warnings
            .push(warning(self.src, message).at(line.line_no, line.offset));
    }

    fn line(&mut self, state: &mut JsonlState, line: &JsonlLine) {
        self.touched = true;
        let value: Value = match serde_json::from_str(&line.text) {
            Ok(v) => v,
            Err(e) => return self.warn(line, format!("invalid JSON: {e}")),
        };
        let Some(obj) = value.as_object() else {
            return self.warn(line, "line is not a JSON object");
        };

        if let Some(target) = obj.get("$rewindTo") {
            if !target.is_string() {
                return self.warn(line, "`$rewindTo` is not a string");
            }
            if let Some(msg) = state.pending.take() {
                self.flush(msg);
            }
        } else if let Some(patch) = obj.get("$patch") {
            match patch.as_object() {
                Some(patch) => self.patch(state, patch),
                None => self.warn(line, "`$patch` is not an object"),
            }
        } else if obj.get("id").is_some_and(Value::is_string) {
            match parse_message(obj) {
                Ok(msg) => self.push(state, msg),
                Err(e) => self.warn(line, e),
            }
        } else if let Some(set) = obj.get("$set") {
            let Some(set) = set.as_object() else {
                return self.warn(line, "`$set` is not an object");
            };
            self.metadata(set);
            self.embedded_messages(state, line, set);
        } else if str_field(obj, "sessionId").is_some() && str_field(obj, "projectHash").is_some() {
            self.metadata(obj);
            self.embedded_messages(state, line, obj);
        }
    }

    fn embedded_messages(
        &mut self,
        state: &mut JsonlState,
        line: &JsonlLine,
        obj: &Map<String, Value>,
    ) {
        let Some(messages) = obj.get("messages").and_then(Value::as_array) else {
            return;
        };
        for (i, m) in messages.iter().enumerate() {
            let parsed = match m.as_object() {
                Some(m) => parse_message(m),
                None => Err("message is not a JSON object".to_owned()),
            };
            match parsed {
                Ok(msg) => self.push(state, msg),
                Err(e) => self.warn(line, format!("messages[{i}]: {e}")),
            }
        }
    }

    fn push(&mut self, state: &mut JsonlState, msg: Msg) {
        match state.pending.take() {
            Some(prev) if prev.id != msg.id => self.flush(prev),
            _ => {}
        }
        state.pending = Some(msg);
    }

    fn patch(&mut self, state: &mut JsonlState, patch: &Map<String, Value>) {
        let Some(pending) = state.pending.as_mut() else {
            return;
        };
        let updates = patch
            .get("updates")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_object);
        for update in std::iter::once(patch).chain(updates) {
            if str_field(update, "id") == Some(pending.id.as_str()) {
                apply_patch(pending, update);
            }
        }
        let removed = patch
            .get("removeIds")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|id| id.as_str() == Some(pending.id.as_str()));
        if removed && let Some(msg) = state.pending.take() {
            self.flush(msg);
        }
    }

    fn metadata(&mut self, obj: &Map<String, Value>) {
        self.touched = true;
        if let Some(id) = str_field(obj, "sessionId")
            && !self.explicit_session
        {
            self.explicit_session = true;
            self.session_id = id.to_owned();
            self.session.ext_id = id.to_owned();
        }
        if let Some(summary) = str_field(obj, "summary") {
            self.session.title = Some(summary.to_owned());
        }
        if let Some(ts) = timestamp_field(obj, "startTime") {
            self.observe(ts);
        }
        if let Some(ts) = timestamp_field(obj, "lastUpdated") {
            self.observe(ts);
        }
    }

    fn observe(&mut self, ts: Timestamp) {
        let s = &mut self.session;
        s.started_at = Some(s.started_at.map_or(ts, |t| t.min(ts)));
        s.ended_at = Some(s.ended_at.map_or(ts, |t| t.max(ts)));
    }

    fn flush(&mut self, msg: Msg) {
        self.touched = true;
        self.observe(msg.ts);
        let event_key = msg.usage.as_ref().map(|_| msg.id.clone());
        if let Some(usage) = &msg.usage {
            let fresh = !self.recent.contains(&msg.id) && self.event_keys.insert(msg.id.clone());
            if fresh {
                self.push_event(&msg, usage);
            }
        }
        for part in msg.parts {
            let key = format!("{}:{}", msg.id, part.key);
            if !self.message_keys.insert(key.clone()) {
                continue;
            }
            let linked = part.role == MessageRole::Assistant;
            self.messages.push(MessageRecord {
                session_ext_id: self.session_id.clone(),
                key,
                ts: msg.ts,
                role: part.role,
                kind: part.kind,
                tool_name: part.tool,
                preview: part.preview,
                event_dedup_key: event_key.clone().filter(|_| linked),
            });
        }
    }

    fn push_event(&mut self, msg: &Msg, usage: &Usage) {
        if self.recent.len() >= MAX_RECENT_EVENTS {
            self.recent.remove(0);
        }
        self.recent.push(msg.id.clone());
        let mut event = UsageEvent {
            harness: HarnessId::GEMINI_CLI,
            dedup_key: msg.id.clone(),
            session_ext_id: self.session_id.clone(),
            ts: msg.ts,
            provider: PROVIDER.to_owned(),
            model: usage.model.clone(),
            input_tokens: 0,
            output_tokens: 0,
            cache_write_5m_tokens: 0,
            cache_write_1h_tokens: 0,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
            reported_cost: None,
            project_path: self.session.project_path.clone(),
            git_branch: None,
            raw: RawValue::from_string(usage.raw.clone()).ok(),
        };
        event.set_tokens(usage.tokens);
        self.events.push(event);
    }

    fn finish(self, next_cursor: Cursor) -> ReadBatch {
        let sessions = if self.touched {
            vec![self.session]
        } else {
            Vec::new()
        };
        ReadBatch {
            sessions,
            events: self.events,
            messages: self.messages,
            next_cursor,
            warnings: self.warnings,
        }
    }
}

fn parse_message(obj: &Map<String, Value>) -> std::result::Result<Msg, String> {
    let id = str_field(obj, "id").ok_or("message has no `id`")?;
    let ts = timestamp_field(obj, "timestamp")
        .ok_or_else(|| format!("message {id} has no valid `timestamp`"))?;
    let usage = match obj.get("tokens") {
        None | Some(Value::Null) => None,
        Some(raw) => {
            let tokens = Tokens::deserialize(raw)
                .map_err(|e| format!("message {id} has invalid `tokens`: {e}"))?
                .counts();
            match str_field(obj, "model") {
                _ if tokens.is_zero() => None,
                Some(model) => Some(Usage {
                    model: model.to_owned(),
                    tokens,
                    raw: raw.to_string(),
                }),
                None => return Err(format!("message {id} has `tokens` but no `model`")),
            }
        }
    };

    let kind = str_field(obj, "type").unwrap_or_default();
    let role = match kind {
        "user" => MessageRole::User,
        "gemini" => MessageRole::Assistant,
        _ => MessageRole::System,
    };
    let mut parts = Vec::new();
    let text = obj
        .get("displayContent")
        .filter(|c| !c.is_null())
        .or_else(|| obj.get("content"))
        .map(content_text)
        .unwrap_or_default();
    if role != MessageRole::Assistant || !text.trim().is_empty() {
        parts.push(text_part(role, &text));
    }
    for (i, thought) in obj
        .get("thoughts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let preview = thought.as_object().map(|t| {
            [str_field(t, "subject"), str_field(t, "description")]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(": ")
        });
        parts.push(Part {
            key: format!("thought:{i}"),
            role: MessageRole::Assistant,
            kind: Some(MessageKind::Thinking),
            tool: None,
            preview: preview.and_then(|p| non_empty_preview(&p)),
        });
    }
    for call in obj
        .get("toolCalls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
    {
        let Some(call_id) = str_field(call, "id") else {
            continue;
        };
        let name = str_field(call, "name").map(str::to_owned);
        parts.push(Part {
            key: format!("tool:{call_id}"),
            role: MessageRole::Assistant,
            kind: Some(MessageKind::ToolUse),
            tool: name.clone(),
            preview: call
                .get("args")
                .and_then(|a| non_empty_preview(&a.to_string())),
        });
        if let Some(result) = call.get("result").filter(|r| has_content(r)) {
            parts.push(result_part(call_id, name, result));
        }
    }
    Ok(Msg {
        id: id.to_owned(),
        ts,
        usage,
        parts,
    })
}

fn apply_patch(msg: &mut Msg, patch: &Map<String, Value>) {
    if let Some(content) = patch.get("content") {
        let role = msg
            .parts
            .iter()
            .find(|p| p.key == "text")
            .map_or(MessageRole::Assistant, |p| p.role);
        let part = text_part(role, &content_text(content));
        match msg.parts.iter_mut().find(|p| p.key == "text") {
            Some(existing) => *existing = part,
            None => msg.parts.insert(0, part),
        }
    }
    for call in patch
        .get("toolCalls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
    {
        let (Some(call_id), Some(result)) = (str_field(call, "id"), call.get("result")) else {
            continue;
        };
        let tool_key = format!("tool:{call_id}");
        let Some(name) = msg
            .parts
            .iter()
            .find(|p| p.key == tool_key)
            .map(|p| p.tool.clone())
        else {
            continue;
        };
        let part = result_part(call_id, name, result);
        msg.parts.retain(|p| p.key != part.key);
        if has_content(result) {
            msg.parts.push(part);
        }
    }
}

fn text_part(role: MessageRole, text: &str) -> Part {
    Part {
        key: "text".to_owned(),
        role,
        kind: Some(MessageKind::Text),
        tool: None,
        preview: non_empty_preview(text),
    }
}

fn result_part(call_id: &str, name: Option<String>, result: &Value) -> Part {
    Part {
        key: format!("result:{call_id}"),
        role: MessageRole::Tool,
        kind: Some(MessageKind::ToolResult),
        tool: name,
        preview: non_empty_preview(&result_text(result)),
    }
}

fn has_content(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Array(a) => !a.is_empty(),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(content_text)
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(part) => str_field(part, "text").unwrap_or_default().to_owned(),
        _ => String::new(),
    }
}

fn result_text(result: &Value) -> String {
    match result {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(result_text)
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(part) => {
            if let Some(text) = str_field(part, "text") {
                return text.to_owned();
            }
            let Some(response) = part
                .get("functionResponse")
                .and_then(Value::as_object)
                .and_then(|f| f.get("response"))
            else {
                return String::new();
            };
            match response.as_object() {
                Some(r) => str_field(r, "output")
                    .or_else(|| str_field(r, "error"))
                    .map_or_else(|| response.to_string(), str::to_owned),
                None => response.to_string(),
            }
        }
        _ => String::new(),
    }
}

fn non_empty_preview(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| truncate_preview(trimmed, PREVIEW_MAX_CHARS))
}

#[derive(Debug, PartialEq)]
struct PathIdentity {
    fallback_id: String,
    parent: Option<String>,
    project_dir: Option<PathBuf>,
}

fn path_identity(path: &Path) -> PathIdentity {
    let fallback_id = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = path.parent();
    let (parent, project_dir) = match chats_depth(path) {
        Some(1) => (None, dir.and_then(Path::parent)),
        Some(_) => (
            dir.map(file_name),
            dir.and_then(Path::parent).and_then(Path::parent),
        ),
        None => (None, None),
    };
    PathIdentity {
        fallback_id,
        parent,
        project_dir: project_dir.map(Path::to_path_buf),
    }
}

fn chats_depth(path: &Path) -> Option<usize> {
    let dir = path.parent()?;
    if file_name(dir) == "chats" {
        return Some(1);
    }
    (dir.parent().map(file_name).as_deref() == Some("chats")).then_some(2)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn project_root(project_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(project_dir.join(PROJECT_ROOT_FILE)).ok()?;
    let root = text.trim();
    (!root.is_empty()).then(|| root.to_owned())
}

fn warning(src: &SourceRef, message: impl Into<String>) -> ParseWarning {
    ParseWarning::new(HarnessId::GEMINI_CLI, src, message)
}

fn str_field<'v>(obj: &'v Map<String, Value>, field: &str) -> Option<&'v str> {
    obj.get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn timestamp_field(obj: &Map<String, Value>, field: &str) -> Option<Timestamp> {
    str_field(obj, field).and_then(|s| s.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_roots_respect_gemini_cli_home() {
        let home = Some(PathBuf::from("/home/u"));
        assert_eq!(
            default_roots_from(None, home.clone())[0],
            PathBuf::from("/home/u/.gemini/tmp")
        );
        assert_eq!(
            default_roots_from(Some("".into()), home.clone())[0],
            PathBuf::from("/home/u/.gemini/tmp")
        );
        assert_eq!(
            default_roots_from(Some("/g".into()), home)[0],
            PathBuf::from("/g/.gemini/tmp")
        );
        assert!(default_roots_from(None, None).is_empty());
    }

    #[test]
    fn path_identity_of_main_and_subagent_files() {
        assert_eq!(
            path_identity(Path::new(
                "/t/proj/chats/session-2026-01-01T00-00-abcd1234.jsonl"
            )),
            PathIdentity {
                fallback_id: "session-2026-01-01T00-00-abcd1234".into(),
                parent: None,
                project_dir: Some(PathBuf::from("/t/proj")),
            }
        );
        assert_eq!(
            path_identity(Path::new("/t/proj/chats/parent-id/child-id.jsonl")),
            PathIdentity {
                fallback_id: "child-id".into(),
                parent: Some("parent-id".into()),
                project_dir: Some(PathBuf::from("/t/proj")),
            }
        );
        assert_eq!(chats_depth(Path::new("/t/proj/logs.json")), None);
    }

    #[test]
    fn tokens_split_cached_and_count_thoughts_and_tool_prompt() {
        let t = Tokens::deserialize(&serde_json::json!({
            "input": 9120, "output": 388, "cached": 8192, "thoughts": 140, "tool": 30, "total": 9678
        }))
        .unwrap()
        .counts();
        assert_eq!(
            t,
            TokenCounts {
                input: 958,
                output: 528,
                cache_write_5m: 0,
                cache_write_1h: 0,
                cache_read: 8192,
                reasoning: 140,
            }
        );
        assert_eq!(t.total(), 9678);

        let sparse = Tokens::deserialize(&serde_json::json!({
            "input": 5, "output": 2, "cached": 9, "total": 7
        }))
        .unwrap()
        .counts();
        assert_eq!(
            (sparse.input, sparse.cache_read, sparse.reasoning),
            (0, 5, 0)
        );
    }

    #[test]
    fn content_and_result_text() {
        let content = serde_json::json!([{"text": "a"}, {"inlineData": {}}, {"text": "b"}]);
        assert_eq!(content_text(&content), "a\nb");
        assert_eq!(content_text(&serde_json::json!("x")), "x");
        let result = serde_json::json!([{"functionResponse": {"response": {"error": "boom"}}}]);
        assert_eq!(result_text(&result), "boom");
        let other = serde_json::json!([{"functionResponse": {"response": {"n": 1}}}]);
        assert_eq!(result_text(&other), r#"{"n":1}"#);
    }
}
