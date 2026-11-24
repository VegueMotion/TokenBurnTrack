use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tbt_core::{
    Cursor, Decimal, Error, HarnessAdapter, HarnessId, ParseWarning, ReadBatch, Result,
    SessionUpsert, SourceKind, SourceRef, Timestamp, TokenCounts, UsageEvent,
};

use crate::discover;

const USAGE_FILE: &str = "usage.json";
const SUMMARY_FILE: &str = "summary.json";
const CWD_FILE: &str = ".cwd";
const UNKNOWN_MODEL: &str = "unknown";
const COST_TICK_SCALE: u32 = 10;

#[derive(Debug, Clone, Copy, Default)]
pub struct GrokBuildAdapter;

impl GrokBuildAdapter {
    pub fn new() -> GrokBuildAdapter {
        GrokBuildAdapter
    }
}

pub fn default_roots_from(grok_home: Option<OsString>, home: Option<PathBuf>) -> Vec<PathBuf> {
    match grok_home.filter(|d| !d.is_empty()) {
        Some(dir) => vec![PathBuf::from(dir).join("sessions")],
        None => home
            .map(|h| h.join(".grok").join("sessions"))
            .into_iter()
            .collect(),
    }
}

impl HarnessAdapter for GrokBuildAdapter {
    fn id(&self) -> HarnessId {
        HarnessId::GROK_BUILD
    }

    fn display_name(&self) -> &str {
        "Grok Build"
    }

    fn default_roots(&self) -> Vec<PathBuf> {
        default_roots_from(std::env::var_os("GROK_HOME"), std::env::home_dir())
    }

    fn discover(&self, roots: &[PathBuf]) -> Result<Vec<SourceRef>> {
        Ok(discover::files_with_extension(roots, "json")
            .into_iter()
            .filter(|p| p.file_name().is_some_and(|n| n == USAGE_FILE))
            .map(|p| SourceRef::new(p, SourceKind::JsonFile))
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

        let meta = std::fs::metadata(&src.path).map_err(|e| Error::io(&src.path, e))?;
        let size = meta.len();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| Timestamp::try_from(t).ok());
        if state.size == size
            && state.mtime.is_some()
            && state.mtime == mtime
            && warnings.is_empty()
        {
            return Ok(ReadBatch::unchanged(cursor.clone()));
        }

        let bytes = std::fs::read(&src.path).map_err(|e| Error::io(&src.path, e))?;
        let mut batch = ReadBatch {
            warnings,
            ..ReadBatch::default()
        };
        let mut next = State {
            size,
            mtime,
            ..state.clone()
        };

        let file = match serde_json::from_slice::<UsageFile>(&bytes) {
            Ok(file) => file,
            Err(e) => {
                batch
                    .warnings
                    .push(warning(src, format!("invalid JSON: {e}")));
                batch.next_cursor = Cursor::from_state(&next)?;
                return Ok(batch);
            }
        };

        let session_dir = src.path.parent().unwrap_or(Path::new(""));
        let summary = read_summary(session_dir, src, &mut batch.warnings);
        if summary
            .as_ref()
            .and_then(|s| s.session_kind.as_deref())
            .is_some_and(|k| k.starts_with("subagent"))
        {
            next.done = 0;
            next.last = None;
            batch.next_cursor = Cursor::from_state(&next)?;
            return Ok(batch);
        }

        let ext_id = file
            .session_id
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| summary.as_ref().and_then(|s| s.info.id.clone()))
            .unwrap_or_else(|| file_name(session_dir));
        let project_path = summary
            .as_ref()
            .and_then(|s| s.info.cwd.clone())
            .or_else(|| session_dir.parent().and_then(cwd_of_group));
        let git_branch = summary.as_ref().and_then(|s| s.head_branch.clone());

        let mut session = SessionUpsert::new(HarnessId::GROK_BUILD, ext_id.clone());
        session.project_path = project_path.clone();
        session.git_branch = git_branch.clone();
        if let Some(s) = &summary {
            session.title = s.generated_title.clone().filter(|t| !t.is_empty());
            session.started_at = s.created_at;
            session.ended_at = s.updated_at;
            session.resumed_from_ext_id = s
                .parent_session_id
                .clone()
                .filter(|p| !p.is_empty() && *p != ext_id);
        }

