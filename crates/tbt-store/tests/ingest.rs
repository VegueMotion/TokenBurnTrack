mod common;

use std::path::{Path, PathBuf};

use common::*;
use tbt_core::{
    HarnessId, MessageKind, MessageRecord, MessageRole, ProjectId, SessionId, SessionUpsert,
    TokenCounts,
};

#[test]
fn same_event_twice_is_counted_once() {
    let mut db = Db::new();
    let e = priced(event("k1", "s1", "2026-01-01T10:00:00Z"), 1234, Some(1));
    assert_eq!(db.ingest("/a.jsonl", &[e.clone(), e.clone()]), 1);
    let raw = db.raw();
    let rollup = hourly(&raw);
    let totals = sessions(&raw);

    assert_eq!(db.ingest("/a.jsonl", std::slice::from_ref(&e)), 0);
    assert_eq!(db.ingest("/copy.jsonl", &[e]), 0);
    assert_eq!(count(&raw, "usage_event"), 1);
    assert_eq!(hourly(&raw), rollup);
    assert_eq!(sessions(&raw), totals);
    assert_eq!(rollup[0].events, 1);
    assert_eq!(rollup[0].cost, 1234);
}

#[test]
fn rollup_is_sum_of_events_with_one_bucket_per_key() {
    let mut db = Db::new();
    let mut batch = Vec::new();
    for i in 0..5 {
        batch.push(priced(
            event(
                &format!("np{i}"),
                "s1",
                &format!("2026-01-01T10:{:02}:00Z", i * 10),
            ),
            100,
            if i == 0 { None } else { Some(1) },
        ));
    }
    let mut with_project = event("p1", "s2", "2026-01-01T10:30:00Z");
    with_project.project_path = Some("/home/u/repo".into());
    batch.push(priced(with_project, 7, Some(1)));
    let mut other_hour = event("h2", "s1", "2026-01-01T11:05:00Z");
    other_hour.model = "claude-opus-4-5".into();
    batch.push(priced(other_hour, 9, Some(1)));
    db.ingest("/a.jsonl", &batch[..3]);
    db.ingest("/a.jsonl", &batch[3..]);
    db.ingest(
        "/b.jsonl",
        &[priced(
            event("np5", "s1", "2026-01-01T10:59:59.999Z"),
            1,
            Some(1),
        )],
    );

    let raw = db.raw();
    let rows = hourly(&raw);
    assert_eq!(rows, hourly_from_events(&raw));
    let no_project: Vec<_> = rows
        .iter()
        .filter(|r| r.project.is_empty() && r.hour.starts_with("2026-01-01T10"))
        .collect();
    assert_eq!(no_project.len(), 1);
    assert_eq!(no_project[0].events, 6);
    assert_eq!(no_project[0].cost, 501);
    assert_eq!(no_project[0].unpriced, 1);
    assert_eq!(no_project[0].cache_write, 6 * 15);
    assert_eq!(rows.len(), 3);
    let null_projects: i64 = raw
        .query_row(
            "SELECT COUNT(*) FROM hourly_usage WHERE project_id IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(null_projects, 0);
}

#[test]
fn event_project_wins_over_session_project() {
    let mut db = Db::new();
    let mut tx = db.store.begin_source_tx(&CC, Path::new("/a")).unwrap();
    let p = tx.upsert_project(Some("/home/u/a")).unwrap();
    assert_eq!(tx.upsert_project(None).unwrap(), ProjectId::NONE);
    assert_eq!(tx.upsert_project(Some("/home/u/a")).unwrap(), p);
    tx.upsert_session(&SessionUpsert::new(CC, "s1"), p).unwrap();
    let mut other = event("k2", "s1", "2026-01-01T10:00:00Z");
    other.project_path = Some("/home/u/b".into());
    tx.insert_events(&[
        priced(event("k1", "s1", "2026-01-01T10:00:00Z"), 1, None),
        priced(other, 1, None),
    ])
    .unwrap();
    tx.commit().unwrap();
    let rows = hourly(&db.raw());
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].project, "/home/u/a");
    assert_eq!(rows[1].project, "/home/u/b");
}

