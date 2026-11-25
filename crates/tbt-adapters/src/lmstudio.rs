use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tbt_core::{
    Cursor, Error, HarnessAdapter, HarnessId, MessageKind, MessageRecord, MessageRole,
    PREVIEW_MAX_CHARS, ParseWarning, ReadBatch, Result, SessionUpsert, SourceKind, SourceRef,
    Timestamp, TokenCounts, UsageEvent, truncate_preview,
};

use crate::discover;

const HARNESS: HarnessId = HarnessId::LMSTUDIO;
const PROVIDER: &str = "lmstudio";
const SUFFIX: &str = ".conversation.json";
const HOME_POINTER: &str = ".lmstudio-home-pointer";

#[derive(Debug, Clone, Copy, Default)]
pub struct LmStudioAdapter;

impl LmStudioAdapter {
    pub fn new() -> LmStudioAdapter {
        LmStudioAdapter
    }
}

pub fn default_roots_from(home: Option<PathBuf>) -> Vec<PathBuf> {
    let Some(home) = home else {
        return Vec::new();
    };
    let pointed = std::fs::read_to_string(home.join(HOME_POINTER))
        .ok()
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| p.is_absolute());
    vec![
        pointed
            .unwrap_or_else(|| home.join(".lmstudio"))
            .join("conversations"),
    ]
}

impl HarnessAdapter for LmStudioAdapter {
    fn id(&self) -> HarnessId {
        HARNESS
    }

    fn display_name(&self) -> &str {
        "LM Studio"
    }

    fn default_roots(&self) -> Vec<PathBuf> {
        default_roots_from(std::env::home_dir())
    }

    fn discover(&self, roots: &[PathBuf]) -> Result<Vec<SourceRef>> {
        Ok(discover::files_with_extension(roots, "json")
            .into_iter()
            .filter(|p| p.to_string_lossy().ends_with(SUFFIX))
            .map(|p| SourceRef::new(p, SourceKind::JsonFile))
            .collect())
    }

