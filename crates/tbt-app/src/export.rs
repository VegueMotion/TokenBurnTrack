use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Serialize;
use tbt_core::Micros;
use tbt_store::{BucketGroup, BucketSize, CostBucket, QueryFilter, SessionRow};

use crate::data::DataSource;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Csv,
    Json,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Json => "json",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Csv => "CSV",
            Format::Json => "JSON",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dataset {
    Sessions,
    HourlyUsage,
}

impl Dataset {
    pub const ALL: [Dataset; 2] = [Dataset::Sessions, Dataset::HourlyUsage];

    pub fn label(self) -> &'static str {
        match self {
            Dataset::Sessions => "Sessions",
            Dataset::HourlyUsage => "Hourly usage",
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Dataset::Sessions => "sessions",
            Dataset::HourlyUsage => "hourly",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    pub text: String,
    pub rows: usize,
}

pub fn usd(m: Micros) -> String {
    m.to_usd().to_string()
}

pub fn rfc3339(ts: Timestamp) -> String {
    ts.to_string()
}

pub fn csv_field(value: &str) -> String {
    let needs_quotes = value.contains([',', '"', '\n', '\r'])
        || value.starts_with(char::is_whitespace)
        || value.ends_with(char::is_whitespace);
    if needs_quotes {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

trait Record: Serialize {
    const HEADER: &'static [&'static str];
    fn cells(&self) -> Vec<String>;
}

fn to_csv<R: Record>(rows: &[R]) -> String {
    let mut out = String::new();
    push_line(&mut out, R::HEADER.iter().map(|h| h.to_string()));
    for row in rows {
        push_line(&mut out, row.cells().into_iter());
    }
    out
}

fn push_line(out: &mut String, cells: impl Iterator<Item = String>) {
    for (i, cell) in cells.enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&csv_field(&cell));
    }
    out.push('\n');
}

fn to_json<R: Record>(rows: &[R]) -> String {
    let mut text = serde_json::to_string_pretty(rows).unwrap_or_else(|_| "[]".to_owned());
    text.push('\n');
    text
}

fn render<R: Record>(rows: &[R], format: Format) -> Export {
    Export {
        text: match format {
            Format::Csv => to_csv(rows),
            Format::Json => to_json(rows),
        },
        rows: rows.len(),
    }
}

fn opt<T: ToString>(v: &Option<T>) -> String {
    v.as_ref().map(T::to_string).unwrap_or_default()
}

#[derive(Serialize)]
struct SessionRecord<'a> {
    id: i64,
    harness: &'a str,
    ext_id: &'a str,
    title: Option<&'a str>,
    project_name: Option<&'a str>,
    project_path: Option<&'a str>,
    git_branch: Option<&'a str>,
    models: &'a [String],
    started_at: String,
    ended_at: Option<String>,
    active_seconds: u64,
    events: u64,
    input_tokens: u64,
    output_tokens: u64,
    cache_write_tokens: u64,
    cache_read_tokens: u64,
    total_tokens: u64,
    cost_usd: String,
    unpriced_events: u64,
    parent_session_id: Option<i64>,
    resumed_from_session_id: Option<i64>,
}

impl<'a> SessionRecord<'a> {
    fn new(r: &'a SessionRow) -> Self {
        SessionRecord {
            id: r.id.0,
            harness: r.harness.as_str(),
            ext_id: &r.ext_id,
            title: r.title.as_deref(),
            project_name: r.project_name.as_deref(),
            project_path: r.project_path.as_deref(),
            git_branch: r.git_branch.as_deref(),
            models: &r.models,
            started_at: rfc3339(r.started_at),
            ended_at: r.ended_at.map(rfc3339),
            active_seconds: r.active_seconds,
            events: r.events,
            input_tokens: r.tokens.input,
            output_tokens: r.tokens.output,
            cache_write_tokens: r.tokens.cache_write,
            cache_read_tokens: r.tokens.cache_read,
            total_tokens: r.tokens.total(),
            cost_usd: usd(r.cost),
            unpriced_events: r.unpriced_events,
            parent_session_id: r.parent.map(|p| p.0),
            resumed_from_session_id: r.resumed_from.map(|p| p.0),
        }
    }
}

