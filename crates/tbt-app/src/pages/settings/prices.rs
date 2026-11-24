use jiff::Timestamp;
use tbt_core::{Decimal, LongContextTier, ModelPrice, PriceSource};

pub const MAX_ROWS: usize = 200;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PriceFilter {
    pub search: String,
    pub show_closed: bool,
    pub source: Option<PriceSource>,
}

pub fn is_closed(p: &ModelPrice, now: Timestamp) -> bool {
    p.effective_to.is_some_and(|to| to <= now)
}

impl PriceFilter {
    pub fn matches(&self, p: &ModelPrice, now: Timestamp) -> bool {
        if !self.show_closed && is_closed(p, now) {
            return false;
        }
        if self.source.is_some_and(|s| s != p.source) {
            return false;
        }
        let needle = self.search.trim().to_lowercase();
        needle.is_empty()
            || needle.split_whitespace().all(|w| {
                p.provider.to_lowercase().contains(w) || p.model_pattern.to_lowercase().contains(w)
            })
    }

    pub fn apply<'a>(&self, prices: &'a [ModelPrice], now: Timestamp) -> Vec<&'a ModelPrice> {
        let mut rows: Vec<_> = prices.iter().filter(|p| self.matches(p, now)).collect();
        rows.sort_by(|a, b| {
            a.provider
                .cmp(&b.provider)
                .then_with(|| a.model_pattern.cmp(&b.model_pattern))
                .then_with(|| b.source.cmp(&a.source))
                .then_with(|| b.effective_from.cmp(&a.effective_from))
        });
        rows
    }
}

pub fn rate(d: Option<Decimal>) -> String {
    d.map(|d| d.normalize().to_string())
        .unwrap_or_else(|| "–".to_owned())
}

pub fn long_context(lc: Option<&LongContextTier>) -> String {
    let Some(lc) = lc else {
        return "–".to_owned();
    };
    let k = if lc.threshold_tokens % 1000 == 0 {
        format!("{}k", lc.threshold_tokens / 1000)
    } else {
        lc.threshold_tokens.to_string()
    };
    format!(">{k} ×{}/×{}", lc.input.normalize(), lc.output.normalize())
}

pub fn source_label(s: PriceSource) -> &'static str {
    match s {
        PriceSource::Bundled => "bundled",
        PriceSource::Imported => "imported",
        PriceSource::User => "user",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tbt_core::PriceId;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn price(
        id: i64,
        provider: &str,
        model: &str,
        src: PriceSource,
        to: Option<&str>,
    ) -> ModelPrice {
        ModelPrice {
            id: Some(PriceId(id)),
            provider: provider.into(),
            model_pattern: model.into(),
            effective_from: ts("2026-01-01T00:00:00Z"),
            effective_to: to.map(ts),
            input_per_mtok: "3.00".parse().unwrap(),
            output_per_mtok: Decimal::from(15),
            cache_write_5m_per_mtok: None,
            cache_write_1h_per_mtok: None,
            cache_read_per_mtok: Some("0.300".parse().unwrap()),
            long_context: None,
            source: src,
        }
    }

    #[test]
    fn filters_and_sorts() {
        let now = ts("2026-11-22T20:00:00Z");
        let prices = vec![
            price(1, "openai", "gpt-5", PriceSource::Bundled, None),
            price(
                2,
                "anthropic",
                "claude-opus-5-5",
                PriceSource::Bundled,
                None,
            ),
            price(3, "anthropic", "claude-opus-5-5", PriceSource::User, None),
            price(
                4,
                "anthropic",
                "claude-haiku-5-5",
                PriceSource::Imported,
                Some("2026-06-01T00:00:00Z"),
            ),
        ];
        let ids = |f: &PriceFilter| -> Vec<i64> {
            f.apply(&prices, now)
                .iter()
                .map(|p| p.id.unwrap().0)
                .collect()
        };
        assert_eq!(ids(&PriceFilter::default()), vec![3, 2, 1]);
        let all = PriceFilter {
            show_closed: true,
            ..PriceFilter::default()
        };
        assert_eq!(ids(&all), vec![4, 3, 2, 1]);
        let search = PriceFilter {
            search: "Anthropic OPUS".into(),
            ..PriceFilter::default()
        };
        assert_eq!(ids(&search), vec![3, 2]);
        let user = PriceFilter {
            source: Some(PriceSource::User),
            ..PriceFilter::default()
        };
        assert_eq!(ids(&user), vec![3]);
    }

    #[test]
    fn display_helpers() {
        assert_eq!(rate(Some("3.00".parse().unwrap())), "3");
        assert_eq!(rate(Some("0.075".parse().unwrap())), "0.075");
        assert_eq!(rate(None), "–");
        let lc = LongContextTier {
            threshold_tokens: 200_000,
            input: Decimal::from(2),
            output: "1.50".parse().unwrap(),
            cache_write: Decimal::from(2),
            cache_read: Decimal::from(2),
        };
        assert_eq!(long_context(Some(&lc)), ">200k ×2/×1.5");
        assert_eq!(long_context(None), "–");
    }
}
