pub mod discover;

#[cfg(feature = "claude_code")]
pub mod claude_code;

#[cfg(feature = "claude_code")]
pub use claude_code::ClaudeCodeAdapter;

#[cfg(feature = "codex")]
pub mod codex;

#[cfg(feature = "opencode")]
pub mod opencode;

#[cfg(feature = "gemini_cli")]
pub mod gemini_cli;

#[cfg(feature = "generic_jsonl")]
pub mod generic_jsonl;
