mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use serde_json::Value;
use tbt_core::{Micros, ModelPrice, PriceSource};
use tbt_pricing::{PriceCatalog, PriceMatch, cost, parse_prices_toml};

const BUNDLED: &str = include_str!("../../../assets/prices.toml");
const NOW: &str = "2026-11-23T12:00:00Z";

const KNOWN_UNPRICED: &[(&str, &str)] = &[
    ("opencode", "beam-free"),
    ("opencode", "deepseek-v4-flash-free"),
    ("ollama", "qwen3.5:9b"),
];

fn bundled() -> Vec<ModelPrice> {
    parse_prices_toml(BUNDLED).expect("assets/prices.toml parses")
}

fn catalog() -> PriceCatalog {
    PriceCatalog::load(bundled()).expect("assets/prices.toml loads")
}

#[test]
fn bundled_file_parses_and_loads() {
    let rows = bundled();
    assert!(rows.len() > 50);
    assert!(rows.iter().all(|p| p.source == PriceSource::Bundled));
    assert!(rows.iter().all(|p| p.id.is_none()));
    for provider in ["anthropic", "openai", "google"] {
        assert!(rows.iter().any(|p| p.provider == provider), "{provider}");
    }
    catalog();
}

#[test]
fn bundled_windows_do_not_overlap() {
    let rows = bundled();
    for (i, a) in rows.iter().enumerate() {
        for b in &rows[i + 1..] {
            if a.provider != b.provider || a.model_pattern != b.model_pattern {
                continue;
            }
            let a_before_b = a.effective_to.is_some_and(|to| to <= b.effective_from);
            let b_before_a = b.effective_to.is_some_and(|to| to <= a.effective_from);
            assert!(
                a_before_b || b_before_a,
                "overlapping rows for {}/{}",
                a.provider,
                a.model_pattern
            );
        }
    }
}

#[test]
fn bundled_spot_checks() {
    let cat = catalog();
    let at = ts(NOW);
    let input = |provider: &str, model: &str, at| {
        cat.price_for(provider, model, at)
            .price()
            .map(|p| p.input_per_mtok)
    };

    let opus = cat.price_for("anthropic", "claude-opus-5-5", at);
    assert!(matches!(opus, PriceMatch::Exact(_)));
    let opus = opus.price().unwrap();
    assert_eq!(opus.output_per_mtok, d("20"));
    assert_eq!(opus.cache_write_5m_per_mtok, Some(d("5")));
    assert_eq!(opus.cache_write_1h_per_mtok, Some(d("8")));
    assert_eq!(opus.cache_read_per_mtok, Some(d("0.20")));

    assert!(matches!(
        cat.price_for("anthropic", "claude-sonnet-4-5-20250929", at),
        PriceMatch::Glob(_)
    ));
    assert_eq!(
        input("anthropic", "claude-sonnet-4-5-20250929", at),
        Some(d("3"))
    );
    assert_eq!(
        input("anthropic", "claude-haiku-4-5-20251001", at),
        Some(d("1"))
    );
    assert_eq!(input("openai", "gpt-5.5-2026-04-23", at), Some(d("5")));
    assert_eq!(
        input("openai", "gpt-5-mini-2025-08-07", at),
        Some(d("0.25"))
    );
    assert_eq!(input("openai", "gpt-5-2025-08-07", at), Some(d("1.25")));

    let carbon_old = ts("2026-11-30T23:59:59Z");
    let carbon_new = ts("2026-12-01T00:00:00Z");
    assert_eq!(input("google", "gemini-4-carbon", carbon_old), Some(d("2")));
    assert_eq!(input("google", "gemini-4-carbon", carbon_new), Some(d("4")));

    let haiku = cat
        .price_for("anthropic", "claude-haiku-5-5", at)
        .price()
        .unwrap();
    assert_eq!(
        haiku.long_context.as_ref().unwrap().threshold_tokens,
        100_000
    );
}

