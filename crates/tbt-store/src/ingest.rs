use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use tbt_core::{
    Cursor, Hour, MessageRecord, Micros, PricedEvent, ProjectId, SessionId, SessionUpsert,
    SourceFileId, Timestamp, TokenCounts,
};

use crate::api::{Result, StoreError};
use crate::db::{DbTs, MAX_ACTIVE_GAP_SECS, fmt_ts, path_text, to_i64};

pub(crate) fn ensure_harness(conn: &Connection, id: &str) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO harness(id, display_name) VALUES (?1, ?1) ON CONFLICT(id) DO NOTHING",
    )?
    .execute([id])?;
    Ok(())
}

pub(crate) fn source_file(conn: &Connection, harness: &str, path: &Path) -> Result<SourceFileId> {
    ensure_harness(conn, harness)?;
    let path = path_text(path);
    conn.execute(
        "INSERT INTO source_file(harness_id, path, cursor_json, first_seen)
         VALUES (?1, ?2, 'null', ?3) ON CONFLICT(harness_id, path) DO NOTHING",
        params![harness, path, DbTs(Timestamp::now())],
    )?;
    let id = conn.query_row(
        "SELECT id FROM source_file WHERE harness_id = ?1 AND path = ?2",
        params![harness, path],
        |r| r.get(0),
    )?;
    Ok(SourceFileId(id))
}

fn project_name(path: &str) -> Option<String> {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

pub(crate) fn upsert_project(conn: &Connection, path: Option<&str>) -> Result<ProjectId> {
    let Some(path) = path.filter(|p| !p.is_empty()) else {
        return Ok(ProjectId::NONE);
    };
    conn.prepare_cached(
        "INSERT INTO project(path, name) VALUES (?1, ?2) ON CONFLICT(path) DO NOTHING",
    )?
    .execute(params![path, project_name(path)])?;
    let id = conn
        .prepare_cached("SELECT id FROM project WHERE path = ?1")?
        .query_row([path], |r| r.get(0))?;
    Ok(ProjectId(id))
}

pub(crate) fn upsert_session(
    conn: &Connection,
    s: &SessionUpsert,
    project: ProjectId,
) -> Result<SessionId> {
    ensure_harness(conn, s.harness.as_str())?;
    let id = conn
        .prepare_cached(
            "INSERT INTO session(harness_id, ext_id, project_id, title, git_branch,
                                 harness_version, started_at, ended_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, COALESCE(?7, ?9), ?8)
             ON CONFLICT(harness_id, ext_id) DO UPDATE SET
               project_id = CASE WHEN excluded.project_id = 0 THEN session.project_id
                                 ELSE excluded.project_id END,
               title = COALESCE(excluded.title, session.title),
               git_branch = COALESCE(excluded.git_branch, session.git_branch),
               harness_version = COALESCE(excluded.harness_version, session.harness_version),
               started_at = CASE WHEN ?7 IS NULL THEN session.started_at
                                 ELSE min(session.started_at, ?7) END,
               ended_at = CASE WHEN ?8 IS NULL THEN session.ended_at
                               WHEN session.ended_at IS NULL THEN ?8
                               ELSE max(session.ended_at, ?8) END
             RETURNING id",
        )?
        .query_row(
            params![
                s.harness.as_str(),
                s.ext_id,
                project.0,
                s.title,
                s.git_branch,
                s.harness_version,
                s.started_at.map(DbTs),
                s.ended_at.map(DbTs),
                DbTs(Timestamp::now()),
            ],
            |r| r.get(0),
        )?;
    Ok(SessionId(id))
}

pub(crate) fn session_id(
    conn: &Connection,
    harness: &str,
    ext_id: &str,
) -> Result<Option<SessionId>> {
    Ok(conn
        .prepare_cached("SELECT id FROM session WHERE harness_id = ?1 AND ext_id = ?2")?
        .query_row([harness, ext_id], |r| r.get(0))
        .optional()?
        .map(SessionId))
}

pub(crate) fn link_session(
    conn: &Connection,
    column: &str,
    session: SessionId,
    target: SessionId,
) -> Result<()> {
    if session == target {
        return Ok(());
    }
    let n = conn.execute(
        &format!("UPDATE session SET {column} = ?2 WHERE id = ?1"),
        params![session.0, target.0],
    )?;
    if n == 0 {
        return Err(StoreError::NotFound(format!("session {session}")));
    }
    Ok(())
}

