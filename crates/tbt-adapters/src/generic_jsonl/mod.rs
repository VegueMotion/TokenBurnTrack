mod config;
mod glob;
mod path;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use tbt_core::jsonl::{FileCursor, JsonlLine, JsonlReader, ReadStart};
use tbt_core::{
    Cursor, Decimal, HarnessAdapter, HarnessId, ParseWarning, ReadBatch, Result, SessionUpsert,
    SourceKind, SourceRef, Timestamp, TokenCounts, UsageEvent,
};

pub use config::{ConfigError, ConfigWarning, DEFAULT_GLOB, GenericJsonlConfig, TimestampFormat};
pub use glob::GlobError;
pub use path::{JsonPath, PathError, Segment};

use config::Compiled;
use path::{as_decimal, as_u64};

const MAX_VALUE_IN_WARNING: usize = 80;
const MAX_UNIX_SECONDS: i64 = 1_000_000_000_000;

#[derive(Debug, Clone)]
pub struct GenericJsonlAdapter {
    config: GenericJsonlConfig,
    id: HarnessId,
    display_name: String,
    compiled: Compiled,
    warnings: Vec<ConfigWarning>,
}

impl GenericJsonlAdapter {
    pub fn new(config: GenericJsonlConfig) -> std::result::Result<Self, ConfigError> {
        let (compiled, warnings) = config.compile()?;
        for w in &warnings {
            tracing::warn!(harness = %config.id, "generic JSONL config: {w}");
        }
        let display_name = match config.display_name.trim() {
            "" => config.id.clone(),
            name => name.to_owned(),
        };
        Ok(GenericJsonlAdapter {
            id: HarnessId::new(config.id.clone()),
            display_name,
            compiled,
            warnings,
            config,
        })
    }

    pub fn config(&self) -> &GenericJsonlConfig {
        &self.config
    }

    pub fn config_warnings(&self) -> &[ConfigWarning] {
        &self.warnings
    }
}

impl HarnessAdapter for GenericJsonlAdapter {
    fn id(&self) -> HarnessId {
        self.id.clone()
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }

    fn default_roots(&self) -> Vec<PathBuf> {
        let home = std::env::home_dir();
        self.config
            .roots
            .iter()
            .map(|r| expand_home(r, home.as_deref()))
            .collect()
    }

    fn discover(&self, roots: &[PathBuf]) -> Result<Vec<SourceRef>> {
        Ok(self
            .compiled
            .glob
            .files(roots)
            .into_iter()
            .map(|p| SourceRef::new(p, SourceKind::JsonlFile))
            .collect())
    }

    fn read(&self, src: &SourceRef, cursor: &Cursor) -> Result<ReadBatch> {
        let mut warnings = Vec::new();
        let file = match cursor.state::<FileCursor>() {
            Ok(state) => state.unwrap_or_default(),
            Err(e) => {
                warnings.push(self.warning(src, format!("{e}; reading from the start")));
                FileCursor::default()
            }
        };
        let mut reader = JsonlReader::open(&src.path, &file)?;
        if reader.start() == ReadStart::Unchanged {
            return Ok(ReadBatch::unchanged(cursor.clone()));
        }

        let mut batch = Builder::new(self, src);
        batch.warnings = warnings;
        for line in reader.by_ref() {
            match line {
                Ok(line) => batch.line(&line),
                Err(e) => {
                    batch.warnings.push(self.warning(src, e.to_string()));
                    break;
                }
            }
        }
        Ok(ReadBatch {
            sessions: batch.sessions,
            events: batch.events,
            messages: Vec::new(),
            next_cursor: reader.into_cursor().to_cursor()?,
            warnings: batch.warnings,
        })
    }
}

impl GenericJsonlAdapter {
    fn warning(&self, src: &SourceRef, message: impl Into<String>) -> ParseWarning {
        ParseWarning::new(self.id.clone(), src, message)
    }
}

fn expand_home(root: &Path, home: Option<&Path>) -> PathBuf {
    let (Some(home), Some(text)) = (home, root.to_str()) else {
        return root.to_path_buf();
    };
    match text.strip_prefix('~') {
        Some("") => home.to_path_buf(),
        Some(rest) if rest.starts_with(['/', '\\']) => home.join(&rest[1..]),
        _ => root.to_path_buf(),
    }
}

struct Builder<'a> {
    adapter: &'a GenericJsonlAdapter,
    src: &'a SourceRef,
    file_session: String,
    sessions: Vec<SessionUpsert>,
    session_index: HashMap<String, usize>,
    events: Vec<UsageEvent>,
    warnings: Vec<ParseWarning>,
    emitted: HashSet<String>,
}

