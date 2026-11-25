use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
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

const HARNESS: HarnessId = HarnessId::ZED;

const DB_FILE: &str = "threads.db";
const FLATPAK_DATA: &str = ".var/app/dev.zed.Zed/data/zed";
const UNKNOWN_MODEL: &str = "unknown";
const LOCAL_PROVIDERS: [&str; 3] = ["lmstudio", "ollama", "llama.cpp"];
const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy)]
pub struct ZedAdapter {
    busy_timeout: Duration,
}

impl Default for ZedAdapter {
    fn default() -> Self {
        ZedAdapter::new()
    }
}

impl ZedAdapter {
    pub fn new() -> ZedAdapter {
        ZedAdapter {
            busy_timeout: DEFAULT_BUSY_TIMEOUT,
        }
    }

    pub fn with_busy_timeout(mut self, timeout: Duration) -> ZedAdapter {
        self.busy_timeout = timeout;
        self
    }
}

pub fn linux_roots_from(data_home: Option<PathBuf>, home: Option<PathBuf>) -> Vec<PathBuf> {
    let native = data_home
        .filter(|d| d.is_absolute())
        .map(|d| d.join("zed").join("threads"));
    let flatpak = home.map(|h| h.join(FLATPAK_DATA).join("threads"));
    native.into_iter().chain(flatpak).collect()
}

fn default_roots() -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        std::env::home_dir()
            .map(|h| h.join("Library/Application Support/Zed/threads"))
            .into_iter()
            .collect()
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .filter(|v: &OsString| !v.is_empty())
            .map(|d| PathBuf::from(d).join("Zed").join("threads"))
            .into_iter()
            .collect()
    } else {
        linux_roots_from(
            tbt_core::sandbox::host_xdg_home(tbt_core::sandbox::XdgDir::Data),
            std::env::home_dir(),
        )
    }
}

impl HarnessAdapter for ZedAdapter {
    fn id(&self) -> HarnessId {
        HARNESS
    }

