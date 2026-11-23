use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{Cursor, HarnessId, MessageRecord, ParseWarning, Result, SessionUpsert, UsageEvent};

pub trait HarnessAdapter: Send + Sync {
    fn id(&self) -> HarnessId;
    fn display_name(&self) -> &str;
    /// Default roots; the user can override them in Settings (`harness.roots_json`).
    fn default_roots(&self) -> Vec<PathBuf>;
    /// Lists every readable unit under `roots`. Missing roots are not an error.
    fn discover(&self, roots: &[PathBuf]) -> Result<Vec<SourceRef>>;
    /// Reads what is new in `src` since `cursor` (empty cursor = from the start).
    /// Per-line parse problems go into `ReadBatch::warnings`; `Err` is only for
    /// failures that make the whole source unreadable.
    fn read(&self, src: &SourceRef, cursor: &Cursor) -> Result<ReadBatch>;
}

/// One readable unit: a JSONL file, a JSON file or a SQLite DB.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceRef {
    pub path: PathBuf,
    pub kind: SourceKind,
}

impl SourceRef {
    pub fn new(path: impl Into<PathBuf>, kind: SourceKind) -> SourceRef {
        SourceRef {
            path: path.into(),
            kind,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    JsonlFile,
    JsonFile,
    Sqlite,
}

#[derive(Debug, Clone, Default)]
pub struct ReadBatch {
    pub sessions: Vec<SessionUpsert>,
    pub events: Vec<UsageEvent>,
    pub messages: Vec<MessageRecord>,
    pub next_cursor: Cursor,
    pub warnings: Vec<ParseWarning>,
}

impl ReadBatch {
    /// A batch with no data, e.g. when the source is unchanged since `cursor`.
    pub fn unchanged(cursor: Cursor) -> ReadBatch {
        ReadBatch {
            next_cursor: cursor,
            ..ReadBatch::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
            && self.events.is_empty()
            && self.messages.is_empty()
            && self.warnings.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dummy;

    impl HarnessAdapter for Dummy {
        fn id(&self) -> HarnessId {
            HarnessId::new("dummy")
        }
        fn display_name(&self) -> &str {
            "Dummy"
        }
        fn default_roots(&self) -> Vec<PathBuf> {
            vec![]
        }
        fn discover(&self, roots: &[PathBuf]) -> Result<Vec<SourceRef>> {
            Ok(roots
                .iter()
                .map(|r| SourceRef::new(r, SourceKind::JsonlFile))
                .collect())
        }
        fn read(&self, _src: &SourceRef, cursor: &Cursor) -> Result<ReadBatch> {
            Ok(ReadBatch::unchanged(cursor.clone()))
        }
    }

    #[test]
    fn trait_is_object_safe_and_send_sync() {
        fn assert_send_sync<T: Send + Sync + ?Sized>() {}
        assert_send_sync::<dyn HarnessAdapter>();
        let adapters: Vec<Box<dyn HarnessAdapter>> = vec![Box::new(Dummy)];
        let a = &adapters[0];
        let srcs = a.discover(&[PathBuf::from("/x.jsonl")]).unwrap();
        let batch = a.read(&srcs[0], &Cursor::empty()).unwrap();
        assert!(batch.is_empty());
        assert!(batch.next_cursor.is_empty());
    }
}
