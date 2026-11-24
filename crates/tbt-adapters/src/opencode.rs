use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tbt_core::{
    Cursor, Decimal, Error, HarnessAdapter, HarnessId, MessageKind, MessageRecord, MessageRole,
    PREVIEW_MAX_CHARS, ParseWarning, ReadBatch, Result, SessionUpsert, SourceKind, SourceRef,
    Timestamp, TokenCounts, UsageEvent, truncate_preview,
};

use crate::sqlite::{is_busy, is_read_only, open_read_only, table_exists, wal_path};

const DB_FILE: &str = "opencode.db";
const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

const KNOWN_MIGRATIONS: &[&str] = &[
    "20260127222353_familiar_lady_ursula",
    "20260211171708_add_project_commands",
    "20260213144116_wakeful_the_professor",
    "20260225215848_workspace",
    "20260227213759_add_session_workspace_id",
    "20260228203230_blue_harpoon",
    "20260303231226_add_workspace_fields",
    "20260309230000_move_org_to_state",
    "20260312043431_session_message_cursor",
    "20260323234822_events",
    "20260410174513_workspace-name",
    "20260413175956_chief_energizer",
    "20260423070820_add_icon_url_override",
    "20260427172553_slow_nightmare",
    "20260428004200_add_session_path",
    "20260501142318_next_venus",
    "20260504145000_add_sync_owner",
    "20260507164347_add_workspace_time",
    "20260510033149_session_usage",
    "20260511000411_data_migration_state",
    "20260511173437_session-metadata",
    "20260601010001_normalize_storage_paths",
    "20260601202201_amazing_prowler",
    "20260602002951_lowly_union_jack",
    "20260602182828_add_project_directories",
    "20260603001617_session_message_projection_indexes",
    "20260603040000_session_message_projection_order",
    "20260603141458_session_input_inbox",
    "20260603160727_jittery_ezekiel_stane",
    "20260604172448_event_sourced_session_input",
    "20260605003541_add_session_context_snapshot",
    "20260605042240_add_context_epoch_agent",
    "20260611035744_credential",
    "20260611192811_lush_chimera",
    "20260612174303_project_dir_strategy",
    "20260622142730_simplify_session_context_epoch",
    "20260622170816_reset_v2_session_state",
    "20260622202450_simplify_session_input",
];

#[derive(Debug, Clone, Copy)]
pub struct OpenCodeAdapter {
    busy_timeout: Duration,
    stale_after: Duration,
}

impl Default for OpenCodeAdapter {
    fn default() -> Self {
        OpenCodeAdapter::new()
    }
}

impl OpenCodeAdapter {
    pub fn new() -> OpenCodeAdapter {
        OpenCodeAdapter {
            busy_timeout: DEFAULT_BUSY_TIMEOUT,
            stale_after: DEFAULT_STALE_AFTER,
        }
    }

    pub fn with_busy_timeout(mut self, timeout: Duration) -> OpenCodeAdapter {
        self.busy_timeout = timeout;
        self
    }

    pub fn with_stale_after(mut self, after: Duration) -> OpenCodeAdapter {
        self.stale_after = after;
        self
    }
}

pub fn default_roots_from(data_home: Option<OsString>, home: Option<PathBuf>) -> Vec<PathBuf> {
    let data_home = data_home
        .map(PathBuf::from)
        .filter(|d| d.is_absolute())
        .or_else(|| home.map(|h| h.join(".local").join("share")));
    data_home.map(|d| d.join("opencode")).into_iter().collect()
}

impl HarnessAdapter for OpenCodeAdapter {
    fn id(&self) -> HarnessId {
        HarnessId::OPENCODE
    }

    fn display_name(&self) -> &str {
        "OpenCode"
    }

