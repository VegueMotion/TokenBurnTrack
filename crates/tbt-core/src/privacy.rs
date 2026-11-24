use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::MessageRecord;
use crate::event::str_enum;

pub const PREVIEW_MAX_CHARS: usize = 280;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyLevel {
    Off,
    #[default]
    MetadataOnly,
    TruncatedPreview,
}

str_enum!(PrivacyLevel {
    Off => "off",
    MetadataOnly => "metadata_only",
    TruncatedPreview => "truncated_preview",
});

impl PrivacyLevel {
    pub fn apply(self, mut messages: Vec<MessageRecord>) -> Vec<MessageRecord> {
        match self {
            PrivacyLevel::Off => Vec::new(),
            PrivacyLevel::MetadataOnly => {
                for m in &mut messages {
                    m.preview = None;
                }
                messages
            }
            PrivacyLevel::TruncatedPreview => {
                for m in &mut messages {
                    if let Some(p) = m.preview.take() {
                        m.preview = Some(truncate_preview(&p, PREVIEW_MAX_CHARS));
                    }
                }
                messages
            }
        }
    }
}

pub fn truncate_preview(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    if max_chars == 0 {
        return String::new();
    }
    let mut out: String = text.chars().take(max_chars - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MessageRole;

    fn msg(preview: Option<&str>) -> MessageRecord {
        MessageRecord {
            session_ext_id: "s".into(),
            key: "k".into(),
            ts: "2026-11-23T10:00:00Z".parse().unwrap(),
            role: MessageRole::User,
            kind: None,
            tool_name: None,
            preview: preview.map(Into::into),
            event_dedup_key: None,
        }
    }

    #[test]
    fn default_is_metadata_only() {
        assert_eq!(PrivacyLevel::default(), PrivacyLevel::MetadataOnly);
        assert_eq!(
            serde_json::to_string(&PrivacyLevel::TruncatedPreview).unwrap(),
            "\"truncated_preview\""
        );
        assert_eq!("off".parse::<PrivacyLevel>(), Ok(PrivacyLevel::Off));
    }

    #[test]
    fn apply_levels() {
        let long = "é".repeat(PREVIEW_MAX_CHARS + 10);
        let msgs = vec![msg(Some("hi")), msg(Some(&long)), msg(None)];
        assert!(PrivacyLevel::Off.apply(msgs.clone()).is_empty());

        let meta = PrivacyLevel::MetadataOnly.apply(msgs.clone());
        assert_eq!(meta.len(), 3);
        assert!(meta.iter().all(|m| m.preview.is_none()));

        let prev = PrivacyLevel::TruncatedPreview.apply(msgs);
        assert_eq!(prev[0].preview.as_deref(), Some("hi"));
        let cut = prev[1].preview.as_deref().unwrap();
        assert_eq!(cut.chars().count(), PREVIEW_MAX_CHARS);
        assert!(cut.ends_with('…'));
        assert!(prev[2].preview.is_none());
    }

    #[test]
    fn truncate_edge_cases() {
        assert_eq!(truncate_preview("abc", 3), "abc");
        assert_eq!(truncate_preview("abcd", 3), "ab…");
        assert_eq!(truncate_preview("abcd", 0), "");
    }
}