        let resumes = match &state.last {
            Some(last) => file
                .turns
                .get(state.done)
                .and_then(|t| Turn::deserialize(t).ok())
                .is_some_and(|t| t.turn_number == last.turn),
            None => state.done > 0 && file.turns.len() >= state.done,
        };
        let (start, prev_last) = if resumes {
            (state.done, state.last.clone())
        } else {
            (0, None)
        };

        next.done = file.turns.len();
        next.last = None;
        for (i, value) in file.turns.iter().enumerate().skip(start) {
            let turn = match Turn::deserialize(value) {
                Ok(turn) => turn,
                Err(e) => {
                    batch
                        .warnings
                        .push(warning(src, format!("turns[{i}]: {e}")));
                    continue;
                }
            };
            let ended_at = turn.ended_at.as_deref().unwrap_or("");
            let Ok(ts) = ended_at.parse::<Timestamp>() else {
                batch.warnings.push(warning(
                    src,
                    format!("turns[{i}] has no valid `endedAt`: {ended_at:?}"),
                ));
                continue;
            };
            session.started_at.get_or_insert(ts);
            session.ended_at = Some(session.ended_at.map_or(ts, |t| t.max(ts)));

            let prev = prev_last
                .as_ref()
                .filter(|l| l.turn == turn.turn_number)
                .map(|l| &l.emitted);
            let mut emitted = BTreeMap::new();
            for (model, row, raw) in turn.rows(value) {
                let tokens = row.tokens();
                let ticks = (!row.cost_is_partial)
                    .then_some(row.cost_usd_ticks)
                    .flatten();
                let before = prev.and_then(|p| p.get(&model));
                let delta = before.map_or(tokens, |b| tokens.delta_since(&b.tokens));
                let cost = match before {
                    None => ticks,
                    Some(b) => match (ticks, b.cost_ticks) {
                        (Some(now), Some(was)) if now > was => Some(now - was),
                        _ => None,
                    },
                };
                emitted.insert(
                    model.clone(),
                    Emitted {
                        tokens,
                        cost_ticks: ticks,
                    },
                );
                if delta.is_zero() {
                    continue;
                }
                batch.events.push(UsageEvent {
                    harness: HarnessId::GROK_BUILD,
                    dedup_key: format!("{ended_at}:{}:{model}", turn.turn_number),
                    session_ext_id: ext_id.clone(),
                    ts,
                    provider: provider_for(&model).to_owned(),
                    model,
                    input_tokens: delta.input,
                    output_tokens: delta.output,
                    cache_write_5m_tokens: delta.cache_write_5m,
                    cache_write_1h_tokens: delta.cache_write_1h,
                    cache_read_tokens: delta.cache_read,
                    reasoning_tokens: delta.reasoning,
                    reported_cost: cost.map(|t| Decimal::new(t, COST_TICK_SCALE)),
                    project_path: project_path.clone(),
                    git_branch: git_branch.clone(),
                    raw: serde_json::value::to_raw_value(raw).ok(),
                });
            }
            if i + 1 == file.turns.len() {
                next.done = i;
                next.last = Some(LastTurn {
                    turn: turn.turn_number,
                    emitted,
                });
            }
        }