#[test]
fn bundled_cost_of_a_claude_code_call() {
    let cat = catalog();
    let price = cat
        .price_for("anthropic", "claude-opus-5-5", ts(NOW))
        .price()
        .unwrap();
    let e = tokens(
        event("anthropic", "claude-opus-5-5", NOW),
        6,
        1_234,
        0,
        12_000,
        180_000,
    );
    assert_eq!(cost(&e, price), Micros(24 + 24_680 + 96_000 + 36_000));
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn collect_models(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::Object(map) => {
            for (k, v) in map {
                if k == "model"
                    && let Value::String(s) = v
                {
                    out.insert(s.clone());
                }
                collect_models(v, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| collect_models(v, out)),
        _ => {}
    }
}

fn json_models(path: &Path) -> BTreeSet<String> {
    let text = fs::read_to_string(path).unwrap();
    let mut models = BTreeSet::new();
    if let Ok(v) = serde_json::from_str::<Value>(&text) {
        collect_models(&v, &mut models);
    }
    for line in text.lines() {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            collect_models(&v, &mut models);
        }
    }
    models.remove("<synthetic>");
    models
}

fn quoted_after<'a>(text: &'a str, key: &str) -> Option<(&'a str, &'a str)> {
    let rest = text.strip_prefix(key)?;
    let end = rest.find('"')?;
    Some((&rest[..end], &rest[end + 1..]))
}

fn opencode_models(path: &Path) -> BTreeSet<(String, String)> {
    let bytes = fs::read(path).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    let mut pairs = BTreeSet::new();
    for (i, _) in text.match_indices('"') {
        let at = &text[i..];
        if let Some((model, rest)) = quoted_after(at, r#""modelID":""#)
            && let Some((provider, _)) = quoted_after(rest, r#","providerID":""#)
        {
            pairs.insert((provider.to_owned(), model.to_owned()));
        }
        if let Some((provider, rest)) = quoted_after(at, r#""providerID":""#)
            && let Some((model, _)) = quoted_after(rest, r#","modelID":""#)
        {
            pairs.insert((provider.to_owned(), model.to_owned()));
        }
    }
    pairs
}

fn fixture_models() -> BTreeSet<(String, String)> {
    let root = fixtures_dir();
    let mut files = Vec::new();
    walk(&root, &mut files);
    let mut pairs = BTreeSet::new();
    for path in files {
        let rel = path.strip_prefix(&root).unwrap();
        let harness = rel
            .components()
            .next()
            .unwrap()
            .as_os_str()
            .to_string_lossy();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let provider = match harness.as_ref() {
            "claude_code" => "anthropic",
            "codex" => "openai",
            "gemini_cli" => "google",
            "opencode" if ext == "db" => {
                pairs.extend(opencode_models(&path));
                continue;
            }
            _ => continue,
        };
        if ext == "json" || ext == "jsonl" {
            for model in json_models(&path) {
                pairs.insert((provider.to_owned(), model));
            }
        }
    }
    pairs
}

#[test]
fn every_fixture_model_is_priced_or_known_unpriced() {
    let cat = catalog();
    let at = ts(NOW);
    let found = fixture_models();

    for expected in [
        ("anthropic", "claude-opus-5-5"),
        ("anthropic", "claude-fable-5-2"),
        ("openai", "gpt-6-astra"),
        ("google", "gemini-2.5-pro"),
        ("openai", "gpt-5.5"),
        ("opencode", "deepseek-v4-flash-free"),
    ] {
        assert!(
            found.contains(&(expected.0.to_owned(), expected.1.to_owned())),
            "fixture scan missed {expected:?}; found {found:?}"
        );
    }

    let mut missing = Vec::new();
    for (provider, model) in &found {
        let known_unpriced = KNOWN_UNPRICED.contains(&(provider.as_str(), model.as_str()));
        let priced = cat.price_for(provider, model, at).price().is_some();
        match (priced, known_unpriced) {
            (true, true) => panic!("{provider}/{model} is priced; drop it from KNOWN_UNPRICED"),
            (false, false) => missing.push(format!("{provider}/{model}")),
            _ => {}
        }
    }
    assert!(
        missing.is_empty(),
        "fixture models without a price: {missing:?}"
    );
}
