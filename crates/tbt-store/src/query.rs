use std::collections::{BTreeMap, HashMap, HashSet};

use jiff::tz::TimeZone;
use rusqlite::{Connection, OptionalExtension, Row};
use tbt_core::{
    Decimal, EventId, HarnessId, Hour, LongContextTier, MessageKind, MessageRole, Micros,
    ModelPrice, PriceId, ProjectId, SessionId, Timestamp, TokenCounts,
};

use crate::admin::{harness_config, model_prices, parse_warnings, with_roots};
use crate::api::{
    BranchStats, BucketGroup, BucketSize, CostBucket, EventRow, HarnessStatus, HeatCell,
    KpiSummary, MessageRow, ModelStats, ProjectStats, QueryFilter, Result, SessionDetail,
    SessionRow, TokenSums, TypeCostBucket, UnpricedModel,
};
use crate::db::{DbTs, Params, corrupt, time_zone, to_u64, ts_opt};

fn hourly_where(filter: &QueryFilter, p: &mut Params) -> String {
    let start = p.push_ts(Hour::containing(filter.range.start).start());
    let end = p.push_ts(filter.range.end);
    format!(
        "h.hour >= {start} AND h.hour < {end}{}",
        p.harness_in("h.harness_id", &filter.harnesses)
    )
}

fn session_in_range(filter: &QueryFilter, p: &mut Params) -> String {
    let start = p.push_ts(filter.range.start);
    let end = p.push_ts(filter.range.end);
    format!(
        "s.started_at < {end} AND COALESCE(s.ended_at, s.started_at) >= {start}{}
         AND EXISTS (SELECT 1 FROM usage_event e
                     WHERE e.session_id = s.id AND e.ts >= {start} AND e.ts < {end})",
        p.harness_in("s.harness_id", &filter.harnesses)
    )
}

const SUM_COLUMNS: &str = "COALESCE(SUM(h.events), 0), COALESCE(SUM(h.input_tokens), 0),
    COALESCE(SUM(h.output_tokens), 0), COALESCE(SUM(h.cache_write_tokens), 0),
    COALESCE(SUM(h.cache_read_tokens), 0), COALESCE(SUM(h.cost_micros), 0)";

fn sums_at(r: &Row<'_>, i: usize) -> rusqlite::Result<(u64, TokenSums, Micros)> {
    Ok((
        to_u64(r.get(i)?),
        TokenSums {
            input: to_u64(r.get(i + 1)?),
            output: to_u64(r.get(i + 2)?),
            cache_write: to_u64(r.get(i + 3)?),
            cache_read: to_u64(r.get(i + 4)?),
        },
        Micros(r.get(i + 5)?),
    ))
}

fn add_sums(a: &mut TokenSums, b: &TokenSums) {
    a.input += b.input;
    a.output += b.output;
    a.cache_write += b.cache_write;
    a.cache_read += b.cache_read;
}

pub(crate) fn kpi_summary(conn: &Connection, filter: &QueryFilter) -> Result<KpiSummary> {
    let mut p = Params::default();
    let sql = format!(
        "SELECT {SUM_COLUMNS}, COALESCE(SUM(h.unpriced_events), 0), COUNT(DISTINCT h.hour)
         FROM hourly_usage h WHERE {}",
        hourly_where(filter, &mut p)
    );
    let (events, tokens, cost, unpriced, hours) = conn.query_row(&sql, p.iter(), |r| {
        let (events, tokens, cost) = sums_at(r, 0)?;
        Ok((events, tokens, cost, to_u64(r.get(6)?), to_u64(r.get(7)?)))
    })?;
    let mut p = Params::default();
    let sql = format!(
        "SELECT COUNT(*) FROM session s WHERE {}",
        session_in_range(filter, &mut p)
    );
    let sessions: i64 = conn.query_row(&sql, p.iter(), |r| r.get(0))?;
    Ok(KpiSummary {
        cost,
        tokens,
        events,
        sessions: to_u64(sessions),
        unpriced_events: unpriced,
        active_hours: hours,
    })
}

