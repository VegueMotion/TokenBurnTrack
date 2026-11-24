mod common;

use common::*;
use rusqlite::Connection;

fn at(key: &str, session: &str, ts: &str, project: &str) -> tbt_core::PricedEvent {
    let mut e = event(key, session, ts);
    e.project_path = Some(project.into());
    priced(e, 10, Some(1))
}

fn projects(conn: &Connection) -> Vec<String> {
    conn.prepare("SELECT path FROM project WHERE id != 0 ORDER BY path")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn session_usage(conn: &Connection) -> Vec<(String, i64, String, i64, i64)> {
    conn.prepare(
        "SELECT h.hour, h.session_id, p.path, h.events, h.cost_micros
         FROM hourly_session_usage h JOIN project p ON p.id = h.project_id
         ORDER BY 1, 2, 3",
    )
    .unwrap()
    .query_map([], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    })
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

#[test]
fn claude_worktree_counts_toward_main_project() {
    let mut db = Db::new();
    db.ingest(
        "/a.jsonl",
        &[
            at("k1", "s1", "2026-01-01T10:00:00Z", "/nonexistent/App"),
            at(
                "k2",
                "s2",
                "2026-01-01T10:10:00Z",
                "/nonexistent/App/.claude/worktrees/issue-1",
            ),
        ],
    );
    let raw = db.raw();
    assert_eq!(projects(&raw), ["/nonexistent/App"]);
    let rows = hourly(&raw);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].events, 2);
}

#[test]
fn stored_worktree_projects_are_folded_into_main_project() {
    let mut db = Db::new();
    // The worktree is seen before its main project exists, so it is stored on its own.
    db.ingest(
        "/a.jsonl",
        &[
            at(
                "k1",
                "s1",
                "2026-01-01T10:00:00Z",
                "/nonexistent/.herdr/worktrees/App/w1",
            ),
            at(
                "k2",
                "s1",
                "2026-01-01T11:00:00Z",
                "/nonexistent/.herdr/worktrees/App/w1",
            ),
        ],
    );
    db.ingest(
        "/b.jsonl",
        &[
            at("k3", "s2", "2026-01-01T10:30:00Z", "/nonexistent/p/App"),
            at("k4", "s1", "2026-01-01T10:40:00Z", "/nonexistent/p/App"),
        ],
    );
    let raw = db.raw();
    assert_eq!(projects(&raw).len(), 2);

    assert_eq!(db.store.merge_worktree_projects().unwrap(), 1);
    assert_eq!(projects(&raw), ["/nonexistent/p/App"]);
    assert_eq!(hourly(&raw), hourly_from_events(&raw));
    let rows = hourly(&raw);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].events, 3);
    assert_eq!(rows[0].cost, 30);
    assert!(rows.iter().all(|r| r.project == "/nonexistent/p/App"));
    let usage = session_usage(&raw);
    assert_eq!(usage.len(), 3);
    assert!(usage.iter().all(|u| u.2 == "/nonexistent/p/App"));
    let s1_10h = usage
        .iter()
        .find(|u| u.0.starts_with("2026-01-01T10") && u.3 == 2);
    assert!(s1_10h.is_some(), "{usage:?}");
    let session_projects: i64 = raw
        .query_row(
            "SELECT COUNT(DISTINCT project_id) FROM session WHERE project_id != 0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(session_projects, 1);

    assert_eq!(db.store.merge_worktree_projects().unwrap(), 0);
}
