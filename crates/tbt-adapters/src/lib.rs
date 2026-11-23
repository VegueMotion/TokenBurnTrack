pub mod discover;

#[cfg(feature = "claude_code")]
pub mod claude_code;

#[cfg(feature = "claude_code")]
pub use claude_code::ClaudeCodeAdapter;
