use std::collections::HashSet;

use serde_json::Value;
use tbt_collector::CollectorConfig;
use tbt_collector::registry::generic_adapters;

const REQUIRED: [&str; 5] = ["id", "timestamp", "provider", "model", "dedup_key"];
const OPTIONAL_TEXT: [&str; 12] = [
    "display_name",
    "glob",
    "filter",
    "session_id",
    "input_tokens",
    "output_tokens",
    "cache_write_5m_tokens",
    "cache_write_1h_tokens",
    "cache_read_tokens",
    "reasoning_tokens",
    "reported_cost",
    "project_path",
];
const OTHER_FIELDS: [&str; 4] = [
    "git_branch",
    "roots",
    "timestamp_format",
    "input_includes_cache_read",
];
const USAGE_FIELDS: [&str; 7] = [
    "input_tokens",
    "output_tokens",
    "cache_write_5m_tokens",
    "cache_write_1h_tokens",
    "cache_read_tokens",
    "reasoning_tokens",
    "reported_cost",
];
const TIMESTAMP_FORMATS: [&str; 3] = ["rfc3339", "unix_seconds", "unix_millis"];
const BUILTIN_IDS: [&str; 4] = ["claude_code", "codex", "gemini_cli", "opencode"];

pub const EXAMPLE: &str = r#"[[generic_jsonl]]
id = "my_tool"
display_name = "My tool"
roots = ["/home/me/.my-tool/logs"]
glob = "**/*.jsonl"
timestamp = "$.ts"
provider = "$.provider"
model = "$.model"
input_tokens = "$.usage.input"
output_tokens = "$.usage.output"
dedup_key = "{$.request_id}"
"#;

