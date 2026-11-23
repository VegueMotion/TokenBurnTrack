use std::path::Path;

use jiff::tz::TimeZone;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, Value, ValueRef};
use tbt_core::{HarnessId, Timestamp};

use crate::api::{Result, StoreError};

pub(crate) const MAX_ACTIVE_GAP_SECS: i128 = 30 * 60;

pub(crate) fn fmt_ts(ts: Timestamp) -> String {
    let dt = TimeZone::UTC.to_datetime(ts);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}Z",
        dt.year(),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second(),
        dt.subsec_nanosecond()
    )
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DbTs(pub Timestamp);

impl ToSql for DbTs {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(fmt_ts(self.0)))
    }
}

impl FromSql for DbTs {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        value
            .as_str()?
            .parse()
            .map(DbTs)
            .map_err(|e| FromSqlError::Other(Box::new(e)))
    }
}

pub(crate) fn ts_opt(v: Option<DbTs>) -> Option<Timestamp> {
    v.map(|t| t.0)
}

pub(crate) fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

pub(crate) fn to_u64(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

pub(crate) fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub(crate) fn time_zone(name: &str) -> Result<TimeZone> {
    TimeZone::get(name).map_err(|e| StoreError::NotFound(format!("time zone {name:?}: {e}")))
}

pub(crate) fn corrupt(what: impl std::fmt::Display) -> StoreError {
    StoreError::Corrupt(what.to_string())
}

#[derive(Default)]
pub(crate) struct Params(pub Vec<Value>);

impl Params {
    pub fn push(&mut self, v: impl Into<Value>) -> String {
        self.0.push(v.into());
        format!("?{}", self.0.len())
    }

    pub fn push_ts(&mut self, ts: Timestamp) -> String {
        self.push(fmt_ts(ts))
    }

    pub fn harness_in(&mut self, col: &str, harnesses: &[HarnessId]) -> String {
        if harnesses.is_empty() {
            return String::new();
        }
        let list: Vec<String> = harnesses
            .iter()
            .map(|h| self.push(h.as_str().to_owned()))
            .collect();
        format!(" AND {col} IN ({})", list.join(", "))
    }

    pub fn iter(&self) -> rusqlite::ParamsFromIter<std::slice::Iter<'_, Value>> {
        rusqlite::params_from_iter(self.0.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_format_sorts_like_time() {
        let a: Timestamp = "2026-11-23T10:15:00Z".parse().unwrap();
        let b: Timestamp = "2026-11-23T10:15:00.5Z".parse().unwrap();
        let c: Timestamp = "2026-11-23T10:15:01Z".parse().unwrap();
        let (fa, fb, fc) = (fmt_ts(a), fmt_ts(b), fmt_ts(c));
        assert_eq!(fa, "2026-11-23T10:15:00.000000000Z");
        assert!(fa < fb && fb < fc);
        assert_eq!(fb.parse::<Timestamp>().unwrap(), b);
    }
}
