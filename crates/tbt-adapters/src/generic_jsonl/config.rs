use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tbt_core::HarnessId;

use super::glob::{Glob, GlobError};
use super::path::{JsonPath, PathError, as_text};

pub const DEFAULT_GLOB: &str = "**/*.jsonl";

const BUILTIN_IDS: [HarnessId; 5] = [
    HarnessId::CLAUDE_CODE,
    HarnessId::CODEX,
    HarnessId::GEMINI_CLI,
    HarnessId::GROK_BUILD,
    HarnessId::OPENCODE,
];
const MAX_ID_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenericJsonlConfig {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub display_name: String,
    #[serde(default)]
    pub roots: Vec<PathBuf>,
    #[serde(default = "default_glob")]
    pub glob: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    pub timestamp: String,
    #[serde(default)]
    pub timestamp_format: TimestampFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_5m_tokens: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_1h_tokens: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub input_includes_cache_read: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_cost: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    pub dedup_key: String,
}

fn default_glob() -> String {
    DEFAULT_GLOB.to_owned()
}

fn is_false(b: &bool) -> bool {
    !b
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimestampFormat {
    #[default]
    Rfc3339,
    UnixSeconds,
    UnixMillis,
}

impl TimestampFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            TimestampFormat::Rfc3339 => "rfc3339",
            TimestampFormat::UnixSeconds => "unix_seconds",
            TimestampFormat::UnixMillis => "unix_millis",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("id `{id}`: {reason}")]
    InvalidId { id: String, reason: String },
    #[error("{field}: a value is required")]
    Required { field: &'static str },
    #[error("{field}: {source}")]
    InvalidPath {
        field: &'static str,
        source: PathError,
    },
    #[error("glob: {0}")]
    InvalidGlob(#[from] GlobError),
    #[error("filter `{filter}`: {message}")]
    InvalidFilter { filter: String, message: String },
    #[error("dedup_key `{template}`: {message}")]
    InvalidDedupKey { template: String, message: String },
    #[error("no usage field is mapped; set at least one of the token fields or `reported_cost`")]
    NoUsageFields,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigWarning {
    pub field: &'static str,
    pub message: String,
}

impl fmt::Display for ConfigWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

impl GenericJsonlConfig {
    pub fn validate(&self) -> Result<Vec<ConfigWarning>, ConfigError> {
        self.compile().map(|(_, warnings)| warnings)
    }

    pub(super) fn compile(&self) -> Result<(Compiled, Vec<ConfigWarning>), ConfigError> {
        validate_id(&self.id)?;
        let session_id = optional_text("session_id", self.session_id.as_deref())?;
        let tokens = TokenPaths {
            input: optional_path("input_tokens", self.input_tokens.as_deref())?,
            output: optional_path("output_tokens", self.output_tokens.as_deref())?,
            cache_write_5m: optional_path(
                "cache_write_5m_tokens",
                self.cache_write_5m_tokens.as_deref(),
            )?,
            cache_write_1h: optional_path(
                "cache_write_1h_tokens",
                self.cache_write_1h_tokens.as_deref(),
            )?,
            cache_read: optional_path("cache_read_tokens", self.cache_read_tokens.as_deref())?,
            reasoning: optional_path("reasoning_tokens", self.reasoning_tokens.as_deref())?,
        };
        let reported_cost = optional_path("reported_cost", self.reported_cost.as_deref())?;
        if tokens.all().all(|(_, p)| p.is_none()) && reported_cost.is_none() {
            return Err(ConfigError::NoUsageFields);
        }
        let compiled = Compiled {
            glob: Glob::parse(self.glob.trim())?,
            filter: self.filter.as_deref().map(Filter::parse).transpose()?,
            timestamp: required_path("timestamp", &self.timestamp)?,
            timestamp_format: self.timestamp_format,
            provider: required_text("provider", &self.provider)?,
            model: required_text("model", &self.model)?,
            tokens,
            input_includes_cache_read: self.input_includes_cache_read,
            reported_cost,
            project_path: optional_text("project_path", self.project_path.as_deref())?,
            git_branch: optional_text("git_branch", self.git_branch.as_deref())?,
            dedup_key: Template::parse(&self.dedup_key)?,
            session_id,
        };

        let mut warnings = Vec::new();
        if self.roots.is_empty() {
            warnings.push(ConfigWarning {
                field: "roots",
                message: "no roots configured; nothing is read until roots are set".into(),
            });
        }
        if let Some(Text::Path(session)) = &compiled.session_id
            && compiled.dedup_key.fields().all(|p| p == session)
        {
            warnings.push(ConfigWarning {
                field: "dedup_key",
                message: format!(
                    "uses only the session id ({session}), so all calls of a session collapse into one event; reference a per-request id"
                ),
            });
        }
        if self.input_includes_cache_read && compiled.tokens.cache_read.is_none() {
            warnings.push(ConfigWarning {
                field: "input_includes_cache_read",
                message: "has no effect because `cache_read_tokens` is not mapped".into(),
            });
        }
        Ok((compiled, warnings))
    }
}

fn validate_id(id: &str) -> Result<(), ConfigError> {
    let invalid = |reason: &str| {
        Err(ConfigError::InvalidId {
            id: id.to_owned(),
            reason: reason.to_owned(),
        })
    };
    if id.is_empty() {
        return invalid("must not be empty");
    }
    if id.len() > MAX_ID_LEN {
        return invalid("must be at most 64 characters");
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return invalid("may contain only lowercase letters a-z, digits and `_`");
    }
    if BUILTIN_IDS.iter().any(|b| b.as_str() == id) {
        return invalid("is a built-in harness id");
    }
    Ok(())
}

fn required_path(field: &'static str, src: &str) -> Result<JsonPath, ConfigError> {
    optional_path(field, Some(src))?.ok_or(ConfigError::Required { field })
}

fn optional_path(field: &'static str, src: Option<&str>) -> Result<Option<JsonPath>, ConfigError> {
    let Some(src) = src.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    JsonPath::parse(src)
        .map(Some)
        .map_err(|source| ConfigError::InvalidPath { field, source })
}

fn required_text(field: &'static str, src: &str) -> Result<Text, ConfigError> {
    optional_text(field, Some(src))?.ok_or(ConfigError::Required { field })
}

fn optional_text(field: &'static str, src: Option<&str>) -> Result<Option<Text>, ConfigError> {
    let Some(src) = src.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if src.starts_with('$') {
        optional_path(field, Some(src)).map(|p| p.map(Text::Path))
    } else {
        Ok(Some(Text::Const(src.to_owned())))
    }
}

#[derive(Debug, Clone)]
pub(super) struct Compiled {
    pub glob: Glob,
    pub filter: Option<Filter>,
    pub timestamp: JsonPath,
    pub timestamp_format: TimestampFormat,
    pub session_id: Option<Text>,
    pub provider: Text,
    pub model: Text,
    pub tokens: TokenPaths,
    pub input_includes_cache_read: bool,
    pub reported_cost: Option<JsonPath>,
    pub project_path: Option<Text>,
    pub git_branch: Option<Text>,
    pub dedup_key: Template,
}

#[derive(Debug, Clone)]
pub(super) struct TokenPaths {
    pub input: Option<JsonPath>,
    pub output: Option<JsonPath>,
    pub cache_write_5m: Option<JsonPath>,
    pub cache_write_1h: Option<JsonPath>,
    pub cache_read: Option<JsonPath>,
    pub reasoning: Option<JsonPath>,
}

impl TokenPaths {
    pub fn all(&self) -> impl Iterator<Item = (&'static str, Option<&JsonPath>)> {
        [
            ("input_tokens", self.input.as_ref()),
            ("output_tokens", self.output.as_ref()),
            ("cache_write_5m_tokens", self.cache_write_5m.as_ref()),
            ("cache_write_1h_tokens", self.cache_write_1h.as_ref()),
            ("cache_read_tokens", self.cache_read.as_ref()),
            ("reasoning_tokens", self.reasoning.as_ref()),
        ]
        .into_iter()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Text {
    Path(JsonPath),
    Const(String),
}

impl Text {
    pub fn eval(&self, line: &Value) -> Option<String> {
        match self {
            Text::Path(p) => p.get_present(line).and_then(as_text),
            Text::Const(c) => Some(c.clone()),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Text::Path(p) => p.to_string(),
            Text::Const(c) => format!("{c:?}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Filter {
    path: JsonPath,
    equals: Option<Value>,
}

impl Filter {
    fn parse(src: &str) -> Result<Filter, ConfigError> {
        let src = src.trim();
        let err = |message: String| ConfigError::InvalidFilter {
            filter: src.to_owned(),
            message,
        };
        let (path, end) = JsonPath::parse_prefix(src).map_err(|e| err(path_message(&e, 0)))?;
        let rest = src[end..].trim_start();
        if rest.is_empty() {
            return Ok(Filter { path, equals: None });
        }
        let Some(literal) = rest.strip_prefix("==") else {
            return Err(err(format!(
                "expected `==` or the end after the path, found `{rest}`"
            )));
        };
        let equals = serde_json::from_str(literal.trim()).map_err(|_| {
            err(format!(
                "`{}` is not a JSON value (strings need double quotes)",
                literal.trim()
            ))
        })?;
        Ok(Filter {
            path,
            equals: Some(equals),
        })
    }

    pub fn matches(&self, line: &Value) -> bool {
        match (&self.equals, self.path.get(line)) {
            (None, found) => found.is_some_and(|v| !v.is_null()),
            (Some(expected), Some(found)) => found == expected,
            (Some(expected), None) => expected.is_null(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Part {
    Lit(String),
    Field(JsonPath),
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Template {
    parts: Vec<Part>,
}

impl Template {
    fn parse(src: &str) -> Result<Template, ConfigError> {
        let err = |message: String| ConfigError::InvalidDedupKey {
            template: src.to_owned(),
            message,
        };
        let mut parts = Vec::new();
        let mut lit = String::new();
        let mut i = 0;
        while let Some(c) = src[i..].chars().next() {
            match c {
                '{' if src[i + 1..].starts_with('{') => {
                    lit.push('{');
                    i += 2;
                }
                '}' if src[i + 1..].starts_with('}') => {
                    lit.push('}');
                    i += 2;
                }
                '{' => {
                    let start = i + 1;
                    let (path, len) = JsonPath::parse_prefix(&src[start..])
                        .map_err(|e| err(path_message(&e, start)))?;
                    let end = start + len;
                    if !src[end..].starts_with('}') {
                        return Err(err(format!("expected `}}` at offset {end}")));
                    }
                    if !lit.is_empty() {
                        parts.push(Part::Lit(std::mem::take(&mut lit)));
                    }
                    parts.push(Part::Field(path));
                    i = end + 1;
                }
                '}' => {
                    return Err(err(format!(
                        "unmatched `}}` at offset {i} (write `}}}}` for a literal brace)"
                    )));
                }
                c => {
                    lit.push(c);
                    i += c.len_utf8();
                }
            }
        }
        if !lit.is_empty() {
            parts.push(Part::Lit(lit));
        }
        let template = Template { parts };
        if template.fields().next().is_none() {
            return Err(err(
                "must reference at least one field, e.g. `{$.request_id}`".into(),
            ));
        }
        Ok(template)
    }

    pub fn fields(&self) -> impl Iterator<Item = &JsonPath> {
        self.parts.iter().filter_map(|p| match p {
            Part::Field(path) => Some(path),
            Part::Lit(_) => None,
        })
    }

    pub fn render(&self, line: &Value) -> Result<String, Vec<&JsonPath>> {
        let mut out = String::new();
        let mut missing = Vec::new();
        for part in &self.parts {
            match part {
                Part::Lit(s) => out.push_str(s),
                Part::Field(path) => match path.get_present(line).and_then(as_text) {
                    Some(v) => out.push_str(&v),
                    None => missing.push(path),
                },
            }
        }
        if missing.is_empty() {
            Ok(out)
        } else {
            Err(missing)
        }
    }
}

fn path_message(e: &PathError, base: usize) -> String {
    format!("invalid path at offset {}: {}", base + e.offset, e.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base() -> GenericJsonlConfig {
        GenericJsonlConfig {
            id: "my_proxy".into(),
            display_name: "My proxy".into(),
            roots: vec![PathBuf::from("/logs")],
            glob: DEFAULT_GLOB.into(),
            filter: None,
            timestamp: "$.ts".into(),
            timestamp_format: TimestampFormat::Rfc3339,
            session_id: Some("$.session".into()),
            provider: "openai".into(),
            model: "$.model".into(),
            input_tokens: Some("$.usage.input".into()),
            output_tokens: None,
            cache_write_5m_tokens: None,
            cache_write_1h_tokens: None,
            cache_read_tokens: None,
            reasoning_tokens: None,
            input_includes_cache_read: false,
            reported_cost: None,
            project_path: None,
            git_branch: None,
            dedup_key: "{$.id}".into(),
        }
    }

    fn error_of(edit: impl FnOnce(&mut GenericJsonlConfig)) -> String {
        let mut c = base();
        edit(&mut c);
        c.validate().unwrap_err().to_string()
    }

    #[test]
    fn base_config_is_valid_without_warnings() {
        assert_eq!(base().validate().unwrap(), []);
    }

    #[test]
    fn id_validation() {
        for (id, reason) in [
            ("", "must not be empty"),
            (
                "My_Proxy",
                "may contain only lowercase letters a-z, digits and `_`",
            ),
            (
                "my-proxy",
                "may contain only lowercase letters a-z, digits and `_`",
            ),
            ("claude_code", "is a built-in harness id"),
            ("codex", "is a built-in harness id"),
            ("gemini_cli", "is a built-in harness id"),
            ("grok_build", "is a built-in harness id"),
            ("opencode", "is a built-in harness id"),
        ] {
            assert_eq!(
                error_of(|c| c.id = id.into()),
                format!("id `{id}`: {reason}")
            );
        }
        let long = "a".repeat(65);
        assert!(error_of(|c| c.id = long).ends_with("must be at most 64 characters"));
        let mut c = base();
        c.id = "proxy_2".into();
        assert!(c.validate().is_ok());
    }

    #[test]
    fn field_errors() {
        assert_eq!(
            error_of(|c| c.timestamp = "  ".into()),
            "timestamp: a value is required"
        );
        assert_eq!(
            error_of(|c| c.model = "".into()),
            "model: a value is required"
        );
        assert_eq!(
            error_of(|c| c.input_tokens = Some("$.usage[".into())),
            "input_tokens: invalid path `$.usage[` at offset 8: expected a quoted field name or an index after `[`"
        );
        assert_eq!(
            error_of(|c| c.timestamp = "ts".into()),
            "timestamp: invalid path `ts` at offset 0: a path must start with `$`"
        );
        assert_eq!(
            error_of(|c| c.provider = "$.a..b".into()),
            "provider: invalid path `$.a..b` at offset 4: expected a field name after `.`, found `.` (use [\"...\"] for other names)"
        );
        assert_eq!(
            error_of(|c| c.glob = "/abs/*.jsonl".into()),
            "glob: invalid glob `/abs/*.jsonl`: the pattern is matched relative to each root and must not start with `/`"
        );
        assert_eq!(
            error_of(|c| c.input_tokens = None),
            "no usage field is mapped; set at least one of the token fields or `reported_cost`"
        );
    }

    #[test]
    fn filter_errors_and_matching() {
        assert_eq!(
            error_of(|c| c.filter = Some("$.type == usage".into())),
            "filter `$.type == usage`: `usage` is not a JSON value (strings need double quotes)"
        );
        assert_eq!(
            error_of(|c| c.filter = Some("$.type = \"usage\"".into())),
            "filter `$.type = \"usage\"`: expected `==` or the end after the path, found `= \"usage\"`"
        );
        assert_eq!(
            error_of(|c| c.filter = Some("type == 1".into())),
            "filter `type == 1`: invalid path at offset 0: a path must start with `$`"
        );

        let eq = Filter::parse(r#" $.type == "usage" "#).unwrap();
        assert!(eq.matches(&json!({"type": "usage"})));
        assert!(!eq.matches(&json!({"type": "other"})));
        assert!(!eq.matches(&json!({})));
        let num = Filter::parse("$.v==2").unwrap();
        assert!(num.matches(&json!({"v": 2})));
        let present = Filter::parse("$.usage").unwrap();
        assert!(present.matches(&json!({"usage": {}})));
        assert!(!present.matches(&json!({"usage": null})));
        assert!(!present.matches(&json!({})));
    }

    #[test]
    fn dedup_key_templates() {
        let t = Template::parse("{$.message.id}:{$.requestId}").unwrap();
        let line = json!({"message": {"id": "m1"}, "requestId": 7});
        assert_eq!(t.render(&line).unwrap(), "m1:7");
        let missing = t.render(&json!({"requestId": "r"})).unwrap_err();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].as_str(), "$.message.id");

        let braces = Template::parse(r#"{{x}}-{$["a}b"]}"#).unwrap();
        assert_eq!(braces.render(&json!({"a}b": "v"})).unwrap(), "{x}-v");

        for (template, message) in [
            (
                "static",
                "must reference at least one field, e.g. `{$.request_id}`",
            ),
            (
                "{{$.id}}",
                "must reference at least one field, e.g. `{$.request_id}`",
            ),
            ("{$.id", "expected `}` at offset 5"),
            ("{$.id x}", "expected `}` at offset 5"),
            (
                "a}b",
                "unmatched `}` at offset 1 (write `}}` for a literal brace)",
            ),
            (
                "x:{id}",
                "invalid path at offset 3: a path must start with `$`",
            ),
            (
                "{$.a[}",
                "invalid path at offset 5: expected a quoted field name or an index after `[`",
            ),
        ] {
            assert_eq!(
                error_of(|c| c.dedup_key = template.into()),
                format!("dedup_key `{template}`: {message}"),
            );
        }
    }

    #[test]
    fn warnings() {
        let mut c = base();
        c.dedup_key = r#"s-{$["session"]}"#.into();
        c.roots.clear();
        c.input_includes_cache_read = true;
        let w: Vec<String> = c
            .validate()
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            w,
            [
                "roots: no roots configured; nothing is read until roots are set",
                "dedup_key: uses only the session id ($.session), so all calls of a session collapse into one event; reference a per-request id",
                "input_includes_cache_read: has no effect because `cache_read_tokens` is not mapped",
            ]
        );
        c.dedup_key = "{$.session}:{$.id}".into();
        assert_eq!(c.validate().unwrap().len(), 2);
    }

    #[test]
    fn text_is_path_or_constant() {
        let line = json!({"p": "anthropic", "n": 5});
        assert_eq!(
            required_text("provider", "openai")
                .unwrap()
                .eval(&line)
                .as_deref(),
            Some("openai")
        );
        assert_eq!(
            required_text("provider", "$.p")
                .unwrap()
                .eval(&line)
                .as_deref(),
            Some("anthropic")
        );
        assert_eq!(
            required_text("provider", "$.n")
                .unwrap()
                .eval(&line)
                .as_deref(),
            Some("5")
        );
        assert_eq!(required_text("provider", "$.x").unwrap().eval(&line), None);
    }

    #[test]
    fn serde_defaults_and_unknown_fields() {
        let c: GenericJsonlConfig = serde_json::from_value(json!({
            "id": "x", "timestamp": "$.t", "provider": "p", "model": "$.m",
            "output_tokens": "$.o", "dedup_key": "{$.id}"
        }))
        .unwrap();
        assert_eq!(c.glob, DEFAULT_GLOB);
        assert_eq!(c.timestamp_format, TimestampFormat::Rfc3339);
        assert!(c.roots.is_empty() && !c.input_includes_cache_read);
        let back = serde_json::to_value(&c).unwrap();
        assert!(back.get("filter").is_none() && back.get("display_name").is_none());

        let typo = serde_json::from_value::<GenericJsonlConfig>(json!({
            "id": "x", "timestamp": "$.t", "provider": "p", "model": "$.m",
            "output_token": "$.o", "dedup_key": "{$.id}"
        }))
        .unwrap_err();
        assert!(typo.to_string().contains("unknown field `output_token`"));

        let fmt: TimestampFormat = serde_json::from_value(json!("unix_millis")).unwrap();
        assert_eq!(fmt.as_str(), "unix_millis");
    }
}
