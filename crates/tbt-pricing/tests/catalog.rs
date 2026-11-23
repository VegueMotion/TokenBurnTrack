mod common;

use common::*;
use tbt_core::{Micros, PriceId, PriceSource};
use tbt_pricing::{PriceCatalog, PriceMatch, PricingError};

const NOW: &str = "2026-11-23T12:00:00Z";

fn input_of(m: PriceMatch<'_>) -> Option<String> {
    m.price().map(|p| p.input_per_mtok.to_string())
}

#[test]
fn exact_beats_longer_glob_beats_shorter_glob() {
    let cat = PriceCatalog::load(vec![
        price("anthropic", "claude-*", "1", "1"),
        price("anthropic", "claude-sonnet-*", "2", "2"),
        price("anthropic", "claude-sonnet-4-5", "3", "3"),
        price("anthropic", "claude-sonnet-4-5-20??????", "4", "4"),
    ])
    .unwrap();
    let at = ts(NOW);

    let m = cat.price_for("anthropic", "claude-sonnet-4-5", at);
    assert!(matches!(m, PriceMatch::Exact(_)));
    assert_eq!(input_of(m).as_deref(), Some("3"));

    let m = cat.price_for("anthropic", "claude-sonnet-4-5-20250929", at);
    assert!(matches!(m, PriceMatch::Glob(_)));
    assert_eq!(input_of(m).as_deref(), Some("4"));

    let m = cat.price_for("anthropic", "claude-sonnet-5-5", at);
    assert_eq!(input_of(m).as_deref(), Some("2"));

    let m = cat.price_for("anthropic", "claude-opus-5-5", at);
    assert_eq!(input_of(m).as_deref(), Some("1"));
}

#[test]
fn source_priority_beats_exactness() {
    let cat = PriceCatalog::load(vec![
        price("anthropic", "claude-opus-5-5", "1", "1"),
        with_source(
            price("anthropic", "claude-*", "2", "2"),
            PriceSource::Imported,
        ),
        with_source(
            price("anthropic", "claude-opus-5-5", "3", "3"),
            PriceSource::Imported,
        ),
        with_source(
            price("anthropic", "claude-opus-*", "4", "4"),
            PriceSource::User,
        ),
    ])
    .unwrap();
    let at = ts(NOW);

    let m = cat.price_for("anthropic", "claude-opus-5-5", at);
    assert!(matches!(m, PriceMatch::Glob(_)));
    assert_eq!(m.price().unwrap().source, PriceSource::User);
    assert_eq!(input_of(m).as_deref(), Some("4"));

    let m = cat.price_for("anthropic", "claude-sonnet-5-5", at);
    assert_eq!(m.price().unwrap().source, PriceSource::Imported);
    assert_eq!(input_of(m).as_deref(), Some("2"));

    let without_user = PriceCatalog::load(vec![
        price("anthropic", "claude-opus-5-5", "1", "1"),
        with_source(
            price("anthropic", "claude-*", "2", "2"),
            PriceSource::Imported,
        ),
    ])
    .unwrap();
    let m = without_user.price_for("anthropic", "claude-opus-5-5", at);
    assert!(matches!(m, PriceMatch::Glob(_)));
    assert_eq!(input_of(m).as_deref(), Some("2"));
}

#[test]
fn provider_must_match() {
    let cat = PriceCatalog::load(vec![price("openai", "gpt-5.5", "5", "30")]).unwrap();
    assert_eq!(
        cat.price_for("azure", "gpt-5.5", ts(NOW)),
        PriceMatch::Unpriced
    );
    assert!(matches!(
        cat.price_for("openai", "gpt-5.5", ts(NOW)),
        PriceMatch::Exact(_)
    ));
}

#[test]
fn unknown_model_is_unpriced_not_zero() {
    let cat = PriceCatalog::load(vec![with_id(price("openai", "gpt-5.5", "5", "30"), 1)]).unwrap();
    assert_eq!(
        cat.price_for("openai", "gpt-9", ts(NOW)),
        PriceMatch::Unpriced
    );
    let priced = cat.apply(tokens(event("openai", "gpt-9", NOW), 1_000, 1_000, 0, 0, 0));
    assert!(!priced.is_priced());
    assert_eq!(priced.price_id, None);
    assert_eq!(priced.cost, Micros::ZERO);
}

#[test]
fn glob_does_not_match_partial_ids() {
    let cat = PriceCatalog::load(vec![price("openai", "gpt-5.5-20??-??-??", "5", "30")]).unwrap();
    let at = ts(NOW);
    assert!(matches!(
        cat.price_for("openai", "gpt-5.5-2026-04-23", at),
        PriceMatch::Glob(_)
    ));
    assert_eq!(cat.price_for("openai", "gpt-5.5", at), PriceMatch::Unpriced);
    assert_eq!(
        cat.price_for("openai", "gpt-5.5-pro-2026-04-23", at),
        PriceMatch::Unpriced
    );
}