    fn default_roots(&self) -> Vec<PathBuf> {
        default_roots_from(
            tbt_core::sandbox::host_xdg_var(tbt_core::sandbox::XdgDir::Data),
            std::env::home_dir(),
        )
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

impl OpenCodeAdapter {
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
        let mut reader = Reader::new(src, &tx, state, now_ms, self.stale_after);
        reader.check_schema()?;
        reader.read_sessions()?;
        reader.read_messages()?;
        reader.read_parts()?;
        let out = reader.finish();
        tx.rollback()?;
        Ok(out)
    }
}

fn is_db_name(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "db")
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct State {
    #[serde(skip_serializing_if = "Option::is_none")]
    schema: Option<String>,
    schema_checked: bool,
    sessions: Watermark,
    messages: Watermark,
    parts: Watermark,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Watermark {
    time_updated: i64,
    id: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    done: Vec<(String, i64)>,
}

struct Scan {
    mark: Watermark,
    done: HashSet<(String, i64)>,
    next_done: Vec<(String, i64)>,
    blocked: bool,
}

enum Step {
    Done,
    Held,
}

impl Scan {
    fn new(mark: &Watermark) -> Scan {
        Scan {
            mark: Watermark {
                time_updated: mark.time_updated,
                id: mark.id.clone(),
                done: Vec::new(),
            },
            done: mark.done.iter().cloned().collect(),
            next_done: Vec::new(),
            blocked: false,
        }
    }

    fn seen(&self, id: &str, time_updated: i64) -> bool {
        self.done.contains(&(id.to_owned(), time_updated))
    }

    fn record(&mut self, id: &str, time_updated: i64, step: Step) {
        match step {
            Step::Held => self.blocked = true,
            Step::Done if self.blocked => self.next_done.push((id.to_owned(), time_updated)),
            Step::Done => {
                self.mark.time_updated = time_updated;
                self.mark.id = id.to_owned();
            }
        }
    }

    fn finish(mut self) -> Watermark {
        self.mark.done = self.next_done;
        self.mark
    }
}

const AFTER_MARK: &str = "time_updated > ?1 OR (time_updated = ?1 AND id > ?2)";

struct SessionRow {
    id: String,
    parent_id: Option<String>,
    directory: Option<String>,
    title: Option<String>,
    version: Option<String>,
    time_created: i64,
    time_updated: i64,
}

impl SessionRow {
    const COLUMNS: &str = "id, parent_id, directory, title, version, time_created, time_updated";

    fn from_row(row: &Row<'_>) -> rusqlite::Result<SessionRow> {
        Ok(SessionRow {
            id: row.get(0)?,
            parent_id: non_empty(row.get(1)?),
            directory: non_empty(row.get(2)?),
            title: non_empty(row.get(3)?),
            version: non_empty(row.get(4)?),
            time_created: row.get(5)?,
            time_updated: row.get(6)?,
        })
    }

    fn upsert(&self) -> SessionUpsert {
        let mut s = SessionUpsert::new(HarnessId::OPENCODE, self.id.clone());
        s.parent_ext_id = self.parent_id.clone();
        s.project_path = self.directory.clone();
        s.title = self.title.clone();
        s.harness_version = self.version.clone();
        s.started_at = Timestamp::from_millisecond(self.time_created).ok();
        s.ended_at = Timestamp::from_millisecond(self.time_updated).ok();
        s
    }
}

#[derive(Debug, Clone)]
struct MessageInfo {
    session_id: String,
    role: Option<MessageRole>,
    ready: bool,
    event_key: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct MessageData {
    role: Option<String>,
    #[serde(rename = "providerID")]
    provider_id: Option<String>,
    #[serde(rename = "modelID")]
    model_id: Option<String>,
    cost: Option<f64>,
    tokens: Option<Value>,
    time: MessageTime,
    error: Option<Value>,
    path: Option<MessagePath>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct MessageTime {
    created: Option<i64>,
    completed: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct MessagePath {
    cwd: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Tokens {
    input: u64,
    output: u64,
    reasoning: u64,
    cache: CacheTokens,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CacheTokens {
    read: u64,
    write: u64,
}

impl Tokens {
    fn counts(&self) -> TokenCounts {
        TokenCounts {
            input: self.input,
            output: self.output + self.reasoning,
            cache_write_5m: self.cache.write,
            cache_write_1h: 0,
            cache_read: self.cache.read,
            reasoning: self.reasoning,
        }
    }
}

struct Reader<'a> {
    src: &'a SourceRef,
    conn: &'a Connection,
    state: State,
    now_ms: i64,
    stale_after_ms: i64,
    session_order: Vec<String>,
    session_rows: HashMap<String, Option<SessionRow>>,
    message_info: HashMap<String, Option<MessageInfo>>,
    events: Vec<UsageEvent>,
    messages: Vec<MessageRecord>,
    warnings: Vec<ParseWarning>,
}

impl<'a> Reader<'a> {
    fn new(
        src: &'a SourceRef,
        conn: &'a Connection,
        state: State,
        now_ms: i64,
        stale_after: Duration,
    ) -> Reader<'a> {
        Reader {
            src,
            conn,
            state,
            now_ms,
            stale_after_ms: i64::try_from(stale_after.as_millis()).unwrap_or(i64::MAX),
            session_order: Vec::new(),
            session_rows: HashMap::new(),
            message_info: HashMap::new(),
            events: Vec::new(),
            messages: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn warn(&mut self, message: impl Into<String>) {
        self.warnings.push(warning(self.src, message));
    }

    fn check_schema(&mut self) -> rusqlite::Result<()> {
        let version = schema_version(self.conn)?;
        let known = version
            .as_deref()
            .is_some_and(|v| KNOWN_MIGRATIONS.contains(&v));
        if !known && (!self.state.schema_checked || version != self.state.schema) {
            let latest = KNOWN_MIGRATIONS[KNOWN_MIGRATIONS.len() - 1];
            match &version {
                Some(v) => self.warn(format!(
                    "unknown schema version {v} (latest tested: {latest}); reading anyway"
                )),
                None => self.warn("no schema version found (`migration` table); reading anyway"),
            }
        }
        self.state.schema = version;
        self.state.schema_checked = true;
        Ok(())
    }

    fn read_sessions(&mut self) -> rusqlite::Result<()> {
        let mark = self.state.sessions.clone();
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM session WHERE {AFTER_MARK} ORDER BY time_updated, id",
            SessionRow::COLUMNS
        ))?;
        let mut rows = stmt.query(params![mark.time_updated, mark.id])?;
        let mut scan = Scan::new(&mark);
        while let Some(row) = rows.next()? {
            let session = SessionRow::from_row(row)?;
            scan.record(&session.id, session.time_updated, Step::Done);
            self.add_session(session);
        }
        self.state.sessions = scan.finish();
        Ok(())
    }

    fn add_session(&mut self, session: SessionRow) {
        if !self.session_order.contains(&session.id) {
            self.session_order.push(session.id.clone());
        }
        self.session_rows.insert(session.id.clone(), Some(session));
    }

    fn reference_session(&mut self, id: &str) -> rusqlite::Result<Option<&SessionRow>> {
        if !self.session_rows.contains_key(id) {
            let row = self
                .conn
                .query_row(
                    &format!("SELECT {} FROM session WHERE id = ?1", SessionRow::COLUMNS),
                    [id],
                    SessionRow::from_row,
                )
                .optional()?;
            self.session_rows.insert(id.to_owned(), row);
        }
        if self.session_rows[id].is_some() && !self.session_order.iter().any(|s| s == id) {
            self.session_order.push(id.to_owned());
        }
        Ok(self.session_rows[id].as_ref())
    }

    fn read_messages(&mut self) -> rusqlite::Result<()> {
        let mark = self.state.messages.clone();
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, session_id, time_created, time_updated, data FROM message \
             WHERE {AFTER_MARK} ORDER BY time_updated, id"
        ))?;
        let mut rows = stmt.query(params![mark.time_updated, mark.id])?;
        let mut scan = Scan::new(&mark);
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let session_id: String = row.get(1)?;
            let time_created: i64 = row.get(2)?;
            let time_updated: i64 = row.get(3)?;
            let data: String = row.get(4)?;
            let seen = scan.seen(&id, time_updated);
            let info = self.message(&id, &session_id, time_created, time_updated, &data, !seen)?;
            let step = if info.ready { Step::Done } else { Step::Held };
            self.message_info.insert(id.clone(), Some(info));
            scan.record(&id, time_updated, step);
        }
        self.state.messages = scan.finish();
        Ok(())
    }

    fn message(
        &mut self,
        id: &str,
        session_id: &str,
        time_created: i64,
        time_updated: i64,
        data: &str,
        emit: bool,
    ) -> rusqlite::Result<MessageInfo> {
        let mut info = MessageInfo {
            session_id: session_id.to_owned(),
            role: None,
            ready: true,
            event_key: None,
        };
        let parsed: MessageData = match serde_json::from_str(data) {
            Ok(d) => d,
            Err(e) => {
                if emit {
                    self.warn(format!("message {id}: invalid `data` JSON: {e}"));
                }
                return Ok(info);
            }
        };
        info.role = match parsed.role.as_deref() {
            Some("user") => Some(MessageRole::User),
            Some("assistant") => Some(MessageRole::Assistant),
            _ => None,
        };
        if info.role != Some(MessageRole::Assistant) {
            return Ok(info);
        }

        let completed = parsed.time.completed.is_some() || parsed.error.is_some();
        let stale = self.now_ms.saturating_sub(time_updated) > self.stale_after_ms;
        info.ready = completed || stale;
        if !info.ready {
            return Ok(info);
        }

        let tokens = match parsed.tokens.as_ref().map(Tokens::deserialize) {
            Some(Ok(t)) => t,
            Some(Err(e)) => {
                if emit {
                    self.warn(format!("message {id}: invalid `tokens`: {e}"));
                }
                return Ok(info);
            }
            None => return Ok(info),
        };
        let counts = tokens.counts();
        if counts.is_zero() {
            return Ok(info);
        }
        info.event_key = Some(id.to_owned());
        if !emit {
            return Ok(info);
        }
        if !completed {
            self.warn(format!(
                "message {id} never completed; counted with its last token values"
            ));
        }

        let Some(model) = parsed.model_id.filter(|m| !m.is_empty()) else {
            self.warn(format!("message {id} has tokens but no `modelID`"));
            info.event_key = None;
            return Ok(info);
        };
        let provider = parsed
            .provider_id
            .as_deref()
            .filter(|p| !p.is_empty())
            .map_or_else(|| "unknown".to_owned(), map_provider);
        let ts_ms = parsed
            .time
            .completed
            .or(parsed.time.created)
            .unwrap_or(time_created);
        let Ok(ts) = Timestamp::from_millisecond(ts_ms) else {
            self.warn(format!("message {id}: invalid timestamp {ts_ms}"));
            info.event_key = None;
            return Ok(info);
        };
        let session_dir = self
            .reference_session(session_id)?
            .and_then(|s| s.directory.clone());
        let project_path = parsed.path.and_then(|p| non_empty(p.cwd)).or(session_dir);

        let mut event = UsageEvent {
            harness: HarnessId::OPENCODE,
            dedup_key: id.to_owned(),
            session_ext_id: session_id.to_owned(),
            ts,
            provider,
            model,
            input_tokens: 0,
            output_tokens: 0,
            cache_write_5m_tokens: 0,
            cache_write_1h_tokens: 0,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
            reported_cost: parsed.cost.and_then(cost_decimal),
            project_path,
            git_branch: None,
            raw: parsed
                .tokens
                .as_ref()
                .and_then(|t| serde_json::value::to_raw_value(t).ok()),
        };
        event.set_tokens(counts);
        self.events.push(event);
        Ok(info)
    }

    fn message_info(&mut self, id: &str) -> rusqlite::Result<Option<MessageInfo>> {
        if let Some(info) = self.message_info.get(id) {
            return Ok(info.clone());
        }
        let row = self
            .conn
            .query_row(
                "SELECT session_id, time_created, time_updated, data FROM message WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        let info = match row {
            Some((session_id, created, updated, data)) => {
                Some(self.message(id, &session_id, created, updated, &data, false)?)
            }
            None => None,
        };
        self.message_info.insert(id.to_owned(), info.clone());
        Ok(info)
    }

    fn read_parts(&mut self) -> rusqlite::Result<()> {
        let mark = self.state.parts.clone();
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, message_id, time_created, time_updated, data FROM part \
             WHERE {AFTER_MARK} ORDER BY time_updated, id"
        ))?;
        let mut rows = stmt.query(params![mark.time_updated, mark.id])?;
        let mut scan = Scan::new(&mark);
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let message_id: String = row.get(1)?;
            let time_created: i64 = row.get(2)?;
            let time_updated: i64 = row.get(3)?;
            if scan.seen(&id, time_updated) {
                scan.record(&id, time_updated, Step::Done);
                continue;
            }
            let Some(parent) = self.message_info(&message_id)? else {
                scan.record(&id, time_updated, Step::Done);
                continue;
            };
            if !parent.ready {
                scan.record(&id, time_updated, Step::Held);
                continue;
            }
            let data: String = row.get(4)?;
            self.part(&id, &parent, time_created, &data)?;
            scan.record(&id, time_updated, Step::Done);
        }
        self.state.parts = scan.finish();
        Ok(())
    }

    fn part(
        &mut self,
        id: &str,
        parent: &MessageInfo,
        time_created: i64,
        data: &str,
    ) -> rusqlite::Result<()> {
        let Some(role) = parent.role else {
            return Ok(());
        };
        let value: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(e) => {
                self.warn(format!("part {id}: invalid `data` JSON: {e}"));
                return Ok(());
            }
        };
        let Some(obj) = value.as_object() else {
            return Ok(());
        };
        let text = |field: &str| str_field(obj, field).map(str::to_owned);
        let (kind, tool_name, preview) = match str_field(obj, "type").unwrap_or_default() {
            "text" => (MessageKind::Text, None, text("text")),
            "reasoning" => (MessageKind::Thinking, None, text("text")),
            "tool" => {
                let input = obj
                    .get("state")
                    .and_then(Value::as_object)
                    .and_then(|s| s.get("input"))
                    .filter(|v| !v.is_null())
                    .map(Value::to_string);
                (MessageKind::ToolUse, text("tool"), input)
            }
            _ => return Ok(()),
        };
        let Ok(ts) = Timestamp::from_millisecond(time_created) else {
            self.warn(format!("part {id}: invalid timestamp {time_created}"));
            return Ok(());
        };
        self.reference_session(&parent.session_id)?;
        self.messages.push(MessageRecord {
            session_ext_id: parent.session_id.clone(),
            key: id.to_owned(),
            ts,
            role,
            kind: Some(kind),
            tool_name,
            preview: preview.and_then(|p| non_empty_preview(&p)),
            event_dedup_key: parent.event_key.clone(),
        });
        Ok(())
    }

    fn finish(mut self) -> (ReadBatch, State) {
        let sessions = std::mem::take(&mut self.session_order)
            .into_iter()
            .filter_map(|id| self.session_rows.get(&id)?.as_ref().map(SessionRow::upsert))
            .collect();
        let batch = ReadBatch {
            sessions,
            events: self.events,
            messages: self.messages,
            next_cursor: Cursor::empty(),
            warnings: self.warnings,
        };
        (batch, self.state)
    }
}