        batch.sessions.push(session);
        batch.next_cursor = Cursor::from_state(&next)?;
        Ok(batch)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct State {
    size: u64,
    mtime: Option<Timestamp>,
    done: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    last: Option<LastTurn>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LastTurn {
    turn: u64,
    emitted: BTreeMap<String, Emitted>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Emitted {
    tokens: TokenCounts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cost_ticks: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct UsageFile {
    session_id: Option<String>,
    turns: Vec<Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Turn {
    turn_number: u64,
    ended_at: Option<String>,
    #[serde(flatten)]
    usage: Row,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Row {
    input_tokens: u64,
    output_tokens: u64,
    cached_read_tokens: u64,
    cache_creation_tokens: u64,
    reasoning_tokens: u64,
    cost_usd_ticks: Option<i64>,
    cost_is_partial: bool,
    primary_model_id: Option<String>,
    model_usage: Map<String, Value>,
}

impl Row {
    fn tokens(&self) -> TokenCounts {
        let cache_read = self.cached_read_tokens;
        let cache_write = self.cache_creation_tokens;
        TokenCounts {
            input: self
                .input_tokens
                .saturating_sub(cache_read.saturating_add(cache_write)),
            output: self.output_tokens,
            cache_write_5m: cache_write,
            cache_write_1h: 0,
            cache_read,
            reasoning: self.reasoning_tokens,
        }
    }
}

impl Turn {
    fn rows<'v>(&self, value: &'v Value) -> Vec<(String, Row, &'v Value)> {
        let model_usage = value.get("modelUsage").and_then(Value::as_object);
        let mut rows: Vec<_> = model_usage
            .into_iter()
            .flatten()
            .filter_map(|(model, v)| Some((model.clone(), Row::deserialize(v).ok()?, v)))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        if !rows.is_empty() || self.usage.tokens().is_zero() {
            return rows;
        }
        let model = self
            .usage
            .primary_model_id
            .clone()
            .unwrap_or_else(|| UNKNOWN_MODEL.to_owned());
        let row = Row {
            model_usage: Map::new(),
            primary_model_id: None,
            ..self.usage.clone()
        };
        vec![(model, row, value)]
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Summary {
    info: Info,
    generated_title: Option<String>,
    created_at: Option<Timestamp>,
    updated_at: Option<Timestamp>,
    parent_session_id: Option<String>,
    session_kind: Option<String>,
    head_branch: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Info {
    id: Option<String>,
    cwd: Option<String>,
}

fn read_summary(dir: &Path, src: &SourceRef, warnings: &mut Vec<ParseWarning>) -> Option<Summary> {
    let path = dir.join(SUMMARY_FILE);
    let bytes = std::fs::read(&path).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(summary) => Some(summary),
        Err(e) => {
            warnings.push(warning(src, format!("invalid {SUMMARY_FILE}: {e}")));
            None
        }
    }
}

fn cwd_of_group(group: &Path) -> Option<String> {
    if let Ok(text) = std::fs::read_to_string(group.join(CWD_FILE)) {
        let text = text.trim_end_matches(['\n', '\r']);
        if !text.is_empty() {
            return Some(text.to_owned());
        }
    }
    let name = group.file_name()?.to_str()?;
    percent_decode(name).filter(|p| p.contains(['/', '\\']))
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn provider_for(model: &str) -> &'static str {
    let m = model.to_ascii_lowercase();
    if m.starts_with("claude") {
        "anthropic"
    } else if m.starts_with("gpt-")
        || m.starts_with("o1")
        || m.starts_with("o3")
        || m.starts_with("o4")
    {
        "openai"
    } else if m.starts_with("gemini") {
        "google"
    } else {
        "xai"
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn warning(src: &SourceRef, message: impl Into<String>) -> ParseWarning {
    ParseWarning::new(HarnessId::GROK_BUILD, src, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_roots_prefer_grok_home() {
        assert_eq!(
            default_roots_from(Some("/g".into()), Some("/h".into())),
            [PathBuf::from("/g/sessions")]
        );
        assert_eq!(
            default_roots_from(Some("".into()), Some("/h".into())),
            [PathBuf::from("/h/.grok/sessions")]
        );
        assert!(default_roots_from(None, None).is_empty());
    }

    #[test]
    fn decodes_url_encoded_cwd() {
        assert_eq!(
            percent_decode("%2Fhome%2Fu%2Fmy%20repo").as_deref(),
            Some("/home/u/my repo")
        );
        assert_eq!(percent_decode("bad%2"), None);
    }

    #[test]
    fn provider_follows_model_family() {
        assert_eq!(provider_for("grok-4.5"), "xai");
        assert_eq!(provider_for("claude-sonnet-4-5"), "anthropic");
        assert_eq!(provider_for("gpt-5.5"), "openai");
        assert_eq!(provider_for("gemini-2.5-pro"), "google");
    }
}
