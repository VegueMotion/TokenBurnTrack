use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tbt_core::{
    Cursor, Error, HarnessAdapter, HarnessId, ParseWarning, ReadBatch, Result, SessionUpsert,
    SourceKind, SourceRef, Timestamp, TokenCounts, UsageEvent,
};

use crate::sqlite::{is_busy, is_read_only, open_read_only, table_exists, wal_path};

const HARNESS: HarnessId = HarnessId::GOOSE;

const DB_FILE: &str = "sessions.db";
const UNKNOWN_MODEL: &str = "unknown";
const LOCAL_PROVIDERS: [&str; 2] = ["ollama", "local"];
const TRACKED_CLI_PROVIDERS: [&str; 7] = [
    "claude-code",
    "claude-acp",
    "codex",
    "codex-acp",
    "gemini-cli",
    "copilot-acp",
    "pi-acp",
];
const CARRIED_FORWARD: &str = "carried_forward";
const PROVIDER_REPORTED: &str = "provider_reported";
const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy)]
pub struct GooseAdapter {
    busy_timeout: Duration,
}

impl Default for GooseAdapter {
    fn default() -> Self {
        GooseAdapter::new()
    }
}

impl GooseAdapter {
    pub fn new() -> GooseAdapter {
        GooseAdapter {
            busy_timeout: DEFAULT_BUSY_TIMEOUT,
        }
    }
}

pub fn roots_from(
    path_root: Option<PathBuf>,
    data_home: Option<PathBuf>,
    appdata: Option<PathBuf>,
    windows: bool,
) -> Vec<PathBuf> {
    if let Some(root) = path_root.filter(|p| p.is_absolute()) {
        return vec![root.join("data").join("sessions")];
    }
    let data = if windows {
        appdata.map(|a| a.join("Block").join("goose").join("data"))
    } else {
        data_home
            .filter(|d| d.is_absolute())
            .map(|d| d.join("goose"))
    };
    data.map(|d| d.join("sessions")).into_iter().collect()
}

fn default_roots() -> Vec<PathBuf> {
    let var = |name: &str| {
        std::env::var_os(name)
            .filter(|v: &OsString| !v.is_empty())
            .map(PathBuf::from)
    };
    let data_home = if cfg!(target_os = "macos") {
        var("XDG_DATA_HOME").or_else(|| std::env::home_dir().map(|h| h.join(".local/share")))
    } else {
        tbt_core::sandbox::host_xdg_home(tbt_core::sandbox::XdgDir::Data)
    };
    roots_from(
        var("GOOSE_PATH_ROOT"),
        data_home,
        var("APPDATA"),
        cfg!(windows),
    )
}

impl HarnessAdapter for GooseAdapter {
    fn id(&self) -> HarnessId {
        HARNESS
    }

    fn display_name(&self) -> &str {
        "Goose"
    }

    fn default_roots(&self) -> Vec<PathBuf> {
        default_roots()
    }

