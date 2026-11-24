mod common;

use std::path::Path;

use common::*;
use tbt_core::{
    HarnessId, MessageKind, MessageRecord, MessageRole, Micros, PricedEvent, ProjectId, SessionId,
    SessionUpsert, TimeRange,
};
use tbt_store::{BucketGroup, BucketSize, QueryFilter, Store};

struct Seed {
    db: Db,
    s1: SessionId,
    s2: SessionId,
}

fn seed() -> Seed {
    let mut db = Db::new();
    let price_id = db
        .store
        .insert_price(&price("claude-sonnet-4-5", "3", Some("0.30")))
        .unwrap();

    let mut tx = db
        .store
        .begin_source_tx(&CC, Path::new("/s1.jsonl"))
        .unwrap();
    let p = tx.upsert_project(Some("/home/u/repo")).unwrap();
    let mut up = SessionUpsert::new(CC, "s1");
    up.title = Some("Fix bug".into());
    up.git_branch = Some("main".into());
    let s1 = tx.upsert_session(&up, p).unwrap();
    let mut e1 = event("k1", "s1", "2026-01-01T22:30:00Z");
    e1.reported_cost = Some("0.000150".parse().unwrap());
    let e2 = event("k2", "s1", "2026-01-01T23:10:00Z");
    tx.insert_events(&[
        priced(e1, 100, Some(price_id.0)),
        priced(e2, 200, Some(price_id.0)),
    ])
    .unwrap();
    let msg = |key: &str, kind: MessageKind, tool: Option<&str>, ev: Option<&str>| MessageRecord {
        session_ext_id: "s1".into(),
        key: key.into(),
        ts: ts("2026-01-01T22:30:00Z"),
        role: MessageRole::Assistant,
        kind: Some(kind),
        tool_name: tool.map(Into::into),
        preview: Some("hi".into()),
        event_dedup_key: ev.map(Into::into),
    };
    tx.insert_messages(&[
        msg("m1", MessageKind::Text, None, Some("k1")),
        msg("m2", MessageKind::ToolUse, Some("Bash"), Some("k1")),
        msg("m3", MessageKind::ToolResult, Some("Bash"), None),
        msg("m4", MessageKind::ToolUse, Some("Bash"), Some("k2")),
        msg("m5", MessageKind::ToolUse, Some("Read"), Some("k2")),
    ])
    .unwrap();
    tx.commit().unwrap();

    let codex = HarnessId::CODEX;
    let mut tx = db
        .store
        .begin_source_tx(&codex, Path::new("/s2.jsonl"))
        .unwrap();
    let mut e3 = event("c1", "s2", "2026-01-02T08:00:00Z");
    e3.harness = codex.clone();
    e3.provider = "openai".into();
    e3.model = "gpt-5".into();
    tx.insert_events(&[PricedEvent::unpriced(e3)]).unwrap();
    tx.commit().unwrap();
    let s2 = {
        let tx = db
            .store
            .begin_source_tx(&codex, Path::new("/s2.jsonl"))
            .unwrap();
        tx.session_id(&codex, "s2").unwrap().unwrap()
    };
    Seed { db, s1, s2 }
}

fn filter(start: &str, end: &str, bucket: BucketSize, tz: &str) -> QueryFilter {
    let mut f = QueryFilter::new(TimeRange::new(ts(start), ts(end)), bucket);
    f.time_zone = tz.into();
    f
}

fn two_days() -> QueryFilter {
    filter(
        "2026-01-01T00:00:00Z",
        "2026-01-03T00:00:00Z",
        BucketSize::Day,
        "Europe/Berlin",
    )
}

#[test]
fn kpi_summary() {
    let s = seed();
    let k = s.db.store.kpi_summary(&two_days()).unwrap();
    assert_eq!(k.cost, Micros(300));
    assert_eq!(k.events, 3);
    assert_eq!(k.sessions, 2);
    assert_eq!(k.unpriced_events, 1);
    assert_eq!(k.active_hours, 3);
    assert_eq!(k.tokens.input, 300);
    assert_eq!(k.tokens.cache_read, 3000);

    let mut f = two_days();
    f.harnesses = vec![HarnessId::CODEX];
    let k = s.db.store.kpi_summary(&f).unwrap();
    assert_eq!((k.events, k.sessions, k.unpriced_events), (1, 1, 1));

    let f = filter(
        "2026-01-02T00:00:00Z",
        "2026-01-03T00:00:00Z",
        BucketSize::Day,
        "UTC",
    );
    let k = s.db.store.kpi_summary(&f).unwrap();
    assert_eq!((k.events, k.sessions), (1, 1));
}

