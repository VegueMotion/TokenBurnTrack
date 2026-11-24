use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};
use rusqlite_migration::{M, Migrations};

use crate::api::Result;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const V1: &str = r#"
CREATE TABLE harness (
  id TEXT PRIMARY KEY, display_name TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1, roots_json TEXT
);

CREATE TABLE source_file (
  id INTEGER PRIMARY KEY, harness_id TEXT NOT NULL REFERENCES harness(id),
  path TEXT NOT NULL, cursor_json TEXT NOT NULL,
  first_seen TEXT NOT NULL, last_read TEXT, missing INTEGER NOT NULL DEFAULT 0,
  UNIQUE(harness_id, path)
);

CREATE TABLE project (
  id INTEGER PRIMARY KEY, path TEXT UNIQUE NOT NULL, name TEXT
);
INSERT INTO project(id, path, name) VALUES (0, '', 'no project');

CREATE TABLE session (
  id INTEGER PRIMARY KEY, harness_id TEXT NOT NULL REFERENCES harness(id),
  ext_id TEXT NOT NULL, project_id INTEGER NOT NULL DEFAULT 0 REFERENCES project(id),
  title TEXT, git_branch TEXT, harness_version TEXT,
  resumed_from_session_id INTEGER REFERENCES session(id),
  parent_session_id INTEGER REFERENCES session(id),
  started_at TEXT NOT NULL, ended_at TEXT, active_seconds INTEGER NOT NULL DEFAULT 0,
  events INTEGER NOT NULL DEFAULT 0,
  input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
  cache_write_tokens INTEGER NOT NULL DEFAULT 0, cache_read_tokens INTEGER NOT NULL DEFAULT 0,
  cost_micros INTEGER NOT NULL DEFAULT 0,
  unpriced_events INTEGER NOT NULL DEFAULT 0,
  cumulative_json TEXT,
  UNIQUE(harness_id, ext_id)
);
CREATE INDEX ix_session_started ON session(started_at);
CREATE INDEX ix_session_project ON session(project_id);
CREATE INDEX ix_session_parent ON session(parent_session_id);

CREATE TABLE session_model (
  session_id INTEGER NOT NULL REFERENCES session(id), model TEXT NOT NULL,
  PRIMARY KEY(session_id, model)
) WITHOUT ROWID;

CREATE TABLE usage_event (
  id INTEGER PRIMARY KEY,
  harness_id TEXT NOT NULL, dedup_key TEXT NOT NULL,
  session_id INTEGER NOT NULL REFERENCES session(id),
  ts TEXT NOT NULL, provider TEXT NOT NULL, model TEXT NOT NULL,
  input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,
  cache_write_5m_tokens INTEGER NOT NULL, cache_write_1h_tokens INTEGER NOT NULL,
  cache_read_tokens INTEGER NOT NULL, reasoning_tokens INTEGER NOT NULL,
  cost_micros INTEGER NOT NULL,
  reported_cost_micros INTEGER,
  price_id INTEGER REFERENCES model_price(id),
  source_file_id INTEGER REFERENCES source_file(id),
  project_id INTEGER NOT NULL DEFAULT 0 REFERENCES project(id),
  raw_json TEXT,
  UNIQUE(harness_id, dedup_key)
);
CREATE INDEX ix_event_ts ON usage_event(ts);
CREATE INDEX ix_event_session ON usage_event(session_id, ts);
CREATE INDEX ix_event_price ON usage_event(price_id);
CREATE INDEX ix_event_unpriced ON usage_event(provider, model) WHERE price_id IS NULL;

CREATE TABLE message (
  id INTEGER PRIMARY KEY, session_id INTEGER NOT NULL REFERENCES session(id),
  key TEXT NOT NULL,
  ts TEXT NOT NULL, role TEXT NOT NULL, kind TEXT,
  tool_name TEXT, preview TEXT,
  usage_event_id INTEGER REFERENCES usage_event(id),
  UNIQUE(session_id, key)
);
CREATE INDEX ix_message_session_ts ON message(session_id, ts);

CREATE TABLE hourly_usage (
  hour TEXT NOT NULL, harness_id TEXT NOT NULL, model TEXT NOT NULL,
  project_id INTEGER NOT NULL REFERENCES project(id), events INTEGER NOT NULL,
  input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,
  cache_write_tokens INTEGER NOT NULL, cache_read_tokens INTEGER NOT NULL,
  cost_micros INTEGER NOT NULL,
  unpriced_events INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(hour, harness_id, model, project_id)
);

