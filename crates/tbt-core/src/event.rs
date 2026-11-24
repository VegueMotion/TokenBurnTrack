use std::fmt;
use std::str::FromStr;

use jiff::Timestamp;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::{HarnessId, Micros, PriceId, SourceRef, TokenCounts};

/// One API call, normalized. Token fields follow the conventions in `CONTRACTS.md`:
/// `input_tokens` excludes cache reads/writes, `output_tokens` includes reasoning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageEvent {
    pub harness: HarnessId,
    /// Stable per real API call; built only from provider/harness ids, never from the
    /// session id or file path.
    pub dedup_key: String,
    pub session_ext_id: String,
    pub ts: Timestamp,
    pub provider: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_write_5m_tokens: u64,
    pub cache_write_1h_tokens: u64,
    pub cache_read_tokens: u64,
    pub reasoning_tokens: u64,
    pub reported_cost: Option<Decimal>,
    pub project_path: Option<String>,
    pub git_branch: Option<String>,
    pub raw: Option<Box<RawValue>>,
}

impl UsageEvent {
    pub fn tokens(&self) -> TokenCounts {
        TokenCounts {
            input: self.input_tokens,
            output: self.output_tokens,
            cache_write_5m: self.cache_write_5m_tokens,
            cache_write_1h: self.cache_write_1h_tokens,
            cache_read: self.cache_read_tokens,
            reasoning: self.reasoning_tokens,
        }
    }

