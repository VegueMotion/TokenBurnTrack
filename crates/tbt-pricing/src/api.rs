#![allow(unused_variables, dead_code)]

use serde::{Deserialize, Serialize};
use tbt_core::{
    Decimal, LongContextTier, Micros, ModelPrice, PriceId, PriceSource, PricedEvent, Timestamp,
    UsageEvent,
};

pub type Result<T, E = PricingError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum PricingError {
    #[error("invalid price TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid model pattern {pattern:?}: {message}")]
    Pattern { pattern: String, message: String },
    #[error("invalid price row: {0}")]
    Invalid(String),
}

/// All known price rows, indexed for lookup.
pub struct PriceCatalog {
    prices: Vec<ModelPrice>,
}

impl PriceCatalog {
    /// Validates rows and compiles glob patterns.
    pub fn load(prices: Vec<ModelPrice>) -> Result<PriceCatalog> {
        todo!()
    }

    pub fn prices(&self) -> &[ModelPrice] {
        &self.prices
    }

    /// Considers only rows of `provider` effective at `ts`. Precedence: source priority
    /// (`User > Imported > Bundled`), then exact id over glob, then the most specific glob
    /// (longest literal prefix).
    pub fn price_for(&self, provider: &str, model: &str, ts: Timestamp) -> PriceMatch<'_> {
        todo!()
    }

    /// Looks up the price and computes the cost; unpriced events get
    /// `cost = 0, price_id = None`. Requires prices loaded from the store (`id` set).
    pub fn apply(&self, event: UsageEvent) -> PricedEvent {
        todo!()
    }

    /// Diffs the LiteLLM `model_prices_and_context_window.json` against the open
    /// `Imported` rows: changed price → `Close` + `Open` (`effective_from = now`), new
    /// model → `Open`, unchanged → nothing. Never touches `Bundled` or `User` rows.
    pub fn import_litellm(&self, json: &str, now: Timestamp) -> Result<Vec<PriceChange>> {
        todo!()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceMatch<'a> {
    Exact(&'a ModelPrice),
    Glob(&'a ModelPrice),
    Unpriced,
}

impl<'a> PriceMatch<'a> {
    pub fn price(&self) -> Option<&'a ModelPrice> {
        match *self {
            PriceMatch::Exact(p) | PriceMatch::Glob(p) => Some(p),
            PriceMatch::Unpriced => None,
        }
    }
}

/// `Σ tokens × rate_per_mtok` (which is already micro-USD), long-context multipliers
/// applied when prompt tokens exceed the threshold, rounded once at the end. Missing
/// cache rates fall back to the input rate. `reasoning_tokens` is not billed separately.
pub fn cost(event: &UsageEvent, price: &ModelPrice) -> Micros {
    todo!()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriceChange {
    Close {
        id: PriceId,
        effective_to: Timestamp,
    },
    Open(Box<ModelPrice>),
}

/// `assets/prices.toml`: a list of `[[price]]` tables.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PriceFile {
    #[serde(rename = "price", default)]
    pub prices: Vec<PriceEntry>,
}

/// One `[[price]]` table. Rates are USD per 1M tokens, written as strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceEntry {
    pub provider: String,
    /// Exact id or glob.
    pub model: String,
    pub effective_from: Timestamp,
    #[serde(default)]
    pub effective_to: Option<Timestamp>,
    pub input: Decimal,
    pub output: Decimal,
    #[serde(default)]
    pub cache_write_5m: Option<Decimal>,
    #[serde(default)]
    pub cache_write_1h: Option<Decimal>,
    #[serde(default)]
    pub cache_read: Option<Decimal>,
    #[serde(default)]
    pub long_context: Option<LongContextTier>,
}

impl PriceEntry {
    pub fn into_model_price(self, source: PriceSource) -> ModelPrice {
        ModelPrice {
            id: None,
            provider: self.provider,
            model_pattern: self.model,
            effective_from: self.effective_from,
            effective_to: self.effective_to,
            input_per_mtok: self.input,
            output_per_mtok: self.output,
            cache_write_5m_per_mtok: self.cache_write_5m,
            cache_write_1h_per_mtok: self.cache_write_1h,
            cache_read_per_mtok: self.cache_read,
            long_context: self.long_context,
            source,
        }
    }
}

/// Parses the bundled price file; every row gets `source = Bundled`, `id = None`.
pub fn parse_prices_toml(text: &str) -> Result<Vec<ModelPrice>> {
    let file: PriceFile = toml::from_str(text)?;
    Ok(file
        .prices
        .into_iter()
        .map(|e| e.into_model_price(PriceSource::Bundled))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
[[price]]
provider = "anthropic"
model = "claude-sonnet-4-5*"
effective_from = "2025-09-29T00:00:00Z"
input = "3"
output = "15"
cache_write_5m = "3.75"
cache_write_1h = "6"
cache_read = "0.30"

[price.long_context]
threshold = 200000
input = "2"
output = "1.5"
cache_write = "2"
cache_read = "2"

[[price]]
provider = "openai"
model = "gpt-5"
effective_from = "2025-08-07T00:00:00Z"
effective_to = "2026-01-01T00:00:00Z"
input = "1.25"
output = "10"
cache_read = "0.125"
"#;

    #[test]
    fn parses_documented_toml_shape() {
        let prices = parse_prices_toml(EXAMPLE).unwrap();
        assert_eq!(prices.len(), 2);

        let sonnet = &prices[0];
        assert_eq!(sonnet.provider, "anthropic");
        assert_eq!(sonnet.model_pattern, "claude-sonnet-4-5*");
        assert!(sonnet.is_glob());
        assert_eq!(sonnet.cache_read_per_mtok, Some("0.30".parse().unwrap()));
        assert_eq!(sonnet.source, PriceSource::Bundled);
        let lc = sonnet.long_context.as_ref().unwrap();
        assert_eq!(lc.threshold_tokens, 200_000);
        assert_eq!(lc.output, "1.5".parse().unwrap());

        let gpt = &prices[1];
        assert_eq!(
            gpt.effective_to,
            Some("2026-01-01T00:00:00Z".parse().unwrap())
        );
        assert_eq!(gpt.cache_write_5m_per_mtok, None);
        assert!(gpt.long_context.is_none());
    }

    #[test]
    fn rejects_unknown_fields() {
        let bad = r#"
[[price]]
provider = "x"
model = "y"
effective_from = "2025-01-01T00:00:00Z"
input = "1"
output = "1"
inptu_typo = "1"
"#;
        assert!(matches!(parse_prices_toml(bad), Err(PricingError::Toml(_))));
    }

    #[test]
    fn empty_file_is_empty_list() {
        assert!(parse_prices_toml("").unwrap().is_empty());
    }
}