    fn display_name(&self) -> &str {
        "Zed Agent"
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

impl ZedAdapter {
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
        let mut reader = Reader {
            src,
            conn: &tx,
            batch: ReadBatch::default(),
        };
        let state = reader.read(state)?;
        let batch = reader.batch;
        tx.rollback()?;
        Ok((batch, state))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct State {
    threads: BTreeMap<String, Usage>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_read_input_tokens: u64,
}

impl Usage {
    fn counts(&self) -> TokenCounts {
        TokenCounts {
            input: self.input_tokens,
            output: self.output_tokens,
            cache_write_5m: self.cache_creation_input_tokens,
            cache_write_1h: 0,
            cache_read: self.cache_read_input_tokens,
            reasoning: 0,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ThreadData {
    cumulative_token_usage: Usage,
    model: Option<ThreadModel>,
}

#[derive(Debug, Deserialize)]
struct ThreadModel {
    provider: String,
    model: String,
}

struct Row {
    id: String,
    parent_id: Option<String>,
    folder_paths: Option<String>,
    folder_paths_order: Option<String>,
    summary: Option<String>,
    updated_at: String,
    created_at: Option<String>,
    data_type: String,
    data: Vec<u8>,
}

struct Reader<'a> {
    src: &'a SourceRef,
    conn: &'a Connection,
    batch: ReadBatch,
}

impl Reader<'_> {
    fn warn(&mut self, message: impl Into<String>) {
        self.batch.warnings.push(warning(self.src, message));
    }

    fn read(&mut self, state: &State) -> rusqlite::Result<State> {
        if !table_exists(self.conn, "threads")? {
            return Ok(state.clone());
        }
        let sql = self.row_sql()?;
        let conn = self.conn;
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        let mut next = State::default();
        while let Some(r) = rows.next()? {
            let row = row(r)?;
            let prev = state.threads.get(&row.id).copied().unwrap_or_default();
            let id = row.id.clone();
            next.threads.insert(id, self.thread(row, prev));
        }
        Ok(next)
    }

    fn row_sql(&self) -> rusqlite::Result<String> {
        let col = |c: &'static str| -> rusqlite::Result<&'static str> {
            let has: bool = self.conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM pragma_table_info('threads') WHERE name = ?1)",
                [c],
                |r| r.get(0),
            )?;
            Ok(if has { c } else { "NULL" })
        };
        Ok(format!(
            "SELECT id, {}, {}, {}, summary, updated_at, {}, data_type, data
             FROM threads ORDER BY updated_at, id",
            col("parent_id")?,
            col("folder_paths")?,
            col("folder_paths_order")?,
            col("created_at")?,
        ))
    }

    fn thread(&mut self, row: Row, prev: Usage) -> Usage {
        let data = match decode(&row.data_type, &row.data) {
            Ok(data) => data,
            Err(e) => {
                self.warn(format!("thread {}: {e}; skipped", row.id));
                return prev;
            }
        };
        let usage = data.cumulative_token_usage;
        let delta = usage.counts().delta_since(&prev.counts());
        if delta.is_zero() {
            return usage;
        }
        let Ok(ts) = row.updated_at.parse::<Timestamp>() else {
            self.warn(format!(
                "thread {}: invalid `updated_at` {:?}; skipped",
                row.id, row.updated_at
            ));
            return prev;
        };
        let (provider, model) = match &data.model {
            Some(m) if !m.model.trim().is_empty() => {
                (provider_for(&m.provider, &m.model), m.model.clone())
            }
            _ => {
                self.warn(format!("thread {}: usage but no model", row.id));
                ("zed".to_owned(), UNKNOWN_MODEL.to_owned())
            }
        };
        let project_path = row
            .folder_paths
            .as_deref()
            .and_then(|p| first_folder(p, row.folder_paths_order.as_deref()));
        let total = usage.counts().total();
        let raw = json!({
            "cumulativeTokenUsage": usage,
            "provider": data.model.as_ref().map(|m| &m.provider),
            "model": data.model.as_ref().map(|m| &m.model),
        });
        let mut event = UsageEvent {
            harness: HARNESS,
            dedup_key: format!("{}:{total}", row.id),
            session_ext_id: row.id.clone(),
            ts,
            provider,
            model,
            input_tokens: 0,
            output_tokens: 0,
            cache_write_5m_tokens: 0,
            cache_write_1h_tokens: 0,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
            reported_cost: None,
            project_path: project_path.clone(),
            git_branch: None,
            raw: serde_json::value::to_raw_value(&raw).ok(),
        };
        event.set_tokens(delta);
        self.batch.events.push(event);

        let mut session = SessionUpsert::new(HARNESS, row.id);
        session.project_path = project_path;
        session.title = row.summary.filter(|s| !s.trim().is_empty());
        session.started_at = row.created_at.and_then(|c| c.parse().ok());
        session.parent_ext_id = row.parent_id.filter(|p| !p.is_empty());
        self.batch.sessions.push(session);
        usage
    }
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok(Row {
        id: r.get(0)?,
        parent_id: r.get(1)?,
        folder_paths: r.get(2)?,
        folder_paths_order: r.get(3)?,
        summary: r.get(4)?,
        updated_at: r.get(5)?,
        created_at: r.get(6)?,
        data_type: r.get(7)?,
        data: r
            .get_ref(8)?
            .as_bytes_or_null()?
            .unwrap_or_default()
            .to_vec(),
    })
}

fn decode(data_type: &str, data: &[u8]) -> std::result::Result<ThreadData, String> {
    let json = match data_type {
        "json" => data.to_vec(),
        "zstd" => {
            let mut out = Vec::new();
            ruzstd::decoding::StreamingDecoder::new(data)
                .map_err(|e| format!("bad zstd data: {e}"))?
                .read_to_end(&mut out)
                .map_err(|e| format!("bad zstd data: {e}"))?;
            out
        }
        other => return Err(format!("unknown data type {other:?}")),
    };
    serde_json::from_slice(&json).map_err(|e| format!("bad thread JSON: {e}"))
}

pub fn provider_for(zed_provider: &str, model: &str) -> String {
    if LOCAL_PROVIDERS.contains(&zed_provider) {
        return zed_provider.to_owned();
    }
    match crate::models::known_provider(model) {
        Some(p) => p.to_owned(),
        None if zed_provider == "x_ai" => "xai".to_owned(),
        None => zed_provider.to_owned(),
    }
}

fn first_folder(paths: &str, order: Option<&str>) -> Option<String> {
    let paths: Vec<&str> = paths.split('\n').filter(|p| !p.is_empty()).collect();
    let order: Vec<usize> = order
        .unwrap_or_default()
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    let first = if order.len() == paths.len() {
        order.iter().position(|&i| i == 0)
    } else {
        None
    };
    paths.get(first.unwrap_or(0)).map(|p| (*p).to_owned())
}

fn warning(src: &SourceRef, message: impl Into<String>) -> ParseWarning {
    ParseWarning::new(HARNESS, src, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_roots_cover_native_and_flatpak() {
        assert_eq!(
            linux_roots_from(Some("/d".into()), Some("/home/u".into())),
            [
                PathBuf::from("/d/zed/threads"),
                PathBuf::from("/home/u/.var/app/dev.zed.Zed/data/zed/threads"),
            ]
        );
        assert_eq!(
            linux_roots_from(Some("rel".into()), None),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn local_providers_keep_their_name() {
        assert_eq!(provider_for("lmstudio", "deepseek-r1"), "lmstudio");
        assert_eq!(provider_for("zed.dev", "claude-sonnet-4-5"), "anthropic");
        assert_eq!(provider_for("openrouter", "z-ai/glm-4.6"), "openrouter");
        assert_eq!(provider_for("x_ai", "code-fast-1"), "xai");
    }

    #[test]
    fn first_folder_follows_the_user_order() {
        assert_eq!(first_folder("/a\n/b", Some("1,0")).as_deref(), Some("/b"));
        assert_eq!(first_folder("/a\n/b", Some("0,1")).as_deref(), Some("/a"));
        assert_eq!(first_folder("/a\n/b", None).as_deref(), Some("/a"));
        assert_eq!(first_folder("", Some("")), None);
    }
}
