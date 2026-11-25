use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tbt_core::{
    Cursor, Error, HarnessAdapter, HarnessId, ParseWarning, ReadBatch, Result, SessionUpsert,
    SourceKind, SourceRef, Timestamp, UsageEvent,
};

use crate::discover;

const HARNESS: HarnessId = HarnessId::DEEPSEEK_HARNESS;
const UNKNOWN: &str = "unknown";
const ZSTD_SUFFIX: &str = ".zstd";

#[derive(Debug, Clone, Copy, Default)]
pub struct DeepSeekHarnessAdapter;

impl DeepSeekHarnessAdapter {
    pub fn new() -> DeepSeekHarnessAdapter {
        DeepSeekHarnessAdapter
    }
}

pub fn default_roots_from(dsh_home: Option<OsString>, home: Option<PathBuf>) -> Vec<PathBuf> {
    let configured = dsh_home
        .and_then(|h| h.into_string().ok())
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty())
        .map(PathBuf::from);
    configured
        .or_else(|| home.map(|h| h.join(".dsh")))
        .map(|h| h.join("sessions"))
        .into_iter()
        .collect()
}

pub fn log_generation(file_name: &str) -> Option<u32> {
    let name = file_name.strip_suffix(ZSTD_SUFFIX).unwrap_or(file_name);
    let stem = name.strip_prefix("session")?.strip_suffix(".jsonl")?;
    if stem.is_empty() {
        return Some(0);
    }
    let digits = stem.strip_prefix(".v")?;
    if digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

impl HarnessAdapter for DeepSeekHarnessAdapter {
    fn id(&self) -> HarnessId {
        HARNESS
    }

    fn display_name(&self) -> &str {
        "DeepSeek Harness"
    }

    fn default_roots(&self) -> Vec<PathBuf> {
        default_roots_from(std::env::var_os("DSH_HOME"), std::env::home_dir())
    }

    fn discover(&self, roots: &[PathBuf]) -> Result<Vec<SourceRef>> {
        let mut latest: BTreeMap<PathBuf, (u32, bool, PathBuf)> = BTreeMap::new();
        let files = discover::files_with_extension(roots, "jsonl")
            .into_iter()
            .chain(discover::files_with_extension(roots, "zstd"));
        for path in files {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(generation) = log_generation(name) else {
                continue;
            };
            let compressed = name.ends_with(ZSTD_SUFFIX);
            let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
            let rank = (generation, compressed);
            match latest.get(&dir) {
                Some((g, c, _)) if (*g, *c) >= rank => {}
                _ => {
                    latest.insert(dir, (generation, compressed, path));
                }
            }
        }
        Ok(latest
            .into_values()
            .map(|(_, _, p)| SourceRef::new(p, SourceKind::JsonlFile))
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
        let next = State {
            size: meta.len(),
            mtime: meta
                .modified()
                .ok()
                .and_then(|m| Timestamp::try_from(m).ok()),
        };
        if next.mtime.is_some() && next == state && warnings.is_empty() {
            return Ok(ReadBatch::unchanged(cursor.clone()));
        }
        let bytes = std::fs::read(&src.path).map_err(|e| Error::io(&src.path, e))?;
        let text = if src.path.to_string_lossy().ends_with(ZSTD_SUFFIX) {
            let (text, torn) = decode_frames(&bytes);
            if let Some(e) = torn
                && text.is_empty()
            {
                warnings.push(warning(src, format!("bad zstd data: {e}")));
            }
            text
        } else {
            bytes
        };
        let mut batch = Reader::new(src).read(&text);
        warnings.append(&mut batch.warnings);
        batch.warnings = warnings;
        batch.next_cursor = Cursor::from_state(&next)?;
        Ok(batch)
    }
}

fn decode_frames(bytes: &[u8]) -> (Vec<u8>, Option<String>) {
    let mut out = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let mut frame = Vec::new();
        let decoded = ruzstd::decoding::StreamingDecoder::new(&mut rest)
            .map_err(|e| e.to_string())
            .and_then(|mut d| d.read_to_end(&mut frame).map_err(|e| e.to_string()));
        if let Err(e) = decoded {
            return (out, Some(e));
        }
        out.extend_from_slice(&frame);
    }
    (out, None)
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct State {
    size: u64,
    mtime: Option<Timestamp>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Header {
    #[serde(rename = "type")]
    kind: String,
    id: String,
    created_at: Option<i64>,
    cwd: Option<String>,
    parent_session: Option<String>,
    origin: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    reasoning_tokens: u64,
}

impl Usage {
    fn is_zero(&self) -> bool {
        self.input_tokens == 0
            && self.output_tokens == 0
            && self.cache_read_tokens == 0
            && self.cache_write_tokens == 0
    }
}

struct Row {
    line_no: u64,
    offset: u64,
    value: Value,
}

struct Call {
    key: String,
    ts: Timestamp,
    route: (String, String),
    usage: Usage,
    raw: Value,
}

struct Reader<'a> {
    src: &'a SourceRef,
    batch: ReadBatch,
}

impl<'a> Reader<'a> {
    fn new(src: &'a SourceRef) -> Reader<'a> {
        Reader {
            src,
            batch: ReadBatch::default(),
        }
    }

    fn warn(&mut self, row: &Row, message: impl Into<String>) {
        self.batch
            .warnings
            .push(warning(self.src, message).at(row.line_no, row.offset));
    }

    fn rows(&mut self, text: &[u8]) -> Vec<Row> {
        let mut rows = Vec::new();
        let mut offset = 0u64;
        let mut lines = text.split_inclusive(|b| *b == b'\n').peekable();
        let mut line_no = 0u64;
        while let Some(line) = lines.next() {
            line_no += 1;
            let start = offset;
            offset += line.len() as u64;
            if lines.peek().is_none() && !line.ends_with(b"\n") {
                break;
            }
            let trimmed = line.trim_ascii();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_slice::<Value>(trimmed) {
                Ok(value) => rows.push(Row {
                    line_no,
                    offset: start,
                    value,
                }),
                Err(e) => self
                    .batch
                    .warnings
                    .push(warning(self.src, format!("invalid JSON: {e}")).at(line_no, start)),
            }
        }
        rows
    }

    fn read(mut self, text: &[u8]) -> ReadBatch {
        let rows = self.rows(text);
        let Some((first, body)) = rows.split_first() else {
            return self.batch;
        };
        let header = match Header::deserialize(&first.value) {
            Ok(h) if h.kind == "session" && !h.id.is_empty() => h,
            _ => {
                self.warn(first, "first line is not a session header");
                return self.batch;
            }
        };
        let cut = body
            .iter()
            .rposition(|r| {
                kind(&r.value) == "session/end-seed"
                    && r.value.pointer("/data/inherited") == Some(&Value::Bool(true))
            })
            .map_or(0, |i| i + 1);

        let mut route: Option<(String, String)> = None;
        let mut title = None;
        let mut last_ts = None;
        let mut calls: Vec<Call> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        let mut attempts: HashMap<(u64, u64), u64> = HashMap::new();

        for (i, row) in body.iter().enumerate() {
            let v = &row.value;
            let data = v.get("data").unwrap_or(&Value::Null);
            match kind(v) {
                "request/header" => {
                    route = route_of(data.pointer("/header/config")).or(route);
                    continue;
                }
                "request/context" => {
                    route = route_of(Some(data)).or(route);
                    continue;
                }
                _ => {}
            }
            if i < cut {
                continue;
            }
            let ts = v
                .get("time")
                .and_then(Value::as_i64)
                .and_then(|ms| Timestamp::from_millisecond(ms).ok());
            if let Some(ts) = ts {
                last_ts = Some(last_ts.map_or(ts, |t: Timestamp| t.max(ts)));
            }
            let turn_step = || Some((data.get("turn")?.as_u64()?, data.get("step")?.as_u64()?));
            let (key, raw, call_route) = match kind(v) {
                "session/title" => {
                    if let Some(t) = data.get("title").and_then(Value::as_str) {
                        let t = t.trim();
                        if !t.is_empty() {
                            title = Some(t.to_owned());
                        }
                    }
                    continue;
                }
                "assistant/message" => {
                    let Some(usage) = data.get("usage") else {
                        continue;
                    };
                    let Some((turn, step)) = turn_step() else {
                        self.warn(row, "assistant/message has no turn or step");
                        continue;
                    };
                    let source = route_of(data.pointer("/message/source"));
                    if source.is_some() {
                        route = source.clone();
                    }
                    (format!("{turn}:{step}"), usage, source)
                }
                "assistant/chunk" => {
                    if data.pointer("/chunk/type").and_then(Value::as_str) != Some("usage") {
                        continue;
                    }
                    let (Some(usage), Some((turn, step))) =
                        (data.pointer("/chunk/usage"), turn_step())
                    else {
                        continue;
                    };
                    (format!("{turn}:{step}"), usage, None)
                }
                "assistant/attempt" => {
                    let usage = data.get("stream").and_then(Value::as_array).and_then(|s| {
                        s.iter().rev().find_map(|r| {
                            (r.pointer("/chunk/type").and_then(Value::as_str) == Some("usage"))
                                .then(|| r.pointer("/chunk/usage"))
                                .flatten()
                        })
                    });
                    let (Some(usage), Some((turn, step))) = (usage, turn_step()) else {
                        continue;
                    };
                    let n = attempts.entry((turn, step)).or_default();
                    *n += 1;
                    (format!("{turn}:{step}:attempt:{n}"), usage, None)
                }
                "compaction/summary" => {
                    let Some(usage) = data.get("usage") else {
                        continue;
                    };
                    let Some(id) = data.get("compactionId").and_then(Value::as_str) else {
                        self.warn(row, "compaction/summary has no compactionId");
                        continue;
                    };
                    (format!("compaction:{id}"), usage, route_of(Some(data)))
                }
                _ => continue,
            };
            let usage = match Usage::deserialize(raw) {
                Ok(u) => u,
                Err(e) => {
                    self.warn(row, format!("{}: usage: {e}", kind(v)));
                    continue;
                }
            };
            if usage.is_zero() {
                continue;
            }
            let Some(ts) = ts else {
                self.warn(row, format!("{} has no valid time", kind(v)));
                continue;
            };
            let call = Call {
                key: key.clone(),
                ts,
                route: call_route
                    .or_else(|| route.clone())
                    .unwrap_or_else(|| (UNKNOWN.to_owned(), UNKNOWN.to_owned())),
                usage,
                raw: raw.clone(),
            };
            match index.get(&key) {
                Some(&at) => calls[at] = call,
                None => {
                    index.insert(key, calls.len());
                    calls.push(call);
                }
            }
        }

        for call in calls {
            let (provider, model) = call.route;
            let u = call.usage;
            self.batch.events.push(UsageEvent {
                harness: HARNESS,
                dedup_key: format!("{}:{}", header.id, call.key),
                session_ext_id: header.id.clone(),
                ts: call.ts,
                provider: normalize_provider(&provider),
                model,
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
                cache_write_5m_tokens: u.cache_write_tokens,
                cache_write_1h_tokens: 0,
                cache_read_tokens: u.cache_read_tokens,
                reasoning_tokens: u.reasoning_tokens.min(u.output_tokens),
                reported_cost: None,
                project_path: header.cwd.clone(),
                git_branch: None,
                raw: serde_json::value::to_raw_value(&call.raw).ok(),
            });
        }

        let started = header
            .created_at
            .and_then(|ms| Timestamp::from_millisecond(ms).ok());
        let mut s = SessionUpsert::new(HARNESS, header.id.clone());
        s.project_path = header.cwd.clone();
        s.title = title;
        s.started_at = started;
        s.ended_at = last_ts.or(started);
        let parent = header.parent_session.clone().filter(|p| *p != header.id);
        if header.origin.as_deref() == Some("subagent") {
            s.parent_ext_id = parent;
        } else {
            s.resumed_from_ext_id = parent;
        }
        self.batch.sessions.push(s);
        self.batch
    }
}

fn kind(v: &Value) -> &str {
    v.get("type").and_then(Value::as_str).unwrap_or_default()
}

fn route_of(v: Option<&Value>) -> Option<(String, String)> {
    let v = v?;
    let provider = v.get("provider")?.as_str().filter(|p| !p.is_empty())?;
    let model = v.get("model")?.as_str().filter(|m| !m.is_empty())?;
    Some((provider.to_owned(), model.to_owned()))
}

fn normalize_provider(provider: &str) -> String {
    if provider.starts_with("deepseek") {
        "deepseek".to_owned()
    } else {
        provider.to_owned()
    }
}

fn warning(src: &SourceRef, message: impl Into<String>) -> ParseWarning {
    ParseWarning::new(HARNESS, src, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_roots_follow_dsh_home() {
        assert_eq!(
            default_roots_from(Some("/d".into()), Some("/h".into())),
            [PathBuf::from("/d/sessions")]
        );
        assert_eq!(
            default_roots_from(Some("  ".into()), Some("/h".into())),
            [PathBuf::from("/h/.dsh/sessions")]
        );
        assert!(default_roots_from(None, None).is_empty());
    }

    #[test]
    fn log_generation_reads_canonical_names_only() {
        assert_eq!(log_generation("session.jsonl"), Some(0));
        assert_eq!(log_generation("session.v4.jsonl.zstd"), Some(4));
        assert_eq!(log_generation("session.v12.jsonl"), Some(12));
        assert_eq!(log_generation("session.v0.jsonl"), None);
        assert_eq!(log_generation("session.v04.jsonl"), None);
        assert_eq!(log_generation("session.v4.jsonl.tmp"), None);
        assert_eq!(log_generation("other.jsonl"), None);
    }

    #[test]
    fn older_logs_count_their_usage_chunks() {
        let src = SourceRef::new("/x/session.v1.jsonl", SourceKind::JsonlFile);
        let text = concat!(
            r#"{"type":"session","version":1,"id":"s1","createdAt":1795000000000}"#,
            "\n",
            r#"{"type":"request/header","seq":0,"time":1795000001000,"data":{"header":{"config":{"provider":"deepseek-official","model":"deepseek-v4-pro"}}}}"#,
            "\n",
            r#"{"type":"assistant/chunk","seq":1,"time":1795000002000,"data":{"turn":1,"step":1,"chunk":{"type":"usage","usage":{"inputTokens":40,"outputTokens":7,"cacheReadTokens":900}}}}"#,
            "\n",
            r#"{"type":"assistant/message","seq":2,"time":1795000003000,"data":{"turn":1,"step":1,"message":{"role":"assistant"}}}"#,
            "\n",
        );
        let batch = Reader::new(&src).read(text.as_bytes());
        let [e] = batch.events.as_slice() else {
            panic!("expected one event: {:?}", batch.events);
        };
        assert_eq!(
            (e.provider.as_str(), e.model.as_str()),
            ("deepseek", "deepseek-v4-pro")
        );
        assert_eq!((e.input_tokens, e.cache_read_tokens), (40, 900));
        assert_eq!(e.dedup_key, "s1:1:1");
    }
}
