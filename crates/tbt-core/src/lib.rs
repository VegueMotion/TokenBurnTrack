mod adapter;
mod cursor;
mod error;
mod event;
mod ids;
mod money;
mod price;
mod privacy;
mod time;
mod tokens;

pub mod jsonl;
pub mod sandbox;

pub const APP_ID: &str = "io.VagueMotion.TokenBurnTrack";

pub use adapter::{HarnessAdapter, ReadBatch, SourceKind, SourceRef};
pub use cursor::Cursor;
pub use error::{Error, Result};
pub use event::{
    BaselineHint, MessageKind, MessageRecord, MessageRole, ParseWarning, PricedEvent,
    SessionUpsert, UsageEvent,
};
pub use ids::{EventId, HarnessId, PriceId, ProjectId, RunId, SessionId, SourceFileId};
pub use money::Micros;
pub use price::{LongContextTier, ModelPrice, PriceSource};
pub use privacy::{PREVIEW_MAX_CHARS, PrivacyLevel, truncate_preview};
pub use time::{Hour, TimeRange};
pub use tokens::TokenCounts;

pub use jiff::Timestamp;
pub use rust_decimal::Decimal;
pub use serde_json::value::RawValue;