    fn discover(&self, roots: &[PathBuf]) -> Result<Vec<SourceRef>> {
        let mut found: Vec<PathBuf> = Vec::new();
        for root in roots {
            match std::fs::metadata(root) {
                Ok(meta) if meta.is_file() => found.push(root.clone()),
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
        match self.read_db(src, &state) {
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

impl GooseAdapter {
    fn read_db(&self, src: &SourceRef, state: &State) -> rusqlite::Result<(ReadBatch, State)> {
        match self.read_db_with(src, state, false) {
            Err(e) if is_read_only(&e) && !wal_path(&src.path).exists() => {
                self.read_db_with(src, state, true)
            }
            other => other,
        }
    }

    fn read_db_with(
        &self,
        src: &SourceRef,
        state: &State,
        immutable: bool,
    ) -> rusqlite::Result<(ReadBatch, State)> {
        let conn = open_read_only(&src.path, immutable)?;
        conn.busy_timeout(self.busy_timeout)?;
        let tx = conn.unchecked_transaction()?;
        let data = read_data(&tx, state.last_id)?;
        tx.rollback()?;
        let Some(data) = data else {
            return Ok((ReadBatch::default(), state.clone()));
        };
        let mut reader = Reader {
            src,
            batch: ReadBatch::default(),
            sessions: data.sessions,
            touched: Vec::new(),
        };
        let state = reader.read(state, data.ledger);
        reader.upsert_sessions();
        Ok((reader.batch, state))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct State {
    last_id: i64,
    totals: BTreeMap<String, Usage>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Usage {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
}

impl Usage {
    fn new(input: i64, output: i64, cache_read: i64, cache_write: i64) -> Usage {
        let n = |v: i64| u64::try_from(v).unwrap_or(0);
        Usage {
            input: n(input),
            output: n(output),
            cache_read: n(cache_read),
            cache_write: n(cache_write),
        }
    }

    fn counts(&self) -> TokenCounts {
        TokenCounts {
            input: self
                .input
                .saturating_sub(self.cache_read)
                .saturating_sub(self.cache_write),
            output: self.output,
            cache_write_5m: self.cache_write,
            cache_write_1h: 0,
            cache_read: self.cache_read,
            reasoning: 0,
        }
    }

    fn minus(&self, other: &Usage) -> Usage {
        Usage {
            input: self.input.saturating_sub(other.input),
            output: self.output.saturating_sub(other.output),
            cache_read: self.cache_read.saturating_sub(other.cache_read),
            cache_write: self.cache_write.saturating_sub(other.cache_write),
        }
    }

    fn is_zero(&self) -> bool {
        *self == Usage::default()
    }
}

struct SessionRow {
    id: String,
    name: Option<String>,
    working_dir: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    parent_id: Option<String>,
    accumulated: Usage,
    has_ledger: bool,
}

struct LedgerRow {
    id: i64,
    session_id: String,
    created: i64,
    model: Option<String>,
    usage: Usage,
    cost: Option<f64>,
    cost_source: Option<String>,
}

struct Data {
    sessions: HashMap<String, SessionRow>,
    ledger: Vec<LedgerRow>,
}

struct Reader<'a> {
    src: &'a SourceRef,
    batch: ReadBatch,
    sessions: HashMap<String, SessionRow>,
    touched: Vec<String>,
}

impl Reader<'_> {
    fn warn(&mut self, message: impl Into<String>) {
        self.batch.warnings.push(warning(self.src, message));
    }

    fn read(&mut self, state: &State, ledger: Vec<LedgerRow>) -> State {
        let mut next = State {
            last_id: state.last_id,
            totals: BTreeMap::new(),
        };
        let mut sessions: Vec<&SessionRow> = self.sessions.values().collect();
        sessions.sort_by(|a, b| a.id.cmp(&b.id));
        let mut legacy = Vec::new();
        for s in sessions {
            if s.has_ledger {
                continue;
            }
            let prev = state.totals.get(&s.id).copied().unwrap_or_default();
            next.totals.insert(s.id.clone(), prev);
            let delta = s.accumulated.minus(&prev);
            if !delta.is_zero() {
                legacy.push((s.id.clone(), s.accumulated, delta));
            }
        }
        for (id, total, delta) in legacy {
            if self.legacy_event(&id, total, delta) {
                next.totals.insert(id, total);
            }
        }
        for row in ledger {
            next.last_id = next.last_id.max(row.id);
            let mut usage = row.usage;
            if row.cost_source.as_deref() == Some(CARRIED_FORWARD)
                && let Some(counted) = state.totals.get(&row.session_id)
            {
                usage = usage.minus(counted);
            }
            if !usage.is_zero() {
                self.ledger_event(row, usage);
            }
        }
        next
    }

    fn model_for(&self, session: &SessionRow, model: Option<&str>) -> (String, String) {
        let provider = session.provider.as_deref().unwrap_or("goose");
        match model.or(session.model.as_deref()).map(str::trim) {
            Some(m) if !m.is_empty() => (provider_for(provider, m), m.to_owned()),
            _ => (provider.to_owned(), UNKNOWN_MODEL.to_owned()),
        }
    }

    fn counted_session(&self, session_id: &str) -> Option<&SessionRow> {
        let session = self.sessions.get(session_id)?;
        let provider = session.provider.as_deref().unwrap_or_default();
        (!TRACKED_CLI_PROVIDERS.contains(&provider)).then_some(session)
    }

    fn legacy_event(&mut self, id: &str, total: Usage, delta: Usage) -> bool {
        let Some(session) = self.counted_session(id) else {
            return true;
        };
        let Some(ts) = session.updated_at.as_deref().and_then(parse_sqlite_time) else {
            let updated = session.updated_at.clone();
            self.warn(format!(
                "session {id}: invalid `updated_at` {updated:?}; skipped"
            ));
            return false;
        };
        let (provider, model) = self.model_for(session, None);
        let raw = json!({
            "accumulated": total,
            "provider": session.provider,
        });
        let event = event(
            session,
            format!("{id}:total:{}", total.counts().total()),
            ts,
            provider,
            model,
            delta,
            None,
            raw,
        );
        self.batch.events.push(event);
        self.touched.push(id.to_owned());
        true
    }

    fn ledger_event(&mut self, row: LedgerRow, usage: Usage) {
        let Some(session) = self.counted_session(&row.session_id) else {
            return;
        };
        let Some(ts) = Timestamp::from_second(row.created).ok() else {
            let (id, created) = (row.id, row.created);
            self.warn(format!(
                "ledger row {id}: invalid `created_timestamp` {created}; skipped"
            ));
            return;
        };
        let (provider, model) = self.model_for(session, row.model.as_deref());
        let reported_cost = match row.cost_source.as_deref() {
            Some(PROVIDER_REPORTED) => row.cost.and_then(cost_decimal),
            _ => None,
        };
        let raw = json!({
            "id": row.id,
            "model": row.model,
            "provider": session.provider,
            "usage": row.usage,
            "cost": row.cost,
            "costSource": row.cost_source,
        });
        let event = event(
            session,
            format!("ledger:{}", row.id),
            ts,
            provider,
            model,
            usage,
            reported_cost,
            raw,
        );
        self.batch.events.push(event);
        self.touched.push(row.session_id);
    }

    fn upsert_sessions(&mut self) {
        self.touched.sort();
        self.touched.dedup();
        for id in std::mem::take(&mut self.touched) {
            let Some(s) = self.sessions.get(&id) else {
                continue;
            };
            let mut session = SessionUpsert::new(HARNESS, id);
            session.project_path = s.working_dir.clone().filter(|p| !p.is_empty());
            session.title = s.name.clone().filter(|n| !n.trim().is_empty());
            session.started_at = s.created_at.as_deref().and_then(parse_sqlite_time);
            session.parent_ext_id = s.parent_id.clone().filter(|p| !p.is_empty());
            self.batch.sessions.push(session);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn event(
    session: &SessionRow,
    dedup_key: String,
    ts: Timestamp,
    provider: String,
    model: String,
    usage: Usage,
    reported_cost: Option<tbt_core::Decimal>,
    raw: serde_json::Value,
) -> UsageEvent {
    let mut event = UsageEvent {
        harness: HARNESS,
        dedup_key,
        session_ext_id: session.id.clone(),
        ts,
        provider,
        model,
        input_tokens: 0,
        output_tokens: 0,
        cache_write_5m_tokens: 0,
        cache_write_1h_tokens: 0,
        cache_read_tokens: 0,
        reasoning_tokens: 0,
        reported_cost,
        project_path: session.working_dir.clone().filter(|p| !p.is_empty()),
        git_branch: None,
        raw: serde_json::value::to_raw_value(&raw).ok(),
    };
    event.set_tokens(usage.counts());
    event
}

fn read_data(conn: &Connection, last_id: i64) -> rusqlite::Result<Option<Data>> {
    if !table_exists(conn, "sessions")? {
        return Ok(None);
    }
    let has_ledger = table_exists(conn, "usage_ledger")?;
    let col = |c: &'static str| -> rusqlite::Result<&'static str> {
        let has: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM pragma_table_info('sessions') WHERE name = ?1)",
            [c],
            |r| r.get(0),
        )?;
        Ok(if has { c } else { "NULL" })
    };
    let ledger_check = if has_ledger {
        "EXISTS (SELECT 1 FROM usage_ledger u WHERE u.session_id = s.id)"
    } else {
        "0"
    };
    let sql = format!(
        "SELECT s.id, {}, s.working_dir, s.created_at, s.updated_at, {}, {}, {},
                {}, {}, {}, {}, {ledger_check}
         FROM sessions s",
        col("name")?,
        col("provider_name")?,
        col("model_config_json")?,
        col("parent_session_id")?,
        col("accumulated_input_tokens")?,
        col("accumulated_output_tokens")?,
        col("accumulated_cache_read_tokens")?,
        col("accumulated_cache_write_tokens")?,
    );
    let sessions = conn
        .prepare(&sql)?
        .query_map([], session_row)?
        .map(|r| r.map(|s| (s.id.clone(), s)))
        .collect::<rusqlite::Result<HashMap<_, _>>>()?;
    let ledger = if has_ledger {
        conn.prepare(
            "SELECT id, session_id, created_timestamp, model, input_tokens, output_tokens,
                    cache_read_tokens, cache_write_tokens, cost, cost_source
             FROM usage_ledger WHERE id > ?1 ORDER BY id",
        )?
        .query_map([last_id], ledger_row)?
        .collect::<rusqlite::Result<_>>()?
    } else {
        Vec::new()
    };
    Ok(Some(Data { sessions, ledger }))
}

fn session_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
    let int = |i: usize| -> rusqlite::Result<i64> { Ok(r.get::<_, Option<i64>>(i)?.unwrap_or(0)) };
    let model_config: Option<String> = r.get(6)?;
    Ok(SessionRow {
        id: r.get(0)?,
        name: r.get(1)?,
        working_dir: r.get(2)?,
        created_at: r.get(3)?,
        updated_at: r.get(4)?,
        provider: r.get(5)?,
        model: model_config.as_deref().and_then(model_name),
        parent_id: r.get(7)?,
        accumulated: Usage::new(int(8)?, int(9)?, int(10)?, int(11)?),
        has_ledger: r.get(12)?,
    })
}

fn ledger_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<LedgerRow> {
    let int = |i: usize| -> rusqlite::Result<i64> { Ok(r.get::<_, Option<i64>>(i)?.unwrap_or(0)) };
    Ok(LedgerRow {
        id: r.get(0)?,
        session_id: r.get(1)?,
        created: r.get(2)?,
        model: r.get(3)?,
        usage: Usage::new(int(4)?, int(5)?, int(6)?, int(7)?),
        cost: r.get(8)?,
        cost_source: r.get(9)?,
    })
}

fn model_name(config: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(config).ok()?;
    value
        .get("model_name")?
        .as_str()
        .filter(|m| !m.trim().is_empty())
        .map(str::to_owned)
}

fn parse_sqlite_time(s: &str) -> Option<Timestamp> {
    s.parse::<Timestamp>().ok().or_else(|| {
        let dt = s.parse::<jiff::civil::DateTime>().ok()?;
        jiff::tz::Offset::UTC.to_timestamp(dt).ok()
    })
}

pub fn provider_for(goose_provider: &str, model: &str) -> String {
    if LOCAL_PROVIDERS.contains(&goose_provider) {
        return goose_provider.to_owned();
    }
    crate::models::known_provider(model)
        .unwrap_or(goose_provider)
        .to_owned()
}

fn cost_decimal(cost: f64) -> Option<tbt_core::Decimal> {
    if !cost.is_finite() || cost <= 0.0 {
        return None;
    }
    cost.to_string().parse().ok()
}

fn warning(src: &SourceRef, message: impl Into<String>) -> ParseWarning {
    ParseWarning::new(HARNESS, src, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_follow_goose_paths() {
        assert_eq!(
            roots_from(Some("/g".into()), Some("/d".into()), None, false),
            [PathBuf::from("/g/data/sessions")]
        );
        assert_eq!(
            roots_from(None, Some("/d".into()), None, false),
            [PathBuf::from("/d/goose/sessions")]
        );
        assert_eq!(
            roots_from(None, None, Some("C:/AppData".into()), true),
            [PathBuf::from("C:/AppData/Block/goose/data/sessions")]
        );
        assert_eq!(
            roots_from(Some("rel".into()), Some("rel".into()), None, false),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn sqlite_times_are_utc() {
        assert_eq!(
            parse_sqlite_time("2026-11-20 09:15:00"),
            "2026-11-20T09:15:00Z".parse().ok()
        );
        assert_eq!(
            parse_sqlite_time("2026-11-20T09:15:00+01:00"),
            "2026-11-20T08:15:00Z".parse().ok()
        );
    }

    #[test]
    fn local_providers_keep_their_name() {
        assert_eq!(provider_for("ollama", "qwen3-coder"), "ollama");
        assert_eq!(provider_for("databricks", "claude-sonnet-4-5"), "anthropic");
        assert_eq!(provider_for("openrouter", "z-ai/glm-4.6"), "openrouter");
    }

    #[test]
    fn input_excludes_cache() {
        let c = Usage::new(1000, 50, 600, 300).counts();
        assert_eq!((c.input, c.cache_read, c.cache_write_5m), (100, 600, 300));
    }
}