fn local_day_start(tz: &TimeZone, ts: Timestamp) -> Result<Timestamp> {
    Ok(ts
        .to_zoned_datetime(tz.clone())
        .start_of_day()
        .map_err(|e| corrupt(format!("start of day for {ts}: {e}")))?
        .timestamp())
}

pub(crate) fn cost_buckets(
    conn: &Connection,
    filter: &QueryFilter,
    group: BucketGroup,
) -> Result<Vec<CostBucket>> {
    let (key_sql, join) = match group {
        BucketGroup::Harness => ("h.harness_id", ""),
        BucketGroup::Model => ("h.model", ""),
        BucketGroup::Project => ("pr.path", " JOIN project pr ON pr.id = h.project_id"),
    };
    let mut p = Params::default();
    let sql = format!(
        "SELECT h.hour, {key_sql}, {SUM_COLUMNS} FROM hourly_usage h{join}
         WHERE {} GROUP BY h.hour, {key_sql}",
        hourly_where(filter, &mut p)
    );
    let mut starts = BucketStarts::new(filter)?;
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(p.iter(), |r| {
        Ok((
            r.get::<_, DbTs>(0)?.0,
            r.get::<_, String>(1)?,
            sums_at(r, 2)?,
        ))
    })?;
    let mut out: BTreeMap<(Timestamp, String), CostBucket> = BTreeMap::new();
    for row in rows {
        let (hour, key, (events, tokens, cost)) = row?;
        let start = starts.of(hour)?;
        let b = out
            .entry((start, key.clone()))
            .or_insert_with(|| CostBucket {
                start,
                key,
                cost: Micros::ZERO,
                tokens: TokenSums::default(),
                events: 0,
            });
        b.cost += cost;
        b.events += events;
        add_sums(&mut b.tokens, &tokens);
    }
    Ok(out.into_values().collect())
}

struct BucketStarts {
    tz: Option<TimeZone>,
    day_of: HashMap<Timestamp, Timestamp>,
}

impl BucketStarts {
    fn new(filter: &QueryFilter) -> Result<BucketStarts> {
        let tz = match filter.bucket {
            BucketSize::Hour => None,
            BucketSize::Day => Some(time_zone(&filter.time_zone)?),
        };
        Ok(BucketStarts {
            tz,
            day_of: HashMap::new(),
        })
    }

    fn of(&mut self, hour: Timestamp) -> Result<Timestamp> {
        let Some(tz) = &self.tz else {
            return Ok(hour);
        };
        if let Some(d) = self.day_of.get(&hour) {
            return Ok(*d);
        }
        let d = local_day_start(tz, hour)?;
        self.day_of.insert(hour, d);
        Ok(d)
    }
}

#[derive(Default)]
struct TypeCostAgg {
    input: Decimal,
    output: Decimal,
    cache_read: Decimal,
    cost: Micros,
}

pub(crate) fn type_cost_buckets(
    conn: &Connection,
    filter: &QueryFilter,
) -> Result<Vec<TypeCostBucket>> {
    let prices: HashMap<i64, ModelPrice> = model_prices(conn)?
        .into_iter()
        .filter_map(|p| p.id.map(|id| (id.0, p)))
        .collect();
    let mut p = Params::default();
    let sql = format!(
        "SELECT h.hour, h.price_id, h.over_tier, SUM(h.input_tokens), SUM(h.output_tokens),
                SUM(h.cache_read_tokens), SUM(h.cost_micros)
         FROM hourly_model_usage h WHERE {} AND h.price_id != 0
         GROUP BY h.hour, h.price_id, h.over_tier",
        hourly_where(filter, &mut p)
    );
    let mut starts = BucketStarts::new(filter)?;
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(p.iter(), |r| {
        Ok((
            r.get::<_, DbTs>(0)?.0,
            r.get::<_, i64>(1)?,
            r.get::<_, bool>(2)?,
            [r.get::<_, i64>(3)?, r.get(4)?, r.get(5)?],
            Micros(r.get(6)?),
        ))
    })?;
    let mut out: BTreeMap<Timestamp, TypeCostAgg> = BTreeMap::new();
    for row in rows {
        let (hour, price_id, over_tier, [input, output, cache_read], cost) = row?;
        let Some(price) = prices.get(&price_id) else {
            continue;
        };
        let tier = price.long_context.as_ref().filter(|_| over_tier);
        let rate =
            |r: Decimal, m: fn(&LongContextTier) -> Decimal| r * tier.map_or(Decimal::ONE, m);
        let input_rate = rate(price.input_per_mtok, |lc| lc.input);
        let cache_read_rate = price
            .cache_read_per_mtok
            .map_or(input_rate, |r| rate(r, |lc| lc.cache_read));
        let a = out.entry(starts.of(hour)?).or_default();
        a.input += Decimal::from(input) * input_rate;
        a.output += Decimal::from(output) * rate(price.output_per_mtok, |lc| lc.output);
        a.cache_read += Decimal::from(cache_read) * cache_read_rate;
        a.cost += cost;
    }
    Ok(out
        .into_iter()
        .map(|(start, a)| {
            let input = Micros::from_micros_decimal(a.input);
            let output = Micros::from_micros_decimal(a.output);
            let cache_read = Micros::from_micros_decimal(a.cache_read);
            let cache_write = Micros((a.cost.0 - input.0 - output.0 - cache_read.0).max(0));
            TypeCostBucket {
                start,
                input,
                output,
                cache_write,
                cache_read,
            }
        })
        .collect())
}

