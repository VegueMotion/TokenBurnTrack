mod common;

use common::*;
use tbt_core::{ModelPrice, PriceId, PriceSource, Timestamp};
use tbt_pricing::{PriceCatalog, PriceChange, PricingError};

const LITELLM: &str = r#"{
  "sample_spec": {
    "litellm_provider": "one of https://docs.litellm.ai/docs/providers",
    "mode": "one of: chat, embedding, completion",
    "input_cost_per_token": 0.0,
    "output_cost_per_token": 0.0
  },
  "claude-opus-5-5": {
    "cache_creation_input_token_cost": 0.000005,
    "cache_creation_input_token_cost_above_1hr": 0.000008,
    "cache_read_input_token_cost": 2E-7,
    "input_cost_per_token": 0.000004,
    "litellm_provider": "anthropic",
    "max_input_tokens": 1000000,
    "mode": "chat",
    "output_cost_per_token": 0.00002,
    "provider_specific_entry": {"us": 1.1, "fast": 2},
    "supports_fast_mode": true
  },
  "claude-haiku-5-5": {
    "cache_creation_input_token_cost": 1.25E-7,
    "cache_creation_input_token_cost_above_1hr": 2E-7,
    "cache_creation_input_token_cost_above_100k_tokens": 6.25E-7,
    "cache_read_input_token_cost": 1E-8,
    "cache_read_input_token_cost_above_100k_tokens": 5E-8,
    "input_cost_per_token": 1E-7,
    "input_cost_per_token_above_100k_tokens": 5E-7,
    "input_cost_per_token_above_100k_tokens_batches": 2.5E-7,
    "litellm_provider": "anthropic",
    "mode": "chat",
    "output_cost_per_token": 5E-7,
    "output_cost_per_token_above_100k_tokens": 0.0000025
  },
  "gpt-6-luna": {
    "cache_creation_input_token_cost": 1.25e-07,
    "cache_creation_input_token_cost_above_12hr": 2.5e-07,
    "cache_read_input_token_cost": 1e-08,
    "input_cost_per_token": 1e-07,
    "input_cost_per_token_above_272k_tokens": 2e-07,
    "litellm_provider": "openai",
    "mode": "chat",
    "output_cost_per_token": 5e-07,
    "output_cost_per_token_above_272k_tokens": 7.5e-07
  },
  "gpt-5.5": {
    "cache_read_input_token_cost": 5e-07,
    "input_cost_per_token": 5e-06,
    "litellm_provider": "openai",
    "mode": "responses",
    "output_cost_per_token": 3e-05
  },
  "gemini/gemini-2.5-pro": {
    "cache_read_input_token_cost": 1.25E-7,
    "cache_read_input_token_cost_above_200k_tokens": 2.5E-7,
    "input_cost_per_token": 0.00000125,
    "input_cost_per_token_above_200k_tokens": 0.0000025,
    "litellm_provider": "gemini",
    "mode": "chat",
    "output_cost_per_token": 0.00001,
    "output_cost_per_token_above_200k_tokens": 0.000015
  },
  "gemini-2.5-pro": {
    "input_cost_per_token": 0.0000013,
    "litellm_provider": "vertex_ai-language-models",
    "mode": "chat",
    "output_cost_per_token": 0.0000105
  },
  "gemini-only-on-vertex": {
    "input_cost_per_token": 3e-06,
    "litellm_provider": "vertex_ai-language-models",
    "mode": "chat",
    "output_cost_per_token": 1.5e-05
  },
  "text-embedding-3-small": {
    "input_cost_per_token": 2e-08,
    "litellm_provider": "openai",
    "mode": "embedding",
    "output_cost_per_token": 0.0
  },
  "bedrock/anthropic.claude-opus-5-5": {
    "input_cost_per_token": 0.000004,
    "litellm_provider": "bedrock",
    "mode": "chat",
    "output_cost_per_token": 0.00002
  },
  "fal_ai/some-image-model": {
    "litellm_provider": "fal_ai",
    "mode": "image_generation",
    "output_cost_per_image": 0.03
  },
  "dall-e-9": {
    "input_cost_per_pixel": 1e-08,
    "litellm_provider": "openai",
    "mode": "chat"
  }
}"#;

const T1: &str = "2026-11-23T10:00:00Z";
const T2: &str = "2026-11-24T10:00:00Z";

fn opened(changes: &[PriceChange]) -> Vec<&ModelPrice> {
    changes
        .iter()
        .filter_map(|c| match c {
            PriceChange::Open(p) => Some(&**p),
            PriceChange::Close { .. } => None,
        })
        .collect()
}

fn find<'a>(rows: &[&'a ModelPrice], provider: &str, model: &str) -> &'a ModelPrice {
    rows.iter()
        .find(|p| p.provider == provider && p.model_pattern == model)
        .unwrap_or_else(|| panic!("{provider}/{model} not imported"))
}

