use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Error, Result};

/// Opaque per-source read position. The collector stores it as JSON and hands it back
/// unchanged; only the adapter that produced it interprets it. `Cursor::default()` (JSON
/// `null`) means "never read".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Cursor(Value);

impl Cursor {
    pub fn empty() -> Cursor {
        Cursor(Value::Null)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_null()
    }

    pub fn from_state<T: Serialize>(state: &T) -> Result<Cursor> {
        Ok(Cursor(serde_json::to_value(state)?))
    }

    /// `Ok(None)` for an empty cursor; an error if the stored JSON does not fit `T`.
    pub fn state<T: DeserializeOwned>(&self) -> Result<Option<T>> {
        if self.is_empty() {
            return Ok(None);
        }
        T::deserialize(&self.0)
            .map(Some)
            .map_err(|e| Error::InvalidCursor(e.to_string()))
    }

    pub fn state_or_default<T: DeserializeOwned + Default>(&self) -> Result<T> {
        Ok(self.state()?.unwrap_or_default())
    }

    pub fn from_json(value: Value) -> Cursor {
        Cursor(value)
    }

    pub fn as_json(&self) -> &Value {
        &self.0
    }

    pub fn into_json(self) -> Value {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    struct Watermark {
        last_rowid: i64,
        last_time_updated: i64,
    }

    #[test]
    fn empty_cursor() {
        let c = Cursor::default();
        assert!(c.is_empty());
        assert_eq!(c, Cursor::empty());
        assert_eq!(c.state::<Watermark>().unwrap(), None);
        assert_eq!(
            c.state_or_default::<Watermark>().unwrap(),
            Watermark::default()
        );
        assert_eq!(serde_json::to_string(&c).unwrap(), "null");
    }

    #[test]
    fn state_roundtrip() {
        let w = Watermark {
            last_rowid: 9,
            last_time_updated: 1700,
        };
        let c = Cursor::from_state(&w).unwrap();
        let json = serde_json::to_string(&c).unwrap();
        let back: Cursor = serde_json::from_str(&json).unwrap();
        assert_eq!(back.state::<Watermark>().unwrap(), Some(w));
    }

    #[test]
    fn mismatched_state_is_invalid_cursor() {
        let c = Cursor::from_json(serde_json::json!({"byte_offset": "x"}));
        assert!(matches!(
            c.state::<Watermark>(),
            Err(Error::InvalidCursor(_))
        ));
    }
}