pub(crate) fn heatmap(conn: &Connection, filter: &QueryFilter) -> Result<Vec<HeatCell>> {
    let tz = time_zone(&filter.time_zone)?;
    let mut p = Params::default();
    let sql = format!(
        "SELECT h.hour, COALESCE(SUM(h.cost_micros), 0), COALESCE(SUM(h.events), 0)
         FROM hourly_usage h WHERE {} GROUP BY h.hour",
        hourly_where(filter, &mut p)
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(p.iter(), |r| {
        Ok((
            r.get::<_, DbTs>(0)?.0,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    let mut cells: BTreeMap<(u8, u8), HeatCell> = BTreeMap::new();
    for row in rows {
        let (hour, cost, events) = row?;
        let local = hour.to_zoned_datetime(tz.clone());
        let weekday = u8::try_from(local.weekday().to_monday_zero_offset()).unwrap_or(0);
        let h = u8::try_from(local.hour()).unwrap_or(0);
        let c = cells.entry((weekday, h)).or_insert_with(|| HeatCell {
            weekday,
            hour: h,
            cost: Micros::ZERO,
            events: 0,
        });
        c.cost += Micros(cost);
        c.events += to_u64(events);
    }
    Ok(cells.into_values().collect())
}

const SESSION_COLUMNS: &str = "s.id, s.harness_id, s.ext_id, s.title,
    CASE WHEN pr.id = 0 THEN NULL ELSE pr.path END,
    CASE WHEN pr.id = 0 THEN NULL ELSE pr.name END,
    s.git_branch, s.started_at, s.ended_at, s.active_seconds, s.events, s.input_tokens,
    s.output_tokens, s.cache_write_tokens, s.cache_read_tokens, s.cost_micros,
    s.unpriced_events, s.parent_session_id, s.resumed_from_session_id, s.harness_version,
    (SELECT group_concat(m.model, char(31)) FROM session_model m WHERE m.session_id = s.id)";

const SESSION_FROM: &str = "FROM session s JOIN project pr ON pr.id = s.project_id";

fn session_row(r: &Row<'_>) -> rusqlite::Result<(SessionRow, Option<String>)> {
    let mut models: Vec<String> = r
        .get::<_, Option<String>>(20)?
        .map(|m| m.split('\u{1f}').map(str::to_owned).collect())
        .unwrap_or_default();
    models.sort();
    Ok((
        SessionRow {
            id: SessionId(r.get(0)?),
            harness: HarnessId::new(r.get::<_, String>(1)?),
            ext_id: r.get(2)?,
            title: r.get(3)?,
            project_path: r.get(4)?,
            project_name: r.get(5)?,
            git_branch: r.get(6)?,
            models,
            started_at: r.get::<_, DbTs>(7)?.0,
            ended_at: ts_opt(r.get(8)?),
            active_seconds: to_u64(r.get(9)?),
            events: to_u64(r.get(10)?),
            tokens: TokenSums {
                input: to_u64(r.get(11)?),
                output: to_u64(r.get(12)?),
                cache_write: to_u64(r.get(13)?),
                cache_read: to_u64(r.get(14)?),
            },
            cost: Micros(r.get(15)?),
            unpriced_events: to_u64(r.get(16)?),
            parent: r.get::<_, Option<i64>>(17)?.map(SessionId),
            resumed_from: r.get::<_, Option<i64>>(18)?.map(SessionId),
        },
        r.get(19)?,
    ))
}

fn session_rows(conn: &Connection, where_sql: &str, p: &Params) -> Result<Vec<SessionRow>> {
    let sql = format!(
        "SELECT {SESSION_COLUMNS} {SESSION_FROM} WHERE {where_sql}
         ORDER BY s.started_at DESC, s.id DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(p.iter(), |r| session_row(r).map(|(s, _)| s))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn sessions(conn: &Connection, filter: &QueryFilter) -> Result<Vec<SessionRow>> {
    let mut p = Params::default();
    let where_sql = session_in_range(filter, &mut p);
    session_rows(conn, &where_sql, &p)
}

fn one_session(conn: &Connection, id: SessionId) -> Result<Option<(SessionRow, Option<String>)>> {
    let sql = format!("SELECT {SESSION_COLUMNS} {SESSION_FROM} WHERE s.id = ?1");
    Ok(conn.query_row(&sql, [id.0], session_row).optional()?)
}

pub(crate) fn session_row_by_id(conn: &Connection, id: SessionId) -> Result<Option<SessionRow>> {
    Ok(one_session(conn, id)?.map(|(row, _)| row))
}

pub(crate) fn session_detail(conn: &Connection, id: SessionId) -> Result<Option<SessionDetail>> {
    let Some((session, harness_version)) = one_session(conn, id)? else {
        return Ok(None);
    };

    let mut stmt = conn.prepare(
        "SELECT id, ts, provider, model, input_tokens, output_tokens, cache_write_5m_tokens,
                cache_write_1h_tokens, cache_read_tokens, reasoning_tokens, cost_micros,
                reported_cost_micros, price_id
         FROM usage_event WHERE session_id = ?1 ORDER BY ts, id",
    )?;
    let events = stmt
        .query_map([id.0], |r| {
            Ok(EventRow {
                id: EventId(r.get(0)?),
                ts: r.get::<_, DbTs>(1)?.0,
                provider: r.get(2)?,
                model: r.get(3)?,
                tokens: TokenCounts {
                    input: to_u64(r.get(4)?),
                    output: to_u64(r.get(5)?),
                    cache_write_5m: to_u64(r.get(6)?),
                    cache_write_1h: to_u64(r.get(7)?),
                    cache_read: to_u64(r.get(8)?),
                    reasoning: to_u64(r.get(9)?),
                },
                cost: Micros(r.get(10)?),
                reported_cost: r.get::<_, Option<i64>>(11)?.map(Micros),
                price_id: r.get::<_, Option<i64>>(12)?.map(PriceId),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let reported: Vec<Micros> = events.iter().filter_map(|e| e.reported_cost).collect();
    let reported_cost = (!reported.is_empty()).then(|| reported.iter().sum());

    let mut stmt = conn.prepare(
        "SELECT ts, role, kind, tool_name, preview, usage_event_id
         FROM message WHERE session_id = ?1 ORDER BY ts, id",
    )?;
    let raw = stmt
        .query_map([id.0], |r| {
            Ok((
                r.get::<_, DbTs>(0)?.0,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<i64>>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut messages = Vec::with_capacity(raw.len());
    let mut tools: HashMap<String, u64> = HashMap::new();
    for (ts, role, kind, tool_name, preview, event) in raw {
        let role: MessageRole = role.parse().map_err(corrupt)?;
        let kind: Option<MessageKind> = kind.map(|k| k.parse().map_err(corrupt)).transpose()?;
        if let Some(t) = &tool_name
            && matches!(kind, None | Some(MessageKind::ToolUse))
        {
            *tools.entry(t.clone()).or_default() += 1;
        }
        messages.push(MessageRow {
            ts,
            role,
            kind,
            tool_name,
            preview,
            event: event.map(EventId),
        });
    }
    let mut tool_counts: Vec<(String, u64)> = tools.into_iter().collect();
    tool_counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let mut resume_chain = vec![session.clone()];
    let mut seen = HashSet::from([session.id]);
    let mut next = session.resumed_from;
    while let Some(prev) = next {
        if !seen.insert(prev) {
            break;
        }
        let Some((row, _)) = one_session(conn, prev)? else {
            break;
        };
        next = row.resumed_from;
        resume_chain.push(row);
    }
    resume_chain.reverse();
    let mut frontier = vec![session.id];
    let mut later = Vec::new();
    while let Some(from) = frontier.pop() {
        let mut p = Params::default();
        let where_sql = format!("s.resumed_from_session_id = {}", p.push(from.0));
        for row in session_rows(conn, &where_sql, &p)? {
            if seen.insert(row.id) {
                frontier.push(row.id);
                later.push(row);
            }
        }
    }
    later.sort_by(|a, b| a.started_at.cmp(&b.started_at).then(a.id.cmp(&b.id)));
    resume_chain.extend(later);
    let chain_cost = resume_chain.iter().map(|s| s.cost).sum();

    let mut p = Params::default();
    let where_sql = format!("s.parent_session_id = {}", p.push(id.0));
    let mut subagents = session_rows(conn, &where_sql, &p)?;
    subagents.reverse();

    Ok(Some(SessionDetail {
        session,
        harness_version,
        events,
        messages,
        tool_counts,
        reported_cost,
        resume_chain,
        chain_cost,
        subagents,
    }))
}

#[derive(Default)]
struct ModelAgg {
    events: u64,
    tokens: TokenSums,
    cost: Micros,
    unpriced: u64,
    savings: Decimal,
}

fn cache_saving(price: &ModelPrice, over_tier: bool, cache_read: u64) -> Decimal {
    let Some(cr_rate) = price.cache_read_per_mtok else {
        return Decimal::ZERO;
    };
    let (m_in, m_cr) = match (&price.long_context, over_tier) {
        (Some(t), true) => (t.input, t.cache_read),
        _ => (Decimal::ONE, Decimal::ONE),
    };
    let per_token = price.input_per_mtok * m_in - cr_rate * m_cr;
    Decimal::from(cache_read) * per_token
}

pub(crate) fn model_stats(conn: &Connection, filter: &QueryFilter) -> Result<Vec<ModelStats>> {
    let prices: HashMap<i64, ModelPrice> = model_prices(conn)?
        .into_iter()
        .filter_map(|p| p.id.map(|id| (id.0, p)))
        .collect();
    let (start, end) = (filter.range.start, filter.range.end);
    let first_full = match Hour::containing(start) {
        h if h.start() == start => start,
        h => h.end(),
    };
    let last_full = Hour::containing(end).start();
    let (a, b) = if first_full < last_full {
        (first_full, last_full)
    } else {
        (end, end)
    };
    let mut p = Params::default();
    let (start, end, a, b) = (p.push_ts(start), p.push_ts(end), p.push_ts(a), p.push_ts(b));
    let rollup_harness = p.harness_in("h.harness_id", &filter.harnesses);
    let event_harness = p.harness_in("+e.harness_id", &filter.harnesses);
    let edge = |from: &str, to: &str| {
        format!(
            "SELECT e.provider, e.model, e.price_id,
                    CASE WHEN mp.long_context_threshold IS NOT NULL
                          AND e.input_tokens + e.cache_write_5m_tokens + e.cache_write_1h_tokens
                              + e.cache_read_tokens > mp.long_context_threshold
                         THEN 1 ELSE 0 END,
                    1, e.input_tokens, e.output_tokens,
                    e.cache_write_5m_tokens + e.cache_write_1h_tokens, e.cache_read_tokens,
                    e.cost_micros
             FROM usage_event e LEFT JOIN model_price mp ON mp.id = e.price_id
             WHERE e.ts >= {from} AND e.ts < {to}{event_harness}"
        )
    };
    let sql = format!(
        "SELECT provider, model, price_id, over_tier, SUM(n), SUM(i), SUM(o), SUM(cw), SUM(cr),
                SUM(c)
         FROM (SELECT h.provider AS provider, h.model AS model,
                      NULLIF(h.price_id, 0) AS price_id, h.over_tier AS over_tier,
                      h.events AS n, h.input_tokens AS i, h.output_tokens AS o,
                      h.cache_write_tokens AS cw, h.cache_read_tokens AS cr, h.cost_micros AS c
               FROM hourly_model_usage h
               WHERE h.hour >= {a} AND h.hour < {b}{rollup_harness}
               UNION ALL {}
               UNION ALL {})
         GROUP BY provider, model, price_id, over_tier",
        edge(&start, &a),
        edge(&b, &end)
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(p.iter(), |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, bool>(3)?,
            sums_at(r, 4)?,
        ))
    })?;
    let mut by_model: BTreeMap<(String, String), ModelAgg> = BTreeMap::new();
    for row in rows {
        let (provider, model, price_id, over_tier, (events, tokens, cost)) = row?;
        let a = by_model.entry((provider, model)).or_default();
        a.events += events;
        add_sums(&mut a.tokens, &tokens);
        a.cost += cost;
        match price_id.and_then(|id| prices.get(&id)) {
            Some(price) => a.savings += cache_saving(price, over_tier, tokens.cache_read),
            None if price_id.is_none() => a.unpriced += events,
            None => {}
        }
    }
    let mut out: Vec<ModelStats> = by_model
        .into_iter()
        .map(|((provider, model), a)| ModelStats {
            provider,
            model,
            events: a.events,
            tokens: a.tokens,
            cost: a.cost,
            unpriced_events: a.unpriced,
            cache_savings: Micros::from_micros_decimal(a.savings),
        })
        .collect();
    out.sort_by_key(|m| std::cmp::Reverse(m.cost));
    Ok(out)
}

pub(crate) fn project_stats(conn: &Connection, filter: &QueryFilter) -> Result<Vec<ProjectStats>> {
    let mut projects: BTreeMap<i64, ProjectStats> = BTreeMap::new();
    let mut entry = |conn: &Connection, id: i64| -> Result<()> {
        if projects.contains_key(&id) {
            return Ok(());
        }
        let (path, name): (String, Option<String>) =
            conn.query_row("SELECT path, name FROM project WHERE id = ?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        let none = id == ProjectId::NONE.0;
        projects.insert(
            id,
            ProjectStats {
                id: ProjectId(id),
                path: (!none).then_some(path),
                name: if none { None } else { name },
                sessions: 0,
                events: 0,
                tokens: TokenSums::default(),
                cost: Micros::ZERO,
                last_active: None,
                branches: Vec::new(),
            },
        );
        Ok(())
    };

    let mut p = Params::default();
    let sql = format!(
        "SELECT h.project_id, {SUM_COLUMNS} FROM hourly_usage h WHERE {}
         GROUP BY h.project_id",
        hourly_where(filter, &mut p)
    );
    let mut stmt = conn.prepare(&sql)?;
    let rollup = stmt
        .query_map(p.iter(), |r| Ok((r.get::<_, i64>(0)?, sums_at(r, 1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut totals = HashMap::new();
    for (id, sums) in rollup {
        entry(conn, id)?;
        totals.insert(id, sums);
    }

    let mut p = Params::default();
    let start = p.push_ts(Hour::containing(filter.range.start).start());
    let end_hour = Hour::containing(filter.range.end);
    let end = p.push_ts(if end_hour.start() == filter.range.end {
        filter.range.end
    } else {
        end_hour.end()
    });
    let sql = format!(
        "SELECT r.project_id, s.git_branch, COUNT(DISTINCT s.id), SUM(r.cost_micros),
                MAX(r.last_ts)
         FROM hourly_session_usage r JOIN session s ON s.id = r.session_id
         WHERE r.hour >= {start} AND r.hour < {end}{}
         GROUP BY r.project_id, s.git_branch",
        p.harness_in("s.harness_id", &filter.harnesses)
    );
    let mut stmt = conn.prepare(&sql)?;
    let branches = stmt
        .query_map(p.iter(), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?,
                to_u64(r.get(2)?),
                Micros(r.get(3)?),
                r.get::<_, DbTs>(4)?.0,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut branch_rows: Vec<(i64, BranchStats, Timestamp)> = Vec::new();
    for (id, branch, sessions, cost, last) in branches {
        entry(conn, id)?;
        branch_rows.push((
            id,
            BranchStats {
                branch,
                sessions,
                cost,
            },
            last,
        ));
    }

    for (id, (events, tokens, cost)) in totals {
        if let Some(ps) = projects.get_mut(&id) {
            ps.events = events;
            ps.tokens = tokens;
            ps.cost = cost;
        }
    }
    for (id, b, last) in branch_rows {
        if let Some(ps) = projects.get_mut(&id) {
            ps.sessions += b.sessions;
            ps.last_active = Some(ps.last_active.map_or(last, |l| l.max(last)));
            ps.branches.push(b);
        }
    }
    let mut out: Vec<ProjectStats> = projects.into_values().collect();
    for ps in &mut out {
        ps.branches
            .sort_by(|a, b| b.cost.cmp(&a.cost).then_with(|| a.branch.cmp(&b.branch)));
    }
    out.sort_by(|a, b| b.cost.cmp(&a.cost).then_with(|| a.id.cmp(&b.id)));
    Ok(out)
}

pub(crate) fn harness_status(conn: &Connection) -> Result<Vec<HarnessStatus>> {
    let last_warnings: Option<Option<String>> = conn
        .query_row(
            "SELECT warnings_json FROM ingest_run WHERE finished_at IS NOT NULL
             ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let warnings = parse_warnings(last_warnings.flatten().as_deref())?;

    let mut stmt = conn.prepare(
        "SELECT h.id, h.display_name, h.enabled, h.roots_json,
           (SELECT COUNT(*) FROM source_file f WHERE f.harness_id = h.id AND f.missing = 0),
           (SELECT COUNT(*) FROM source_file f WHERE f.harness_id = h.id AND f.missing = 1),
           (SELECT MAX(f.last_read) FROM source_file f WHERE f.harness_id = h.id),
           (SELECT COALESCE(SUM(s.events), 0) FROM session s WHERE s.harness_id = h.id),
           (SELECT COUNT(*) FROM session s WHERE s.harness_id = h.id)
         FROM harness h ORDER BY h.id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                harness_config(r, 0)?,
                to_u64(r.get(4)?),
                to_u64(r.get(5)?),
                ts_opt(r.get(6)?),
                to_u64(r.get(7)?),
                to_u64(r.get(8)?),
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|(config, tracked, missing, last_read, events, sessions)| {
            let config = with_roots(config)?;
            let last_warnings = warnings
                .iter()
                .filter(|w| w.harness == config.id)
                .cloned()
                .collect();
            Ok(HarnessStatus {
                config,
                files_tracked: tracked,
                files_missing: missing,
                last_read,
                events,
                sessions,
                last_warnings,
            })
        })
        .collect()
}

pub(crate) fn unpriced_models(conn: &Connection) -> Result<Vec<UnpricedModel>> {
    let mut stmt = conn.prepare(
        "SELECT provider, model, COUNT(*), SUM(input_tokens), SUM(output_tokens),
                SUM(cache_write_5m_tokens + cache_write_1h_tokens), SUM(cache_read_tokens),
                MIN(ts), MAX(ts)
         FROM usage_event WHERE price_id IS NULL
         GROUP BY provider, model ORDER BY COUNT(*) DESC, provider, model",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(UnpricedModel {
            provider: r.get(0)?,
            model: r.get(1)?,
            events: to_u64(r.get(2)?),
            tokens: TokenSums {
                input: to_u64(r.get(3)?),
                output: to_u64(r.get(4)?),
                cache_write: to_u64(r.get(5)?),
                cache_read: to_u64(r.get(6)?),
            },
            first_seen: r.get::<_, DbTs>(7)?.0,
            last_seen: r.get::<_, DbTs>(8)?.0,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
#[path = "query_equivalence.rs"]
mod equivalence;