pub(crate) fn session_holding_dedup_key(
    conn: &Connection,
    harness: &str,
    key: &str,
) -> Result<Option<SessionId>> {
    Ok(conn
        .prepare_cached(
            "SELECT session_id FROM usage_event WHERE harness_id = ?1 AND dedup_key = ?2",
        )?
        .query_row([harness, key], |r| r.get(0))
        .optional()?
        .map(SessionId))
}

pub(crate) fn last_cumulative_total(
    conn: &Connection,
    session: SessionId,
) -> Result<Option<TokenCounts>> {
    let json: Option<Option<String>> = conn
        .query_row(
            "SELECT cumulative_json FROM session WHERE id = ?1",
            [session.0],
            |r| r.get(0),
        )
        .optional()?;
    match json.flatten() {
        Some(j) => Ok(Some(serde_json::from_str(&j)?)),
        None => Ok(None),
    }
}

pub(crate) fn save_cumulative_total(
    conn: &Connection,
    session: SessionId,
    total: &TokenCounts,
) -> Result<()> {
    let n = conn.execute(
        "UPDATE session SET cumulative_json = ?2 WHERE id = ?1",
        params![session.0, serde_json::to_string(total)?],
    )?;
    if n == 0 {
        return Err(StoreError::NotFound(format!("session {session}")));
    }
    Ok(())
}

