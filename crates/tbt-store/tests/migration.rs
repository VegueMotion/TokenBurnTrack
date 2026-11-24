mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use common::*;
use rusqlite::Connection;
use tbt_core::{HarnessId, Micros, TimeRange};
use tbt_store::{BucketGroup, BucketSize, QueryFilter, Store};
use tempfile::TempDir;

const TOTAL_EVENTS: u64 = 640;
const TOTAL_COST: i64 = 42_995_719;
const TOTAL_SOURCES: u64 = 26;

struct HarnessTotals {
    harness: &'static str,
    events: i64,
    input: i64,
    output: i64,
    cache_write: i64,
    cache_read: i64,
    cost: i64,
    unpriced: i64,
    sessions: i64,
}

const WAVE2_TOTALS: &[HarnessTotals] = &[
    HarnessTotals {
        harness: "claude_code",
        events: 204,
        input: 462,
        output: 124_290,
        cache_write: 595_693,
        cache_read: 18_913_979,
        cost: 10_924_072,
        unpriced: 0,
        sessions: 8,
    },
    HarnessTotals {
        harness: "codex",
        events: 381,
        input: 954_373,
        output: 153_043,
        cache_write: 0,
        cache_read: 39_721_472,
        cost: 31_682_157,
        unpriced: 0,
        sessions: 6,
    },
    HarnessTotals {
        harness: "gemini_cli",
        events: 12,
        input: 42_355,
        output: 3_573,
        cache_write: 0,
        cache_read: 75_008,
        cost: 75_486,
        unpriced: 0,
        sessions: 3,
    },
    HarnessTotals {
        harness: "in_house_agent",
        events: 4,
        input: 223,
        output: 1_675,
        cache_write: 37_000,
        cache_read: 32_900,
        cost: 296_612,
        unpriced: 1,
        sessions: 2,
    },
    HarnessTotals {
        harness: "llm_proxy",
        events: 6,
        input: 2_956,
        output: 2_070,
        cache_write: 0,
        cache_read: 12_144,
        cost: 17_392,
        unpriced: 2,
        sessions: 4,
    },
    HarnessTotals {
        harness: "opencode",
        events: 33,
        input: 39_686,
        output: 12_865,
        cache_write: 0,
        cache_read: 457_344,
        cost: 0,
        unpriced: 33,
        sessions: 4,
    },
];

fn wave2_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/db/wave2/tbt.db")
}

struct Copy {
    _dir: TempDir,
    path: PathBuf,
}

fn wave2_copy() -> Copy {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tbt.sqlite");
    std::fs::copy(wave2_fixture(), &path).unwrap();
    Copy { _dir: dir, path }
}

fn migrated() -> Copy {
    let copy = wave2_copy();
    drop(Store::open(&copy.path).unwrap());
    copy
}

fn user_version(conn: &Connection) -> i64 {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}

