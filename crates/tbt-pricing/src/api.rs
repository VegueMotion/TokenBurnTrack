use globset::{GlobBuilder, GlobMatcher};
use serde::{Deserialize, Serialize};
use tbt_core::{
    Decimal, LongContextTier, Micros, ModelPrice, PriceId, PriceSource, PricedEvent, Timestamp,
    UsageEvent,
};

use crate::litellm;

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
    matchers: Vec<Option<GlobMatcher>>,
}

impl PriceCatalog {
    /// Validates rows and compiles glob patterns.
    pub fn load(prices: Vec<ModelPrice>) -> Result<PriceCatalog> {
        let matchers = prices
            .iter()
            .map(|p| {
                validate(p)?;
                p.is_glob()
                    .then(|| compile_glob(&p.model_pattern))
                    .transpose()
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(PriceCatalog { prices, matchers })
    }

    pub fn prices(&self) -> &[ModelPrice] {
        &self.prices
    }

    /// Considers only rows of `provider` effective at `ts`. Precedence: source priority
    /// (`User > Imported > Bundled`), then exact id over glob, then the most specific glob
    /// (longest literal prefix).
    pub fn price_for(&self, provider: &str, model: &str, ts: Timestamp) -> PriceMatch<'_> {
        let mut best: Option<(MatchRank, &ModelPrice)> = None;
        for (price, matcher) in self.prices.iter().zip(&self.matchers) {
            if price.provider != provider || !price.is_effective_at(ts) {
                continue;
            }
            let specificity = match matcher {
                None if price.model_pattern == model => usize::MAX,
                Some(glob) if glob.is_match(model) => literal_prefix_len(&price.model_pattern),
                _ => continue,
            };
            let rank = MatchRank {
                source: price.source,
                exact: matcher.is_none(),
                specificity,
                effective_from: price.effective_from,
                id: price.id.map(|id| id.0),
            };
            if best.as_ref().is_none_or(|(b, _)| rank > *b) {
                best = Some((rank, price));
            }
        }
        match best {
            Some((rank, price)) if rank.exact => PriceMatch::Exact(price),
            Some((_, price)) => PriceMatch::Glob(price),
            None => PriceMatch::Unpriced,
        }
    }

    /// Looks up the price and computes the cost; unpriced events get
    /// `cost = 0, price_id = None`. Requires prices loaded from the store (`id` set).
    pub fn apply(&self, event: UsageEvent) -> PricedEvent {
        let Some(price) = self
            .price_for(&event.provider, &event.model, event.ts)
            .price()
        else {
            return PricedEvent::unpriced(event);
        };
        debug_assert!(
            price.id.is_some(),
            "PriceCatalog::apply needs stored prices (id set), got {:?}",
            price.model_pattern
        );
        let Some(id) = price.id else {
            return PricedEvent::unpriced(event);
        };
        PricedEvent {
            cost: cost(&event, price),
            price_id: Some(id),
            event,
        }
    }

    /// Diffs the LiteLLM `model_prices_and_context_window.json` against the open
    /// `Imported` rows: changed price → `Close` + `Open` (`effective_from = now`), new
    /// model → `Open`, unchanged → nothing. Never touches `Bundled` or `User` rows.
    pub fn import_litellm(&self, json: &str, now: Timestamp) -> Result<Vec<PriceChange>> {
        let mut changes = Vec::new();
        for new in litellm::parse(json, now)? {
            let current = self
                .prices
                .iter()
                .filter(|p| {
                    p.source == PriceSource::Imported
                        && p.provider == new.provider
                        && p.model_pattern == new.model_pattern
                        && p.is_effective_at(now)
                })
                .max_by_key(|p| p.effective_from);
            match current {
                Some(cur) if same_rates(cur, &new) => {}
                Some(cur) => {
                    let id = cur.id.ok_or_else(|| {
                        PricingError::Invalid(format!(
                            "imported row {}/{} has no id",
                            cur.provider, cur.model_pattern
                        ))
                    })?;
                    changes.push(PriceChange::Close {
                        id,
                        effective_to: now,
                    });
                    changes.push(PriceChange::Open(Box::new(new)));
                }
                None => changes.push(PriceChange::Open(Box::new(new))),
            }
        }
        Ok(changes)
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct MatchRank {
    source: PriceSource,
    exact: bool,
    specificity: usize,
    effective_from: Timestamp,
    id: Option<i64>,
}

fn literal_prefix_len(pattern: &str) -> usize {
    pattern
        .find(['*', '?', '[', '{', '\\'])
        .unwrap_or(pattern.len())
}

fn compile_glob(pattern: &str) -> Result<GlobMatcher> {
    GlobBuilder::new(pattern)
        .literal_separator(false)
        .build()
        .map(|g| g.compile_matcher())
        .map_err(|e| PricingError::Pattern {
            pattern: pattern.to_owned(),
            message: e.kind().to_string(),
        })
}

fn validate(p: &ModelPrice) -> Result<()> {
    let invalid = |what: &str| {
        Err(PricingError::Invalid(format!(
            "{}/{}: {what}",
            p.provider, p.model_pattern
        )))
    };
    if p.provider.trim().is_empty() {
        return invalid("empty provider");
    }
    if p.model_pattern.trim().is_empty() {
        return invalid("empty model pattern");
    }
    if p.effective_to.is_some_and(|to| to <= p.effective_from) {
        return invalid("effective_to is not after effective_from");
    }
    let rates = [Some(p.input_per_mtok), Some(p.output_per_mtok)]
        .into_iter()
        .chain([
            p.cache_write_5m_per_mtok,
            p.cache_write_1h_per_mtok,
            p.cache_read_per_mtok,
        ])
        .flatten();
    let multipliers = p
        .long_context
        .iter()
        .flat_map(|lc| [lc.input, lc.output, lc.cache_write, lc.cache_read]);
    if rates.chain(multipliers).any(|r| r.is_sign_negative()) {
        return invalid("negative rate or multiplier");
    }
    Ok(())
}

fn same_rates(a: &ModelPrice, b: &ModelPrice) -> bool {
    a.input_per_mtok == b.input_per_mtok
        && a.output_per_mtok == b.output_per_mtok
        && a.cache_write_5m_per_mtok == b.cache_write_5m_per_mtok
        && a.cache_write_1h_per_mtok == b.cache_write_1h_per_mtok
        && a.cache_read_per_mtok == b.cache_read_per_mtok
        && a.long_context == b.long_context
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
    let tokens = event.tokens();
    let tier = price
        .long_context
        .as_ref()
        .filter(|lc| tokens.prompt() > lc.threshold_tokens);
    let multiplier = |pick: fn(&LongContextTier) -> Decimal| tier.map_or(Decimal::ONE, pick);

    let input = mul(price.input_per_mtok, multiplier(|lc| lc.input));
    let output = mul(price.output_per_mtok, multiplier(|lc| lc.output));
    let cache_rate = |rate: Option<Decimal>, m: Decimal| rate.map_or(input, |r| mul(r, m));
    let cache_write_5m = cache_rate(
        price.cache_write_5m_per_mtok,
        multiplier(|lc| lc.cache_write),
    );
    let cache_write_1h = cache_rate(
        price.cache_write_1h_per_mtok,
        multiplier(|lc| lc.cache_write),
    );
    let cache_read = cache_rate(price.cache_read_per_mtok, multiplier(|lc| lc.cache_read));

    let total = [
        (tokens.input, input),
        (tokens.output, output),
        (tokens.cache_write_5m, cache_write_5m),
        (tokens.cache_write_1h, cache_write_1h),
        (tokens.cache_read, cache_read),
    ]
    .into_iter()
    .fold(Decimal::ZERO, |acc, (n, rate)| {
        add(acc, mul(Decimal::from(n), rate))
    });
    Micros::from_micros_decimal(total)
}

fn mul(a: Decimal, b: Decimal) -> Decimal {
    a.checked_mul(b)
        .unwrap_or_else(|| saturated(a.is_sign_negative() != b.is_sign_negative()))
}

fn add(a: Decimal, b: Decimal) -> Decimal {
    a.checked_add(b)
        .unwrap_or_else(|| saturated(a.is_sign_negative()))
}

fn saturated(negative: bool) -> Decimal {
    if negative { Decimal::MIN } else { Decimal::MAX }
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
