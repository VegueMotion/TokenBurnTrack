mod common;

use common::*;
use tbt_core::Micros;
use tbt_pricing::cost;

#[test]
fn cache_5m_1h_and_read_math() {
    let p = with_cache(price("anthropic", "m", "3", "15"), "3.75", "6", "0.30");
    let e = tokens(
        event("anthropic", "m", "2026-11-23T00:00:00Z"),
        1_000,
        500,
        2_000,
        4_000,
        100_000,
    );
    assert_eq!(
        cost(&e, &p),
        Micros(3_000 + 7_500 + 7_500 + 24_000 + 30_000)
    );
}

#[test]
fn reasoning_is_not_billed_again() {
    let p = price("openai", "m", "1.25", "10");
    let mut e = tokens(
        event("openai", "m", "2026-11-23T00:00:00Z"),
        0,
        1_000,
        0,
        0,
        0,
    );
    let without = cost(&e, &p);
    e.reasoning_tokens = 800;
    assert_eq!(cost(&e, &p), without);
    assert_eq!(without, Micros(10_000));
}

#[test]
fn missing_cache_rates_fall_back_to_input() {
    let p = price("openai", "m", "2", "8");
    let e = tokens(
        event("openai", "m", "2026-11-23T00:00:00Z"),
        10,
        0,
        10,
        10,
        10,
    );
    assert_eq!(cost(&e, &p), Micros(80));
}

#[test]
fn rounds_once_midpoint_away_from_zero() {
    let p = price("x", "m", "0.4", "0.4");
    let one_each = tokens(event("x", "m", "2026-11-23T00:00:00Z"), 1, 1, 0, 0, 0);
    assert_eq!(cost(&one_each, &p), Micros(1));

    let half = price("x", "m", "0.5", "0");
    let e = tokens(event("x", "m", "2026-11-23T00:00:00Z"), 1, 0, 0, 0, 0);
    assert_eq!(cost(&e, &half), Micros(1));

    let below = price("x", "m", "0.49", "0");
    assert_eq!(cost(&e, &below), Micros(0));

    let fine = price("x", "m", "0.000001", "0");
    let many = tokens(
        event("x", "m", "2026-11-23T00:00:00Z"),
        2_500_000,
        0,
        0,
        0,
        0,
    );
    assert_eq!(cost(&many, &fine), Micros(3));
}

fn haiku_like() -> tbt_core::ModelPrice {
    let mut p = with_cache(
        price("anthropic", "h", "0.10", "0.50"),
        "0.125",
        "0.20",
        "0.01",
    );
    p.long_context = Some(tier(100_000, "5", "5", "5", "5"));
    p
}

#[test]
fn tier_not_applied_exactly_at_threshold() {
    let e = tokens(
        event("anthropic", "h", "2026-11-23T00:00:00Z"),
        40_000,
        1_000,
        30_000,
        0,
        30_000,
    );
    assert_eq!(e.tokens().prompt(), 100_000);
    assert_eq!(cost(&e, &haiku_like()), Micros(4_000 + 500 + 3_750 + 300));
}

#[test]
fn tier_applied_one_token_above_threshold() {
    let e = tokens(
        event("anthropic", "h", "2026-11-23T00:00:00Z"),
        40_001,
        1_000,
        30_000,
        0,
        30_000,
    );
    assert_eq!(e.tokens().prompt(), 100_001);
    // 40001×0.50 + 1000×2.50 + 30000×0.625 + 30000×0.05 = 42750.5
    assert_eq!(cost(&e, &haiku_like()), Micros(42_751));
}

#[test]
fn tier_counts_cache_writes_and_reads_in_prompt() {
    let e = tokens(
        event("anthropic", "h", "2026-11-23T00:00:00Z"),
        0,
        0,
        0,
        100_001,
        0,
    );
    // 100001 × 0.20 × 5
    assert_eq!(cost(&e, &haiku_like()), Micros(100_001));
}

#[test]
fn tier_with_distinct_multipliers_and_input_fallback() {
    let mut p = price("openai", "gpt-5.5", "5", "30");
    p.cache_read_per_mtok = Some(d("0.50"));
    p.long_context = Some(tier(272_000, "2", "1.5", "1", "2"));
    let e = tokens(
        event("openai", "gpt-5.5", "2026-11-23T00:00:00Z"),
        100_000,
        2_000,
        1_000,
        0,
        171_001,
    );
    assert_eq!(e.tokens().prompt(), 272_001);
    // 100000×10 + 2000×45 + 1000×10 (cache write billed as tiered input) + 171001×1
    assert_eq!(cost(&e, &p), Micros(1_000_000 + 90_000 + 10_000 + 171_001));

    let below = tokens(
        event("openai", "gpt-5.5", "2026-11-23T00:00:00Z"),
        100_000,
        2_000,
        1_000,
        0,
        171_000,
    );
    assert_eq!(cost(&below, &p), Micros(500_000 + 60_000 + 5_000 + 85_500));
}

#[test]
fn zero_tokens_cost_zero() {
    let p = haiku_like();
    let e = event("anthropic", "h", "2026-11-23T00:00:00Z");
    assert_eq!(cost(&e, &p), Micros::ZERO);
}

#[test]
fn huge_counts_saturate_instead_of_panicking() {
    let p = price("x", "m", "79228162514264337593543950335", "0");
    let e = tokens(
        event("x", "m", "2026-11-23T00:00:00Z"),
        u64::MAX,
        0,
        0,
        0,
        0,
    );
    assert_eq!(cost(&e, &p), Micros(i64::MAX));
}