    fn read(&self, src: &SourceRef, cursor: &Cursor) -> Result<ReadBatch> {
        let mut warnings = Vec::new();
        let state = match cursor.state::<State>() {
            Ok(state) => state.unwrap_or_default(),
            Err(e) => {
                warnings.push(warning(src, format!("{e}; reading from the start")));
                State::default()
            }
        };
        let meta = std::fs::metadata(&src.path).map_err(|e| Error::io(&src.path, e))?;
        let next = State {
            size: meta.len(),
            mtime: meta
                .modified()
                .ok()
                .and_then(|m| Timestamp::try_from(m).ok()),
        };
        if next.mtime.is_some() && next == state && warnings.is_empty() {
            return Ok(ReadBatch::unchanged(cursor.clone()));
        }
        let bytes = std::fs::read(&src.path).map_err(|e| Error::io(&src.path, e))?;
        let mut batch = match serde_json::from_slice::<Conversation>(&bytes) {
            Ok(conv) => Reader::new(src, &conv).read(&conv),
            Err(e) => ReadBatch {
                warnings: vec![warning(src, format!("invalid JSON: {e}"))],
                ..ReadBatch::default()
            },
        };
        warnings.append(&mut batch.warnings);
        batch.warnings = warnings;
        batch.next_cursor = Cursor::from_state(&next)?;
        Ok(batch)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct State {
    size: u64,
    mtime: Option<Timestamp>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Conversation {
    name: Option<String>,
    created_at: Option<i64>,
    user_last_messaged_at: Option<i64>,
    assistant_last_messaged_at: Option<i64>,
    last_used_model: Option<ModelRef>,
    messages: Vec<Message>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ModelRef {
    identifier: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Message {
    versions: Vec<Version>,
    currently_selected: usize,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Version {
    role: String,
    content: Vec<Content>,
    steps: Vec<Step>,
    sender_info: Option<SenderInfo>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct SenderInfo {
    sender_name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Step {
    #[serde(rename = "type")]
    kind: String,
    step_identifier: Option<String>,
    content: Vec<Content>,
    style: Option<Style>,
    gen_info: Option<GenInfo>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Content {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    tokens_count: Option<u64>,
    is_structural: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Style {
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct GenInfo {
    identifier: Option<String>,
    indexed_model_identifier: Option<String>,
    stats: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Stats {
    prompt_tokens_count: u64,
    predicted_tokens_count: u64,
}

impl Step {
    fn ts(&self) -> Option<Timestamp> {
        let ms = self.step_identifier.as_deref()?.split('-').next()?;
        let ms: i64 = ms.parse().ok().filter(|ms| *ms > 0)?;
        Timestamp::from_millisecond(ms).ok()
    }

    fn is_thinking(&self) -> bool {
        self.style
            .as_ref()
            .and_then(|s| s.kind.as_deref())
            .is_some_and(|k| k == "thinking")
    }

    fn text(&self) -> Option<String> {
        joined_text(self.content.iter().filter(|c| !c.is_structural))
    }

    fn thinking_tokens(&self) -> u64 {
        self.content
            .iter()
            .filter(|c| !c.is_structural)
            .filter_map(|c| c.tokens_count)
            .sum()
    }
}

impl Message {
    fn selected(&self) -> Option<&Version> {
        self.versions
            .get(self.currently_selected)
            .or(self.versions.last())
    }

    fn first_ts(&self) -> Option<Timestamp> {
        self.versions
            .iter()
            .flat_map(|v| &v.steps)
            .filter_map(Step::ts)
            .min()
    }
}

struct Reader<'a> {
    src: &'a SourceRef,
    session_id: String,
    fallback_ts: Option<Timestamp>,
    fallback_model: Option<String>,
    events: Vec<UsageEvent>,
    messages: Vec<MessageRecord>,
    warnings: Vec<ParseWarning>,
}

impl<'a> Reader<'a> {
    fn new(src: &'a SourceRef, conv: &Conversation) -> Reader<'a> {
        let session_id = src
            .path
            .file_name()
            .map(|n| n.to_string_lossy())
            .and_then(|n| n.strip_suffix(SUFFIX).map(str::to_owned))
            .unwrap_or_default();
        Reader {
            src,
            session_id,
            fallback_ts: millis(conv.assistant_last_messaged_at).or(millis(conv.created_at)),
            fallback_model: conv
                .last_used_model
                .as_ref()
                .and_then(|m| non_empty(m.identifier.as_deref())),
            events: Vec::new(),
            messages: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn read(mut self, conv: &Conversation) -> ReadBatch {
        for (i, message) in conv.messages.iter().enumerate() {
            match message.selected().map(|v| v.role.as_str()) {
                Some("assistant") => self.assistant(message, i + 1 == conv.messages.len()),
                Some("user") => {
                    let ts = conv.messages[i + 1..]
                        .iter()
                        .find_map(Message::first_ts)
                        .or(millis(conv.user_last_messaged_at))
                        .or(millis(conv.created_at));
                    self.user(i, message, ts);
                }
                _ => {}
            }
        }
        let mut session = SessionUpsert::new(HARNESS, self.session_id.clone());
        session.title = non_empty(conv.name.as_deref());
        session.started_at = millis(conv.created_at);
        session.ended_at =
            millis(conv.user_last_messaged_at).max(millis(conv.assistant_last_messaged_at));
        ReadBatch {
            sessions: vec![session],
            events: self.events,
            messages: self.messages,
            next_cursor: Cursor::empty(),
            warnings: self.warnings,
        }
    }

    fn user(&mut self, index: usize, message: &Message, ts: Option<Timestamp>) {
        let Some(version) = message.selected() else {
            return;
        };
        let Some(ts) = ts else {
            return;
        };
        let text: String = version
            .content
            .iter()
            .filter_map(|c| c.text.as_deref())
            .collect();
        self.messages.push(MessageRecord {
            session_ext_id: self.session_id.clone(),
            key: format!("user-{index}-{:016x}", fnv1a(text.as_bytes())),
            ts,
            role: MessageRole::User,
            kind: Some(MessageKind::Text),
            tool_name: None,
            preview: joined_text(version.content.iter()),
            event_dedup_key: None,
        });
    }

    fn assistant(&mut self, message: &Message, is_last: bool) {
        for (vi, version) in message.versions.iter().enumerate() {
            let finished = version
                .steps
                .iter()
                .any(|s| s.gen_info.as_ref().is_some_and(|g| g.stats.is_some()));
            let selected = vi == message.currently_selected && (finished || !is_last);
            let sender = version
                .sender_info
                .as_ref()
                .and_then(|s| non_empty(s.sender_name.as_deref()));
            let mut thinking = 0u64;
            for step in &version.steps {
                if step.kind != "contentBlock" {
                    continue;
                }
                if step.is_thinking() {
                    thinking = thinking.saturating_add(step.thinking_tokens());
                }
                let event_key = step
                    .gen_info
                    .as_ref()
                    .and_then(|g| self.event(step, g, sender.as_deref(), thinking));
                if event_key.is_some() {
                    thinking = 0;
                }
                if selected {
                    self.step_message(step, event_key);
                }
            }
        }
    }

    fn event(
        &mut self,
        step: &Step,
        gen_info: &GenInfo,
        sender: Option<&str>,
        thinking: u64,
    ) -> Option<String> {
        let raw = gen_info.stats.as_ref()?;
        let id = step.step_identifier.clone().filter(|s| !s.is_empty())?;
        let stats = match Stats::deserialize(raw) {
            Ok(s) => s,
            Err(e) => {
                self.warn(format!("step {id}: invalid `stats`: {e}"));
                return None;
            }
        };
        let counts = TokenCounts {
            input: stats.prompt_tokens_count,
            output: stats.predicted_tokens_count,
            reasoning: thinking.min(stats.predicted_tokens_count),
            ..TokenCounts::default()
        };
        if counts.is_zero() {
            return None;
        }
        let Some(model) = non_empty(gen_info.identifier.as_deref())
            .or_else(|| non_empty(gen_info.indexed_model_identifier.as_deref()))
            .or_else(|| non_empty(sender))
            .or_else(|| self.fallback_model.clone())
        else {
            self.warn(format!("step {id} has tokens but no model name; skipped"));
            return None;
        };
        let Some(ts) = step.ts().or(self.fallback_ts) else {
            self.warn(format!("step {id}: no valid timestamp; skipped"));
            return None;
        };
        let mut event = UsageEvent {
            harness: HARNESS,
            dedup_key: id.clone(),
            session_ext_id: self.session_id.clone(),
            ts,
            provider: PROVIDER.to_owned(),
            model,
            input_tokens: 0,
            output_tokens: 0,
            cache_write_5m_tokens: 0,
            cache_write_1h_tokens: 0,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
            reported_cost: None,
            project_path: None,
            git_branch: None,
            raw: serde_json::value::to_raw_value(raw).ok(),
        };
        event.set_tokens(counts);
        self.events.push(event);
        Some(id)
    }

    fn step_message(&mut self, step: &Step, event_key: Option<String>) {
        let Some(key) = step.step_identifier.clone().filter(|s| !s.is_empty()) else {
            return;
        };
        let Some(ts) = step.ts().or(self.fallback_ts) else {
            return;
        };
        let preview = step.text();
        if preview.is_none() && event_key.is_none() {
            return;
        }
        let kind = if step.is_thinking() {
            MessageKind::Thinking
        } else {
            MessageKind::Text
        };
        self.messages.push(MessageRecord {
            session_ext_id: self.session_id.clone(),
            key,
            ts,
            role: MessageRole::Assistant,
            kind: Some(kind),
            tool_name: None,
            preview,
            event_dedup_key: event_key,
        });
    }

    fn warn(&mut self, message: impl Into<String>) {
        self.warnings.push(warning(self.src, message));
    }
}

fn joined_text<'c>(content: impl Iterator<Item = &'c Content>) -> Option<String> {
    let text: Vec<&str> = content
        .filter(|c| c.kind == "text")
        .filter_map(|c| c.text.as_deref())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    (!text.is_empty()).then(|| truncate_preview(&text.join("\n"), PREVIEW_MAX_CHARS))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn millis(ms: Option<i64>) -> Option<Timestamp> {
    ms.filter(|ms| *ms > 0)
        .and_then(|ms| Timestamp::from_millisecond(ms).ok())
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn warning(src: &SourceRef, message: impl Into<String>) -> ParseWarning {
    ParseWarning::new(HARNESS, src, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_root_follows_the_home_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        assert_eq!(
            default_roots_from(Some(home.clone())),
            [home.join(".lmstudio/conversations")]
        );
        std::fs::write(home.join(HOME_POINTER), "/data/lms\n").unwrap();
        assert_eq!(
            default_roots_from(Some(home.clone())),
            [PathBuf::from("/data/lms/conversations")]
        );
        std::fs::write(home.join(HOME_POINTER), "relative").unwrap();
        assert_eq!(
            default_roots_from(Some(home.clone())),
            [home.join(".lmstudio/conversations")]
        );
        assert!(default_roots_from(None).is_empty());
    }

    #[test]
    fn step_identifiers_carry_the_time() {
        let step = Step {
            step_identifier: Some("1766925515470-0.428".into()),
            ..Step::default()
        };
        assert_eq!(step.ts().unwrap().as_millisecond(), 1_766_925_515_470);
        let step = Step {
            step_identifier: Some("abc".into()),
            ..Step::default()
        };
        assert_eq!(step.ts(), None);
    }
}