pub(crate) fn save_cursor(
    conn: &Connection,
    source: SourceFileId,
    cursor: &Cursor,
    now: Timestamp,
) -> Result<()> {
    conn.execute(
        "UPDATE source_file SET cursor_json = ?2, last_read = ?3, missing = 0 WHERE id = ?1",
        params![
            source.0,
            serde_json::to_string(cursor.as_json())?,
            DbTs(now)
        ],
    )?;
    Ok(())
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Sums {
    pub events: i64,
    pub input: i64,
    pub output: i64,
    pub cache_write: i64,
    pub cache_read: i64,
    pub cost: i64,
    pub unpriced: i64,
}

impl Sums {
    fn add(&mut self, e: &PricedEvent) {
        let t = e.event.tokens();
        self.events += 1;
        self.input += to_i64(t.input);
        self.output += to_i64(t.output);
        self.cache_write += to_i64(t.cache_write());
        self.cache_read += to_i64(t.cache_read);
        self.cost += e.cost.0;
        self.unpriced += i64::from(e.price_id.is_none());
    }
}

struct SessionAgg {
    sums: Sums,
    first: Timestamp,
    last: Timestamp,
    project: Option<i64>,
    branch: Option<String>,
}

#[derive(Clone, Copy)]
struct CachedSession {
    id: i64,
    project: i64,
}

fn session_for(
    conn: &Connection,
    harness: &str,
    ext_id: &str,
    ts: Timestamp,
    project: ProjectId,
    branch: Option<&str>,
) -> Result<CachedSession> {
    if let Some(found) = conn
        .prepare_cached("SELECT id, project_id FROM session WHERE harness_id = ?1 AND ext_id = ?2")?
        .query_row([harness, ext_id], |r| {
            Ok(CachedSession {
                id: r.get(0)?,
                project: r.get(1)?,
            })
        })
        .optional()?
    {
        return Ok(found);
    }
    ensure_harness(conn, harness)?;
    let id = conn
        .prepare_cached(
            "INSERT INTO session(harness_id, ext_id, project_id, git_branch, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5) RETURNING id",
        )?
        .query_row(params![harness, ext_id, project.0, branch, DbTs(ts)], |r| {
            r.get(0)
        })?;
    Ok(CachedSession {
        id,
        project: project.0,
    })
}

pub(crate) fn insert_events(
    conn: &Connection,
    source: SourceFileId,
    events: &[PricedEvent],
) -> Result<usize> {
    let mut sessions: HashMap<(&str, &str), CachedSession> = HashMap::new();
    let mut projects: HashMap<&str, ProjectId> = HashMap::new();
    let mut by_session: BTreeMap<i64, SessionAgg> = BTreeMap::new();
    let mut by_hour: BTreeMap<(String, &str, &str, i64), Sums> = BTreeMap::new();
    let mut models: BTreeSet<(i64, &str)> = BTreeSet::new();
    let mut inserted = 0;

    let mut insert = conn.prepare_cached(
        "INSERT INTO usage_event(harness_id, dedup_key, session_id, ts, provider, model,
           input_tokens, output_tokens, cache_write_5m_tokens, cache_write_1h_tokens,
           cache_read_tokens, reasoning_tokens, cost_micros, reported_cost_micros, price_id,
           source_file_id, project_id, raw_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
         ON CONFLICT(harness_id, dedup_key) DO NOTHING
         RETURNING id",
    )?;

    for pe in events {
        let e = &pe.event;
        let event_project = match e.project_path.as_deref().filter(|p| !p.is_empty()) {
            Some(p) => match projects.get(p) {
                Some(id) => Some(*id),
                None => {
                    let id = upsert_project(conn, Some(p))?;
                    projects.insert(p, id);
                    Some(id)
                }
            },
            None => None,
        };
        let key = (e.harness.as_str(), e.session_ext_id.as_str());
        let session = match sessions.get(&key) {
            Some(s) => *s,
            None => {
                let s = session_for(
                    conn,
                    key.0,
                    key.1,
                    e.ts,
                    event_project.unwrap_or(ProjectId::NONE),
                    e.git_branch.as_deref(),
                )?;
                sessions.insert(key, s);
                s
            }
        };
        let bucket_project = event_project.map_or(session.project, |p| p.0);
        let t = e.tokens();
        let new_id: Option<i64> = insert
            .query_row(
                params![
                    e.harness.as_str(),
                    e.dedup_key,
                    session.id,
                    DbTs(e.ts),
                    e.provider,
                    e.model,
                    to_i64(t.input),
                    to_i64(t.output),
                    to_i64(t.cache_write_5m),
                    to_i64(t.cache_write_1h),
                    to_i64(t.cache_read),
                    to_i64(t.reasoning),
                    pe.cost.0,
                    e.reported_cost.map(|c| Micros::from_usd(c).0),
                    pe.price_id.map(|p| p.0),
                    source.0,
                    bucket_project,
                    e.raw.as_ref().map(|r| r.get()),
                ],
                |r| r.get(0),
            )
            .optional()?;
        if new_id.is_none() {
            continue;
        }
        inserted += 1;

        let agg = by_session.entry(session.id).or_insert_with(|| SessionAgg {
            sums: Sums::default(),
            first: e.ts,
            last: e.ts,
            project: None,
            branch: None,
        });
        agg.sums.add(pe);
        agg.first = agg.first.min(e.ts);
        agg.last = agg.last.max(e.ts);
        if agg.project.is_none() {
            agg.project = event_project.map(|p| p.0);
        }
        if agg.branch.is_none() {
            agg.branch.clone_from(&e.git_branch);
        }
        by_hour
            .entry((
                fmt_ts(Hour::containing(e.ts).start()),
                e.harness.as_str(),
                e.model.as_str(),
                bucket_project,
            ))
            .or_default()
            .add(pe);
        models.insert((session.id, e.model.as_str()));
    }
    drop(insert);

    let mut bump_hour = conn.prepare_cached(
        "INSERT INTO hourly_usage(hour, harness_id, model, project_id, events, input_tokens,
           output_tokens, cache_write_tokens, cache_read_tokens, cost_micros, unpriced_events)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(hour, harness_id, model, project_id) DO UPDATE SET
           events = events + excluded.events,
           input_tokens = input_tokens + excluded.input_tokens,
           output_tokens = output_tokens + excluded.output_tokens,
           cache_write_tokens = cache_write_tokens + excluded.cache_write_tokens,
           cache_read_tokens = cache_read_tokens + excluded.cache_read_tokens,
           cost_micros = cost_micros + excluded.cost_micros,
           unpriced_events = unpriced_events + excluded.unpriced_events",
    )?;
    for ((hour, harness, model, project), s) in &by_hour {
        bump_hour.execute(params![
            hour,
            harness,
            model,
            project,
            s.events,
            s.input,
            s.output,
            s.cache_write,
            s.cache_read,
            s.cost,
            s.unpriced
        ])?;
    }

    let mut bump_session = conn.prepare_cached(
        "UPDATE session SET
           events = events + ?2, input_tokens = input_tokens + ?3,
           output_tokens = output_tokens + ?4, cache_write_tokens = cache_write_tokens + ?5,
           cache_read_tokens = cache_read_tokens + ?6, cost_micros = cost_micros + ?7,
           unpriced_events = unpriced_events + ?8,
           started_at = min(started_at, ?9),
           ended_at = CASE WHEN ended_at IS NULL OR ended_at < ?10 THEN ?10 ELSE ended_at END,
           project_id = CASE WHEN project_id = 0 AND ?11 IS NOT NULL THEN ?11 ELSE project_id END,
           git_branch = COALESCE(git_branch, ?12)
         WHERE id = ?1",
    )?;
    for (id, a) in &by_session {
        let s = a.sums;
        bump_session.execute(params![
            id,
            s.events,
            s.input,
            s.output,
            s.cache_write,
            s.cache_read,
            s.cost,
            s.unpriced,
            DbTs(a.first),
            DbTs(a.last),
            a.project,
            a.branch
        ])?;
    }

    let mut add_model = conn.prepare_cached(
        "INSERT INTO session_model(session_id, model) VALUES (?1, ?2) ON CONFLICT DO NOTHING",
    )?;
    for (session, model) in &models {
        add_model.execute(params![session, model])?;
    }

    for id in by_session.keys() {
        update_active_seconds(conn, *id)?;
    }
    Ok(inserted)
}

