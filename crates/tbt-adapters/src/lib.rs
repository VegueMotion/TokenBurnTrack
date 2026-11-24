pub mod discover;

#[cfg(feature = "claude_code")]
pub mod claude_code;

#[cfg(feature = "claude_code")]
pub use claude_code::ClaudeCodeAdapter;

#[cfg(feature = "codex")]
pub mod codex;

#[cfg(feature = "codex")]
pub use codex::CodexAdapter;

#[cfg(feature = "opencode")]
pub mod opencode;

#[cfg(feature = "opencode")]
pub use opencode::OpenCodeAdapter;

#[cfg(feature = "gemini_cli")]
pub mod gemini_cli;

#[cfg(feature = "gemini_cli")]
pub use gemini_cli::GeminiCliAdapter;

#[cfg(feature = "generic_jsonl")]
pub mod generic_jsonl;

#[cfg(feature = "generic_jsonl")]
pub use generic_jsonl::{GenericJsonlAdapter, GenericJsonlConfig};