fn schema_version(conn: &Connection) -> rusqlite::Result<Option<String>> {
    let mut latest: Option<String> = None;
    for (table, column) in [("migration", "id"), ("__drizzle_migrations", "name")] {
        if !table_exists(conn, table)? {
            continue;
        }
        let v: Option<String> = conn.query_row(
            &format!("SELECT max(\"{column}\") FROM \"{table}\""),
            [],
            |r| r.get(0),
        )?;
        latest = latest.max(v);
    }
    Ok(latest)
}

pub fn map_provider(provider_id: &str) -> String {
    match provider_id {
        "google-vertex" => "google",
        "google-vertex-anthropic" => "anthropic",
        other => other,
    }
    .to_owned()
}

fn cost_decimal(cost: f64) -> Option<Decimal> {
    if !cost.is_finite() || cost <= 0.0 {
        return None;
    }
    cost.to_string().parse().ok()
}

fn warning(src: &SourceRef, message: impl Into<String>) -> ParseWarning {
    ParseWarning::new(HarnessId::OPENCODE, src, message)
}

fn str_field<'v>(obj: &'v Map<String, Value>, field: &str) -> Option<&'v str> {
    obj.get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|s| !s.is_empty())
}

fn non_empty_preview(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| truncate_preview(trimmed, PREVIEW_MAX_CHARS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_roots_respect_xdg_data_home() {
        let home = Some(PathBuf::from("/home/u"));
        assert_eq!(
            default_roots_from(None, home.clone()),
            [PathBuf::from("/home/u/.local/share/opencode")]
        );
        assert_eq!(
            default_roots_from(Some("".into()), home.clone()),
            [PathBuf::from("/home/u/.local/share/opencode")]
        );
        assert_eq!(
            default_roots_from(Some("relative".into()), home.clone()),
            [PathBuf::from("/home/u/.local/share/opencode")]
        );
        assert_eq!(
            default_roots_from(Some("/data".into()), home),
            [PathBuf::from("/data/opencode")]
        );
        assert!(default_roots_from(None, None).is_empty());
    }

    #[test]
    fn token_mapping_adds_reasoning_to_output() {
        let t: Tokens = serde_json::from_value(serde_json::json!({
            "total": 8738, "input": 8445, "output": 198, "reasoning": 95,
            "cache": {"write": 7, "read": 11}
        }))
        .unwrap();
        let c = t.counts();
        assert_eq!(
            c,
            TokenCounts {
                input: 8445,
                output: 293,
                cache_write_5m: 7,
                cache_write_1h: 0,
                cache_read: 11,
                reasoning: 95,
            }
        );
        assert_eq!(c.total(), 8445 + 198 + 95 + 7 + 11);
    }

    #[test]
    fn provider_mapping() {
        assert_eq!(map_provider("anthropic"), "anthropic");
        assert_eq!(map_provider("google-vertex"), "google");
        assert_eq!(map_provider("google-vertex-anthropic"), "anthropic");
        assert_eq!(map_provider("opencode"), "opencode");
        assert_eq!(map_provider("ollama"), "ollama");
    }

    #[test]
    fn cost_only_when_positive() {
        assert_eq!(cost_decimal(0.0), None);
        assert_eq!(cost_decimal(-1.0), None);
        assert_eq!(cost_decimal(f64::NAN), None);
        assert_eq!(cost_decimal(0.0123), Some("0.0123".parse().unwrap()));
        assert_eq!(cost_decimal(1.5e-7), Some("0.00000015".parse().unwrap()));
    }

    #[test]
    fn scan_holds_back_after_first_incomplete_row() {
        let mut scan = Scan::new(&Watermark::default());
        scan.record("a", 1, Step::Done);
        scan.record("b", 2, Step::Held);
        scan.record("c", 3, Step::Done);
        let mark = scan.finish();
        assert_eq!((mark.time_updated, mark.id.as_str()), (1, "a"));
        assert_eq!(mark.done, [("c".to_owned(), 3)]);

        let scan = Scan::new(&mark);
        assert!(scan.seen("c", 3));
        assert!(!scan.seen("c", 4));
    }
}