pub(crate) fn update_active_seconds(conn: &Connection, session: i64) -> Result<()> {
    let mut stmt =
        conn.prepare_cached("SELECT ts FROM usage_event WHERE session_id = ?1 ORDER BY ts")?;
    let mut rows = stmt.query([session])?;
    let mut prev: Option<i128> = None;
    let mut total: i128 = 0;
    while let Some(row) = rows.next()? {
        let ns = row.get::<_, DbTs>(0)?.0.as_nanosecond();
        if let Some(p) = prev {
            let gap = ns - p;
            if gap <= MAX_ACTIVE_GAP_SECS * 1_000_000_000 {
                total += gap;
            }
        }
        prev = Some(ns);
    }
    let secs = i64::try_from(total / 1_000_000_000).unwrap_or(i64::MAX);
    conn.prepare_cached("UPDATE session SET active_seconds = ?2 WHERE id = ?1")?
        .execute(params![session, secs])?;
    Ok(())
}

pub(crate) fn insert_messages(
    conn: &Connection,
    harness: &str,
    messages: &[MessageRecord],
) -> Result<usize> {
    let mut sessions: HashMap<&str, i64> = HashMap::new();
    let mut find_event =
        conn.prepare_cached("SELECT id FROM usage_event WHERE harness_id = ?1 AND dedup_key = ?2")?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO message(session_id, key, ts, role, kind, tool_name, preview, usage_event_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(session_id, key) DO NOTHING",
    )?;
    let mut link = conn.prepare_cached(
        "UPDATE message SET usage_event_id = ?3
         WHERE session_id = ?1 AND key = ?2 AND usage_event_id IS NULL",
    )?;
    let mut inserted = 0;
    for m in messages {
        let session = match sessions.get(m.session_ext_id.as_str()) {
            Some(id) => *id,
            None => {
                let s = session_for(
                    conn,
                    harness,
                    &m.session_ext_id,
                    m.ts,
                    ProjectId::NONE,
                    None,
                )?;
                sessions.insert(&m.session_ext_id, s.id);
                s.id
            }
        };
        let event: Option<i64> = match &m.event_dedup_key {
            Some(k) => find_event
                .query_row(params![harness, k], |r| r.get(0))
                .optional()?,
            None => None,
        };
        let n = insert.execute(params![
            session,
            m.key,
            DbTs(m.ts),
            m.role.as_str(),
            m.kind.map(|k| k.as_str()),
            m.tool_name,
            m.preview,
            event
        ])?;
        if n > 0 {
            inserted += 1;
        } else if let Some(ev) = event {
            link.execute(params![session, m.key, ev])?;
        }
    }
    Ok(inserted)
}