#[test]
fn price_switch_mid_session() {
    let old = with_id(
        with_window(
            price("anthropic", "claude-opus-5-5", "4", "20"),
            "2026-11-01T00:00:00Z",
            Some("2026-11-15T00:00:00Z"),
        ),
        10,
    );
    let new = with_id(
        with_window(
            price("anthropic", "claude-opus-5-5", "5", "25"),
            "2026-11-15T00:00:00Z",
            None,
        ),
        11,
    );
    let cat = PriceCatalog::load(vec![old, new]).unwrap();

    let before = cat.apply(tokens(
        event("anthropic", "claude-opus-5-5", "2026-11-14T23:59:59Z"),
        1_000,
        100,
        0,
        0,
        0,
    ));
    assert_eq!(before.price_id, Some(PriceId(10)));
    assert_eq!(before.cost, Micros(4_000 + 2_000));

    let after = cat.apply(tokens(
        event("anthropic", "claude-opus-5-5", "2026-11-15T00:00:00Z"),
        1_000,
        100,
        0,
        0,
        0,
    ));
    assert_eq!(after.price_id, Some(PriceId(11)));
    assert_eq!(after.cost, Micros(5_000 + 2_500));

    let too_early = cat.apply(event(
        "anthropic",
        "claude-opus-5-5",
        "2026-10-31T23:59:59Z",
    ));
    assert!(!too_early.is_priced());
}

#[test]
fn closed_row_is_ignored_after_effective_to() {
    let cat = PriceCatalog::load(vec![with_window(
        price("google", "gemini-2.5-pro", "1.25", "10"),
        "2025-06-01T00:00:00Z",
        Some("2026-12-01T00:00:00Z"),
    )])
    .unwrap();
    assert!(
        cat.price_for("google", "gemini-2.5-pro", ts("2026-11-30T23:59:59Z"))
            .price()
            .is_some()
    );
    assert_eq!(
        cat.price_for("google", "gemini-2.5-pro", ts("2026-12-01T00:00:00Z")),
        PriceMatch::Unpriced
    );
}

#[test]
fn newer_row_wins_among_equal_candidates() {
    let cat = PriceCatalog::load(vec![
        with_window(price("x", "m", "1", "1"), "2025-01-01T00:00:00Z", None),
        with_window(price("x", "m", "2", "2"), "2026-01-01T00:00:00Z", None),
    ])
    .unwrap();
    assert_eq!(
        input_of(cat.price_for("x", "m", ts(NOW))).as_deref(),
        Some("2")
    );
}

#[test]
fn apply_uses_row_id_and_cost() {
    let p = with_id(
        with_cache(
            price("anthropic", "claude-opus-5-5", "4", "20"),
            "5",
            "8",
            "0.20",
        ),
        7,
    );
    let cat = PriceCatalog::load(vec![p]).unwrap();
    let e = tokens(
        event("anthropic", "claude-opus-5-5", NOW),
        3,
        200,
        1_000,
        2_000,
        50_000,
    );
    let priced = cat.apply(e);
    assert_eq!(priced.price_id, Some(PriceId(7)));
    assert_eq!(priced.cost, Micros(12 + 4_000 + 5_000 + 16_000 + 10_000));
    assert_eq!(priced.event.model, "claude-opus-5-5");
}

#[test]
fn load_rejects_bad_glob() {
    let err = PriceCatalog::load(vec![price("x", "claude-[", "1", "1")])
        .err()
        .unwrap();
    assert!(matches!(err, PricingError::Pattern { .. }), "{err}");
}

#[test]
fn load_rejects_inverted_window_and_negative_rates() {
    let inverted = with_window(
        price("x", "m", "1", "1"),
        "2026-01-01T00:00:00Z",
        Some("2026-01-01T00:00:00Z"),
    );
    assert!(matches!(
        PriceCatalog::load(vec![inverted]),
        Err(PricingError::Invalid(_))
    ));
    assert!(matches!(
        PriceCatalog::load(vec![price("x", "m", "-1", "1")]),
        Err(PricingError::Invalid(_))
    ));
    assert!(matches!(
        PriceCatalog::load(vec![price("", "m", "1", "1")]),
        Err(PricingError::Invalid(_))
    ));
    let mut neg_tier = price("x", "m", "1", "1");
    neg_tier.long_context = Some(tier(10, "1", "-2", "1", "1"));
    assert!(matches!(
        PriceCatalog::load(vec![neg_tier]),
        Err(PricingError::Invalid(_))
    ));
}

#[test]
fn empty_catalog_prices_nothing() {
    let cat = PriceCatalog::load(Vec::new()).unwrap();
    assert!(cat.prices().is_empty());
    assert_eq!(cat.price_for("x", "m", ts(NOW)), PriceMatch::Unpriced);
}
