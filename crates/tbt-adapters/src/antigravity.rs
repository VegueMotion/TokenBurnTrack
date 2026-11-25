use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tbt_core::{
    Cursor, Error, HarnessAdapter, HarnessId, ParseWarning, ReadBatch, Result, SessionUpsert,
    SourceKind, SourceRef, Timestamp, TokenCounts, UsageEvent,
};

use crate::proto;
use crate::sqlite::{is_busy, is_read_only, open_read_only, table_exists, wal_path};
use crate::uri::file_uri_to_path;

const HARNESS: HarnessId = HarnessId::ANTIGRAVITY;

const DIRS: [&str; 3] = ["antigravity", "antigravity-insiders", "antigravity-cli"];
const PLANNER_RESPONSE: i64 = 15;
const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy)]
pub struct AntigravityAdapter {
    busy_timeout: Duration,
    stale_after: Duration,
}

impl Default for AntigravityAdapter {
    fn default() -> Self {
        AntigravityAdapter::new()
    }
}

impl AntigravityAdapter {
    pub fn new() -> AntigravityAdapter {
        AntigravityAdapter {
            busy_timeout: DEFAULT_BUSY_TIMEOUT,
            stale_after: DEFAULT_STALE_AFTER,
        }
    }

    pub fn with_busy_timeout(mut self, timeout: Duration) -> AntigravityAdapter {
        self.busy_timeout = timeout;
        self
    }

    pub fn with_stale_after(mut self, after: Duration) -> AntigravityAdapter {
        self.stale_after = after;
        self
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
    DIRS.iter()
        .map(|d| base.join(".gemini").join(d).join("conversations"))
        .collect()
}

impl HarnessAdapter for AntigravityAdapter {
    fn id(&self) -> HarnessId {
        HARNESS
    }

    fn display_name(&self) -> &str {
        "Antigravity"
    }

    fn default_roots(&self) -> Vec<PathBuf> {
        default_roots_from(std::env::var_os("GEMINI_CLI_HOME"), std::env::home_dir())
    }