impl Record for SessionRecord<'_> {
    const HEADER: &'static [&'static str] = &[
        "id",
        "harness",
        "ext_id",
        "title",
        "project_name",
        "project_path",
        "git_branch",
        "models",
        "started_at",
        "ended_at",
        "active_seconds",
        "events",
        "input_tokens",
        "output_tokens",
        "cache_write_tokens",
        "cache_read_tokens",
        "total_tokens",
        "cost_usd",
        "unpriced_events",
        "parent_session_id",
        "resumed_from_session_id",
    ];

    fn cells(&self) -> Vec<String> {
        vec![
            self.id.to_string(),
            self.harness.to_owned(),
            self.ext_id.to_owned(),
            opt(&self.title),
            opt(&self.project_name),
            opt(&self.project_path),
            opt(&self.git_branch),
            self.models.join(";"),
            self.started_at.clone(),
            opt(&self.ended_at),
            self.active_seconds.to_string(),
            self.events.to_string(),
            self.input_tokens.to_string(),
            self.output_tokens.to_string(),
            self.cache_write_tokens.to_string(),
            self.cache_read_tokens.to_string(),
            self.total_tokens.to_string(),
            self.cost_usd.clone(),
            self.unpriced_events.to_string(),
            opt(&self.parent_session_id),
            opt(&self.resumed_from_session_id),
        ]
    }
}

#[derive(Serialize)]
struct BucketRecord<'a> {
    hour: String,
    group: &'static str,
    key: &'a str,
    events: u64,
    input_tokens: u64,
    output_tokens: u64,
    cache_write_tokens: u64,
    cache_read_tokens: u64,
    total_tokens: u64,
    cost_usd: String,
}

impl<'a> BucketRecord<'a> {
    fn new(group: &'static str, b: &'a CostBucket) -> Self {
        BucketRecord {
            hour: rfc3339(b.start),
            group,
            key: &b.key,
            events: b.events,
            input_tokens: b.tokens.input,
            output_tokens: b.tokens.output,
            cache_write_tokens: b.tokens.cache_write,
            cache_read_tokens: b.tokens.cache_read,
            total_tokens: b.tokens.total(),
            cost_usd: usd(b.cost),
        }
    }
}

impl Record for BucketRecord<'_> {
    const HEADER: &'static [&'static str] = &[
        "hour",
        "group",
        "key",
        "events",
        "input_tokens",
        "output_tokens",
        "cache_write_tokens",
        "cache_read_tokens",
        "total_tokens",
        "cost_usd",
    ];

    fn cells(&self) -> Vec<String> {
        vec![
            self.hour.clone(),
            self.group.to_owned(),
            self.key.to_owned(),
            self.events.to_string(),
            self.input_tokens.to_string(),
            self.output_tokens.to_string(),
            self.cache_write_tokens.to_string(),
            self.cache_read_tokens.to_string(),
            self.total_tokens.to_string(),
            self.cost_usd.clone(),
        ]
    }
}

pub fn sessions(rows: &[SessionRow], format: Format) -> Export {
    let records: Vec<_> = rows.iter().map(SessionRecord::new).collect();
    render(&records, format)
}

pub fn hourly_usage(by_harness: &[CostBucket], by_model: &[CostBucket], format: Format) -> Export {
    let mut records: Vec<_> = by_harness
        .iter()
        .map(|b| BucketRecord::new("harness", b))
        .chain(by_model.iter().map(|b| BucketRecord::new("model", b)))
        .collect();
    records.sort_by(|a, b| a.hour.cmp(&b.hour).then(a.group.cmp(b.group)));
    render(&records, format)
}

pub fn build(
    data: &dyn DataSource,
    filter: &QueryFilter,
    dataset: Dataset,
    format: Format,
) -> anyhow::Result<Export> {
    Ok(match dataset {
        Dataset::Sessions => sessions(&data.sessions(filter)?, format),
        Dataset::HourlyUsage => {
            let mut hourly = filter.clone();
            hourly.bucket = BucketSize::Hour;
            let by_harness = data.cost_buckets(&hourly, BucketGroup::Harness)?;
            let by_model = data.cost_buckets(&hourly, BucketGroup::Model)?;
            hourly_usage(&by_harness, &by_model, format)
        }
    })
}