fn apply_changes(rows: &mut Vec<ModelPrice>, changes: Vec<PriceChange>, next_id: &mut i64) {
    for change in changes {
        match change {
            PriceChange::Close { id, effective_to } => {
                let row = rows.iter_mut().find(|p| p.id == Some(id)).unwrap();
                row.effective_to = Some(effective_to);
            }
            PriceChange::Open(p) => {
                let mut p = *p;
                p.id = Some(PriceId(*next_id));
                *next_id += 1;
                rows.push(p);
            }
        }
    }
}

fn import(rows: &[ModelPrice], json: &str, now: Timestamp) -> Vec<PriceChange> {
    PriceCatalog::load(rows.to_vec())
        .unwrap()
        .import_litellm(json, now)
        .unwrap()
}

#[test]
fn first_import_opens_every_supported_model() {
    let changes = import(&[], LITELLM, ts(T1));
    let rows = opened(&changes);
    assert_eq!(rows.len(), changes.len());
    let mut ids: Vec<_> = rows
        .iter()
        .map(|p| format!("{}/{}", p.provider, p.model_pattern))
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        [
            "anthropic/claude-haiku-5-5",
            "anthropic/claude-opus-5-5",
            "google/gemini-2.5-pro",
            "google/gemini-only-on-vertex",
            "openai/gpt-5.5",
            "openai/gpt-6-luna",
        ]
    );
    for p in &rows {
        assert_eq!(p.source, PriceSource::Imported);
        assert_eq!(p.effective_from, ts(T1));
        assert_eq!(p.effective_to, None);
        assert_eq!(p.id, None);
    }
}

#[test]
fn float_text_converts_exactly_to_per_mtok() {
    let changes = import(&[], LITELLM, ts(T1));
    let rows = opened(&changes);

    let opus = find(&rows, "anthropic", "claude-opus-5-5");
    assert_eq!(opus.input_per_mtok.to_string(), "4");
    assert_eq!(opus.output_per_mtok.to_string(), "20");
    assert_eq!(opus.cache_write_5m_per_mtok, Some(d("5")));
    assert_eq!(opus.cache_write_1h_per_mtok, Some(d("8")));
    assert_eq!(opus.cache_read_per_mtok.unwrap().to_string(), "0.2");
    assert_eq!(opus.long_context, None);

    let luna = find(&rows, "openai", "gpt-6-luna");
    assert_eq!(luna.input_per_mtok.to_string(), "0.1");
    assert_eq!(luna.cache_write_5m_per_mtok.unwrap().to_string(), "0.125");
    assert_eq!(luna.cache_write_1h_per_mtok.unwrap().to_string(), "0.25");
    assert_eq!(luna.cache_read_per_mtok.unwrap().to_string(), "0.01");

    let gpt55 = find(&rows, "openai", "gpt-5.5");
    assert_eq!(gpt55.input_per_mtok, d("5"));
    assert_eq!(gpt55.output_per_mtok, d("30"));
    assert_eq!(gpt55.cache_write_5m_per_mtok, None);
}

#[test]
fn long_context_tiers_become_multipliers() {
    let changes = import(&[], LITELLM, ts(T1));
    let rows = opened(&changes);

    let haiku = find(&rows, "anthropic", "claude-haiku-5-5");
    assert_eq!(haiku.long_context, Some(tier(100_000, "5", "5", "5", "5")));

    let luna = find(&rows, "openai", "gpt-6-luna");
    assert_eq!(luna.long_context, Some(tier(272_000, "2", "1.5", "1", "1")));

    let gemini = find(&rows, "google", "gemini-2.5-pro");
    assert_eq!(
        gemini.long_context,
        Some(tier(200_000, "2", "1.5", "1", "2"))
    );
}

#[test]
fn provider_mapping_prefers_first_party_route() {
    let changes = import(&[], LITELLM, ts(T1));
    let rows = opened(&changes);
    let gemini = find(&rows, "google", "gemini-2.5-pro");
    assert_eq!(gemini.input_per_mtok, d("1.25"));
    assert_eq!(gemini.output_per_mtok, d("10"));
    let vertex_only = find(&rows, "google", "gemini-only-on-vertex");
    assert_eq!(vertex_only.input_per_mtok, d("3"));
    assert!(
        rows.iter()
            .all(|p| p.provider != "bedrock" && p.provider != "fal_ai")
    );
}

#[test]
fn import_is_idempotent() {
    let mut rows = Vec::new();
    let mut next_id = 1;
    let first = import(&rows, LITELLM, ts(T1));
    apply_changes(&mut rows, first, &mut next_id);
    assert_eq!(rows.len(), 6);
    assert!(import(&rows, LITELLM, ts(T2)).is_empty());
}

