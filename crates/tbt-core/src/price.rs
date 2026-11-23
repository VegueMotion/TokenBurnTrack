use std::fmt;
use std::str::FromStr;

use jiff::Timestamp;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::PriceId;
use crate::event::str_enum;

/// One versioned price row (`model_price`). Rates are USD per 1M tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPrice {
    /// `None` until the row is stored.
    pub id: Option<PriceId>,
    pub provider: String,
    /// Exact model id, or a glob such as `claude-sonnet-4-5*`.
    pub model_pattern: String,
    pub effective_from: Timestamp,
    /// Exclusive; `None` = still in effect.
    pub effective_to: Option<Timestamp>,
    pub input_per_mtok: Decimal,
    pub output_per_mtok: Decimal,
    pub cache_write_5m_per_mtok: Option<Decimal>,
    pub cache_write_1h_per_mtok: Option<Decimal>,
    pub cache_read_per_mtok: Option<Decimal>,
    pub long_context: Option<LongContextTier>,
    pub source: PriceSource,
}

impl ModelPrice {
    /// `effective_from <= ts < effective_to`.
    pub fn is_effective_at(&self, ts: Timestamp) -> bool {
        self.effective_from <= ts && self.effective_to.is_none_or(|to| ts < to)
    }

    pub fn is_glob(&self) -> bool {
        self.model_pattern.contains(['*', '?', '['])
    }
}

/// Multipliers applied to every rate of the call when its prompt tokens
/// (input + cache writes + cache reads) exceed `threshold_tokens`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LongContextTier {
    #[serde(rename = "threshold")]
    pub threshold_tokens: u64,
    #[serde(default = "one")]
    pub input: Decimal,
    #[serde(default = "one")]
    pub output: Decimal,
    #[serde(default = "one")]
    pub cache_write: Decimal,
    #[serde(default = "one")]
    pub cache_read: Decimal,
}

fn one() -> Decimal {
    Decimal::ONE
}

/// Ordered by priority: `User > Imported > Bundled`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum PriceSource {
    #[default]
    Bundled,
    Imported,
    User,
}

str_enum!(PriceSource {
    Bundled => "bundled",
    Imported => "imported",
    User => "user",
});

#[cfg(test)]
mod tests {
    use super::*;

    fn price(pattern: &str, from: &str, to: Option<&str>) -> ModelPrice {
        ModelPrice {
            id: None,
            provider: "anthropic".into(),
            model_pattern: pattern.into(),
            effective_from: from.parse().unwrap(),
            effective_to: to.map(|t| t.parse().unwrap()),
            input_per_mtok: Decimal::from(3),
            output_per_mtok: Decimal::from(15),
            cache_write_5m_per_mtok: None,
            cache_write_1h_per_mtok: None,
            cache_read_per_mtok: None,
            long_context: None,
            source: PriceSource::Bundled,
        }
    }

    #[test]
    fn effective_window_is_half_open() {
        let p = price(
            "claude-sonnet-4-5",
            "2026-01-01T00:00:00Z",
            Some("2026-02-01T00:00:00Z"),
        );
        assert!(!p.is_effective_at("2025-12-31T23:59:59Z".parse().unwrap()));
        assert!(p.is_effective_at("2026-01-01T00:00:00Z".parse().unwrap()));
        assert!(!p.is_effective_at("2026-02-01T00:00:00Z".parse().unwrap()));
        let open = price("x", "2026-01-01T00:00:00Z", None);
        assert!(open.is_effective_at("2099-01-01T00:00:00Z".parse().unwrap()));
    }

    #[test]
    fn glob_detection() {
        assert!(price("claude-sonnet-4-5*", "2026-01-01T00:00:00Z", None).is_glob());
        assert!(!price("gpt-5", "2026-01-01T00:00:00Z", None).is_glob());
    }

    #[test]
    fn source_priority_order() {
        assert!(PriceSource::User > PriceSource::Imported);
        assert!(PriceSource::Imported > PriceSource::Bundled);
        assert_eq!("imported".parse::<PriceSource>(), Ok(PriceSource::Imported));
        assert_eq!(PriceSource::User.as_str(), "user");
    }

    #[test]
    fn long_context_defaults_to_one() {
        let t: LongContextTier =
            serde_json::from_str(r#"{"threshold":200000,"input":"2"}"#).unwrap();
        assert_eq!(t.threshold_tokens, 200_000);
        assert_eq!(t.input, Decimal::from(2));
        assert_eq!(t.output, Decimal::ONE);
    }
}