#[test]
fn day_buckets_follow_time_zone_across_midnight() {
    let s = seed();
    let b =
        s.db.store
            .cost_buckets(&two_days(), BucketGroup::Harness)
            .unwrap();
    let got: Vec<_> = b
        .iter()
        .map(|b| (b.start, b.key.as_str(), b.cost.0, b.events))
        .collect();
    assert_eq!(
        got,
        [
            (ts("2025-12-31T23:00:00Z"), "claude_code", 100, 1),
            (ts("2026-01-01T23:00:00Z"), "claude_code", 200, 1),
            (ts("2026-01-01T23:00:00Z"), "codex", 0, 1),
        ]
    );

    let mut utc = two_days();
    utc.time_zone = "UTC".into();
    let b = s.db.store.cost_buckets(&utc, BucketGroup::Harness).unwrap();
    assert_eq!(b[0].start, ts("2026-01-01T00:00:00Z"));
    assert_eq!((b[0].cost.0, b[0].events), (300, 2));

    let mut bad = two_days();
    bad.time_zone = "Mars/Olympus".into();
    assert!(s.db.store.cost_buckets(&bad, BucketGroup::Model).is_err());
}

#[test]
fn hour_buckets_by_project_and_model() {
    let s = seed();
    let mut f = two_days();
    f.bucket = BucketSize::Hour;
    let b = s.db.store.cost_buckets(&f, BucketGroup::Project).unwrap();
    let got: Vec<_> = b
        .iter()
        .map(|b| (b.start, b.key.as_str(), b.cost.0))
        .collect();
    assert_eq!(
        got,
        [
            (ts("2026-01-01T22:00:00Z"), "/home/u/repo", 100),
            (ts("2026-01-01T23:00:00Z"), "/home/u/repo", 200),
            (ts("2026-01-02T08:00:00Z"), "", 0),
        ]
    );
    let b = s.db.store.cost_buckets(&f, BucketGroup::Model).unwrap();
    assert_eq!(b[2].key, "gpt-5");
}

#[test]
fn heatmap_in_local_time() {
    let s = seed();
    let cells = s.db.store.heatmap(&two_days()).unwrap();
    let got: Vec<_> = cells
        .iter()
        .map(|c| (c.weekday, c.hour, c.cost.0, c.events))
        .collect();
    assert_eq!(got, [(3, 23, 100, 1), (4, 0, 200, 1), (4, 9, 0, 1)]);
}

#[test]
fn sessions_list() {
    let s = seed();
    let rows = s.db.store.sessions(&two_days()).unwrap();
    assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), [s.s2, s.s1]);
    let r = &rows[1];
    assert_eq!(r.title.as_deref(), Some("Fix bug"));
    assert_eq!(r.project_path.as_deref(), Some("/home/u/repo"));
    assert_eq!(r.project_name.as_deref(), Some("repo"));
    assert_eq!(r.models, ["claude-sonnet-4-5"]);
    assert_eq!(r.started_at, ts("2026-01-01T22:30:00Z"));
    assert_eq!(r.ended_at, Some(ts("2026-01-01T23:10:00Z")));
    assert_eq!(r.active_seconds, 0);
    assert_eq!(r.events, 2);
    assert_eq!(r.cost, Micros(300));
    assert_eq!(r.unpriced_events, 0);
    assert_eq!(rows[0].unpriced_events, 1);
    assert_eq!(rows[0].project_path, None);

    let later = filter(
        "2026-01-02T00:00:00Z",
        "2026-01-03T00:00:00Z",
        BucketSize::Day,
        "UTC",
    );
    let rows = s.db.store.sessions(&later).unwrap();
    assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), [s.s2]);

    let mut cc = two_days();
    cc.harnesses = vec![CC];
    assert_eq!(s.db.store.sessions(&cc).unwrap().len(), 1);
}

#[test]
fn session_detail() {
    let s = seed();
    let d = s.db.store.session_detail(s.s1).unwrap().unwrap();
    assert_eq!(d.events.len(), 2);
    assert!(d.events[0].ts < d.events[1].ts);
    assert_eq!(d.events[0].reported_cost, Some(Micros(150)));
    assert_eq!(d.events[0].tokens.cache_write_1h, 5);
    assert_eq!(d.reported_cost, Some(Micros(150)));
    assert_eq!(d.messages.len(), 5);
    assert_eq!(d.messages[0].event, Some(d.events[0].id));
    assert_eq!(d.messages[0].preview.as_deref(), Some("hi"));
    assert_eq!(
        d.tool_counts,
        [("Bash".to_owned(), 2), ("Read".to_owned(), 1)]
    );
    assert_eq!(d.resume_chain.len(), 1);
    assert_eq!(d.chain_cost, Micros(300));
    assert!(d.subagents.is_empty());

    let d2 = s.db.store.session_detail(s.s2).unwrap().unwrap();
    assert_eq!(d2.reported_cost, None);
    assert!(d2.messages.is_empty());
    assert!(s.db.store.session_detail(SessionId(999)).unwrap().is_none());
}

