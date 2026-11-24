use std::collections::BTreeSet;

use rusqlite::{Connection, params};
use tbt_core::{EventId, HarnessId, Hour, Micros, PriceId, SessionId, Timestamp, UsageEvent};

use crate::api::{Affected, CostUpdate, RecomputeFilter, RecomputeRow, Result};
use crate::db::{DbTs, Params, fmt_ts, to_u64};
use crate::ingest::update_active_seconds;

pub(crate) fn events_for_recompute(
    conn: &Connection,
    filter: &RecomputeFilter,
    after: Option<EventId>,
    limit: usize,
) -> Result<Vec<RecomputeRow>> {
    let mut p = Params::default();
    let mut sql = format!(
        "SELECT e.id, e.session_id, e.harness_id, e.dedup_key, s.ext_id, e.ts, e.provider,
                e.model, e.input_tokens, e.output_tokens, e.cache_write_5m_tokens,
                e.cache_write_1h_tokens, e.cache_read_tokens, e.reasoning_tokens,
                e.reported_cost_micros, CASE WHEN e.project_id = 0 THEN NULL ELSE pr.path END,
                e.cost_micros, e.price_id
         FROM usage_event e
         JOIN session s ON s.id = e.session_id
         JOIN project pr ON pr.id = e.project_id
         WHERE e.id > {}",
        p.push(after.map_or(i64::MIN, |a| a.0))
    );
    if let Some(id) = filter.price_id {
        sql += &format!(" AND e.price_id = {}", p.push(id.0));
    }
    if let Some(r) = filter.range {
        sql += &format!(
            " AND +e.ts >= {} AND +e.ts < {}",
            p.push_ts(r.start),
            p.push_ts(r.end)
        );
    }
    if let Some(provider) = &filter.provider {
        let col = if filter.model.is_some() {
            "e.provider"
        } else {
            "+e.provider"
        };
        sql += &format!(" AND {col} = {}", p.push(provider.clone()));
    }
    if let Some(model) = &filter.model {
        sql += &format!(" AND e.model = {}", p.push(model.clone()));
    }
    if filter.unpriced_only {
        sql += " AND e.price_id IS NULL";
    }
    sql += &format!(
        " ORDER BY e.id LIMIT {}",
        p.push(i64::try_from(limit).unwrap_or(i64::MAX))
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(p.iter(), |r| {
        Ok(RecomputeRow {
            id: EventId(r.get(0)?),
            session: SessionId(r.get(1)?),
            event: UsageEvent {
                harness: HarnessId::new(r.get::<_, String>(2)?),
                dedup_key: r.get(3)?,
                session_ext_id: r.get(4)?,
                ts: r.get::<_, DbTs>(5)?.0,
                provider: r.get(6)?,
                model: r.get(7)?,
                input_tokens: to_u64(r.get(8)?),
                output_tokens: to_u64(r.get(9)?),
                cache_write_5m_tokens: to_u64(r.get(10)?),
                cache_write_1h_tokens: to_u64(r.get(11)?),
                cache_read_tokens: to_u64(r.get(12)?),
                reasoning_tokens: to_u64(r.get(13)?),
                reported_cost: r.get::<_, Option<i64>>(14)?.map(|m| Micros(m).to_usd()),
                project_path: r.get(15)?,
                git_branch: None,
                raw: None,
            },
            cost: Micros(r.get(16)?),
            price_id: r.get::<_, Option<i64>>(17)?.map(PriceId),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn update_event_costs(conn: &Connection, updates: &[CostUpdate]) -> Result<Affected> {
    let mut stmt = conn.prepare_cached(
        "UPDATE usage_event SET cost_micros = ?2, price_id = ?3
         WHERE id = ?1 AND (cost_micros IS NOT ?2 OR price_id IS NOT ?3)
         RETURNING session_id, ts",
    )?;
    let mut sessions = BTreeSet::new();
    let mut hours = BTreeSet::new();
    for u in updates {
        let mut rows = stmt.query(params![u.event.0, u.cost.0, u.price_id.map(|p| p.0)])?;
        while let Some(row) = rows.next()? {
            sessions.insert(SessionId(row.get(0)?));
            hours.insert(Hour::containing(row.get::<_, DbTs>(1)?.0));
        }
    }
    Ok(Affected {
        sessions: sessions.into_iter().collect(),
        hours: hours.into_iter().collect(),
    })
}

pub(crate) fn rebuild_session_totals(conn: &Connection, sessions: &[SessionId]) -> Result<()> {
    let mut totals = conn.prepare_cached(
        "UPDATE session SET (events, input_tokens, output_tokens, cache_write_tokens,
                             cache_read_tokens, cost_micros, unpriced_events) =
           (SELECT COUNT(*), COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0),
                   COALESCE(SUM(cache_write_5m_tokens + cache_write_1h_tokens), 0),
                   COALESCE(SUM(cache_read_tokens), 0), COALESCE(SUM(cost_micros), 0),
                   COALESCE(SUM(price_id IS NULL), 0)
            FROM usage_event WHERE session_id = ?1)
         WHERE id = ?1",
    )?;
    let mut models = conn.prepare_cached(
        "INSERT INTO session_model(session_id, model)
         SELECT DISTINCT session_id, model FROM usage_event WHERE session_id = ?1
         ON CONFLICT DO NOTHING",
    )?;
    for s in sessions {
        totals.execute([s.0])?;
        models.execute([s.0])?;
        update_active_seconds(conn, s.0)?;
    }
    Ok(())
}

fn model_rollup_insert(hour: &str, filter: &str) -> String {
    format!(
        "INSERT INTO hourly_model_usage(hour, harness_id, provider, model, price_id, over_tier,
           events, input_tokens, output_tokens, cache_write_tokens, cache_read_tokens,
           cost_micros)
         SELECT {hour} AS h, e.harness_id, e.provider, e.model, COALESCE(e.price_id, 0),
                CASE WHEN mp.long_context_threshold IS NOT NULL
                      AND e.input_tokens + e.cache_write_5m_tokens + e.cache_write_1h_tokens
                          + e.cache_read_tokens > mp.long_context_threshold
                     THEN 1 ELSE 0 END AS over_tier,
                COUNT(*), SUM(e.input_tokens), SUM(e.output_tokens),
                SUM(e.cache_write_5m_tokens + e.cache_write_1h_tokens), SUM(e.cache_read_tokens),
                SUM(e.cost_micros)
         FROM usage_event e LEFT JOIN model_price mp ON mp.id = e.price_id
         WHERE {filter}
         GROUP BY h, e.harness_id, e.provider, e.model, e.price_id, over_tier"
    )
}

pub(crate) fn rebuild_model_rollup_for_price(conn: &Connection, price: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM hourly_model_usage WHERE price_id = ?1",
        [price],
    )?;
    conn.execute(
        &model_rollup_insert(
            "substr(e.ts, 1, 13) || ':00:00.000000000Z'",
            "e.price_id = ?1",
        ),
        [price],
    )?;
    Ok(())
}

pub(crate) fn rebuild_hourly(conn: &Connection, hours: &[Hour]) -> Result<()> {
    let mut delete = conn.prepare_cached("DELETE FROM hourly_usage WHERE hour = ?1")?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO hourly_usage(hour, harness_id, model, project_id, events, input_tokens,
           output_tokens, cache_write_tokens, cache_read_tokens, cost_micros, unpriced_events)
         SELECT ?1, harness_id, model, project_id, COUNT(*), SUM(input_tokens),
                SUM(output_tokens), SUM(cache_write_5m_tokens + cache_write_1h_tokens),
                SUM(cache_read_tokens), SUM(cost_micros), SUM(price_id IS NULL)
         FROM usage_event WHERE ts >= ?1 AND ts < ?2
         GROUP BY harness_id, model, project_id",
    )?;
    let mut delete_model = conn.prepare_cached("DELETE FROM hourly_model_usage WHERE hour = ?1")?;
    let mut insert_model =
        conn.prepare_cached(&model_rollup_insert("?1", "e.ts >= ?1 AND e.ts < ?2"))?;
    let mut delete_session =
        conn.prepare_cached("DELETE FROM hourly_session_usage WHERE hour = ?1")?;
    let mut insert_session = conn.prepare_cached(
        "INSERT INTO hourly_session_usage(hour, session_id, project_id, events, cost_micros,
           last_ts)
         SELECT ?1, session_id, project_id, COUNT(*), SUM(cost_micros), MAX(ts)
         FROM usage_event WHERE ts >= ?1 AND ts < ?2
         GROUP BY session_id, project_id",
    )?;
    for h in hours {
        let start = fmt_ts(h.start());
        let end = fmt_ts(h.end());
        delete.execute([&start])?;
        insert.execute(params![start, end])?;
        delete_model.execute([&start])?;
        insert_model.execute(params![start, end])?;
        delete_session.execute([&start])?;
        insert_session.execute(params![start, end])?;
    }
    Ok(())
}

pub(crate) fn drop_raw_json_older_than(
    conn: &Connection,
    days: u32,
    now: Timestamp,
) -> Result<usize> {
    let cutoff = now
        .checked_sub(jiff::SignedDuration::from_hours(i64::from(days) * 24))
        .unwrap_or(Timestamp::MIN);
    Ok(conn.execute(
        "UPDATE usage_event SET raw_json = NULL WHERE ts < ?1 AND raw_json IS NOT NULL",
        [DbTs(cutoff)],
    )?)
}
