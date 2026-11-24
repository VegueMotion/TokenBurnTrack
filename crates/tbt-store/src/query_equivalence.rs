use std::path::Path;

use proptest::prelude::*;
use tbt_core::{LongContextTier, PriceSource, PricedEvent, SourceFileId, TimeRange, UsageEvent};

use super::*;
use crate::api::CostUpdate;
use crate::{admin, ingest, recompute, schema};

const BASE: i64 = 1_780_000_000;

#[derive(Debug, Clone)]
struct Ev {
    codex: bool,
    session: u8,
    secs: i64,
    model: u8,
    input: u64,
    cache_read: u64,
    cost: i64,
    priced: bool,
    project: u8,
}

fn ev() -> impl Strategy<Value = Ev> {
    (
        (any::<bool>(), 0u8..5, 0i64..3 * 86_400, 0u8..3),
        (0u64..300, 0u64..300, 0i64..10_000, any::<bool>(), 0u8..3),
    )
        .prop_map(
            |((codex, session, secs, model), (input, cache_read, cost, priced, project))| Ev {
                codex,
                session,
                secs,
                model,
                input,
                cache_read,
                cost,
                priced,
                project,
            },
        )
}

fn harness(codex: bool) -> HarnessId {
    if codex {
        HarnessId::CODEX
    } else {
        HarnessId::CLAUDE_CODE
    }
}

fn tier(threshold_tokens: u64) -> LongContextTier {
    LongContextTier {
        threshold_tokens,
        input: Decimal::TWO,
        output: Decimal::ONE,
        cache_write: Decimal::ONE,
        cache_read: Decimal::TWO,
    }
}

fn price(model: &str, threshold: Option<u64>) -> ModelPrice {
    ModelPrice {
        id: None,
        provider: "anthropic".into(),
        model_pattern: model.into(),
        effective_from: Timestamp::from_second(0).unwrap(),
        effective_to: None,
        input_per_mtok: Decimal::from(3),
        output_per_mtok: Decimal::from(15),
        cache_write_5m_per_mtok: None,
        cache_write_1h_per_mtok: None,
        cache_read_per_mtok: Some(Decimal::new(3, 1)),
        long_context: threshold.map(tier),
        source: PriceSource::User,
    }
}

const MODELS: [&str; 3] = ["claude-sonnet-4-5", "claude-opus-4-5", "local-model"];

fn to_event(i: usize, e: &Ev) -> PricedEvent {
    let h = harness(e.codex);
    let priced = e.priced && e.model < 2;
    PricedEvent {
        event: UsageEvent {
            harness: h.clone(),
            dedup_key: format!("k{i}"),
            session_ext_id: format!("{h}-s{}", e.session),
            ts: Timestamp::from_second(BASE + e.secs).unwrap()
                + jiff::SignedDuration::from_millis(i64::try_from(i % 1000).unwrap()),
            provider: "anthropic".into(),
            model: MODELS[usize::from(e.model)].into(),
            input_tokens: e.input,
            output_tokens: 7,
            cache_write_5m_tokens: 3,
            cache_write_1h_tokens: 2,
            cache_read_tokens: e.cache_read,
            reasoning_tokens: 0,
            reported_cost: None,
            project_path: match e.project {
                0 => None,
                p => Some(format!("/p/{p}")),
            },
            git_branch: Some(format!("b{}", e.session % 2)),
            raw: None,
        },
        cost: Micros(if priced { e.cost } else { 0 }),
        price_id: priced.then(|| PriceId(i64::from(e.model) + 1)),
    }
}