#[test]
fn dropped_tx_rolls_back_data_and_cursor() {
    let mut db = Db::new();
    let src = Path::new("/a.jsonl");
    {
        let mut tx = db.store.begin_source_tx(&CC, src).unwrap();
        tx.insert_events(&[priced(event("k1", "s1", "2026-01-01T10:00:00Z"), 5, None)])
            .unwrap();
        tx.save_cursor(&cursor(10), ts("2026-01-01T12:00:00Z"))
            .unwrap();
        tx.commit().unwrap();
    }
    let raw = db.raw();
    let before = (hourly(&raw), sessions(&raw), count(&raw, "usage_event"));
    {
        let mut tx = db.store.begin_source_tx(&CC, src).unwrap();
        tx.insert_events(&[priced(event("k2", "s1", "2026-01-01T10:10:00Z"), 5, None)])
            .unwrap();
        tx.insert_messages(&[message("m1", "s1", None)]).unwrap();
        tx.save_cursor(&cursor(99), ts("2026-01-01T13:00:00Z"))
            .unwrap();
    }
    {
        let _tx = db
            .store
            .begin_source_tx(&CC, Path::new("/never.jsonl"))
            .unwrap();
    }
    assert_eq!(db.store.cursor_for(&CC, src).unwrap(), Some(cursor(10)));
    assert_eq!(
        (hourly(&raw), sessions(&raw), count(&raw, "usage_event")),
        before
    );
    assert_eq!(count(&raw, "message"), 0);
    assert_eq!(
        db.store.cursor_for(&CC, Path::new("/never.jsonl")).unwrap(),
        None
    );
}

