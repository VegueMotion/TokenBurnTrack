pub mod discover;

#[cfg(any(feature = "opencode", feature = "cursor"))]
mod sqlite;

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

#[cfg(feature = "cursor")]
pub mod cursor;

#[cfg(feature = "cursor")]
pub use cursor::CursorAdapter;

#[cfg(any(feature = "cursor", feature = "copilot_cli"))]
mod models;

#[cfg(feature = "copilot_cli")]
pub mod copilot_cli;

#[cfg(feature = "copilot_cli")]
pub use copilot_cli::CopilotCliAdapter;

#[cfg(feature = "gemini_cli")]
pub mod gemini_cli;

#[cfg(feature = "gemini_cli")]
pub use gemini_cli::GeminiCliAdapter;

#[cfg(feature = "grok_build")]
pub mod grok_build;

#[cfg(feature = "grok_build")]
pub use grok_build::GrokBuildAdapter;

#[cfg(feature = "generic_jsonl")]
pub mod generic_jsonl;

#[cfg(feature = "generic_jsonl")]
pub use generic_jsonl::{GenericJsonlAdapter, GenericJsonlConfig};
