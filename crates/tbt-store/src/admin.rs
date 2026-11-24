use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Row, params};
use tbt_core::{
    Cursor, Decimal, HarnessId, LongContextTier, ModelPrice, ParseWarning, PriceId, PriceSource,
    RunId, Timestamp,
};

use crate::api::{HarnessConfig, IngestRunSummary, Result, RunStats, RunTrigger, StoreError};
use crate::db::{DbTs, corrupt, path_text, to_i64, to_u64, ts_opt};
use crate::recompute;

fn trigger_str(t: RunTrigger) -> &'static str {
    match t {
        RunTrigger::Hourly => "hourly",
        RunTrigger::Startup => "startup",
        RunTrigger::Manual => "manual",
        RunTrigger::Watch => "watch",
    }
}

fn parse_trigger(s: &str) -> Result<RunTrigger> {
    Ok(match s {
        "hourly" => RunTrigger::Hourly,
        "startup" => RunTrigger::Startup,
        "manual" => RunTrigger::Manual,
        "watch" => RunTrigger::Watch,
        other => return Err(corrupt(format!("run trigger {other:?}"))),
    })
}

pub(crate) fn start_run(
    conn: &Connection,
    trigger: RunTrigger,
    started_at: Timestamp,
) -> Result<RunId> {
    let id = conn.query_row(
        "INSERT INTO ingest_run(trigger, started_at) VALUES (?1, ?2) RETURNING id",
        params![trigger_str(trigger), DbTs(started_at)],
        |r| r.get(0),
    )?;
    Ok(RunId(id))
}

pub(crate) fn finish_run(
    conn: &Connection,
    run: RunId,
    finished_at: Timestamp,
    stats: &RunStats,
    warnings: &[ParseWarning],
    error: Option<&str>,
) -> Result<()> {
    let n = conn.execute(
        "UPDATE ingest_run SET finished_at = ?2, sources_scanned = ?3, events_new = ?4,
                               warnings_json = ?5, error = ?6
         WHERE id = ?1",
        params![
            run.0,
            DbTs(finished_at),
            to_i64(stats.sources_scanned),
            to_i64(stats.events_new),
            serde_json::to_string(warnings)?,
            error
        ],
    )?;
    if n == 0 {
        return Err(StoreError::NotFound(format!("ingest run {run}")));
    }
    Ok(())
}

pub(crate) fn ingest_runs(conn: &Connection, limit: usize) -> Result<Vec<IngestRunSummary>> {
    let mut stmt = conn.prepare(
        "SELECT id, trigger, started_at, finished_at, sources_scanned, events_new,
                warnings_json, error
         FROM ingest_run ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([i64::try_from(limit).unwrap_or(i64::MAX)], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, DbTs>(2)?,
            r.get::<_, Option<DbTs>>(3)?,
            r.get::<_, Option<i64>>(4)?,
            r.get::<_, Option<i64>>(5)?,
            r.get::<_, Option<String>>(6)?,
            r.get::<_, Option<String>>(7)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, trigger, started, finished, scanned, new, warnings, error) = row?;
        out.push(IngestRunSummary {
            id: RunId(id),
            trigger: parse_trigger(&trigger)?,
            started_at: started.0,
            finished_at: ts_opt(finished),
            stats: RunStats {
                sources_scanned: to_u64(scanned.unwrap_or(0)),
                events_new: to_u64(new.unwrap_or(0)),
            },
            warnings: parse_warnings(warnings.as_deref())?,
            error,
        });
    }
    Ok(out)
}

pub(crate) fn parse_warnings(json: Option<&str>) -> Result<Vec<ParseWarning>> {
    match json {
        Some(j) => Ok(serde_json::from_str(j)?),
        None => Ok(Vec::new()),
    }
}

pub(crate) fn cursor_for(
    conn: &Connection,
    harness: &HarnessId,
    path: &Path,
) -> Result<Option<Cursor>> {
    let json: Option<String> = conn
        .query_row(
            "SELECT cursor_json FROM source_file WHERE harness_id = ?1 AND path = ?2",
            params![harness.as_str(), path_text(path)],
            |r| r.get(0),
        )
        .optional()?;
    let Some(json) = json else {
        return Ok(None);
    };
    let cursor = Cursor::from_json(serde_json::from_str(&json)?);
    Ok((!cursor.is_empty()).then_some(cursor))
}