pub fn write(path: &Path, export: &Export) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, &export.text)
}

pub fn home_dir() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .into_iter()
        .find_map(|k| std::env::var_os(k).filter(|v| !v.is_empty()))
        .map(PathBuf::from)
}

pub fn file_name(dataset: Dataset, format: Format, date: jiff::civil::Date) -> String {
    format!(
        "tokenburntrack-{}-{}.{}",
        dataset.slug(),
        date,
        format.extension()
    )
}

pub fn default_dir(home: Option<&Path>) -> PathBuf {
    match home {
        Some(home) => {
            let downloads = home.join("Downloads");
            if downloads.is_dir() {
                downloads
            } else {
                home.to_path_buf()
            }
        }
        None => PathBuf::from("."),
    }
}

pub fn expand_home(text: &str, home: Option<&Path>) -> PathBuf {
    match (text.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ if text == "~" => home.map_or_else(|| PathBuf::from(text), Path::to_path_buf),
        _ => PathBuf::from(text),
    }
}

pub fn target_path(text: &str, format: Format, home: Option<&Path>) -> Option<PathBuf> {
    let text = text.trim();
    if text.is_empty() || text.ends_with(['/', '\\']) {
        return None;
    }
    let path = expand_home(text, home);
    Some(match path.extension().and_then(|e| e.to_str()) {
        Some("csv" | "json") => path.with_extension(format.extension()),
        _ => {
            let mut s = path.into_os_string();
            s.push(".");
            s.push(format.extension());
            PathBuf::from(s)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tbt_core::{HarnessId, SessionId};
    use tbt_store::TokenSums;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn session() -> SessionRow {
        SessionRow {
            id: SessionId(7),
            harness: HarnessId::CLAUDE_CODE,
            ext_id: "abc".into(),
            title: Some("Fix \"quotes\", commas\nand lines".into()),
            project_path: Some("/home/u/proj".into()),
            project_name: Some("proj".into()),
            git_branch: None,
            models: vec!["claude-opus-5-5".into(), "claude-haiku-5-5".into()],
            started_at: ts("2026-11-22T20:00:00Z"),
            ended_at: Some(ts("2026-11-22T20:30:15.5Z")),
            active_seconds: 1815,
            events: 12,
            tokens: TokenSums {
                input: 100,
                output: 50,
                cache_write: 10,
                cache_read: 1000,
            },
            cost: Micros(1_234_567),
            unpriced_events: 0,
            parent: None,
            resumed_from: Some(SessionId(3)),
        }
    }

    #[test]
    fn money_and_time_format() {
        assert_eq!(usd(Micros(1_234_567)), "1.234567");
        assert_eq!(usd(Micros(0)), "0.000000");
        assert_eq!(usd(Micros(5)), "0.000005");
        assert_eq!(usd(Micros(-1_500_000)), "-1.500000");
        assert_eq!(usd(Micros(12_000_000_000)), "12000.000000");
        assert_eq!(
            rfc3339(ts("2026-11-22T21:00:00+01:00")),
            "2026-11-22T20:00:00Z"
        );
    }

    #[test]
    fn csv_quoting() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("two\nlines"), "\"two\nlines\"");
        assert_eq!(csv_field(" padded"), "\" padded\"");
        assert_eq!(csv_field(""), "");
    }

    #[test]
    fn sessions_csv_header_and_row() {
        let e = sessions(&[session()], Format::Csv);
        assert_eq!(e.rows, 1);
        let header = e.text.lines().next().unwrap();
        assert_eq!(
            header,
            "id,harness,ext_id,title,project_name,project_path,git_branch,models,started_at,\
             ended_at,active_seconds,events,input_tokens,output_tokens,cache_write_tokens,\
             cache_read_tokens,total_tokens,cost_usd,unpriced_events,parent_session_id,\
             resumed_from_session_id"
        );
        let body = &e.text[header.len() + 1..];
        assert_eq!(
            body,
            "7,claude_code,abc,\"Fix \"\"quotes\"\", commas\nand lines\",proj,/home/u/proj,,\
             claude-opus-5-5;claude-haiku-5-5,2026-11-22T20:00:00Z,2026-11-22T20:30:15.5Z,\
             1815,12,100,50,10,1000,1160,1.234567,0,,3\n"
        );
    }

    #[test]
    fn sessions_json_matches_csv_columns() {
        let e = sessions(&[session()], Format::Json);
        let v: serde_json::Value = serde_json::from_str(&e.text).unwrap();
        let row = &v[0];
        assert_eq!(row["cost_usd"], "1.234567");
        assert_eq!(row["started_at"], "2026-11-22T20:00:00Z");
        assert_eq!(row["git_branch"], serde_json::Value::Null);
        assert_eq!(row["models"][1], "claude-haiku-5-5");
        assert_eq!(row["resumed_from_session_id"], 3);
        let mut last = 0;
        for key in SessionRecord::HEADER {
            let pos = e.text.find(&format!("\"{key}\":")).unwrap();
            assert!(pos > last, "{key} out of order");
            last = pos;
        }
        assert_eq!(row.as_object().unwrap().len(), SessionRecord::HEADER.len());
    }

    #[test]
    fn hourly_usage_merges_groups_by_hour() {
        let bucket = |hour: &str, key: &str, cost: i64| CostBucket {
            start: ts(hour),
            key: key.into(),
            cost: Micros(cost),
            tokens: TokenSums {
                input: 1,
                output: 2,
                cache_write: 3,
                cache_read: 4,
            },
            events: 1,
        };
        let harness = [
            bucket("2026-11-22T10:00:00Z", "claude_code", 1_000_000),
            bucket("2026-11-22T11:00:00Z", "codex", 75_000),
        ];
        let model = [bucket("2026-11-22T10:00:00Z", "gpt-5, mini", 1)];
        let e = hourly_usage(&harness, &model, Format::Csv);
        assert_eq!(e.rows, 3);
        let lines: Vec<_> = e.text.lines().collect();
        assert_eq!(
            lines[0],
            "hour,group,key,events,input_tokens,output_tokens,cache_write_tokens,cache_read_tokens,total_tokens,cost_usd"
        );
        assert_eq!(
            lines[1],
            "2026-11-22T10:00:00Z,harness,claude_code,1,1,2,3,4,10,1.000000"
        );
        assert_eq!(
            lines[2],
            "2026-11-22T10:00:00Z,model,\"gpt-5, mini\",1,1,2,3,4,10,0.000001"
        );
        assert_eq!(
            lines[3],
            "2026-11-22T11:00:00Z,harness,codex,1,1,2,3,4,10,0.075000"
        );

        let j: serde_json::Value =
            serde_json::from_str(&hourly_usage(&harness, &model, Format::Json).text).unwrap();
        assert_eq!(j.as_array().unwrap().len(), 3);
        assert_eq!(j[2]["cost_usd"], "0.075000");
        assert_eq!(j[1]["group"], "model");
    }

    #[test]
    fn empty_export_has_header_only() {
        let e = sessions(&[], Format::Csv);
        assert_eq!(e.rows, 0);
        assert_eq!(e.text.lines().count(), 1);
        assert_eq!(sessions(&[], Format::Json).text, "[]\n");
    }

    #[test]
    fn target_paths() {
        let home = Path::new("/home/u");
        assert_eq!(
            target_path("~/Downloads/x.csv", Format::Json, Some(home)),
            Some(PathBuf::from("/home/u/Downloads/x.json"))
        );
        assert_eq!(
            target_path(" /tmp/report ", Format::Csv, Some(home)),
            Some(PathBuf::from("/tmp/report.csv"))
        );
        assert_eq!(
            target_path("/tmp/a.json", Format::Csv, None),
            Some(PathBuf::from("/tmp/a.csv"))
        );
        assert_eq!(target_path("  ", Format::Csv, None), None);
        assert_eq!(target_path("/tmp/", Format::Csv, None), None);
        let date: jiff::civil::Date = "2026-11-22".parse().unwrap();
        assert_eq!(
            file_name(Dataset::Sessions, Format::Csv, date),
            "tokenburntrack-sessions-2026-11-22.csv"
        );
    }

    #[test]
    fn writes_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/out.csv");
        let e = sessions(&[session()], Format::Csv);
        write(&path, &e).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), e.text);
    }
}