    fn discover(&self, roots: &[PathBuf]) -> Result<Vec<SourceRef>> {
        Ok(crate::discover::sources_with_extension(
            roots,
            "db",
            SourceKind::Sqlite,
        ))
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
        let now = Timestamp::now();
        match self.read_db(src, state, now) {
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

impl AntigravityAdapter {
    fn read_db(
        &self,
        src: &SourceRef,
        state: State,
        now: Timestamp,
    ) -> rusqlite::Result<(ReadBatch, State)> {
        match self.read_db_with(src, state.clone(), now, false) {
            Err(e) if is_read_only(&e) && !wal_path(&src.path).exists() => {
                self.read_db_with(src, state, now, true)
            }
            other => other,
        }
    }

    fn read_db_with(
        &self,
        src: &SourceRef,
        state: State,
        now: Timestamp,
        immutable: bool,
    ) -> rusqlite::Result<(ReadBatch, State)> {
        let conn = open_read_only(&src.path, immutable)?;
        conn.busy_timeout(self.busy_timeout)?;
        let tx = conn.unchecked_transaction()?;
        let mut reader = Reader {
            src,
            conn: &tx,
            now,
            stale_after: self.stale_after,
            session_id: session_id(src),
            steps: None,
            events: Vec::new(),
            warnings: Vec::new(),
        };
        let (state, session) = reader.read(state)?;
        let batch = ReadBatch {
            sessions: session.into_iter().collect(),
            events: reader.events,
            messages: Vec::new(),
            next_cursor: Cursor::empty(),
            warnings: reader.warnings,
        };
        tx.rollback()?;
        Ok((batch, state))
    }
}

fn session_id(src: &SourceRef) -> String {
    src.path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct State {
    next_idx: i64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    done: Vec<i64>,
}

#[derive(Debug, Default)]
struct Trajectory {
    created_at: Option<Timestamp>,
    project_path: Option<String>,
}

#[derive(Debug, Default)]
struct Steps {
    by_response: HashMap<String, Timestamp>,
    by_index: HashMap<u64, Timestamp>,
}

#[derive(Debug)]
struct Generation {
    response_id: Option<String>,
    model: Option<String>,
    counts: TokenCounts,
    raw: serde_json::Value,
    ts: Option<Timestamp>,
}

impl Generation {
    fn parse(data: &[u8]) -> Generation {
        let chat = proto::bytes(data, 1).unwrap_or_default();
        let usage = proto::bytes(chat, 4).unwrap_or_default();
        let field = |n| proto::varint(usage, n).unwrap_or(0);
        let (input, total_output, cache_write, cache_read, thinking, response) =
            (field(2), field(3), field(4), field(5), field(9), field(10));
        let output = if total_output > 0 {
            total_output
        } else {
            thinking + response
        };
        let label = non_empty(proto::string(chat, 21));
        let response_model = non_empty(proto::string(chat, 19));
        let model = label
            .as_deref()
            .and_then(model_from_label)
            .or_else(|| response_model.clone());
        let response_id = non_empty(proto::string(usage, 11));
        let ts = proto::bytes(data, 9)
            .and_then(|m| proto::bytes(m, 4))
            .and_then(proto::timestamp);
        Generation {
            raw: json!({
                "responseId": response_id,
                "label": label,
                "responseModel": response_model,
                "inputTokens": input,
                "outputTokens": total_output,
                "cacheWriteTokens": cache_write,
                "cacheReadTokens": cache_read,
                "thinkingOutputTokens": thinking,
                "responseOutputTokens": response,
            }),
            response_id,
            model,
            counts: TokenCounts {
                input,
                output,
                cache_write_5m: cache_write,
                cache_write_1h: 0,
                cache_read,
                reasoning: thinking.min(output),
            },
            ts,
        }
    }
}

enum Step {
    Done,
    Held,
}

struct Reader<'a> {
    src: &'a SourceRef,
    conn: &'a Connection,
    now: Timestamp,
    stale_after: Duration,
    session_id: String,
    steps: Option<Steps>,
    events: Vec<UsageEvent>,
    warnings: Vec<ParseWarning>,
}

impl Reader<'_> {
    fn warn(&mut self, message: impl Into<String>) {
        self.warnings.push(warning(self.src, message));
    }

    fn read(&mut self, mut state: State) -> rusqlite::Result<(State, Option<SessionUpsert>)> {
        if !table_exists(self.conn, "gen_metadata")? {
            return Ok((state, None));
        }
        let max_idx: i64 =
            self.conn
                .query_row("SELECT coalesce(max(idx), -1) FROM gen_metadata", [], |r| {
                    r.get(0)
                })?;
        if max_idx + 1 < state.next_idx {
            state = State::default();
        }
        let done: HashSet<i64> = state.done.iter().copied().collect();
        let mut stmt = self
            .conn
            .prepare("SELECT idx, data FROM gen_metadata WHERE idx >= ?1 ORDER BY idx")?;
        let mut rows = stmt.query(params![state.next_idx])?;
        let mut next = State {
            next_idx: state.next_idx,
            done: Vec::new(),
        };
        let trajectory = self.trajectory()?;
        let mut blocked = false;
        while let Some(row) = rows.next()? {
            let idx: i64 = row.get(0)?;
            let data = row.get_ref(1)?.as_bytes_or_null()?.map(<[u8]>::to_vec);
            let step = if done.contains(&idx) {
                Step::Done
            } else {
                self.generation(idx, data.as_deref(), &trajectory)?
            };
            match step {
                Step::Held => blocked = true,
                Step::Done if blocked => next.done.push(idx),
                Step::Done => next.next_idx = idx + 1,
            }
        }
        let session = (!self.events.is_empty()).then(|| {
            let mut s = SessionUpsert::new(HARNESS, self.session_id.clone());
            s.project_path = trajectory.project_path.clone();
            s.started_at = trajectory.created_at;
            s
        });
        Ok((next, session))
    }

    fn trajectory(&self) -> rusqlite::Result<Trajectory> {
        if !table_exists(self.conn, "trajectory_metadata_blob")? {
            return Ok(Trajectory::default());
        }
        let data: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT data FROM trajectory_metadata_blob LIMIT 1",
                [],
                |r| Ok(r.get_ref(0)?.as_bytes_or_null()?.map(<[u8]>::to_vec)),
            )
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                e => Err(e),
            })?;
        let Some(data) = data else {
            return Ok(Trajectory::default());
        };
        Ok(Trajectory {
            created_at: proto::bytes(&data, 2).and_then(proto::timestamp),
            project_path: proto::bytes(&data, 1)
                .and_then(|w| proto::string(w, 1))
                .and_then(file_uri_to_path),
        })
    }

    fn steps(&mut self) -> rusqlite::Result<&Steps> {
        if self.steps.is_none() {
            let mut steps = Steps::default();
            if table_exists(self.conn, "steps")? {
                let mut stmt = self.conn.prepare(
                    "SELECT metadata FROM steps WHERE step_type = ?1 AND metadata IS NOT NULL",
                )?;
                let mut rows = stmt.query(params![PLANNER_RESPONSE])?;
                while let Some(row) = rows.next()? {
                    let Some(meta) = row.get_ref(0)?.as_bytes_or_null()? else {
                        continue;
                    };
                    let Some(ts) = proto::bytes(meta, 1).and_then(proto::timestamp) else {
                        continue;
                    };
                    if let Some(id) = proto::bytes(meta, 9).and_then(|m| proto::string(m, 11)) {
                        steps.by_response.entry(id.to_owned()).or_insert(ts);
                    }
                    if let Some(i) = proto::bytes(meta, 20).and_then(|m| proto::varint(m, 3)) {
                        steps.by_index.entry(i).or_insert(ts);
                    }
                }
            }
            self.steps = Some(steps);
        }
        Ok(self.steps.get_or_insert_default())
    }