pub(crate) fn known_sources(conn: &Connection, harness: &HarnessId) -> Result<Vec<PathBuf>> {
    let mut stmt = conn.prepare(
        "SELECT path FROM source_file WHERE harness_id = ?1 AND missing = 0 ORDER BY path",
    )?;
    let rows = stmt.query_map([harness.as_str()], |r| r.get::<_, String>(0))?;
    Ok(rows
        .map(|p| p.map(PathBuf::from))
        .collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn mark_source_missing(
    conn: &Connection,
    harness: &HarnessId,
    path: &Path,
) -> Result<()> {
    conn.execute(
        "UPDATE source_file SET missing = 1 WHERE harness_id = ?1 AND path = ?2",
        params![harness.as_str(), path_text(path)],
    )?;
    Ok(())
}

pub(crate) fn register_harness(
    conn: &Connection,
    id: &HarnessId,
    display_name: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO harness(id, display_name) VALUES (?1, ?2)
         ON CONFLICT(id) DO UPDATE SET display_name = excluded.display_name",
        params![id.as_str(), display_name],
    )?;
    Ok(())
}

pub(crate) fn harness_config(
    r: &Row<'_>,
    offset: usize,
) -> rusqlite::Result<(HarnessConfig, Option<String>)> {
    Ok((
        HarnessConfig {
            id: HarnessId::new(r.get::<_, String>(offset)?),
            display_name: r.get(offset + 1)?,
            enabled: r.get(offset + 2)?,
            roots: None,
        },
        r.get(offset + 3)?,
    ))
}

pub(crate) fn with_roots(
    (mut config, roots): (HarnessConfig, Option<String>),
) -> Result<HarnessConfig> {
    config.roots = match roots {
        Some(j) => Some(
            serde_json::from_str::<Vec<String>>(&j)?
                .into_iter()
                .map(PathBuf::from)
                .collect(),
        ),
        None => None,
    };
    Ok(config)
}

pub(crate) fn harnesses(conn: &Connection) -> Result<Vec<HarnessConfig>> {
    let mut stmt =
        conn.prepare("SELECT id, display_name, enabled, roots_json FROM harness ORDER BY id")?;
    let rows = stmt.query_map([], |r| harness_config(r, 0))?;
    rows.map(|r| with_roots(r?)).collect()
}

fn update_harness(
    conn: &Connection,
    id: &HarnessId,
    sql: &str,
    value: impl rusqlite::ToSql,
) -> Result<()> {
    if conn.execute(sql, params![id.as_str(), value])? == 0 {
        return Err(StoreError::NotFound(format!("harness {id}")));
    }
    Ok(())
}

pub(crate) fn set_harness_enabled(conn: &Connection, id: &HarnessId, enabled: bool) -> Result<()> {
    update_harness(
        conn,
        id,
        "UPDATE harness SET enabled = ?2 WHERE id = ?1",
        enabled,
    )
}

pub(crate) fn set_harness_roots(
    conn: &Connection,
    id: &HarnessId,
    roots: Option<&[PathBuf]>,
) -> Result<()> {
    let json = match roots {
        Some(r) => Some(serde_json::to_string(
            &r.iter().map(|p| path_text(p)).collect::<Vec<_>>(),
        )?),
        None => None,
    };
    update_harness(
        conn,
        id,
        "UPDATE harness SET roots_json = ?2 WHERE id = ?1",
        json,
    )
}

const PRICE_COLUMNS: &str = "id, provider, model_pattern, effective_from, effective_to,
    input_per_mtok, output_per_mtok, cache_write_5m_per_mtok, cache_write_1h_per_mtok,
    cache_read_per_mtok, long_context_threshold, long_context_multiplier_json, source";

struct PriceText {
    id: i64,
    provider: String,
    pattern: String,
    from: DbTs,
    to: Option<DbTs>,
    rates: [Option<String>; 5],
    long_context: Option<String>,
    source: String,
}

fn decimal(s: &str) -> Result<Decimal> {
    s.parse()
        .map_err(|e| corrupt(format!("decimal {s:?}: {e}")))
}

fn opt_decimal(s: Option<&str>) -> Result<Option<Decimal>> {
    s.map(decimal).transpose()
}

impl PriceText {
    fn read(r: &Row<'_>) -> rusqlite::Result<PriceText> {
        Ok(PriceText {
            id: r.get(0)?,
            provider: r.get(1)?,
            pattern: r.get(2)?,
            from: r.get(3)?,
            to: r.get(4)?,
            rates: [r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?],
            long_context: r.get(11)?,
            source: r.get(12)?,
        })
    }

    fn into_price(self) -> Result<ModelPrice> {
        let [input, output, cw5, cw1, cr] = self.rates;
        Ok(ModelPrice {
            id: Some(PriceId(self.id)),
            provider: self.provider,
            model_pattern: self.pattern,
            effective_from: self.from.0,
            effective_to: ts_opt(self.to),
            input_per_mtok: decimal(input.as_deref().unwrap_or_default())?,
            output_per_mtok: decimal(output.as_deref().unwrap_or_default())?,
            cache_write_5m_per_mtok: opt_decimal(cw5.as_deref())?,
            cache_write_1h_per_mtok: opt_decimal(cw1.as_deref())?,
            cache_read_per_mtok: opt_decimal(cr.as_deref())?,
            long_context: match self.long_context {
                Some(j) => Some(serde_json::from_str::<LongContextTier>(&j)?),
                None => None,
            },
            source: self.source.parse::<PriceSource>().map_err(corrupt)?,
        })
    }
}

pub(crate) fn model_prices(conn: &Connection) -> Result<Vec<ModelPrice>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {PRICE_COLUMNS} FROM model_price ORDER BY id"
    ))?;
    let rows = stmt.query_map([], PriceText::read)?;
    rows.map(|r| r?.into_price()).collect()
}

