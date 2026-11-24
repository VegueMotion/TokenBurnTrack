mod common;

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use tbt_adapters::ClaudeCodeAdapter;
use tbt_app::charts::format::micros_to_usd;
use tbt_app::charts::stack::totals;
use tbt_app::data::{DataSource, StoreData};
use tbt_app::pages::models::{ModelsData, rates};
use tbt_app::pages::session_detail::{
    DetailData, METADATA_ONLY_NOTE, NO_MESSAGES_NOTE, call_token_series,
};
use tbt_core::{Cursor, HarnessAdapter, Micros, PrivacyLevel, SessionId, TimeRange};
use tbt_store::{BucketSize, QueryFilter, SessionRow, Store};
use tempfile::TempDir;

const HAIKU: &str = "claude-haiku-5-5";
const TOOL_HEAVY: &str = "f001a22b-ee17-44c8-ab5f-c1ab7bd4b32b";

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

struct Fixture {
    _dir: TempDir,
    db: PathBuf,
    roots: Vec<PathBuf>,
}

fn fixture(unpriced: &[&str]) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let roots = common::copy_claude_code_fixtures(&dir.path().join("logs")).unwrap();
    let db = dir.path().join("tbt.sqlite");
    let n = common::build_db(&db, &roots, unpriced, ts("2026-11-23T00:00:00Z")).unwrap();
    assert!(n > 0);
    Fixture {
        _dir: dir,
        db,
        roots,
    }
}

fn all_time() -> QueryFilter {
    QueryFilter::new(
        TimeRange::new(ts("2026-11-01T00:00:00Z"), ts("2026-12-01T00:00:00Z")),
        BucketSize::Day,
    )
}

fn load(data: &StoreData, id: SessionId) -> DetailData {
    DetailData::load(data, id, TimeZone::UTC)
        .unwrap()
        .expect("session exists")
}

fn ids(rows: &[SessionRow]) -> Vec<SessionId> {
    rows.iter().map(|r| r.id).collect()
}

fn store_metadata_messages(db: &Path, roots: &[PathBuf], ext_id: &str) {
    let adapter = ClaudeCodeAdapter::new();
    let src = adapter
        .discover(roots)
        .unwrap()
        .into_iter()
        .find(|s| s.path.to_string_lossy().contains(ext_id))
        .unwrap();
    let mut cursor = Cursor::empty();
    let mut messages = Vec::new();
    for _ in 0..1000 {
        let batch = adapter.read(&src, &cursor).unwrap();
        let done = batch.is_empty() && batch.next_cursor == cursor;
        messages.extend(batch.messages);
        cursor = batch.next_cursor;
        if done {
            break;
        }
    }
    assert!(!messages.is_empty());
    let messages = PrivacyLevel::MetadataOnly.apply(messages);
    let mut store = Store::open(db).unwrap();
    let mut tx = store.begin_source_tx(&adapter.id(), &src.path).unwrap();
    assert!(tx.insert_messages(&messages).unwrap() > 0);
    tx.commit().unwrap();
}

fn assert_timeline_matches_session(d: &DetailData) {
    let s = &d.detail.session;
    let n = d.detail.events.len();
    assert_eq!(n as u64, s.events);
    assert_eq!(d.timeline.labels.len(), n);
    assert_eq!(d.timeline.tooltips.len(), n);
    let stacks = call_token_series(&d.detail.events);
    let stacked: f64 = totals(&stacks).iter().sum();
    assert_eq!(stacked as u64, s.tokens.total());
    assert_eq!(d.tokens.total(), s.tokens.total());
    let last = *d.timeline.cumulative.values.last().unwrap();
    assert!((last - micros_to_usd(s.cost)).abs() < 1e-9);
    assert!(
        d.timeline
            .cumulative
            .values
            .windows(2)
            .all(|w| w[0] <= w[1])
    );
}

#[test]
fn subagent_detail_links_to_parent_and_parent_lists_it() {
    let fx = fixture(&[]);
    let data = StoreData::new(fx.db.clone());
    let rows = data.sessions(&all_time()).unwrap();
    let sub = rows
        .iter()
        .find(|r| r.parent.is_some())
        .expect("subagent session");
    let d = load(&data, sub.id);
    let parent = d.parent.as_ref().expect("parent row");
    assert_eq!(Some(parent.id), sub.parent);
    assert!(!d.chain.is_chain());
    assert!(d.detail.subagents.is_empty());
    assert_eq!(d.detail.session.models, [HAIKU]);
    assert_timeline_matches_session(&d);

    let p = load(&data, parent.id);
    assert!(p.parent.is_none());
    assert_eq!(ids(&p.detail.subagents), [sub.id]);
    assert_eq!(p.subagents_total(), sub.cost);
    assert_timeline_matches_session(&p);
}