fn seeded(evs: &[Ev], cuts: &[usize]) -> Connection {
    let conn = schema::open_in_memory().unwrap();
    admin::insert_price(&conn, &price(MODELS[0], Some(350))).unwrap();
    admin::insert_price(&conn, &price(MODELS[1], None)).unwrap();
    let events: Vec<PricedEvent> = evs
        .iter()
        .enumerate()
        .map(|(i, e)| to_event(i, e))
        .collect();
    let mut rest = events.as_slice();
    for size in cuts.iter().cycle() {
        if rest.is_empty() {
            break;
        }
        let (batch, tail) = rest.split_at((*size).min(rest.len()));
        for codex in [false, true] {
            let part: Vec<PricedEvent> = batch
                .iter()
                .filter(|e| e.event.harness == harness(codex))
                .cloned()
                .collect();
            let tx = conn.unchecked_transaction().unwrap();
            let src: SourceFileId =
                ingest::source_file(&tx, harness(codex).as_str(), Path::new("/src")).unwrap();
            ingest::insert_events(&tx, src, &part).unwrap();
            tx.commit().unwrap();
        }
        rest = tail;
    }
    conn
}

type Dump = (Vec<Vec<String>>, Vec<Vec<String>>);

fn rollups(conn: &Connection) -> Dump {
    let dump = |sql: &str| -> Vec<Vec<String>> {
        let mut stmt = conn.prepare(sql).unwrap();
        let n = stmt.column_count();
        stmt.query_map([], |r| {
            (0..n)
                .map(|i| {
                    Ok(match r.get_ref(i)? {
                        rusqlite::types::ValueRef::Integer(v) => v.to_string(),
                        rusqlite::types::ValueRef::Text(t) => {
                            String::from_utf8_lossy(t).into_owned()
                        }
                        other => format!("{other:?}"),
                    })
                })
                .collect()
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    };
    (
        dump("SELECT * FROM hourly_model_usage ORDER BY 1, 2, 3, 4, 5, 6"),
        dump("SELECT * FROM hourly_session_usage ORDER BY 1, 2, 3"),
    )
}

fn rebuilt_from_events(conn: &Connection) -> Dump {
    conn.execute_batch(
        "SAVEPOINT rebuild; DELETE FROM hourly_model_usage; DELETE FROM hourly_session_usage;",
    )
    .unwrap();
    let hours: Vec<Hour> = {
        let mut stmt = conn
            .prepare("SELECT DISTINCT substr(ts, 1, 13) || ':00:00Z' FROM usage_event")
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|h| Hour::containing(h.unwrap().parse().unwrap()))
            .collect()
    };
    recompute::rebuild_hourly(conn, &hours).unwrap();
    let out = rollups(conn);
    conn.execute_batch("ROLLBACK TO rebuild; RELEASE rebuild;")
        .unwrap();
    out
}

fn filter(start: i64, len: i64, aligned: bool, harnesses: u8) -> QueryFilter {
    let (start, len) = if aligned {
        (start - start.rem_euclid(3_600), len - len.rem_euclid(3_600))
    } else {
        (start, len)
    };
    let mut f = QueryFilter::new(
        TimeRange::new(
            Timestamp::from_second(BASE + start).unwrap(),
            Timestamp::from_second(BASE + start + len).unwrap(),
        ),
        BucketSize::Day,
    );
    f.harnesses = match harnesses {
        0 => vec![],
        1 => vec![HarnessId::CLAUDE_CODE],
        2 => vec![HarnessId::CODEX],
        _ => vec![HarnessId::CLAUDE_CODE, HarnessId::CODEX],
    };
    f
}

fn assert_same(conn: &Connection, filters: &[QueryFilter]) -> Result<(), TestCaseError> {
    prop_assert_eq!(rollups(conn), rebuilt_from_events(conn));
    for f in filters {
        prop_assert_eq!(
            model_stats(conn, f).unwrap(),
            legacy_model_stats(conn, f).unwrap()
        );
        prop_assert_eq!(
            project_stats(conn, f).unwrap(),
            legacy_project_stats(conn, f).unwrap()
        );
    }
    Ok(())
}

fn filters() -> impl Strategy<Value = Vec<QueryFilter>> {
    prop::collection::vec(
        (
            -7_200i64..3 * 86_400,
            0i64..2 * 86_400,
            any::<bool>(),
            0u8..4,
        )
            .prop_map(|(s, l, a, h)| filter(s, l, a, h)),
        1..6,
    )
    .prop_map(|mut v| {
        v.push(filter(-86_400, 6 * 86_400, true, 0));
        v.push(filter(1_800, 600, false, 1));
        v
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn rollup_queries_match_event_scans(
        evs in prop::collection::vec(ev(), 1..60),
        cuts in prop::collection::vec(1usize..15, 1..6),
        filters in filters(),
        repriced in prop::collection::vec((0usize..60, 0i64..5_000, 0u8..3), 0..10),
        new_threshold in prop::option::of(0u64..700),
    ) {
        let conn = seeded(&evs, &cuts);
        assert_same(&conn, &filters)?;

        let updates: Vec<CostUpdate> = repriced
            .iter()
            .filter(|(i, _, _)| *i < evs.len())
            .map(|(i, cost, p)| CostUpdate {
                event: EventId(i64::try_from(*i).unwrap() + 1),
                cost: Micros(*cost),
                price_id: (*p > 0).then(|| PriceId(i64::from(*p))),
            })
            .collect();
        let tx = conn.unchecked_transaction().unwrap();
        let affected = recompute::update_event_costs(&tx, &updates).unwrap();
        recompute::rebuild_session_totals(&tx, &affected.sessions).unwrap();
        recompute::rebuild_hourly(&tx, &affected.hours).unwrap();
        tx.commit().unwrap();
        assert_same(&conn, &filters)?;

        let mut p = admin::model_prices(&conn).unwrap().remove(0);
        p.long_context = new_threshold.map(tier);
        admin::update_price(&conn, &p).unwrap();
        assert_same(&conn, &filters)?;
    }

    #[test]
    fn migration_backfill_matches_ingest_rollups(
        evs in prop::collection::vec(ev(), 1..60),
        cuts in prop::collection::vec(1usize..15, 1..6),
    ) {
        let mut conn = seeded(&evs, &cuts);
        let maintained = rollups(&conn);
        conn.execute_batch(
            "DROP TABLE hourly_model_usage; DROP TABLE hourly_session_usage;
             DROP INDEX ix_session_resumed;
             PRAGMA user_version = 1;",
        )
        .unwrap();
        schema::migrations().to_latest(&mut conn).unwrap();
        prop_assert_eq!(rollups(&conn), maintained);
    }
}

fn legacy_model_stats(conn: &Connection, filter: &QueryFilter) -> Result<Vec<ModelStats>> {
    let prices: HashMap<i64, ModelPrice> = model_prices(conn)?
        .into_iter()
        .filter_map(|p| p.id.map(|id| (id.0, p)))
        .collect();
    let mut p = Params::default();
    let start = p.push_ts(filter.range.start);
    let end = p.push_ts(filter.range.end);
    let sql = format!(
        "SELECT e.provider, e.model, e.price_id,
                CASE WHEN mp.long_context_threshold IS NOT NULL
                      AND e.input_tokens + e.cache_write_5m_tokens + e.cache_write_1h_tokens
                          + e.cache_read_tokens > mp.long_context_threshold
                     THEN 1 ELSE 0 END AS over_tier,
                COUNT(*), SUM(e.input_tokens), SUM(e.output_tokens),
                SUM(e.cache_write_5m_tokens + e.cache_write_1h_tokens), SUM(e.cache_read_tokens),
                SUM(e.cost_micros)
         FROM usage_event e LEFT JOIN model_price mp ON mp.id = e.price_id
         WHERE e.ts >= {start} AND e.ts < {end}{}
         GROUP BY e.provider, e.model, e.price_id, over_tier",
        p.harness_in("e.harness_id", &filter.harnesses)
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

fn legacy_project_stats(conn: &Connection, filter: &QueryFilter) -> Result<Vec<ProjectStats>> {
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
        "SELECT e.project_id, s.git_branch, COUNT(DISTINCT s.id), SUM(e.cost_micros), MAX(e.ts)
         FROM usage_event e JOIN session s ON s.id = e.session_id
         WHERE e.ts >= {start} AND e.ts < {end}{}
         GROUP BY e.project_id, s.git_branch",
        p.harness_in("e.harness_id", &filter.harnesses)
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