fn schema(conn: &Connection) -> BTreeSet<(String, String, Option<String>)> {
    let mut stmt = conn
        .prepare("SELECT type, name, sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn all_time() -> QueryFilter {
    QueryFilter::new(
        TimeRange::new(ts("2000-01-01T00:00:00Z"), ts("2100-01-01T00:00:00Z")),
        BucketSize::Hour,
    )
}

fn assert_session_totals_match_events(conn: &Connection) {
    let mismatches: i64 = conn
        .query_row(
            "SELECT count(*) FROM session s LEFT JOIN (
                SELECT session_id, count(*) n, sum(input_tokens) i, sum(output_tokens) o,
                       sum(cache_write_5m_tokens + cache_write_1h_tokens) w,
                       sum(cache_read_tokens) r, sum(cost_micros) c, sum(price_id IS NULL) u
                FROM usage_event GROUP BY session_id) e ON e.session_id = s.id
             WHERE s.events != coalesce(e.n, 0) OR s.input_tokens != coalesce(e.i, 0)
                OR s.output_tokens != coalesce(e.o, 0) OR s.cache_write_tokens != coalesce(e.w, 0)
                OR s.cache_read_tokens != coalesce(e.r, 0) OR s.cost_micros != coalesce(e.c, 0)
                OR s.unpriced_events != coalesce(e.u, 0)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(mismatches, 0, "session totals differ from their events");
}

#[test]
fn wave2_fixture_is_a_wave2_database() {
    let copy = wave2_copy();
    let conn = Connection::open(&copy.path).unwrap();
    assert_eq!(user_version(&conn), 1);
    assert_eq!(count(&conn, "usage_event"), TOTAL_EVENTS as i64);
    assert_eq!(count(&conn, "source_file"), TOTAL_SOURCES as i64);
}

#[test]
fn wave2_db_migrates_to_the_latest_schema() {
    let fresh_dir = tempfile::tempdir().unwrap();
    let fresh_path = fresh_dir.path().join("tbt.sqlite");
    drop(Store::open(&fresh_path).unwrap());
    let fresh = Connection::open(&fresh_path).unwrap();

    let copy = migrated();
    let conn = Connection::open(&copy.path).unwrap();
    assert_eq!(user_version(&conn), user_version(&fresh));
    assert_eq!(schema(&conn), schema(&fresh));

    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    let fk_violations: i64 = conn
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(fk_violations, 0);

    drop(Store::open(&copy.path).unwrap());
    assert_eq!(user_version(&conn), user_version(&fresh));
    assert_eq!(count(&conn, "usage_event"), TOTAL_EVENTS as i64);
}

#[test]
fn wave2_data_keeps_its_pinned_totals() {
    let copy = migrated();
    let conn = Connection::open(&copy.path).unwrap();
    for want in WAVE2_TOTALS {
        let got: [i64; 8] = conn
            .query_row(
                "SELECT count(*), sum(input_tokens), sum(output_tokens),
                        sum(cache_write_5m_tokens + cache_write_1h_tokens), sum(cache_read_tokens),
                        sum(cost_micros), sum(price_id IS NULL), count(DISTINCT session_id)
                 FROM usage_event WHERE harness_id = ?1",
                [want.harness],
                |r| {
                    Ok([
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ])
                },
            )
            .unwrap();
        assert_eq!(
            got,
            [
                want.events,
                want.input,
                want.output,
                want.cache_write,
                want.cache_read,
                want.cost,
                want.unpriced,
                want.sessions,
            ],
            "{}",
            want.harness
        );
    }
    assert_eq!(hourly(&conn), hourly_from_events(&conn));
    assert_session_totals_match_events(&conn);
    assert_eq!(
        count(&conn, "session"),
        31,
        "sessions incl. placeholders and subagents"
    );
    assert_eq!(count(&conn, "message"), 1455);
}

#[test]
fn all_read_queries_work_on_the_migrated_wave2_db() {
    let copy = migrated();
    let store = Store::open_read_only(&copy.path).unwrap();
    let filter = all_time();

    let kpi = store.kpi_summary(&filter).unwrap();
    assert_eq!(kpi.events, TOTAL_EVENTS);
    assert_eq!(kpi.cost, Micros(TOTAL_COST));
    assert_eq!(kpi.unpriced_events, 36);
    assert!(kpi.sessions > 0 && kpi.active_hours > 0);

    for bucket in [BucketSize::Hour, BucketSize::Day] {
        let mut f = filter.clone();
        f.bucket = bucket;
        f.time_zone = "Europe/Berlin".into();
        for group in [
            BucketGroup::Harness,
            BucketGroup::Model,
            BucketGroup::Project,
        ] {
            let buckets = store.cost_buckets(&f, group).unwrap();
            assert_eq!(
                buckets.iter().map(|b| b.events).sum::<u64>(),
                TOTAL_EVENTS,
                "{bucket:?} {group:?}"
            );
            assert_eq!(
                buckets.iter().map(|b| b.cost.0).sum::<i64>(),
                TOTAL_COST,
                "{bucket:?} {group:?}"
            );
        }
        let heat = store.heatmap(&f).unwrap();
        assert_eq!(heat.iter().map(|c| c.events).sum::<u64>(), TOTAL_EVENTS);
    }

    let rows = store.sessions(&filter).unwrap();
    assert!(!rows.is_empty());
    let mut events = 0;
    for row in &rows {
        assert_eq!(store.session_row(row.id).unwrap().as_ref(), Some(row));
        let detail = store.session_detail(row.id).unwrap().unwrap();
        assert_eq!(detail.events.len() as u64, row.events, "{}", row.ext_id);
        assert_eq!(
            detail.events.iter().map(|e| e.cost.0).sum::<i64>(),
            row.cost.0
        );
        assert!(detail.resume_chain.iter().any(|s| s.id == row.id));
        events += row.events;
    }
    assert_eq!(events, TOTAL_EVENTS);
    assert!(rows.iter().any(|r| r.resumed_from.is_some()));
    assert!(rows.iter().any(|r| r.parent.is_some()));

    let models = store.model_stats(&filter).unwrap();
    assert_eq!(models.iter().map(|m| m.events).sum::<u64>(), TOTAL_EVENTS);
    let projects = store.project_stats(&filter).unwrap();
    assert_eq!(projects.iter().map(|p| p.events).sum::<u64>(), TOTAL_EVENTS);

    let harnesses = store.harness_status().unwrap();
    let ids: BTreeSet<String> = harnesses.iter().map(|h| h.config.id.to_string()).collect();
    assert_eq!(
        ids,
        WAVE2_TOTALS.iter().map(|t| t.harness.to_owned()).collect()
    );
    assert_eq!(
        harnesses.iter().map(|h| h.files_tracked).sum::<u64>(),
        TOTAL_SOURCES
    );
    assert_eq!(
        harnesses.iter().map(|h| h.events).sum::<u64>(),
        TOTAL_EVENTS
    );

    let unpriced = store.unpriced_models().unwrap();
    assert_eq!(unpriced.iter().map(|u| u.events).sum::<u64>(), 36);

    let runs = store.ingest_runs(10).unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[1].stats.events_new, TOTAL_EVENTS);
    assert_eq!(runs[1].stats.sources_scanned, TOTAL_SOURCES);
    assert_eq!(runs[1].warnings.len(), 5);
    assert_eq!(runs[0].stats.events_new, 0);
    assert!(!store.model_prices().unwrap().is_empty());
    assert!(!store.harnesses().unwrap().is_empty());
}