#[test]
fn resumed_pair_forms_one_chain_from_both_ends() {
    let fx = fixture(&[]);
    let data = StoreData::new(fx.db.clone());
    let rows = data.sessions(&all_time()).unwrap();
    let cont = rows
        .iter()
        .find(|r| r.resumed_from.is_some())
        .expect("resumed session");
    let original = rows
        .iter()
        .find(|r| Some(r.id) == cont.resumed_from)
        .unwrap();

    let c = load(&data, cont.id);
    assert_eq!(ids(&c.chain.links), [original.id, cont.id]);
    assert_eq!(c.chain.current, 1);
    assert_eq!(c.chain.previous().map(|r| r.id), Some(original.id));
    assert_eq!(c.chain.next(), None);
    assert_eq!(c.chain.total, original.cost + cont.cost);
    assert_eq!(c.chain.total, c.detail.chain_cost);
    assert!(c.chain.total > cont.cost);

    let o = load(&data, original.id);
    assert_eq!(ids(&o.chain.links), [original.id, cont.id]);
    assert_eq!(o.chain.current, 0);
    assert_eq!(o.chain.next().map(|r| r.id), Some(cont.id));
    assert_eq!(o.chain.total, c.chain.total);
    assert_eq!(o.detail.chain_cost, c.chain.total);
    assert_timeline_matches_session(&o);
    assert_timeline_matches_session(&c);
}

#[test]
fn claude_code_costs_are_api_equivalent() {
    let fx = fixture(&[]);
    let data = StoreData::new(fx.db.clone());
    for row in data.sessions(&all_time()).unwrap() {
        let d = load(&data, row.id);
        assert_eq!(d.detail.reported_cost, None);
        assert!(d.compare.api_equivalent);
        assert_eq!(d.compare.label(), None);
        assert_eq!(d.compare.computed, row.cost);
        assert_eq!(d.note, Some(NO_MESSAGES_NOTE));
    }
}

#[test]
fn metadata_only_messages_feed_tool_counts_without_text() {
    let fx = fixture(&[]);
    store_metadata_messages(&fx.db, &fx.roots, TOOL_HEAVY);
    let data = StoreData::new(fx.db.clone());
    let row = data
        .sessions(&all_time())
        .unwrap()
        .into_iter()
        .find(|r| r.ext_id == TOOL_HEAVY)
        .unwrap();
    let d = load(&data, row.id);
    assert!(!d.messages.is_empty());
    assert_eq!(d.messages.len(), d.detail.messages.len());
    assert!(d.messages.iter().all(|m| m.preview.is_none()));
    assert_eq!(d.note, Some(METADATA_ONLY_NOTE));
    assert!(!d.detail.tool_counts.is_empty());
    assert!(d.detail.tool_counts.windows(2).all(|w| w[0].1 >= w[1].1));
}

#[test]
fn unknown_session_is_not_found() {
    let fx = fixture(&[]);
    let data = StoreData::new(fx.db.clone());
    assert!(
        DetailData::load(&data, SessionId(999_999), TimeZone::UTC)
            .unwrap()
            .is_none()
    );
}

#[test]
fn unpriced_subagent_counts_calls_as_zero_cost() {
    let fx = fixture(&[HAIKU]);
    let data = StoreData::new(fx.db.clone());
    let sub = data
        .sessions(&all_time())
        .unwrap()
        .into_iter()
        .find(|r| r.parent.is_some())
        .unwrap();
    let d = load(&data, sub.id);
    assert_eq!(d.detail.session.unpriced_events, d.detail.session.events);
    assert!(d.timeline.tooltips.iter().all(|rows| {
        rows.iter()
            .any(|r| r.label.as_ref() == "Cost" && r.value.as_ref() == "$0.00")
    }));
    let p = load(&data, sub.parent.unwrap());
    assert_eq!(p.subagents_total(), Micros::ZERO);
    assert_eq!(p.detail.subagents[0].unpriced_events, sub.events);
}

#[test]
fn models_page_totals_match_kpi() {
    let fx = fixture(&[HAIKU]);
    let data = StoreData::new(fx.db.clone());
    let f = all_time();
    let kpi = data.kpi_summary(&f).unwrap();
    let m = ModelsData::load(&data, &f).unwrap();
    assert!(!m.no_data);
    assert_eq!(m.cost_total, kpi.cost);
    let charted: f64 = m.cost.series.iter().flat_map(|s| s.values.iter()).sum();
    assert!((charted - micros_to_usd(kpi.cost)).abs() < 1e-6);
    let tokens: f64 = m.tokens.series.iter().flat_map(|s| s.values.iter()).sum();
    assert_eq!(tokens as u64, kpi.tokens.total());

    let haiku = m.rows.iter().find(|r| r.stats.model == HAIKU).unwrap();
    assert_eq!(haiku.rates.total, None);
    assert_eq!(m.rows.last().unwrap().stats.model, HAIKU);
    for r in m.rows.iter().filter(|r| r.stats.unpriced_events == 0) {
        let rate = rates(&r.stats).total.unwrap();
        let expect = micros_to_usd(r.stats.cost) / r.stats.tokens.total() as f64 * 1e6;
        assert!((rate - expect).abs() < 1e-9);
        assert!(r.stats.cache_savings >= Micros::ZERO);
    }

    let mut f = all_time();
    f.harnesses = vec![tbt_core::HarnessId::CODEX];
    let none = ModelsData::load(&data, &f).unwrap();
    assert!(none.rows.is_empty());
    assert!(!none.no_data);
}
