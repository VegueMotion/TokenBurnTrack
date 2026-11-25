use std::borrow::Cow;
use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HarnessId(Cow<'static, str>);

impl HarnessId {
    pub const ANTIGRAVITY: HarnessId = HarnessId(Cow::Borrowed("antigravity"));
    pub const CLAUDE_CODE: HarnessId = HarnessId(Cow::Borrowed("claude_code"));
    pub const CODEX: HarnessId = HarnessId(Cow::Borrowed("codex"));
    pub const COPILOT_CLI: HarnessId = HarnessId(Cow::Borrowed("copilot_cli"));
    pub const CURSOR: HarnessId = HarnessId(Cow::Borrowed("cursor"));
    pub const GEMINI_CLI: HarnessId = HarnessId(Cow::Borrowed("gemini_cli"));
    pub const GROK_BUILD: HarnessId = HarnessId(Cow::Borrowed("grok_build"));
    pub const LMSTUDIO: HarnessId = HarnessId(Cow::Borrowed("lmstudio"));
    pub const OPENCODE: HarnessId = HarnessId(Cow::Borrowed("opencode"));

    pub fn new(id: impl Into<String>) -> Self {
        HarnessId(Cow::Owned(id.into()))
    }

    pub const fn from_static(id: &'static str) -> Self {
        HarnessId(Cow::Borrowed(id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HarnessId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for HarnessId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<&str> for HarnessId {
    fn from(s: &str) -> Self {
        HarnessId::new(s)
    }
}

impl From<String> for HarnessId {
    fn from(s: String) -> Self {
        HarnessId::new(s)
    }
}

macro_rules! row_id {
    ($($(#[$meta:meta])* $name:ident),* $(,)?) => {$(
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub i64);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    )*};
}

row_id!(SessionId, EventId, PriceId, ProjectId, SourceFileId, RunId);

impl ProjectId {
    pub const NONE: ProjectId = ProjectId(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_id_consts_equal_owned() {
        assert_eq!(HarnessId::CLAUDE_CODE, HarnessId::new("claude_code"));
        assert_eq!(HarnessId::CODEX.as_str(), "codex");
        assert_eq!(HarnessId::GEMINI_CLI.to_string(), "gemini_cli");
        assert_eq!(HarnessId::OPENCODE, HarnessId::from("opencode"));
    }

    #[test]
    fn harness_id_serde_is_plain_string() {
        let json = serde_json::to_string(&HarnessId::CODEX).unwrap();
        assert_eq!(json, "\"codex\"");
        let back: HarnessId = serde_json::from_str("\"my_tool\"").unwrap();
        assert_eq!(back, HarnessId::new("my_tool"));
    }

    #[test]
    fn row_ids_are_transparent() {
        assert_eq!(serde_json::to_string(&SessionId(7)).unwrap(), "7");
        assert_eq!(ProjectId::NONE, ProjectId(0));
    }
}
