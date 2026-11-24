use std::collections::BTreeMap;
use std::str::FromStr;

use serde_json::value::RawValue;
use tbt_core::{Decimal, LongContextTier, ModelPrice, PriceSource, Timestamp};

use crate::Result;

const ROUTE_PREFIXES: &[&str] = &[
    "anthropic",
    "openai",
    "gemini",
    "deepseek",
    "xai",
    "mistral",
];

const ACCEPTED_MODES: &[&str] = &["chat", "completion", "responses"];

fn map_provider(litellm_provider: &str) -> Option<(&'static str, u8)> {
    Some(match litellm_provider {
        "anthropic" => ("anthropic", 0),
        "openai" => ("openai", 0),
        "text-completion-openai" => ("openai", 1),
        "gemini" => ("google", 0),
        "vertex_ai-language-models" => ("google", 1),
        "deepseek" => ("deepseek", 0),
        "xai" => ("xai", 0),
        "mistral" => ("mistral", 0),
        _ => return None,
    })
}

type Entry<'a> = BTreeMap<String, &'a RawValue>;

pub(crate) fn parse(json: &str, now: Timestamp) -> Result<Vec<ModelPrice>> {
    let root: BTreeMap<String, &RawValue> = serde_json::from_str(json)?;
    let mut best: BTreeMap<(String, String), (u8, ModelPrice)> = BTreeMap::new();
    for (key, raw) in root {
        let Ok(entry) = serde_json::from_str::<Entry>(raw.get()) else {
            continue;
        };
        let Some((price, rank)) = entry_to_price(&key, &entry, now) else {
            continue;
        };
        let slot = (price.provider.clone(), price.model_pattern.clone());
        match best.get(&slot) {
            Some((r, _)) if *r <= rank => {}
            _ => {
                best.insert(slot, (rank, price));
            }
        }
    }
    Ok(best.into_values().map(|(_, p)| p).collect())
}

fn entry_to_price(key: &str, entry: &Entry, now: Timestamp) -> Option<(ModelPrice, u8)> {
    let (provider, rank) = map_provider(&string_field(entry, "litellm_provider")?)?;
    if let Some(mode) = string_field(entry, "mode")
        && !ACCEPTED_MODES.contains(&mode.as_str())
    {
        return None;
    }
    let model = model_id(key)?;

    let per_token = |field: &str| {
        entry
            .get(field)
            .and_then(|raw| decimal_from_json(raw.get()))
    };
    let input = per_token("input_cost_per_token")?;
    let output = per_token("output_cost_per_token")?;
    let cache_write_5m = per_token("cache_creation_input_token_cost");
    let cache_write_1h = per_token("cache_creation_input_token_cost_above_1hr")
        .or_else(|| per_token("cache_creation_input_token_cost_above_12hr"));
    let cache_read = per_token("cache_read_input_token_cost");

    let long_context = tier_threshold_k(entry).and_then(|k| {
        let above = |field: &str| per_token(&format!("{field}_above_{k}k_tokens"));
        let ratio = |above: Option<Decimal>, base: Option<Decimal>| {
            match (above, base) {
                (Some(a), Some(b)) if !b.is_zero() => a.checked_div(b).map(|r| r.normalize()),
                _ => None,
            }
            .unwrap_or(Decimal::ONE)
        };
        Some(LongContextTier {
            threshold_tokens: k.checked_mul(1000)?,
            input: ratio(above("input_cost_per_token"), Some(input)),
            output: ratio(above("output_cost_per_token"), Some(output)),
            cache_write: ratio(above("cache_creation_input_token_cost"), cache_write_5m),
            cache_read: ratio(above("cache_read_input_token_cost"), cache_read),
        })
    });

    let price = ModelPrice {
        id: None,
        provider: provider.to_owned(),
        model_pattern: model,
        effective_from: now,
        effective_to: None,
        input_per_mtok: per_mtok(input)?,
        output_per_mtok: per_mtok(output)?,
        cache_write_5m_per_mtok: cache_write_5m.and_then(per_mtok),
        cache_write_1h_per_mtok: cache_write_1h.and_then(per_mtok),
        cache_read_per_mtok: cache_read.and_then(per_mtok),
        long_context,
        source: PriceSource::Imported,
    };
    Some((price, rank))
}

fn string_field(entry: &Entry, field: &str) -> Option<String> {
    serde_json::from_str(entry.get(field)?.get()).ok()
}