pub fn entries_text(cfg: &CollectorConfig) -> String {
    if cfg.generic_jsonl.is_empty() {
        return String::new();
    }
    let only = CollectorConfig {
        generic_jsonl: cfg.generic_jsonl.clone(),
        ..CollectorConfig::default()
    };
    let text = only.to_toml().unwrap_or_default();
    let kept: Vec<&str> = text
        .lines()
        .skip_while(|l| {
            l.trim().is_empty()
                || l.starts_with("privacy =")
                || l.starts_with("sync_interval_minutes =")
        })
        .collect();
    let mut out = kept.join("\n");
    out.push('\n');
    out
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntryReport {
    pub label: String,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Checked {
    pub config: CollectorConfig,
    pub entries: Vec<EntryReport>,
}

impl Checked {
    pub fn is_valid(&self) -> bool {
        self.entries.iter().all(|e| e.errors.is_empty())
    }
}

pub fn check(text: &str) -> Result<Checked, String> {
    let parsed = CollectorConfig::parse(text).map_err(|e| format!("TOML: {e}"))?;
    let rest = CollectorConfig {
        generic_jsonl: Vec::new(),
        ..parsed.clone()
    };
    if rest != CollectorConfig::default() {
        return Err(
            "Only [[generic_jsonl]] tables belong here; set privacy, sync interval and data directory under General"
                .into(),
        );
    }
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for (i, raw) in parsed.generic_jsonl.iter().enumerate() {
        let json = serde_json::to_value(raw).unwrap_or(Value::Null);
        let mut report = check_entry(&json);
        let id = json.get("id").and_then(Value::as_str).unwrap_or_default();
        report.label = if id.is_empty() {
            format!("Entry {}", i + 1)
        } else {
            format!("Entry {} ({id})", i + 1)
        };
        if !id.is_empty() && !seen.insert(id.to_owned()) {
            report.errors.push(format!("id `{id}` is used twice"));
        }
        if report.errors.is_empty() {
            let single = CollectorConfig {
                generic_jsonl: vec![raw.clone()],
                ..CollectorConfig::default()
            };
            if generic_adapters(&single).is_empty() {
                report.errors.push(
                    "rejected by the adapter: check the JSON paths (they start with `$`), glob, filter and dedup_key template"
                        .into(),
                );
            }
        }
        entries.push(report);
    }
    Ok(Checked {
        config: parsed,
        entries,
    })
}

fn check_entry(v: &Value) -> EntryReport {
    let mut r = EntryReport::default();
    let Some(table) = v.as_object() else {
        r.errors.push("must be a table".into());
        return r;
    };
    for key in table.keys() {
        let known = REQUIRED
            .iter()
            .chain(&OPTIONAL_TEXT)
            .chain(&OTHER_FIELDS)
            .any(|k| k == key);
        if !known {
            r.errors.push(format!("unknown field `{key}`"));
        }
    }
    let text = |key: &str| table.get(key).and_then(Value::as_str).map(str::trim);
    for key in REQUIRED {
        match table.get(key) {
            None => r.errors.push(format!("`{key}` is required")),
            Some(Value::String(s)) if s.trim().is_empty() => {
                r.errors.push(format!("`{key}`: a value is required"))
            }
            Some(Value::String(_)) => {}
            Some(_) => r.errors.push(format!("`{key}` must be a string")),
        }
    }
    for key in OPTIONAL_TEXT.iter().chain(&["git_branch"]) {
        if table.get(*key).is_some_and(|v| !v.is_string()) {
            r.errors.push(format!("`{key}` must be a string"));
        }
    }
    match table.get("roots") {
        None => r
            .warnings
            .push("roots: no roots configured; nothing is read until roots are set".into()),
        Some(Value::Array(a)) if a.iter().all(Value::is_string) => {
            if a.is_empty() {
                r.warnings
                    .push("roots: no roots configured; nothing is read until roots are set".into());
            }
        }
        Some(_) => r.errors.push("`roots` must be a list of paths".into()),
    }
    if let Some(f) = table.get("timestamp_format")
        && !f.as_str().is_some_and(|f| TIMESTAMP_FORMATS.contains(&f))
    {
        r.errors.push(format!(
            "`timestamp_format` must be one of {}",
            TIMESTAMP_FORMATS.join(", ")
        ));
    }
    let includes_cache = table.get("input_includes_cache_read");
    if includes_cache.is_some_and(|v| !v.is_boolean()) {
        r.errors
            .push("`input_includes_cache_read` must be true or false".into());
    }
    if let Some(id) = text("id").filter(|id| !id.is_empty()) {
        if id.len() > 64 {
            r.errors.push("id: must be at most 64 characters".into());
        } else if !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            r.errors
                .push("id: may contain only lowercase letters a-z, digits and `_`".into());
        } else if BUILTIN_IDS.contains(&id) {
            r.errors
                .push(format!("id: `{id}` is a built-in harness id"));
        }
    }
    let mapped = |key: &str| text(key).is_some_and(|s| !s.is_empty());
    if !USAGE_FIELDS.iter().any(|k| mapped(k)) {
        r.errors.push(
            "no usage field is mapped; set at least one of the token fields or `reported_cost`"
                .into(),
        );
    }
    if includes_cache.and_then(Value::as_bool) == Some(true) && !mapped("cache_read_tokens") {
        r.warnings.push(
            "input_includes_cache_read: has no effect because `cache_read_tokens` is not mapped"
                .into(),
        );
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use tbt_adapters::GenericJsonlConfig;

    fn adapter_verdict(text: &str) -> Vec<Result<usize, String>> {
        CollectorConfig::parse(text)
            .unwrap()
            .generic_jsonl
            .into_iter()
            .map(|v| {
                let cfg: GenericJsonlConfig = v.try_into().map_err(|e| format!("{e}"))?;
                cfg.validate().map(|w| w.len()).map_err(|e| e.to_string())
            })
            .collect()
    }

    fn assert_agrees(text: &str) -> Checked {
        let checked = check(text).unwrap();
        let verdict = adapter_verdict(text);
        assert_eq!(checked.entries.len(), verdict.len());
        for (mine, theirs) in checked.entries.iter().zip(&verdict) {
            assert_eq!(
                mine.errors.is_empty(),
                theirs.is_ok(),
                "{mine:?} vs {theirs:?}"
            );
            if let Ok(n) = theirs {
                assert_eq!(mine.warnings.len(), *n, "{mine:?}");
            }
        }
        checked
    }

    #[test]
    fn example_is_valid() {
        let c = assert_agrees(EXAMPLE);
        assert!(c.is_valid());
        assert_eq!(c.entries[0].label, "Entry 1 (my_tool)");
        assert!(c.entries[0].warnings.is_empty());
    }

    #[test]
    fn missing_fields_and_unknown_keys() {
        let text = r#"
[[generic_jsonl]]
id = "tool"
timestamp = "$.ts"
model = "$.model"
dedup_key = "{$.id}"
input_tokens = "$.in"
colour = "blue"
"#;
        let c = assert_agrees(text);
        assert!(!c.is_valid());
        let errors = &c.entries[0].errors;
        assert!(
            errors.contains(&"unknown field `colour`".to_owned()),
            "{errors:?}"
        );
        assert!(
            errors.contains(&"`provider` is required".to_owned()),
            "{errors:?}"
        );
    }

    #[test]
    fn id_rules_and_usage_fields() {
        let entry = |id: &str, usage: &str| {
            format!(
                "[[generic_jsonl]]\nid = \"{id}\"\nroots = [\"/x\"]\ntimestamp = \"$.ts\"\nprovider = \"openai\"\nmodel = \"$.m\"\ndedup_key = \"{{$.id}}\"\n{usage}\n"
            )
        };
        let c = assert_agrees(&entry("My-Tool", "input_tokens = \"$.in\""));
        assert!(c.entries[0].errors[0].contains("lowercase"));
        let c = assert_agrees(&entry("codex", "input_tokens = \"$.in\""));
        assert!(c.entries[0].errors[0].contains("built-in"));
        let c = assert_agrees(&entry("tool", ""));
        assert!(c.entries[0].errors[0].contains("no usage field"));
        let c = assert_agrees(&entry("tool", "input_tokens = \"no-dollar\""));
        assert!(c.entries[0].errors[0].contains("rejected by the adapter"));
        let c = assert_agrees(&entry("tool", "timestamp_format = \"iso\""));
        assert!(!c.is_valid());
    }

    #[test]
    fn warnings_match_adapter() {
        let text = r#"
[[generic_jsonl]]
id = "tool"
timestamp = "$.ts"
provider = "openai"
model = "$.m"
dedup_key = "{$.id}"
input_tokens = "$.in"
input_includes_cache_read = true
"#;
        let c = assert_agrees(text);
        assert!(c.is_valid());
        let w = &c.entries[0].warnings;
        assert_eq!(w.len(), 2);
        assert!(w[0].starts_with("roots:"));
        assert!(w[1].starts_with("input_includes_cache_read:"));
    }

    #[test]
    fn duplicate_ids_and_foreign_keys() {
        let twice = format!("{EXAMPLE}\n{EXAMPLE}");
        let c = check(&twice).unwrap();
        assert!(c.entries[0].errors.is_empty());
        assert_eq!(c.entries[1].errors, vec!["id `my_tool` is used twice"]);

        assert!(
            check("privacy = \"off\"\n")
                .unwrap_err()
                .contains("General")
        );
        assert!(
            check("[[generic_jsonl]\nid=")
                .unwrap_err()
                .starts_with("TOML")
        );
        let empty = check("").unwrap();
        assert!(empty.entries.is_empty() && empty.is_valid());
    }

    #[test]
    fn text_round_trip() {
        let c = check(EXAMPLE).unwrap();
        let text = entries_text(&c.config);
        assert!(text.starts_with("[[generic_jsonl]]"), "{text}");
        assert!(!text.contains("privacy"));
        let again = check(&text).unwrap();
        assert_eq!(again.config, c.config);
        assert_eq!(entries_text(&CollectorConfig::default()), "");
    }
}
