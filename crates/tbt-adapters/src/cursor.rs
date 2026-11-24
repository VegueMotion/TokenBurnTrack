use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tbt_core::{
    Cursor, Error, HarnessAdapter, HarnessId, MessageKind, MessageRecord, MessageRole,
    PREVIEW_MAX_CHARS, ParseWarning, ReadBatch, Result, SessionUpsert, SourceKind, SourceRef,
    Timestamp, TokenCounts, UsageEvent, truncate_preview,
};

use crate::sqlite::{is_busy, is_read_only, open_read_only, table_exists, wal_path};

const HARNESS: HarnessId = HarnessId::CURSOR;

const DB_FILE: &str = "state.vscdb";
const COMPOSER_PREFIX: &str = "composerData:";
const BUBBLE_PREFIX: &str = "bubbleId:";
const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy)]
pub struct CursorAdapter {
    busy_timeout: Duration,
    stale_after: Duration,
}

impl Default for CursorAdapter {
    fn default() -> Self {
        CursorAdapter::new()
    }
}

impl CursorAdapter {
    pub fn new() -> CursorAdapter {
        CursorAdapter {
            busy_timeout: DEFAULT_BUSY_TIMEOUT,
            stale_after: DEFAULT_STALE_AFTER,
        }
    }

    pub fn with_busy_timeout(mut self, timeout: Duration) -> CursorAdapter {
        self.busy_timeout = timeout;
        self
    }

    pub fn with_stale_after(mut self, after: Duration) -> CursorAdapter {
        self.stale_after = after;
        self
    }
}

pub fn default_roots_from(config_home: Option<PathBuf>) -> Vec<PathBuf> {
    config_home
        .filter(|d| d.is_absolute())
        .map(|d| d.join("Cursor").join("User").join("globalStorage"))
        .into_iter()
        .collect()
}

fn config_home() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        std::env::home_dir().map(|h| h.join("Library").join("Application Support"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA")
            .filter(|v: &OsString| !v.is_empty())
            .map(PathBuf::from)
    } else {
        tbt_core::sandbox::host_xdg_home(tbt_core::sandbox::XdgDir::Config)
    }
}

impl HarnessAdapter for CursorAdapter {
    fn id(&self) -> HarnessId {
        HARNESS
    }

    fn display_name(&self) -> &str {
        "Cursor"
    }

    fn default_roots(&self) -> Vec<PathBuf> {
        default_roots_from(config_home())
    }

    fn discover(&self, roots: &[PathBuf]) -> Result<Vec<SourceRef>> {
        let mut found: Vec<PathBuf> = Vec::new();
        for root in roots {
            match std::fs::metadata(root) {
                Ok(meta) if meta.is_file() && is_db_name(root) => found.push(root.clone()),
                Ok(meta) if meta.is_dir() => {
                    let candidate = root.join(DB_FILE);
                    if std::fs::metadata(&candidate).is_ok_and(|m| m.is_file()) {
                        found.push(candidate);
                    }
                }
                _ => {}
            }
        }
        found.sort();
        found.dedup();
        Ok(found
            .into_iter()
            .map(|p| SourceRef::new(p, SourceKind::Sqlite))
            .collect())
    }

    fn read(&self, src: &SourceRef, cursor: &Cursor) -> Result<ReadBatch> {
        let mut warnings = Vec::new();
        let state = match cursor.state::<State>() {
            Ok(state) => state.unwrap_or_default(),
            Err(e) => {
                warnings.push(warning(src, format!("{e}; reading from the start")));
                State::default()
            }
        };
        if !src.path.is_file() {
            return Err(Error::io(
                &src.path,
                std::io::Error::new(std::io::ErrorKind::NotFound, "database not found"),
            ));
        }
        let now_ms = Timestamp::now().as_millisecond();
        match self.read_db(src, state, now_ms) {
            Ok((mut batch, state)) => {
                warnings.append(&mut batch.warnings);
                batch.warnings = warnings;
                batch.next_cursor = Cursor::from_state(&state)?;
                Ok(batch)
            }
            Err(e) if is_busy(&e) => {
                warnings.push(warning(
                    src,
                    format!("database is locked ({e}); will retry on the next run"),
                ));
                Ok(ReadBatch {
                    warnings,
                    ..ReadBatch::unchanged(cursor.clone())
                })
            }
            Err(e) => Err(Error::other(format!("{}: {e}", src.path.display()))),
        }
    }
}