    fn timestamp(
        &mut self,
        idx: i64,
        g: &Generation,
        trajectory: &Trajectory,
    ) -> rusqlite::Result<Option<Timestamp>> {
        if g.ts.is_some() {
            return Ok(g.ts);
        }
        let steps = self.steps()?;
        let from_steps = g
            .response_id
            .as_ref()
            .and_then(|id| steps.by_response.get(id))
            .or_else(|| u64::try_from(idx).ok().and_then(|i| steps.by_index.get(&i)))
            .copied();
        Ok(from_steps.or(trajectory.created_at))
    }

    fn is_fresh(&self, ts: Option<Timestamp>) -> bool {
        let Some(ts) = ts else {
            return false;
        };
        let age = self.now.duration_since(ts);
        age.as_secs() < 0 || age.unsigned_abs() < self.stale_after
    }

    fn generation(
        &mut self,
        idx: i64,
        data: Option<&[u8]>,
        trajectory: &Trajectory,
    ) -> rusqlite::Result<Step> {
        let Some(data) = data else {
            return Ok(Step::Done);
        };
        let g = Generation::parse(data);
        let ts = self.timestamp(idx, &g, trajectory)?;
        if g.counts.is_zero() {
            return Ok(if self.is_fresh(ts) {
                Step::Held
            } else {
                Step::Done
            });
        }
        let Some(response_id) = g.response_id.clone() else {
            self.warn(format!(
                "generation {idx} has tokens but no response id; skipped"
            ));
            return Ok(Step::Done);
        };
        let Some(model) = g.model.clone() else {
            self.warn(format!(
                "generation {idx} has tokens but no model name; skipped"
            ));
            return Ok(Step::Done);
        };
        let Some(ts) = ts.or_else(|| file_mtime(self.src)) else {
            self.warn(format!("generation {idx}: no valid timestamp; skipped"));
            return Ok(Step::Done);
        };
        let mut event = UsageEvent {
            harness: HARNESS,
            dedup_key: response_id,
            session_ext_id: self.session_id.clone(),
            ts,
            provider: provider_for(&model).to_owned(),
            model,
            input_tokens: 0,
            output_tokens: 0,
            cache_write_5m_tokens: 0,
            cache_write_1h_tokens: 0,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
            reported_cost: None,
            project_path: trajectory.project_path.clone(),
            git_branch: None,
            raw: serde_json::value::to_raw_value(&g.raw).ok(),
        };
        event.set_tokens(g.counts);
        self.events.push(event);
        Ok(Step::Done)
    }
}

fn file_mtime(src: &SourceRef) -> Option<Timestamp> {
    let modified = std::fs::metadata(&src.path).ok()?.modified().ok()?;
    Timestamp::try_from(modified).ok()
}

pub fn model_from_label(label: &str) -> Option<String> {
    let base = match label.trim().rsplit_once(" (") {
        Some((base, rest)) if rest.ends_with(')') => base,
        _ => label.trim(),
    };
    let slug = base
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
        .to_ascii_lowercase();
    if slug.is_empty() {
        None
    } else if slug.starts_with("claude-") {
        Some(slug.replace('.', "-"))
    } else {
        Some(slug)
    }
}

pub fn provider_for(model: &str) -> &'static str {
    crate::models::known_provider(model).unwrap_or("antigravity")
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn warning(src: &SourceRef, message: impl Into<String>) -> ParseWarning {
    ParseWarning::new(HARNESS, src, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_roots_cover_app_insiders_and_cli() {
        let home = Some(PathBuf::from("/home/u"));
        assert_eq!(
            default_roots_from(None, home.clone()),
            [
                PathBuf::from("/home/u/.gemini/antigravity/conversations"),
                PathBuf::from("/home/u/.gemini/antigravity-insiders/conversations"),
                PathBuf::from("/home/u/.gemini/antigravity-cli/conversations"),
            ]
        );
        assert_eq!(
            default_roots_from(Some("/g".into()), home)[0],
            PathBuf::from("/g/.gemini/antigravity/conversations")
        );
        assert!(default_roots_from(None, None).is_empty());
    }

    #[test]
    fn labels_become_api_model_names() {
        assert_eq!(
            model_from_label("Gemini 3.6 Flash (High)").as_deref(),
            Some("gemini-3.6-flash")
        );
        assert_eq!(
            model_from_label("Claude Sonnet 4.5 (Thinking)").as_deref(),
            Some("claude-sonnet-4-5")
        );
        assert_eq!(
            model_from_label("GPT-OSS 120B").as_deref(),
            Some("gpt-oss-120b")
        );
        assert_eq!(model_from_label("  "), None);
    }
}