fn model_id(key: &str) -> Option<String> {
    let model = match key.split_once('/') {
        Some((prefix, rest)) if ROUTE_PREFIXES.contains(&prefix) => rest,
        Some(_) => return None,
        None => key,
    };
    if model.is_empty() || model.contains(['/', '*', '?', '[']) {
        return None;
    }
    Some(model.to_owned())
}

fn tier_threshold_k(entry: &Entry) -> Option<u64> {
    entry
        .keys()
        .filter_map(|k| {
            k.strip_prefix("input_cost_per_token_above_")?
                .strip_suffix("k_tokens")?
                .parse::<u64>()
                .ok()
        })
        .min()
}

fn per_mtok(per_token: Decimal) -> Option<Decimal> {
    per_token
        .checked_mul(Decimal::from(1_000_000))
        .map(|d| d.normalize())
}

pub(crate) fn decimal_from_json(text: &str) -> Option<Decimal> {
    let text = text.trim();
    if text.starts_with('"') {
        let s: String = serde_json::from_str(text).ok()?;
        return decimal_from_text(s.trim());
    }
    decimal_from_text(text)
}

fn decimal_from_text(text: &str) -> Option<Decimal> {
    let (mantissa, exponent) = match text.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.parse::<i64>().ok()?),
        None => (text, 0),
    };
    if mantissa.is_empty() || mantissa.starts_with('+') {
        return None;
    }
    let m = Decimal::from_str(mantissa).ok()?.normalize();
    let scale = i64::from(m.scale()) - exponent;
    if scale >= 0 {
        let scale = u32::try_from(scale).ok()?;
        Decimal::try_from_i128_with_scale(m.mantissa(), scale).ok()
    } else {
        let factor = 10i128.checked_pow(u32::try_from(-scale).ok()?)?;
        let value = m.mantissa().checked_mul(factor)?;
        Decimal::try_from_i128_with_scale(value, 0).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[test]
    fn json_number_text_is_exact() {
        assert_eq!(decimal_from_json("3e-06"), Some(d("0.000003")));
        assert_eq!(decimal_from_json("3E-06"), Some(d("0.000003")));
        assert_eq!(decimal_from_json("2E-7"), Some(d("0.0000002")));
        assert_eq!(decimal_from_json("1.25E-7"), Some(d("0.000000125")));
        assert_eq!(decimal_from_json("0.00000375"), Some(d("0.00000375")));
        assert_eq!(decimal_from_json("1.5e+3"), Some(d("1500")));
        assert_eq!(decimal_from_json("1e2"), Some(d("100")));
        assert_eq!(decimal_from_json("0"), Some(Decimal::ZERO));
        assert_eq!(decimal_from_json("-2.5E-1"), Some(d("-0.25")));
        assert_eq!(decimal_from_json("\"0.000015\""), Some(d("0.000015")));
        assert_eq!(
            decimal_from_json("1.0000000000000002e-7"),
            Some(d("0.00000010000000000000002"))
        );
    }

    #[test]
    fn per_mtok_has_no_f64_noise() {
        for (text, expected) in [
            ("1E-7", "0.1"),
            ("2E-7", "0.2"),
            ("4E-7", "0.4"),
            ("0.000003", "3"),
            ("0.00000375", "3.75"),
            ("1.25E-7", "0.125"),
            ("1.8E-6", "1.8"),
        ] {
            let got = per_mtok(decimal_from_json(text).unwrap()).unwrap();
            assert_eq!(got, d(expected), "{text}");
            assert_eq!(got.to_string(), expected, "{text}");
        }
    }

    #[test]
    fn rejects_non_numbers() {
        assert_eq!(decimal_from_json("null"), None);
        assert_eq!(decimal_from_json("true"), None);
        assert_eq!(decimal_from_json("{}"), None);
        assert_eq!(decimal_from_json("\"abc\""), None);
        assert_eq!(decimal_from_json("e5"), None);
        assert_eq!(decimal_from_json("1e"), None);
        assert_eq!(decimal_from_json("1e-40"), None);
    }

    #[test]
    fn model_id_strips_route_prefix() {
        assert_eq!(
            model_id("gemini/gemini-2.5-pro").as_deref(),
            Some("gemini-2.5-pro")
        );
        assert_eq!(model_id("gpt-5.5").as_deref(), Some("gpt-5.5"));
        assert_eq!(model_id("bedrock/foo"), None);
        assert_eq!(model_id("openai/a/b"), None);
        assert_eq!(model_id("weird*model"), None);
    }
}