    pub fn set_tokens(&mut self, t: TokenCounts) {
        self.input_tokens = t.input;
        self.output_tokens = t.output;
        self.cache_write_5m_tokens = t.cache_write_5m;
        self.cache_write_1h_tokens = t.cache_write_1h;
        self.cache_read_tokens = t.cache_read;
        self.reasoning_tokens = t.reasoning;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricedEvent {
    pub event: UsageEvent,
    pub cost: Micros,
    pub price_id: Option<PriceId>,
}

impl PricedEvent {
    pub fn unpriced(event: UsageEvent) -> PricedEvent {
        PricedEvent {
            event,
            cost: Micros::ZERO,
            price_id: None,
        }
    }

    pub fn is_priced(&self) -> bool {
        self.price_id.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionUpsert {
    pub harness: HarnessId,
    pub ext_id: String,
    pub project_path: Option<String>,
    pub title: Option<String>,
    pub git_branch: Option<String>,
    pub harness_version: Option<String>,
    pub started_at: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
    pub parent_ext_id: Option<String>,
    pub resumed_from_ext_id: Option<String>,
    pub baseline_hint: Option<BaselineHint>,
}

impl SessionUpsert {
    pub fn new(harness: HarnessId, ext_id: impl Into<String>) -> SessionUpsert {
        SessionUpsert {
            harness,
            ext_id: ext_id.into(),
            project_path: None,
            title: None,
            git_branch: None,
            harness_version: None,
            started_at: None,
            ended_at: None,
            parent_ext_id: None,
            resumed_from_ext_id: None,
            baseline_hint: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineHint {
    pub first_event_key: Option<String>,
    pub last_total: TokenCounts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageRecord {
    pub session_ext_id: String,
    pub key: String,
    pub ts: Timestamp,
    pub role: MessageRole,
    pub kind: Option<MessageKind>,
    pub tool_name: Option<String>,
    pub preview: Option<String>,
    pub event_dedup_key: Option<String>,
}

macro_rules! str_enum {
    ($name:ident { $($variant:ident => $s:literal),* $(,)? }) => {
        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $s),* }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = String;
            fn from_str(s: &str) -> Result<Self, String> {
                match s {
                    $($s => Ok($name::$variant),)*
                    other => Err(format!(concat!("unknown ", stringify!($name), ": {}"), other)),
                }
            }
        }
    };
}
pub(crate) use str_enum;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
}

str_enum!(MessageRole {
    User => "user",
    Assistant => "assistant",
    System => "system",
    Tool => "tool",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Text,
    ToolUse,
    ToolResult,
    Thinking,
}

str_enum!(MessageKind {
    Text => "text",
    ToolUse => "tool_use",
    ToolResult => "tool_result",
    Thinking => "thinking",
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseWarning {
    pub harness: HarnessId,
    pub source: String,
    pub line: Option<u64>,
    pub byte_offset: Option<u64>,
    pub message: String,
}

impl ParseWarning {
    pub fn new(harness: HarnessId, src: &SourceRef, message: impl Into<String>) -> ParseWarning {
        ParseWarning {
            harness,
            source: src.path.to_string_lossy().into_owned(),
            line: None,
            byte_offset: None,
            message: message.into(),
        }
    }

    pub fn at(mut self, line: u64, byte_offset: u64) -> ParseWarning {
        self.line = Some(line);
        self.byte_offset = Some(byte_offset);
        self
    }
}

impl fmt::Display for ParseWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.harness, self.source)?;
        if let Some(line) = self.line {
            write!(f, ":{line}")?;
        }
        write!(f, ": {}", self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SourceKind;

    fn sample_event() -> UsageEvent {
        UsageEvent {
            harness: HarnessId::CLAUDE_CODE,
            dedup_key: "msg_1:req_1".into(),
            session_ext_id: "s1".into(),
            ts: "2026-11-23T10:15:00Z".parse().unwrap(),
            provider: "anthropic".into(),
            model: "claude-sonnet-4-5".into(),
            input_tokens: 10,
            output_tokens: 20,
            cache_write_5m_tokens: 30,
            cache_write_1h_tokens: 40,
            cache_read_tokens: 50,
            reasoning_tokens: 5,
            reported_cost: Some("0.0123".parse().unwrap()),
            project_path: Some("/home/u/repo".into()),
            git_branch: Some("main".into()),
            raw: Some(RawValue::from_string(r#"{"a":1}"#.into()).unwrap()),
        }
    }

    #[test]
    fn usage_event_serde_roundtrip() {
        let e = sample_event();
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""reported_cost":"0.0123""#));
        assert!(json.contains(r#""raw":{"a":1}"#));
        assert!(json.contains(r#""ts":"2026-11-23T10:15:00Z""#));
        let back: UsageEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(serde_json::to_string(&back).unwrap(), json);
    }

    #[test]
    fn tokens_roundtrip() {
        let mut e = sample_event();
        let t = e.tokens();
        assert_eq!(t.total(), 150);
        e.set_tokens(TokenCounts::default());
        assert_eq!(e.input_tokens, 0);
        e.set_tokens(t);
        assert_eq!(e.tokens(), t);
    }

    #[test]
    fn priced_event_unpriced() {
        let p = PricedEvent::unpriced(sample_event());
        assert!(!p.is_priced());
        assert_eq!(p.cost, Micros::ZERO);
    }

    #[test]
    fn message_enums_str() {
        assert_eq!(MessageKind::ToolResult.as_str(), "tool_result");
        assert_eq!("tool_use".parse::<MessageKind>(), Ok(MessageKind::ToolUse));
        assert_eq!(
            "assistant".parse::<MessageRole>(),
            Ok(MessageRole::Assistant)
        );
        assert!("bogus".parse::<MessageRole>().is_err());
        assert_eq!(
            serde_json::to_string(&MessageKind::ToolUse).unwrap(),
            "\"tool_use\""
        );
    }

    #[test]
    fn parse_warning_display() {
        let src = SourceRef::new("/logs/a.jsonl", SourceKind::JsonlFile);
        let w = ParseWarning::new(HarnessId::CODEX, &src, "bad json").at(3, 120);
        assert_eq!(w.to_string(), "[codex] /logs/a.jsonl:3: bad json");
        assert_eq!(w.byte_offset, Some(120));
    }

    #[test]
    fn session_upsert_new_is_minimal() {
        let s = SessionUpsert::new(HarnessId::CODEX, "abc");
        assert_eq!(s.ext_id, "abc");
        assert!(s.parent_ext_id.is_none() && s.baseline_hint.is_none());
    }
}