impl<'a> Builder<'a> {
    fn new(adapter: &'a GenericJsonlAdapter, src: &'a SourceRef) -> Builder<'a> {
        let file_session = src
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        Builder {
            adapter,
            src,
            file_session,
            sessions: Vec::new(),
            session_index: HashMap::new(),
            events: Vec::new(),
            warnings: Vec::new(),
            emitted: HashSet::new(),
        }
    }

    fn warn(&mut self, line: &JsonlLine, message: impl Into<String>) {
        let w = self
            .adapter
            .warning(self.src, message)
            .at(line.line_no, line.offset);
        self.warnings.push(w);
    }

    fn line(&mut self, line: &JsonlLine) {
        let value: Value = match serde_json::from_str(&line.text) {
            Ok(v) => v,
            Err(e) => return self.warn(line, format!("invalid JSON: {e}")),
        };
        let c = &self.adapter.compiled;
        if c.filter.as_ref().is_some_and(|f| !f.matches(&value)) {
            return;
        }
        match extract(c, &value) {
            Ok(Some(event)) => self.push(event),
            Ok(None) => {}
            Err(message) => self.warn(line, message),
        }
    }

    fn push(&mut self, mut e: Extracted) {
        if !self.emitted.insert(e.dedup_key.clone()) {
            return;
        }
        let session_ext_id = e
            .session_id
            .take()
            .unwrap_or_else(|| self.file_session.clone());
        let idx = match self.session_index.get(&session_ext_id) {
            Some(&idx) => idx,
            None => {
                let idx = self.sessions.len();
                self.sessions.push(SessionUpsert::new(
                    self.adapter.id.clone(),
                    session_ext_id.clone(),
                ));
                self.session_index.insert(session_ext_id.clone(), idx);
                idx
            }
        };
        let s = &mut self.sessions[idx];
        s.started_at = Some(s.started_at.map_or(e.ts, |t| t.min(e.ts)));
        s.ended_at = Some(s.ended_at.map_or(e.ts, |t| t.max(e.ts)));
        if e.project_path.is_some() {
            s.project_path.clone_from(&e.project_path);
        }
        if e.git_branch.is_some() {
            s.git_branch.clone_from(&e.git_branch);
        }

        let mut event = UsageEvent {
            harness: self.adapter.id.clone(),
            dedup_key: e.dedup_key,
            session_ext_id,
            ts: e.ts,
            provider: e.provider,
            model: e.model,
            input_tokens: 0,
            output_tokens: 0,
            cache_write_5m_tokens: 0,
            cache_write_1h_tokens: 0,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
            reported_cost: e.reported_cost,
            project_path: e.project_path,
            git_branch: e.git_branch,
            raw: serde_json::value::to_raw_value(&e.raw).ok(),
        };
        event.set_tokens(e.tokens);
        self.events.push(event);
    }
}

struct Extracted {
    dedup_key: String,
    session_id: Option<String>,
    ts: Timestamp,
    provider: String,
    model: String,
    tokens: TokenCounts,
    reported_cost: Option<Decimal>,
    project_path: Option<String>,
    git_branch: Option<String>,
    raw: Map<String, Value>,
}

fn extract(c: &Compiled, line: &Value) -> std::result::Result<Option<Extracted>, String> {
    let mut raw = Map::new();
    let mut missing = Vec::new();

    let ts_value = c.timestamp.get_present(line);
    if ts_value.is_none() {
        missing.push(format!("`timestamp` ({})", c.timestamp));
    }
    let provider = c.provider.eval(line);
    if provider.is_none() {
        missing.push(format!("`provider` ({})", c.provider.describe()));
    }
    let model = c.model.eval(line);
    if model.is_none() {
        missing.push(format!("`model` ({})", c.model.describe()));
    }
    let dedup_key = c.dedup_key.render(line).map_err(|paths| {
        for p in paths {
            missing.push(format!("`dedup_key` field ({p})"));
        }
    });
    let (Some(ts_value), Some(provider), Some(model), Ok(dedup_key)) =
        (ts_value, provider, model, dedup_key)
    else {
        return Err(format!("missing required field(s): {}", missing.join(", ")));
    };
    let ts = parse_timestamp(ts_value, c.timestamp_format).ok_or_else(|| {
        format!(
            "invalid timestamp {} at {} (expected {})",
            show(ts_value),
            c.timestamp,
            c.timestamp_format.as_str()
        )
    })?;
    raw.insert("timestamp".into(), ts_value.clone());
    raw.insert("model".into(), Value::String(model.clone()));
    raw.insert("provider".into(), Value::String(provider.clone()));

    let mut counts = [0u64; 6];
    for (slot, (field, path)) in counts.iter_mut().zip(c.tokens.all()) {
        let Some(path) = path else { continue };
        let Some(v) = path.get_present(line) else {
            continue;
        };
        *slot = as_u64(v).ok_or_else(|| {
            format!(
                "`{field}` at {path}: expected a non-negative whole number, found {}",
                show(v)
            )
        })?;
        raw.insert(field.into(), v.clone());
    }
    let [
        input,
        output,
        cache_write_5m,
        cache_write_1h,
        cache_read,
        reasoning,
    ] = counts;
    let tokens = TokenCounts {
        input: if c.input_includes_cache_read {
            input.saturating_sub(cache_read)
        } else {
            input
        },
        output,
        cache_write_5m,
        cache_write_1h,
        cache_read,
        reasoning,
    };

    let reported_cost = match c.reported_cost.as_ref().and_then(|p| p.get_present(line)) {
        None => None,
        Some(v) => {
            let cost = as_decimal(v).ok_or_else(|| {
                format!(
                    "`reported_cost` at {}: expected a number, found {}",
                    c.reported_cost.as_ref().map_or("", JsonPath::as_str),
                    show(v)
                )
            })?;
            raw.insert("reported_cost".into(), v.clone());
            Some(cost)
        }
    };
    if tokens.is_zero() && reported_cost.is_none_or(|c| c.is_zero()) {
        return Ok(None);
    }

    Ok(Some(Extracted {
        dedup_key,
        session_id: c.session_id.as_ref().and_then(|t| t.eval(line)),
        ts,
        provider,
        model,
        tokens,
        reported_cost,
        project_path: c.project_path.as_ref().and_then(|t| t.eval(line)),
        git_branch: c.git_branch.as_ref().and_then(|t| t.eval(line)),
        raw,
    }))
}