impl CursorAdapter {
    fn read_db(
        &self,
        src: &SourceRef,
        state: State,
        now_ms: i64,
    ) -> rusqlite::Result<(ReadBatch, State)> {
        match self.read_db_with(src, state.clone(), now_ms, false) {
            Err(e) if is_read_only(&e) && !wal_path(&src.path).exists() => {
                self.read_db_with(src, state, now_ms, true)
            }
            other => other,
        }
    }

    fn read_db_with(
        &self,
        src: &SourceRef,
        state: State,
        now_ms: i64,
        immutable: bool,
    ) -> rusqlite::Result<(ReadBatch, State)> {
        let conn = open_read_only(&src.path, immutable)?;
        conn.busy_timeout(self.busy_timeout)?;
        let tx = conn.unchecked_transaction()?;
        let stale_after_ms = i64::try_from(self.stale_after.as_millis()).unwrap_or(i64::MAX);
        let mut reader = Reader::new(src, &tx, now_ms, stale_after_ms)?;
        let state = reader.read(state)?;
        let batch = reader.finish();
        tx.rollback()?;
        Ok((batch, state))
    }
}

fn is_db_name(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "vscdb")
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct State {
    last_rowid: i64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    done: Vec<i64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ComposerData {
    name: Option<String>,
    #[serde(rename = "createdAt")]
    created_at: Option<i64>,
    #[serde(rename = "lastUpdatedAt")]
    last_updated_at: Option<i64>,
    #[serde(rename = "generatingBubbleIds")]
    generating_bubble_ids: Vec<String>,
    #[serde(rename = "modelConfig")]
    model_config: ModelConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ModelConfig {
    #[serde(rename = "modelName")]
    model_name: Option<String>,
}

struct Composer {
    data: ComposerData,
    project_path: Option<String>,
}

impl Composer {
    fn upsert(&self, id: &str) -> SessionUpsert {
        let mut s = SessionUpsert::new(HARNESS, id);
        s.project_path = self.project_path.clone();
        s.title = self.data.name.clone().filter(|n| !n.trim().is_empty());
        s.started_at = self
            .data
            .created_at
            .and_then(|t| Timestamp::from_millisecond(t).ok());
        s.ended_at = self
            .data
            .last_updated_at
            .and_then(|t| Timestamp::from_millisecond(t).ok());
        s
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Bubble {
    #[serde(rename = "type")]
    kind: Option<i64>,
    text: Option<String>,
    #[serde(rename = "createdAt")]
    created_at: Option<Value>,
    #[serde(rename = "tokenCount")]
    token_count: Option<Value>,
    #[serde(rename = "modelInfo")]
    model_info: ModelInfo,
    #[serde(rename = "toolFormerData")]
    tool_former_data: Option<ToolFormerData>,
    thinking: Option<Thinking>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ModelInfo {
    #[serde(rename = "modelName")]
    model_name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ToolFormerData {
    name: Option<String>,
    params: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Thinking {
    text: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct TokenCount {
    #[serde(rename = "inputTokens")]
    input_tokens: u64,
    #[serde(rename = "outputTokens")]
    output_tokens: u64,
}

impl Bubble {
    fn ts_ms(&self) -> Option<i64> {
        match self.created_at.as_ref()? {
            Value::Number(n) => n.as_i64(),
            Value::String(s) => s.parse::<Timestamp>().ok().map(|t| t.as_millisecond()),
            _ => None,
        }
    }
}

struct Reader<'a> {
    src: &'a SourceRef,
    conn: &'a Connection,
    now_ms: i64,
    stale_after_ms: i64,
    has_headers: bool,
    composers: HashMap<String, Option<Composer>>,
    session_order: Vec<String>,
    events: Vec<UsageEvent>,
    messages: Vec<MessageRecord>,
    warnings: Vec<ParseWarning>,
}

enum Step {
    Done,
    Held,
}

impl<'a> Reader<'a> {
    fn new(
        src: &'a SourceRef,
        conn: &'a Connection,
        now_ms: i64,
        stale_after_ms: i64,
    ) -> rusqlite::Result<Reader<'a>> {
        Ok(Reader {
            src,
            conn,
            now_ms,
            stale_after_ms,
            has_headers: table_exists(conn, "composerHeaders")?,
            composers: HashMap::new(),
            session_order: Vec::new(),
            events: Vec::new(),
            messages: Vec::new(),
            warnings: Vec::new(),
        })
    }

    fn warn(&mut self, message: impl Into<String>) {
        self.warnings.push(warning(self.src, message));
    }

    fn read(&mut self, mut state: State) -> rusqlite::Result<State> {
        if !table_exists(self.conn, "cursorDiskKV")? {
            self.warn("no `cursorDiskKV` table; nothing to read");
            return Ok(state);
        }
        let max_rowid: i64 = self.conn.query_row(
            "SELECT coalesce(max(rowid), 0) FROM cursorDiskKV",
            [],
            |r| r.get(0),
        )?;
        if max_rowid < state.last_rowid {
            state = State::default();
        }
        let done: std::collections::HashSet<i64> = state.done.iter().copied().collect();
        let mut stmt = self.conn.prepare(
            "SELECT rowid, key, value FROM cursorDiskKV \
             WHERE rowid > ?1 AND (key LIKE 'composerData:%' OR key LIKE 'bubbleId:%') \
             ORDER BY rowid",
        )?;
        let mut rows = stmt.query(params![state.last_rowid])?;
        let mut next = State {
            last_rowid: state.last_rowid,
            done: Vec::new(),
        };
        let mut blocked = false;
        while let Some(row) = rows.next()? {
            let rowid: i64 = row.get(0)?;
            let key: String = row.get(1)?;
            let value = row.get_ref(2)?.as_bytes_or_null()?.map(<[u8]>::to_vec);
            let step = if done.contains(&rowid) {
                Step::Done
            } else {
                self.row(&key, value.as_deref())?
            };
            match step {
                Step::Held => blocked = true,
                Step::Done if blocked => next.done.push(rowid),
                Step::Done => next.last_rowid = rowid,
            }
        }
        Ok(next)
    }

    fn row(&mut self, key: &str, value: Option<&[u8]>) -> rusqlite::Result<Step> {
        let Some(value) = value else {
            return Ok(Step::Done);
        };
        if let Some(id) = key.strip_prefix(COMPOSER_PREFIX) {
            let composer = self.parse_composer(id, value)?;
            self.composers.insert(id.to_owned(), composer);
            self.touch_session(id);
            return Ok(Step::Done);
        }
        let Some((composer_id, bubble_id)) = key
            .strip_prefix(BUBBLE_PREFIX)
            .and_then(|rest| rest.split_once(':'))
        else {
            return Ok(Step::Done);
        };
        self.bubble(composer_id, bubble_id, value)
    }

    fn parse_composer(&mut self, id: &str, value: &[u8]) -> rusqlite::Result<Option<Composer>> {
        let data: ComposerData = match serde_json::from_slice(value) {
            Ok(d) => d,
            Err(e) => {
                self.warn(format!("composer {id}: invalid JSON: {e}"));
                return Ok(None);
            }
        };
        let project_path = self.project_path(id)?;
        Ok(Some(Composer { data, project_path }))
    }

    fn composer(&mut self, id: &str) -> rusqlite::Result<Option<&Composer>> {
        if !self.composers.contains_key(id) {
            let value: Option<Vec<u8>> = self
                .conn
                .query_row(
                    "SELECT value FROM cursorDiskKV WHERE key = ?1",
                    [format!("{COMPOSER_PREFIX}{id}")],
                    |r| Ok(r.get_ref(0)?.as_bytes_or_null()?.map(<[u8]>::to_vec)),
                )
                .optional()?
                .flatten();
            let composer = match value {
                Some(v) => self.parse_composer(id, &v)?,
                None => None,
            };
            self.composers.insert(id.to_owned(), composer);
        }
        Ok(self.composers[id].as_ref())
    }

    fn touch_session(&mut self, id: &str) {
        if !self.session_order.iter().any(|s| s == id) {
            self.session_order.push(id.to_owned());
        }
    }

    fn project_path(&self, composer_id: &str) -> rusqlite::Result<Option<String>> {
        if !self.has_headers {
            return Ok(None);
        }
        let workspace: Option<String> = self
            .conn
            .query_row(
                "SELECT workspaceId FROM composerHeaders WHERE composerId = ?1",
                [composer_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let Some(workspace) = workspace.filter(|w| is_plain_name(w)) else {
            return Ok(None);
        };
        let Some(storage) = self.src.path.parent().and_then(Path::parent) else {
            return Ok(None);
        };
        let file = storage
            .join("workspaceStorage")
            .join(workspace)
            .join("workspace.json");
        Ok(std::fs::read(file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|v| v.get("folder")?.as_str().and_then(file_uri_to_path)))
    }

    fn bubble(
        &mut self,
        composer_id: &str,
        bubble_id: &str,
        value: &[u8],
    ) -> rusqlite::Result<Step> {
        let bubble: Bubble = match serde_json::from_slice(value) {
            Ok(b) => b,
            Err(e) => {
                self.warn(format!("bubble {bubble_id}: invalid JSON: {e}"));
                return Ok(Step::Done);
            }
        };
        let role = match bubble.kind {
            Some(1) => MessageRole::User,
            Some(2) => MessageRole::Assistant,
            _ => return Ok(Step::Done),
        };
        let (now_ms, stale_after_ms) = (self.now_ms, self.stale_after_ms);
        let Some(composer) = self.composer(composer_id)? else {
            self.warn(format!(
                "bubble {bubble_id}: composer {composer_id} not found; skipped"
            ));
            return Ok(Step::Done);
        };
        let ts_ms = bubble
            .ts_ms()
            .or(composer.data.last_updated_at)
            .or(composer.data.created_at);
        let generating = composer
            .data
            .generating_bubble_ids
            .iter()
            .any(|b| b == bubble_id);
        let stale = ts_ms.is_none_or(|t| now_ms.saturating_sub(t) > stale_after_ms);
        if generating && !stale {
            return Ok(Step::Held);
        }
        let fallback_model = composer.data.model_config.model_name.clone();
        let project_path = composer.project_path.clone();

        let Some(ts) = ts_ms.and_then(|t| Timestamp::from_millisecond(t).ok()) else {
            self.warn(format!("bubble {bubble_id}: no valid timestamp; skipped"));
            return Ok(Step::Done);
        };
        self.touch_session(composer_id);

        let mut event_key = None;
        if role == MessageRole::Assistant {
            event_key = self.event(
                composer_id,
                bubble_id,
                &bubble,
                ts,
                fallback_model,
                project_path,
                generating,
            );
        }
        self.message(composer_id, bubble_id, &bubble, role, ts, event_key);
        Ok(Step::Done)
    }

    #[allow(clippy::too_many_arguments)]
    fn event(
        &mut self,
        composer_id: &str,
        bubble_id: &str,
        bubble: &Bubble,
        ts: Timestamp,
        fallback_model: Option<String>,
        project_path: Option<String>,
        generating: bool,
    ) -> Option<String> {
        let tokens = match bubble.token_count.as_ref().map(TokenCount::deserialize) {
            Some(Ok(t)) => t,
            Some(Err(e)) => {
                self.warn(format!("bubble {bubble_id}: invalid `tokenCount`: {e}"));
                return None;
            }
            None => return None,
        };
        let counts = TokenCounts {
            input: tokens.input_tokens,
            output: tokens.output_tokens,
            ..TokenCounts::default()
        };
        if counts.is_zero() {
            return None;
        }
        let Some(model) = bubble
            .model_info
            .model_name
            .clone()
            .or(fallback_model)
            .filter(|m| !m.is_empty())
        else {
            self.warn(format!("bubble {bubble_id} has tokens but no model name"));
            return None;
        };
        if generating {
            self.warn(format!(
                "bubble {bubble_id} never finished generating; counted with its last token values"
            ));
        }
        let mut event = UsageEvent {
            harness: HARNESS,
            dedup_key: bubble_id.to_owned(),
            session_ext_id: composer_id.to_owned(),
            ts,
            provider: provider_for(&model).to_owned(),
            model: canonical_model(&model),
            input_tokens: 0,
            output_tokens: 0,
            cache_write_5m_tokens: 0,
            cache_write_1h_tokens: 0,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
            reported_cost: None,
            project_path,
            git_branch: None,
            raw: bubble
                .token_count
                .as_ref()
                .and_then(|t| serde_json::value::to_raw_value(t).ok()),
        };
        event.set_tokens(counts);
        self.events.push(event);
        Some(bubble_id.to_owned())
    }

    fn message(
        &mut self,
        composer_id: &str,
        bubble_id: &str,
        bubble: &Bubble,
        role: MessageRole,
        ts: Timestamp,
        event_key: Option<String>,
    ) {
        let text = bubble.text.as_deref().and_then(non_empty_preview);
        let tool = bubble
            .tool_former_data
            .as_ref()
            .and_then(|t| t.name.clone().filter(|n| !n.is_empty()).map(|n| (n, t)));
        let thinking = bubble
            .thinking
            .as_ref()
            .and_then(|t| t.text.as_deref())
            .and_then(non_empty_preview);
        let (kind, tool_name, preview) = match (tool, text, thinking) {
            (Some((name, t)), _, _) => {
                let params = t.params.as_ref().filter(|p| !p.is_null()).map(|p| match p {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                });
                (
                    MessageKind::ToolUse,
                    Some(name),
                    params.as_deref().and_then(non_empty_preview),
                )
            }
            (None, Some(text), _) => (MessageKind::Text, None, Some(text)),
            (None, None, Some(thinking)) => (MessageKind::Thinking, None, Some(thinking)),
            (None, None, None) if event_key.is_some() => (MessageKind::Text, None, None),
            (None, None, None) => return,
        };
        self.messages.push(MessageRecord {
            session_ext_id: composer_id.to_owned(),
            key: bubble_id.to_owned(),
            ts,
            role,
            kind: Some(kind),
            tool_name,
            preview,
            event_dedup_key: event_key,
        });
    }

    fn finish(mut self) -> ReadBatch {
        let sessions = std::mem::take(&mut self.session_order)
            .into_iter()
            .filter_map(|id| self.composers.get(&id)?.as_ref().map(|c| c.upsert(&id)))
            .collect();
        ReadBatch {
            sessions,
            events: self.events,
            messages: self.messages,
            next_cursor: Cursor::empty(),
            warnings: self.warnings,
        }
    }
}

pub fn provider_for(model: &str) -> &'static str {
    let m = model.to_ascii_lowercase();
    if m.starts_with("claude") {
        "anthropic"
    } else if m.starts_with("gpt")
        || m.starts_with("o1")
        || m.starts_with("o3")
        || m.starts_with("o4")
    {
        "openai"
    } else if m.starts_with("gemini") {
        "google"
    } else if m.starts_with("grok") {
        "xai"
    } else if m.starts_with("deepseek") {
        "deepseek"
    } else {
        "cursor"
    }
}

pub fn canonical_model(model: &str) -> String {
    let base = model.strip_suffix("-thinking").unwrap_or(model);
    let parts: Vec<&str> = base.split('-').collect();
    if let ["claude", version, family] = parts[..]
        && version.chars().all(|c| c.is_ascii_digit() || c == '.')
        && version.starts_with(|c: char| c.is_ascii_digit())
        && family.chars().all(|c| c.is_ascii_lowercase())
    {
        return format!("claude-{family}-{}", version.replace('.', "-"));
    }
    model.to_owned()
}

fn is_plain_name(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && !s.contains(['/', '\\'])
}

fn file_uri_to_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let path = rest.get(rest.find('/')?..)?;
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(b) = path
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    let decoded = String::from_utf8(out).ok()?;
    let windows_drive = decoded.len() >= 3
        && decoded.as_bytes()[2] == b':'
        && decoded.as_bytes()[1].is_ascii_alphabetic();
    Some(if windows_drive {
        decoded[1..].to_owned()
    } else {
        decoded
    })
}

fn warning(src: &SourceRef, message: impl Into<String>) -> ParseWarning {
    ParseWarning::new(HARNESS, src, message)
}

fn non_empty_preview(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| truncate_preview(trimmed, PREVIEW_MAX_CHARS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_roots_use_the_config_home() {
        assert_eq!(
            default_roots_from(Some(PathBuf::from("/home/u/.config"))),
            [PathBuf::from("/home/u/.config/Cursor/User/globalStorage")]
        );
        assert!(default_roots_from(Some(PathBuf::from("relative"))).is_empty());
        assert!(default_roots_from(None).is_empty());
    }

    #[test]
    fn file_uris_become_paths() {
        assert_eq!(
            file_uri_to_path("file:///home/u/my%20proj").as_deref(),
            Some("/home/u/my proj")
        );
        assert_eq!(
            file_uri_to_path("file:///c%3A/Users/u/proj").as_deref(),
            Some("c:/Users/u/proj")
        );
        assert_eq!(file_uri_to_path("vscode-remote://ssh/x"), None);
    }

    #[test]
    fn provider_from_model_name() {
        assert_eq!(provider_for("claude-4.5-sonnet-thinking"), "anthropic");
        assert_eq!(provider_for("gpt-5"), "openai");
        assert_eq!(provider_for("o3"), "openai");
        assert_eq!(provider_for("gemini-2.5-pro"), "google");
        assert_eq!(provider_for("grok-4"), "xai");
        assert_eq!(provider_for("default"), "cursor");
    }

    #[test]
    fn cursor_claude_names_become_api_names() {
        assert_eq!(canonical_model("claude-4.5-sonnet"), "claude-sonnet-4-5");
        assert_eq!(
            canonical_model("claude-4.5-sonnet-thinking"),
            "claude-sonnet-4-5"
        );
        assert_eq!(canonical_model("claude-4.1-opus"), "claude-opus-4-1");
        assert_eq!(canonical_model("claude-sonnet-4-5"), "claude-sonnet-4-5");
        assert_eq!(canonical_model("gpt-5"), "gpt-5");
        assert_eq!(canonical_model("default"), "default");
    }

    #[test]
    fn bubble_timestamps_accept_millis_and_iso() {
        let b: Bubble = serde_json::from_str(r#"{"createdAt": 1700000000000}"#).unwrap();
        assert_eq!(b.ts_ms(), Some(1_700_000_000_000));
        let b: Bubble = serde_json::from_str(r#"{"createdAt": "2023-11-14T22:13:20Z"}"#).unwrap();
        assert_eq!(b.ts_ms(), Some(1_700_000_000_000));
    }
}