fn price_values(p: &ModelPrice) -> Result<Vec<rusqlite::types::Value>> {
    use rusqlite::types::Value;
    let text = |d: &Decimal| Value::Text(d.to_string());
    let opt = |d: &Option<Decimal>| d.as_ref().map_or(Value::Null, text);
    Ok(vec![
        Value::Text(p.provider.clone()),
        Value::Text(p.model_pattern.clone()),
        Value::Text(crate::db::fmt_ts(p.effective_from)),
        p.effective_to
            .map_or(Value::Null, |t| Value::Text(crate::db::fmt_ts(t))),
        text(&p.input_per_mtok),
        text(&p.output_per_mtok),
        opt(&p.cache_write_5m_per_mtok),
        opt(&p.cache_write_1h_per_mtok),
        opt(&p.cache_read_per_mtok),
        p.long_context
            .as_ref()
            .map_or(Value::Null, |t| Value::Integer(to_i64(t.threshold_tokens))),
        match &p.long_context {
            Some(t) => Value::Text(serde_json::to_string(t)?),
            None => Value::Null,
        },
        Value::Text(p.source.as_str().to_owned()),
    ])
}

pub(crate) fn insert_price(conn: &Connection, price: &ModelPrice) -> Result<PriceId> {
    let id = conn
        .prepare_cached(
            "INSERT INTO model_price(provider, model_pattern, effective_from, effective_to,
               input_per_mtok, output_per_mtok, cache_write_5m_per_mtok, cache_write_1h_per_mtok,
               cache_read_per_mtok, long_context_threshold, long_context_multiplier_json, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) RETURNING id",
        )?
        .query_row(rusqlite::params_from_iter(price_values(price)?), |r| {
            r.get(0)
        })?;
    Ok(PriceId(id))
}

pub(crate) fn update_price(conn: &Connection, price: &ModelPrice) -> Result<()> {
    let Some(id) = price.id else {
        return Err(StoreError::NotFound("price row without id".into()));
    };
    let mut values = price_values(price)?;
    values.push(rusqlite::types::Value::Integer(id.0));
    let tx = conn.unchecked_transaction()?;
    let old_threshold: Option<Option<i64>> = tx
        .query_row(
            "SELECT long_context_threshold FROM model_price WHERE id = ?1",
            [id.0],
            |r| r.get(0),
        )
        .optional()?;
    let n = tx.execute(
        "UPDATE model_price SET provider = ?1, model_pattern = ?2, effective_from = ?3,
           effective_to = ?4, input_per_mtok = ?5, output_per_mtok = ?6,
           cache_write_5m_per_mtok = ?7, cache_write_1h_per_mtok = ?8, cache_read_per_mtok = ?9,
           long_context_threshold = ?10, long_context_multiplier_json = ?11, source = ?12
         WHERE id = ?13",
        rusqlite::params_from_iter(values),
    )?;
    if n == 0 {
        return Err(StoreError::NotFound(format!("price {id}")));
    }
    let new_threshold = price
        .long_context
        .as_ref()
        .map(|t| to_i64(t.threshold_tokens));
    if old_threshold.flatten() != new_threshold {
        recompute::rebuild_model_rollup_for_price(&tx, id.0)?;
    }
    tx.commit()?;
    Ok(())
}

pub(crate) fn close_price(conn: &Connection, id: PriceId, effective_to: Timestamp) -> Result<()> {
    let n = conn.execute(
        "UPDATE model_price SET effective_to = ?2 WHERE id = ?1",
        params![id.0, DbTs(effective_to)],
    )?;
    if n == 0 {
        return Err(StoreError::NotFound(format!("price {id}")));
    }
    Ok(())
}

pub(crate) fn seed_prices_if_empty(conn: &Connection, prices: &[ModelPrice]) -> Result<usize> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM model_price", [], |r| r.get(0))?;
    if count > 0 {
        return Ok(0);
    }
    for p in prices {
        insert_price(conn, p)?;
    }
    Ok(prices.len())
}