fn parse_timestamp(v: &Value, format: TimestampFormat) -> Option<Timestamp> {
    let nanos_per_unit: i64 = match format {
        TimestampFormat::Rfc3339 => return v.as_str()?.trim().parse().ok(),
        TimestampFormat::UnixSeconds => 1_000_000_000,
        TimestampFormat::UnixMillis => 1_000_000,
    };
    let d = as_decimal(v)?;
    let bound = Decimal::from(MAX_UNIX_SECONDS * (1_000_000_000 / nanos_per_unit));
    if d.abs() > bound {
        return None;
    }
    let nanos = i128::try_from((d * Decimal::from(nanos_per_unit)).trunc()).ok()?;
    Timestamp::from_nanosecond(nanos).ok()
}

fn show(v: &Value) -> String {
    let text = v.to_string();
    if text.chars().count() <= MAX_VALUE_IN_WARNING {
        return text;
    }
    let mut cut: String = text.chars().take(MAX_VALUE_IN_WARNING).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn timestamps() {
        let ts = |v: Value, f| parse_timestamp(&v, f).map(|t| t.to_string());
        assert_eq!(
            ts(json!("2026-11-23T10:00:00+02:00"), TimestampFormat::Rfc3339).as_deref(),
            Some("2026-11-23T08:00:00Z")
        );
        assert_eq!(
            ts(json!("2026-11-23 10:00"), TimestampFormat::Rfc3339),
            None
        );
        assert_eq!(ts(json!(1_700_000_000), TimestampFormat::Rfc3339), None);
        assert_eq!(
            ts(json!(1_700_000_000), TimestampFormat::UnixSeconds).as_deref(),
            Some("2023-11-14T22:13:20Z")
        );
        assert_eq!(
            ts(json!(1_700_000_000.25), TimestampFormat::UnixSeconds).as_deref(),
            Some("2023-11-14T22:13:20.25Z")
        );
        assert_eq!(
            ts(json!("1700000000123"), TimestampFormat::UnixMillis).as_deref(),
            Some("2023-11-14T22:13:20.123Z")
        );
        assert_eq!(ts(json!(1e30), TimestampFormat::UnixSeconds), None);
        assert_eq!(ts(json!("soon"), TimestampFormat::UnixMillis), None);
    }

    #[test]
    fn home_expansion() {
        let home = Some(Path::new("/home/u"));
        let e = |r: &str| expand_home(Path::new(r), home);
        assert_eq!(e("~/logs"), PathBuf::from("/home/u/logs"));
        assert_eq!(e("~"), PathBuf::from("/home/u"));
        assert_eq!(e("/var/~/x"), PathBuf::from("/var/~/x"));
        assert_eq!(e("~other/x"), PathBuf::from("~other/x"));
        assert_eq!(expand_home(Path::new("~/x"), None), PathBuf::from("~/x"));
    }

    #[test]
    fn long_values_are_cut_in_warnings() {
        let v = json!("x".repeat(200));
        assert_eq!(show(&v).chars().count(), MAX_VALUE_IN_WARNING + 1);
        assert_eq!(show(&json!(1)), "1");
    }
}