#[test]
fn current_code_ingests_into_the_migrated_wave2_db() {
    let copy = migrated();
    let mut store = Store::open(&copy.path).unwrap();
    let conn = Connection::open(&copy.path).unwrap();
    let (source, dedup_key, session): (String, String, String) = conn
        .query_row(
            "SELECT f.path, e.dedup_key, s.ext_id FROM usage_event e
             JOIN source_file f ON f.id = e.source_file_id JOIN session s ON s.id = e.session_id
             WHERE e.harness_id = 'claude_code' ORDER BY e.id LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    let cursor = store
        .cursor_for(&HarnessId::CLAUDE_CODE, Path::new(&source))
        .unwrap();
    assert!(cursor.is_some());

    let dup = event(&dedup_key, &session, "2026-11-23T10:00:00Z");
    let new = event("e2e-migration-new", &session, "2026-11-23T10:00:00Z");
    let mut tx = store
        .begin_source_tx(&HarnessId::CLAUDE_CODE, Path::new(&source))
        .unwrap();
    let inserted = tx
        .insert_events(&[priced(dup, 5, None), priced(new, 7, None)])
        .unwrap();
    tx.save_cursor(&cursor.unwrap(), ts("2026-11-23T10:00:00Z"))
        .unwrap();
    tx.commit().unwrap();
    assert_eq!(inserted, 1);

    assert_eq!(count(&conn, "usage_event"), TOTAL_EVENTS as i64 + 1);
    assert_eq!(hourly(&conn), hourly_from_events(&conn));
    assert_session_totals_match_events(&conn);
    let kpi = store.kpi_summary(&all_time()).unwrap();
    assert_eq!(kpi.events, TOTAL_EVENTS + 1);
    assert_eq!(kpi.cost, Micros(TOTAL_COST + 7));
}