#[test]
fn model_stats_with_cache_savings() {
    let s = seed();
    let m = s.db.store.model_stats(&two_days()).unwrap();
    assert_eq!(m.len(), 2);
    assert_eq!(m[0].model, "claude-sonnet-4-5");
    assert_eq!(m[0].events, 2);
    assert_eq!(m[0].cost, Micros(300));
    assert_eq!(m[0].cache_savings, Micros(5400));
    assert_eq!(m[0].unpriced_events, 0);
    assert_eq!(m[1].provider, "openai");
    assert_eq!(m[1].unpriced_events, 1);
    assert_eq!(m[1].cache_savings, Micros::ZERO);
}

#[test]
fn project_stats_with_branches() {
    let s = seed();
    let p = s.db.store.project_stats(&two_days()).unwrap();
    assert_eq!(p.len(), 2);
    assert_eq!(p[0].path.as_deref(), Some("/home/u/repo"));
    assert_eq!(p[0].name.as_deref(), Some("repo"));
    assert_eq!((p[0].sessions, p[0].events, p[0].cost.0), (1, 2, 300));
    assert_eq!(p[0].last_active, Some(ts("2026-01-01T23:10:00Z")));
    assert_eq!(p[0].branches.len(), 1);
    assert_eq!(p[0].branches[0].branch.as_deref(), Some("main"));
    assert_eq!(p[0].branches[0].cost, Micros(300));
    assert_eq!(p[1].id, ProjectId::NONE);
    assert_eq!(p[1].path, None);
    assert_eq!((p[1].sessions, p[1].events), (1, 1));
}

#[test]
fn harness_status_and_unpriced_models() {
    let s = seed();
    let st = s.db.store.harness_status().unwrap();
    assert_eq!(st.len(), 2);
    assert_eq!(st[0].config.id, CC);
    assert_eq!(
        (st[0].events, st[0].sessions, st[0].files_tracked),
        (2, 1, 1)
    );
    assert_eq!((st[1].events, st[1].sessions), (1, 1));

    let u = s.db.store.unpriced_models().unwrap();
    assert_eq!(u.len(), 1);
    assert_eq!(
        (u[0].provider.as_str(), u[0].model.as_str()),
        ("openai", "gpt-5")
    );
    assert_eq!(u[0].events, 1);
    assert_eq!(u[0].first_seen, ts("2026-01-02T08:00:00Z"));
}

#[test]
fn queries_work_on_read_only_connection() {
    let s = seed();
    let ro = Store::open_read_only(&s.db.path).unwrap();
    assert_eq!(ro.kpi_summary(&two_days()).unwrap().events, 3);
    assert_eq!(ro.sessions(&two_days()).unwrap().len(), 2);
    assert!(ro.session_detail(s.s1).unwrap().is_some());
}

#[test]
fn project_branches_follow_event_projects() {
    let mut db = Db::new();
    let mut tx = db
        .store
        .begin_source_tx(&CC, Path::new("/x.jsonl"))
        .unwrap();
    let repo = tx.upsert_project(Some("/home/u/repo")).unwrap();
    let mut up = SessionUpsert::new(CC, "sx");
    up.git_branch = Some("feat".into());
    tx.upsert_session(&up, repo).unwrap();
    let in_repo = event("a", "sx", "2026-01-01T10:05:00Z");
    let mut in_sub = event("b", "sx", "2026-01-01T10:20:00Z");
    in_sub.project_path = Some("/home/u/repo/sub".into());
    let edge = event("c", "sx", "2026-01-01T23:59:00Z");
    tx.insert_events(&[
        priced(in_repo, 100, None),
        priced(in_sub, 40, None),
        priced(edge, 7, None),
    ])
    .unwrap();
    tx.commit().unwrap();

    let f = filter(
        "2026-01-01T10:15:00Z",
        "2026-01-01T23:30:00Z",
        BucketSize::Hour,
        "UTC",
    );
    let projects = db.store.project_stats(&f).unwrap();
    assert_eq!(projects.len(), 2);
    for p in &projects {
        let branch_cost: Micros = p.branches.iter().map(|b| b.cost).sum();
        assert_eq!(branch_cost, p.cost, "{:?}", p.path);
        assert_eq!(p.sessions, 1, "{:?}", p.path);
        assert!(p.last_active.is_some());
    }
    let cost = |path: &str| {
        projects
            .iter()
            .find(|p| p.path.as_deref() == Some(path))
            .unwrap()
            .cost
    };
    assert_eq!(cost("/home/u/repo"), Micros(107));
    assert_eq!(cost("/home/u/repo/sub"), Micros(40));
}