#[test]
fn cursors_and_sources() {
    let mut db = Db::new();
    let a = Path::new("/a.jsonl");
    let b = Path::new("/b.jsonl");
    assert_eq!(db.store.cursor_for(&CC, a).unwrap(), None);
    for (p, off) in [(a, 1), (b, 2)] {
        let mut tx = db.store.begin_source_tx(&CC, p).unwrap();
        tx.save_cursor(&cursor(off), ts("2026-01-01T12:00:00Z"))
            .unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(db.store.cursor_for(&CC, b).unwrap(), Some(cursor(2)));
    assert_eq!(db.store.cursor_for(&HarnessId::CODEX, b).unwrap(), None);
    db.store.mark_source_missing(&CC, a).unwrap();
    assert_eq!(db.store.known_sources(&CC).unwrap(), [PathBuf::from(b)]);
    let st = &db.store.harness_status().unwrap()[0];
    assert_eq!((st.files_tracked, st.files_missing), (1, 1));
    assert_eq!(st.last_read, Some(ts("2026-01-01T12:00:00Z")));

    let mut tx = db.store.begin_source_tx(&CC, a).unwrap();
    tx.save_cursor(&cursor(3), ts("2026-01-01T13:00:00Z"))
        .unwrap();
    tx.commit().unwrap();
    assert_eq!(db.store.known_sources(&CC).unwrap().len(), 2);
}

#[test]
fn session_totals_and_active_seconds_out_of_order() {
    let mut db = Db::new();
    let at = |k: &str, t: &str| priced(event(k, "s1", t), 10, Some(1));
    db.ingest(
        "/a",
        &[
            at("c", "2026-01-01T10:20:00Z"),
            at("e", "2026-01-01T12:00:00Z"),
        ],
    );
    db.ingest(
        "/a",
        &[
            at("a", "2026-01-01T10:00:00Z"),
            at("f", "2026-01-01T12:30:00Z"),
        ],
    );
    db.ingest(
        "/a",
        &[
            at("d", "2026-01-01T10:50:00Z"),
            at("b", "2026-01-01T10:05:00Z"),
            at("b", "2026-01-01T10:05:00Z"),
        ],
    );
    let s = &sessions(&db.raw())[0];
    assert_eq!(s.events, 6);
    assert_eq!(s.cost, 60);
    assert_eq!(s.input, 600);
    assert_eq!(s.cache_write, 90);
    assert_eq!(s.started_at, "2026-01-01T10:00:00.000000000Z");
    assert_eq!(
        s.ended_at.as_deref(),
        Some("2026-01-01T12:30:00.000000000Z")
    );
    // 10:00→10:05→10:20→10:50 = 50 min; 10:50→12:00 gap excluded; 12:00→12:30 = exactly 30 min.
    assert_eq!(s.active_seconds, 80 * 60);
}

#[test]
fn upsert_session_merges_without_clobbering() {
    let mut db = Db::new();
    let mut tx = db.store.begin_source_tx(&CC, Path::new("/a")).unwrap();
    let p = tx.upsert_project(Some("/repo")).unwrap();
    let mut s = SessionUpsert::new(CC, "s1");
    s.title = Some("first".into());
    s.git_branch = Some("main".into());
    s.started_at = Some(ts("2026-01-01T10:00:00Z"));
    s.ended_at = Some(ts("2026-01-01T11:00:00Z"));
    let id = tx.upsert_session(&s, p).unwrap();

    let mut later = SessionUpsert::new(CC, "s1");
    later.harness_version = Some("2.0".into());
    later.started_at = Some(ts("2026-01-01T10:30:00Z"));
    later.ended_at = Some(ts("2026-01-01T12:00:00Z"));
    assert_eq!(tx.upsert_session(&later, ProjectId::NONE).unwrap(), id);
    let mut earlier = SessionUpsert::new(CC, "s1");
    earlier.started_at = Some(ts("2026-01-01T09:00:00Z"));
    earlier.ended_at = Some(ts("2026-01-01T09:30:00Z"));
    tx.upsert_session(&earlier, ProjectId::NONE).unwrap();
    assert_eq!(tx.session_id(&CC, "s1").unwrap(), Some(id));
    assert_eq!(tx.session_id(&CC, "nope").unwrap(), None);
    tx.commit().unwrap();

    let d = db.store.session_detail(id).unwrap().unwrap();
    assert_eq!(d.session.title.as_deref(), Some("first"));
    assert_eq!(d.session.git_branch.as_deref(), Some("main"));
    assert_eq!(d.session.project_path.as_deref(), Some("/repo"));
    assert_eq!(d.session.project_name.as_deref(), Some("repo"));
    assert_eq!(d.harness_version.as_deref(), Some("2.0"));
    assert_eq!(d.session.started_at, ts("2026-01-01T09:00:00Z"));
    assert_eq!(d.session.ended_at, Some(ts("2026-01-01T12:00:00Z")));
}

#[test]
fn session_without_start_hint_moves_to_first_event() {
    let mut db = Db::new();
    let mut tx = db.store.begin_source_tx(&CC, Path::new("/a")).unwrap();
    let id = tx
        .upsert_session(&SessionUpsert::new(CC, "s1"), ProjectId::NONE)
        .unwrap();
    tx.insert_events(&[priced(event("k", "s1", "2025-05-05T05:05:05Z"), 1, None)])
        .unwrap();
    tx.commit().unwrap();
    let d = db.store.session_detail(id).unwrap().unwrap();
    assert_eq!(d.session.started_at, ts("2025-05-05T05:05:05Z"));
    assert_eq!(d.session.ended_at, Some(ts("2025-05-05T05:05:05Z")));
}

#[test]
fn resumed_session_links_via_dedup_key() {
    let mut db = Db::new();
    db.ingest(
        "/orig.jsonl",
        &[
            priced(event("m1", "orig", "2026-03-01T10:00:00Z"), 100, Some(1)),
            priced(event("m2", "orig", "2026-03-01T10:05:00Z"), 100, Some(1)),
        ],
    );

    let mut tx = db
        .store
        .begin_source_tx(&CC, Path::new("/resumed.jsonl"))
        .unwrap();
    let orig = tx.session_holding_dedup_key(&CC, "m1").unwrap().unwrap();
    assert_eq!(tx.session_holding_dedup_key(&CC, "zz").unwrap(), None);
    assert_eq!(
        tx.session_holding_dedup_key(&HarnessId::CODEX, "m1")
            .unwrap(),
        None
    );
    let new = tx
        .upsert_session(&SessionUpsert::new(CC, "resumed"), ProjectId::NONE)
        .unwrap();
    tx.link_resumed_session(new, orig).unwrap();
    let n = tx
        .insert_events(&[
            priced(event("m1", "resumed", "2026-03-01T10:00:00Z"), 100, Some(1)),
            priced(event("m2", "resumed", "2026-03-01T10:05:00Z"), 100, Some(1)),
            priced(event("m3", "resumed", "2026-11-01T09:00:00Z"), 40, Some(1)),
        ])
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(tx.session_holding_dedup_key(&CC, "m3").unwrap(), Some(new));
    tx.commit().unwrap();

    let d = db.store.session_detail(new).unwrap().unwrap();
    assert_eq!(d.session.events, 1);
    assert_eq!(d.session.resumed_from, Some(orig));
    assert_eq!(
        d.resume_chain.iter().map(|s| s.id).collect::<Vec<_>>(),
        [orig, new]
    );
    assert_eq!(d.chain_cost.0, 240);
    let from_original = db.store.session_detail(orig).unwrap().unwrap();
    assert_eq!(
        from_original
            .resume_chain
            .iter()
            .map(|s| s.id)
            .collect::<Vec<_>>(),
        [orig, new]
    );
    assert_eq!(from_original.chain_cost.0, 240);
    assert_eq!(db.store.session_row(new).unwrap().unwrap().id, new);
    assert_eq!(db.store.session_row(SessionId(9999)).unwrap(), None);
    let rows = hourly(&db.raw());
    assert_eq!(rows.last().unwrap().hour, "2026-11-01T09:00:00.000000000Z");
    assert_eq!(rows.last().unwrap().cost, 40);
}

#[test]
fn subagent_parent_link() {
    let mut db = Db::new();
    let mut tx = db.store.begin_source_tx(&CC, Path::new("/a")).unwrap();
    let parent = tx
        .upsert_session(&SessionUpsert::new(CC, "p"), ProjectId::NONE)
        .unwrap();
    let child = tx
        .upsert_session(&SessionUpsert::new(CC, "agent-1"), ProjectId::NONE)
        .unwrap();
    tx.set_parent_session(child, parent).unwrap();
    tx.commit().unwrap();
    let d = db.store.session_detail(parent).unwrap().unwrap();
    assert_eq!(d.subagents.len(), 1);
    assert_eq!(d.subagents[0].id, child);
    assert_eq!(d.subagents[0].parent, Some(parent));
}

#[test]
fn cumulative_totals_roundtrip() {
    let mut db = Db::new();
    let mut tx = db
        .store
        .begin_source_tx(&HarnessId::CODEX, Path::new("/r.jsonl"))
        .unwrap();
    let s = tx
        .upsert_session(&SessionUpsert::new(HarnessId::CODEX, "x"), ProjectId::NONE)
        .unwrap();
    assert_eq!(tx.last_cumulative_total(s).unwrap(), None);
    let total = TokenCounts {
        input: 5,
        output: 6,
        cache_read: 7,
        reasoning: 1,
        ..Default::default()
    };
    tx.save_cumulative_total(s, &total).unwrap();
    tx.commit().unwrap();
    let tx = db
        .store
        .begin_source_tx(&HarnessId::CODEX, Path::new("/r.jsonl"))
        .unwrap();
    assert_eq!(tx.last_cumulative_total(s).unwrap(), Some(total));
}

fn message(key: &str, session: &str, event: Option<&str>) -> MessageRecord {
    MessageRecord {
        session_ext_id: session.into(),
        key: key.into(),
        ts: ts("2026-01-01T10:00:00Z"),
        role: MessageRole::Assistant,
        kind: Some(MessageKind::ToolUse),
        tool_name: Some("Bash".into()),
        preview: None,
        event_dedup_key: event.map(Into::into),
    }
}

#[test]
fn messages_are_idempotent_and_linked() {
    let mut db = Db::new();
    let mut tx = db.store.begin_source_tx(&CC, Path::new("/a")).unwrap();
    let msgs = [
        message("u1", "s1", None),
        message("a1", "s1", Some("k1")),
        message("a2", "s1", Some("k2")),
    ];
    assert_eq!(tx.insert_messages(&msgs).unwrap(), 3);
    tx.insert_events(&[
        priced(event("k1", "s1", "2026-01-01T10:00:00Z"), 1, None),
        priced(event("k2", "s1", "2026-01-01T10:01:00Z"), 1, None),
    ])
    .unwrap();
    assert_eq!(tx.insert_messages(&msgs).unwrap(), 0);
    tx.commit().unwrap();

    let mut tx = db.store.begin_source_tx(&CC, Path::new("/a")).unwrap();
    assert_eq!(tx.insert_messages(&msgs[..2]).unwrap(), 0);
    assert_eq!(tx.insert_messages(&[message("a3", "s1", None)]).unwrap(), 1);
    tx.commit().unwrap();

    let raw = db.raw();
    assert_eq!(count(&raw, "message"), 4);
    let linked: i64 = raw
        .query_row(
            "SELECT COUNT(*) FROM message m JOIN usage_event e ON e.id = m.usage_event_id",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(linked, 2);
}
