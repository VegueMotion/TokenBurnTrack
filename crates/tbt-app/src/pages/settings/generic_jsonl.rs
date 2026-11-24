use std::collections::HashSet;

use tbt_adapters::GenericJsonlConfig;
use tbt_collector::CollectorConfig;

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
        let mut report = EntryReport::default();
        let id = raw.get("id").and_then(|v| v.as_str()).unwrap_or_default();
        report.label = if id.is_empty() {
            format!("Entry {}", i + 1)
        } else {
            format!("Entry {} ({id})", i + 1)
        };
        match raw.clone().try_into::<GenericJsonlConfig>() {
            Err(e) => report.errors.push(e.message().trim().to_owned()),
            Ok(cfg) => match cfg.validate() {
                Ok(warnings) => report.warnings = warnings.iter().map(|w| w.to_string()).collect(),
                Err(e) => report.errors.push(e.to_string()),
            },
        }
        if !id.is_empty() && !seen.insert(id.to_owned()) {
            report.errors.push(format!("id `{id}` is used twice"));
        }
        entries.push(report);
    }
    Ok(Checked {
        config: parsed,
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_ok(text: &str) -> Checked {
        check(text).unwrap()
    }

    #[test]
    fn example_is_valid() {
        let c = check_ok(EXAMPLE);
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
        let c = check_ok(text);
        assert!(!c.is_valid());
        let errors = &c.entries[0].errors;
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("colour") || errors[0].contains("provider"),
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
        let c = check_ok(&entry("My-Tool", "input_tokens = \"$.in\""));
        assert!(c.entries[0].errors[0].contains("My-Tool"));
        let c = check_ok(&entry("codex", "input_tokens = \"$.in\""));
        assert!(c.entries[0].errors[0].contains("codex"));
        let c = check_ok(&entry("tool", ""));
        assert!(!c.is_valid());
        let c = check_ok(&entry("tool", "input_tokens = \"$.in[\""));
        assert!(
            c.entries[0].errors[0].contains("$.in["),
            "{:?}",
            c.entries[0].errors
        );
        let c = check_ok(&entry("tool", "timestamp_format = \"iso\""));
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
        let c = check_ok(text);
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
