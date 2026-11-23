#![allow(dead_code)]

use tbt_core::{
    Decimal, HarnessId, LongContextTier, ModelPrice, PriceId, PriceSource, Timestamp, UsageEvent,
};

pub fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

pub fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

pub fn price(provider: &str, pattern: &str, input: &str, output: &str) -> ModelPrice {
    ModelPrice {
        id: None,
        provider: provider.into(),
        model_pattern: pattern.into(),
        effective_from: ts("2025-01-01T00:00:00Z"),
        effective_to: None,
        input_per_mtok: d(input),
        output_per_mtok: d(output),
        cache_write_5m_per_mtok: None,
        cache_write_1h_per_mtok: None,
        cache_read_per_mtok: None,
        long_context: None,
        source: PriceSource::Bundled,
    }
}

pub fn with_id(mut p: ModelPrice, id: i64) -> ModelPrice {
    p.id = Some(PriceId(id));
    p
}

pub fn with_source(mut p: ModelPrice, source: PriceSource) -> ModelPrice {
    p.source = source;
    p
}

pub fn with_cache(mut p: ModelPrice, cw5m: &str, cw1h: &str, cr: &str) -> ModelPrice {
    p.cache_write_5m_per_mtok = Some(d(cw5m));
    p.cache_write_1h_per_mtok = Some(d(cw1h));
    p.cache_read_per_mtok = Some(d(cr));
    p
}

pub fn with_window(mut p: ModelPrice, from: &str, to: Option<&str>) -> ModelPrice {
    p.effective_from = ts(from);
    p.effective_to = to.map(ts);
    p
}

pub fn tier(threshold: u64, input: &str, output: &str, cw: &str, cr: &str) -> LongContextTier {
    LongContextTier {
        threshold_tokens: threshold,
        input: d(input),
        output: d(output),
        cache_write: d(cw),
        cache_read: d(cr),
    }
}

pub fn event(provider: &str, model: &str, at: &str) -> UsageEvent {
    UsageEvent {
        harness: HarnessId::CLAUDE_CODE,
        dedup_key: format!("{model}@{at}"),
        session_ext_id: "s1".into(),
        ts: ts(at),
        provider: provider.into(),
        model: model.into(),
        input_tokens: 0,
        output_tokens: 0,
        cache_write_5m_tokens: 0,
        cache_write_1h_tokens: 0,
        cache_read_tokens: 0,
        reasoning_tokens: 0,
        reported_cost: None,
        project_path: None,
        git_branch: None,
        raw: None,
    }
}

pub fn tokens(
    mut e: UsageEvent,
    input: u64,
    output: u64,
    cw5m: u64,
    cw1h: u64,
    cr: u64,
) -> UsageEvent {
    e.input_tokens = input;
    e.output_tokens = output;
    e.cache_write_5m_tokens = cw5m;
    e.cache_write_1h_tokens = cw1h;
    e.cache_read_tokens = cr;
    e
}