CREATE TABLE model_price (
  id INTEGER PRIMARY KEY, provider TEXT NOT NULL, model_pattern TEXT NOT NULL,
  effective_from TEXT NOT NULL, effective_to TEXT,
  input_per_mtok TEXT NOT NULL, output_per_mtok TEXT NOT NULL,
  cache_write_5m_per_mtok TEXT, cache_write_1h_per_mtok TEXT,
  cache_read_per_mtok TEXT, long_context_threshold INTEGER,
  long_context_multiplier_json TEXT, source TEXT NOT NULL
);

CREATE TABLE ingest_run (
  id INTEGER PRIMARY KEY, trigger TEXT NOT NULL,
  started_at TEXT NOT NULL, finished_at TEXT,
  sources_scanned INTEGER, events_new INTEGER, warnings_json TEXT, error TEXT
);
"#;

const V2: &str = r#"
CREATE INDEX ix_session_resumed ON session(resumed_from_session_id);

CREATE TABLE hourly_model_usage (
  hour TEXT NOT NULL, harness_id TEXT NOT NULL, provider TEXT NOT NULL, model TEXT NOT NULL,
  price_id INTEGER NOT NULL, over_tier INTEGER NOT NULL,
  events INTEGER NOT NULL, input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,
  cache_write_tokens INTEGER NOT NULL, cache_read_tokens INTEGER NOT NULL,
  cost_micros INTEGER NOT NULL,
  PRIMARY KEY(hour, harness_id, provider, model, price_id, over_tier)
) WITHOUT ROWID;

INSERT INTO hourly_model_usage
SELECT substr(e.ts, 1, 13) || ':00:00.000000000Z', e.harness_id, e.provider, e.model,
       COALESCE(e.price_id, 0),
       CASE WHEN mp.long_context_threshold IS NOT NULL
             AND e.input_tokens + e.cache_write_5m_tokens + e.cache_write_1h_tokens
                 + e.cache_read_tokens > mp.long_context_threshold
            THEN 1 ELSE 0 END,
       COUNT(*), SUM(e.input_tokens), SUM(e.output_tokens),
       SUM(e.cache_write_5m_tokens + e.cache_write_1h_tokens), SUM(e.cache_read_tokens),
       SUM(e.cost_micros)
FROM usage_event e LEFT JOIN model_price mp ON mp.id = e.price_id
GROUP BY 1, 2, 3, 4, 5, 6;

CREATE TABLE hourly_session_usage (
  hour TEXT NOT NULL, session_id INTEGER NOT NULL REFERENCES session(id),
  project_id INTEGER NOT NULL REFERENCES project(id),
  events INTEGER NOT NULL, cost_micros INTEGER NOT NULL, last_ts TEXT NOT NULL,
  PRIMARY KEY(hour, session_id, project_id)
) WITHOUT ROWID;

INSERT INTO hourly_session_usage
SELECT substr(ts, 1, 13) || ':00:00.000000000Z', session_id, project_id,
       COUNT(*), SUM(cost_micros), MAX(ts)
FROM usage_event
GROUP BY 1, 2, 3;
"#;

/// Git worktrees seen while they existed, with the main checkout they belong to,
/// so their sessions still resolve after the worktree is deleted.
const V3: &str = r#"
CREATE TABLE worktree (path TEXT PRIMARY KEY, main TEXT NOT NULL) WITHOUT ROWID;
"#;

pub(crate) fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(V1).foreign_key_check(),
        M::up(V2).foreign_key_check(),
        M::up(V3).foreign_key_check(),
    ])
}

fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL;")?;
    Ok(())
}

fn migrate(mut conn: Connection) -> Result<Connection> {
    configure(&conn)?;
    migrations().to_latest(&mut conn)?;
    Ok(conn)
}

pub(crate) fn open(path: &Path) -> Result<Connection> {
    migrate(Connection::open(path)?)
}

pub(crate) fn open_in_memory() -> Result<Connection> {
    migrate(Connection::open_in_memory()?)
}

pub(crate) fn open_read_only(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.execute_batch("PRAGMA query_only = ON;")?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_valid() {
        migrations().validate().unwrap();
    }
}
