use gpui::{Hsla, Rgba, rgb};
use tbt_core::HarnessId;

pub const CATEGORICAL: [u32; 8] = [
    0x3987e5, 0xd95926, 0x199e70, 0xc98500, 0xd55181, 0x0ca30c, 0x9085e9, 0xe66767,
];

const HEAT_RAMP: [u32; 4] = [0x184f95, 0x2a78d6, 0x6da7ec, 0xb7d3f6];

pub fn categorical(slot: usize) -> Hsla {
    rgb(CATEGORICAL[slot % CATEGORICAL.len()]).into()
}

pub fn harness_slot(id: &HarnessId) -> usize {
    match id.as_str() {
        "claude_code" => 1,
        "codex" => 0,
        "gemini_cli" => 2,
        "opencode" => 3,
        "grok_build" => 4,
        other => 5 + (fnv1a(other) % 3) as usize,
    }
}

pub fn harness_color(id: &HarnessId) -> Hsla {
    categorical(harness_slot(id))
}

pub fn harness_label(id: &HarnessId) -> String {
    match id.as_str() {
        "claude_code" => "Claude Code".to_owned(),
        "codex" => "Codex CLI".to_owned(),
        "gemini_cli" => "Gemini CLI".to_owned(),
        "opencode" => "OpenCode".to_owned(),
        "cursor" => "Cursor".to_owned(),
        "copilot_cli" => "Copilot CLI".to_owned(),
        "grok_build" => "Grok Build".to_owned(),
        other => other.to_owned(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Input,
    Output,
    CacheWrite,
    CacheRead,
}

impl TokenKind {
    pub const ALL: [TokenKind; 4] = [
        TokenKind::Input,
        TokenKind::Output,
        TokenKind::CacheWrite,
        TokenKind::CacheRead,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TokenKind::Input => "Input",
            TokenKind::Output => "Output",
            TokenKind::CacheWrite => "Cache write",
            TokenKind::CacheRead => "Cache read",
        }
    }

    pub fn color(self) -> Hsla {
        categorical(self as usize)
    }

    pub fn of(self, t: &tbt_store::TokenSums) -> u64 {
        match self {
            TokenKind::Input => t.input,
            TokenKind::Output => t.output,
            TokenKind::CacheWrite => t.cache_write,
            TokenKind::CacheRead => t.cache_read,
        }
    }
}

pub fn heat_color(t: f32) -> Hsla {
    let t = t.clamp(0.0, 1.0) * (HEAT_RAMP.len() - 1) as f32;
    let i = (t.floor() as usize).min(HEAT_RAMP.len() - 2);
    let f = t - i as f32;
    let (a, b) = (rgb(HEAT_RAMP[i]), rgb(HEAT_RAMP[i + 1]));
    Rgba {
        r: a.r + (b.r - a.r) * f,
        g: a.g + (b.g - a.g) * f,
        b: a.b + (b.b - a.b) * f,
        a: 1.0,
    }
    .into()
}

fn fnv1a(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_harnesses_have_distinct_fixed_colors() {
        let ids = [
            HarnessId::CLAUDE_CODE,
            HarnessId::CODEX,
            HarnessId::GEMINI_CLI,
            HarnessId::OPENCODE,
            HarnessId::GROK_BUILD,
        ];
        let slots: Vec<usize> = ids.iter().map(harness_slot).collect();
        assert_eq!(slots, vec![1, 0, 2, 3, 4]);
        assert_eq!(
            harness_color(&HarnessId::new("claude_code")),
            harness_color(&HarnessId::CLAUDE_CODE)
        );
    }

    #[test]
    fn custom_harness_color_is_stable_and_outside_builtin_slots() {
        let a = harness_slot(&HarnessId::new("my_tool"));
        assert_eq!(a, harness_slot(&HarnessId::new("my_tool")));
        assert!((5..8).contains(&a));
    }

    #[test]
    fn heat_ramp_endpoints() {
        assert_eq!(heat_color(0.0), rgb(HEAT_RAMP[0]).into());
        let top: Hsla = rgb(HEAT_RAMP[3]).into();
        assert!((heat_color(1.0).l - top.l).abs() < 1e-4);
        assert!(heat_color(1.0).l > heat_color(0.5).l);
        assert_eq!(heat_color(7.0), heat_color(1.0));
    }
}
