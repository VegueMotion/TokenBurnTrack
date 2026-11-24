mod common;

use common::*;
use std::fs;
use std::path::Path;

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

/// A main checkout at `<tmp>/repo` with git metadata for a linked worktree at
/// `<tmp>/wt/w1`; the worktree directory itself is created only when `live`.
fn repo_with_worktree(live: bool) -> (tempfile::TempDir, String, String) {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("repo");
    let admin = main.join(".git/worktrees/w1");
    fs::create_dir_all(&admin).unwrap();
    fs::write(admin.join("commondir"), "../..\n").unwrap();
    let wt = tmp.path().join("wt/w1");
    fs::write(
        admin.join("gitdir"),
        format!("{}\n", wt.join(".git").display()),
    )
    .unwrap();
    if live {
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
    }
    let s = |p: &Path| p.to_str().unwrap().to_owned();
    (tmp, s(&main), s(&wt))
}

#[test]
fn stored_worktree_projects_are_folded_into_main_project() {
    let (_tmp, main, wt) = repo_with_worktree(false);
    let mut db = Db::new();
    // The worktree is seen before its main project exists, so it is stored on its own.
    db.ingest(
        "/a.jsonl",
        &[
            at("k1", "s1", "2026-01-01T10:00:00Z", &wt),
            at("k2", "s1", "2026-01-01T11:00:00Z", &wt),
        ],
    );
    db.ingest(
        "/b.jsonl",
        &[
            at("k3", "s2", "2026-01-01T10:30:00Z", &main),
            at("k4", "s1", "2026-01-01T10:40:00Z", &main),
        ],
    );
    let raw = db.raw();
    assert_eq!(projects(&raw).len(), 2);

    // Git still lists the deleted worktree under the main checkout.
    assert_eq!(db.store.merge_worktree_projects().unwrap(), 1);
    assert_eq!(projects(&raw), std::slice::from_ref(&main));
    assert_eq!(hourly(&raw), hourly_from_events(&raw));
    let rows = hourly(&raw);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].events, 3);
    assert_eq!(rows[0].cost, 30);
    assert!(rows.iter().all(|r| r.project == main));
    let usage = session_usage(&raw);
    assert_eq!(usage.len(), 3);
    assert!(usage.iter().all(|u| u.2 == main));
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

#[test]
fn same_named_project_without_evidence_is_not_merged() {
    let mut db = Db::new();
    db.ingest(
        "/a.jsonl",
        &[
            at("k1", "s1", "2026-01-01T10:00:00Z", "/nonexistent/p/App"),
            at(
                "k2",
                "s2",
                "2026-01-01T10:10:00Z",
                "/nonexistent/.herdr/worktrees/App/w1",
            ),
            at(
                "k3",
                "s3",
                "2026-01-01T10:20:00Z",
                "/nonexistent/worktrees/App/tmp",
            ),
        ],
    );
    assert_eq!(db.store.merge_worktree_projects().unwrap(), 0);
    assert_eq!(projects(&db.raw()).len(), 3);
}

#[test]
fn worktree_seen_live_still_resolves_after_deletion() {
    let (_tmp, main, wt) = repo_with_worktree(true);
    let mut db = Db::new();
    db.ingest("/a.jsonl", &[at("k1", "s1", "2026-01-01T10:00:00Z", &wt)]);
    let raw = db.raw();
    assert_eq!(projects(&raw), std::slice::from_ref(&main));

    // `git worktree remove` deletes both the worktree and git's record of it.
    fs::remove_dir_all(&wt).unwrap();
    fs::remove_dir_all(Path::new(&main).join(".git/worktrees")).unwrap();
    db.ingest("/b.jsonl", &[at("k2", "s2", "2026-01-01T11:00:00Z", &wt)]);
    assert_eq!(projects(&raw), [main]);
}