#[test]
fn changed_price_closes_and_reopens() {
    let mut rows = Vec::new();
    let mut next_id = 1;
    let first = import(&rows, LITELLM, ts(T1));
    apply_changes(&mut rows, first, &mut next_id);
    let old_id = rows
        .iter()
        .find(|p| p.model_pattern == "claude-opus-5-5")
        .unwrap()
        .id
        .unwrap();

    let cheaper = LITELLM.replacen(
        r#""input_cost_per_token": 0.000004,"#,
        r#""input_cost_per_token": 3.5e-06,"#,
        1,
    );
    let changes = import(&rows, &cheaper, ts(T2));
    assert_eq!(changes.len(), 2);
    assert_eq!(
        changes[0],
        PriceChange::Close {
            id: old_id,
            effective_to: ts(T2)
        }
    );
    let PriceChange::Open(new) = &changes[1] else {
        panic!("expected Open, got {:?}", changes[1]);
    };
    assert_eq!(new.model_pattern, "claude-opus-5-5");
    assert_eq!(new.input_per_mtok.to_string(), "3.5");
    assert_eq!(new.effective_from, ts(T2));
    assert_eq!(new.source, PriceSource::Imported);

    apply_changes(&mut rows, changes, &mut next_id);
    assert!(import(&rows, &cheaper, ts("2026-11-25T00:00:00Z")).is_empty());

    let cat = PriceCatalog::load(rows).unwrap();
    let before = cat.price_for("anthropic", "claude-opus-5-5", ts("2026-11-24T09:59:59Z"));
    assert_eq!(before.price().unwrap().input_per_mtok, d("4"));
    let after = cat.price_for("anthropic", "claude-opus-5-5", ts(T2));
    assert_eq!(after.price().unwrap().input_per_mtok, d("3.5"));
}

#[test]
fn changed_long_context_tier_counts_as_change() {
    let mut rows = Vec::new();
    let mut next_id = 1;
    let first = import(&rows, LITELLM, ts(T1));
    apply_changes(&mut rows, first, &mut next_id);
    let changed = LITELLM.replacen(
        r#""output_cost_per_token_above_272k_tokens": 7.5e-07"#,
        r#""output_cost_per_token_above_272k_tokens": 1e-06"#,
        1,
    );
    let changes = import(&rows, &changed, ts(T2));
    assert_eq!(changes.len(), 2);
    let PriceChange::Open(new) = &changes[1] else {
        panic!("expected Open");
    };
    assert_eq!(new.model_pattern, "gpt-6-luna");
    assert_eq!(new.long_context.as_ref().unwrap().output, d("2"));
}

#[test]
fn bundled_and_user_rows_are_never_closed() {
    let rows = vec![
        with_id(
            with_cache(
                price("anthropic", "claude-opus-5-5", "9", "9"),
                "9",
                "9",
                "9",
            ),
            100,
        ),
        with_id(
            with_source(price("openai", "gpt-5.5", "1", "1"), PriceSource::User),
            101,
        ),
    ];
    let changes = import(&rows, LITELLM, ts(T1));
    assert!(changes.iter().all(|c| matches!(c, PriceChange::Open(_))));
    assert_eq!(changes.len(), 6);
}

#[test]
fn expired_imported_row_is_not_current() {
    let mut old = with_source(
        with_window(
            price("openai", "gpt-5.5", "5", "30"),
            "2026-01-01T00:00:00Z",
            Some("2026-06-01T00:00:00Z"),
        ),
        PriceSource::Imported,
    );
    old.cache_read_per_mtok = Some(d("0.5"));
    let rows = vec![with_id(old, 5)];
    let changes = import(&rows, LITELLM, ts(T1));
    assert!(
        !changes
            .iter()
            .any(|c| matches!(c, PriceChange::Close { .. }))
    );
    assert!(
        opened(&changes)
            .iter()
            .any(|p| p.model_pattern == "gpt-5.5")
    );
}

#[test]
fn changed_row_without_id_is_an_error() {
    let mut stale = with_source(price("openai", "gpt-5.5", "1", "1"), PriceSource::Imported);
    stale.effective_from = ts("2026-01-01T00:00:00Z");
    let cat = PriceCatalog::load(vec![stale]).unwrap();
    assert!(matches!(
        cat.import_litellm(LITELLM, ts(T1)),
        Err(PricingError::Invalid(_))
    ));
}

#[test]
fn malformed_json_is_an_error() {
    let cat = PriceCatalog::load(Vec::new()).unwrap();
    assert!(matches!(
        cat.import_litellm("{not json", ts(T1)),
        Err(PricingError::Json(_))
    ));
    assert!(matches!(
        cat.import_litellm("[1, 2]", ts(T1)),
        Err(PricingError::Json(_))
    ));
}
