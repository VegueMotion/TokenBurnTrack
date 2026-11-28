use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{
    AnyElement, Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Task,
    Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{InputEvent, Textarea, TextareaState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme, Disableable, Sizable, StyledExt, h_flex, v_flex};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use tbt_collector::{CollectorConfig, builtin_adapters};
use tbt_core::{HarnessId, ParseWarning};
use tbt_store::{HarnessConfig, HarnessStatus, IngestRunSummary, Store, StoreError};

use super::{Loadable, spawn_load};
use crate::app_state::{AppState, AppStateEvent, relative_time};
use crate::charts::format::{fmt_count, fmt_duration};
use crate::data::{self, DataSource};
use crate::palette::harness_color;
use crate::ui::{self, Col};

const RUN_LIMIT: usize = 20;
const READ_ONLY_TIP: &str = "Mock data is read-only; start the app with a data directory to edit";
const SAVED_NOTICE: &str = "Saved. The change takes effect on the next sync.";

pub fn default_roots() -> HashMap<HarnessId, Vec<PathBuf>> {
    let config = CollectorConfig::load_default().unwrap_or_default();
    builtin_adapters(&config)
        .iter()
        .map(|a| (a.id(), a.default_roots()))
        .collect()
}

pub fn effective_roots(config: &HarnessConfig, defaults: &[PathBuf]) -> (Vec<PathBuf>, bool) {
    match &config.roots {
        Some(custom) => (custom.clone(), true),
        None => (defaults.to_vec(), false),
    }
}

pub fn parse_roots(text: &str, home: Option<&Path>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let path = match (line, home) {
            ("~", Some(h)) => h.to_path_buf(),
            (l, Some(h)) if l.starts_with("~/") => h.join(&l[2..]),
            (l, _) => PathBuf::from(l),
        };
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

pub fn roots_text(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootIssue {
    Empty,
    Relative(PathBuf),
    Missing(PathBuf),
}

impl RootIssue {
    pub fn message(&self) -> String {
        match self {
            RootIssue::Empty => "Enter at least one path, or reset to the defaults".to_owned(),
            RootIssue::Relative(p) => format!("{} is not an absolute path", p.display()),
            RootIssue::Missing(p) => match tbt_core::sandbox::override_command(p) {
                Some(cmd) => format!(
                    "{} does not exist (yet) or is outside the Flatpak sandbox; to grant access run: {cmd}",
                    p.display()
                ),
                None => format!("{} does not exist (yet)", p.display()),
            },
        }
    }

    pub fn blocks_save(&self) -> bool {
        matches!(self, RootIssue::Empty)
    }
}

pub fn check_roots(roots: &[PathBuf]) -> Vec<RootIssue> {
    if roots.is_empty() {
        return vec![RootIssue::Empty];
    }
    roots
        .iter()
        .filter_map(|p| {
            if !p.is_absolute() {
                Some(RootIssue::Relative(p.clone()))
            } else if !p.exists() {
                Some(RootIssue::Missing(p.clone()))
            } else {
                None
            }
        })
        .collect()
}

pub fn is_error(w: &ParseWarning) -> bool {
    w.message.trim_start().starts_with("error:")
}

pub fn warning_text(w: &ParseWarning) -> String {
    match w.line {
        Some(line) => format!("{}:{line} — {}", w.source, w.message),
        None => format!("{} — {}", w.source, w.message),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunCounts {
    pub warnings: usize,
    pub errors: usize,
}

pub fn run_counts(run: &IngestRunSummary) -> RunCounts {
    let errors = run.warnings.iter().filter(|w| is_error(w)).count();
    RunCounts {
        warnings: run.warnings.len() - errors,
        errors: errors + usize::from(run.error.is_some()),
    }
}

pub fn run_duration(run: &IngestRunSummary) -> String {
    match run.finished_at {
        Some(end) => fmt_duration((end.as_second() - run.started_at.as_second()).max(0) as u64),
        None => "running".to_owned(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessEdit {
    Enabled(bool),
    Roots(Option<Vec<PathBuf>>),
}

pub fn apply_edit(db: &Path, id: &HarnessId, edit: &HarnessEdit) -> tbt_store::Result<()> {
    if !db.exists() {
        return Err(StoreError::NotFound(db.display().to_string()));
    }
    let mut store = Store::open(db)?;
    match edit {
        HarnessEdit::Enabled(on) => store.set_harness_enabled(id, *on),
        HarnessEdit::Roots(roots) => store.set_harness_roots(id, roots.as_deref()),
    }
}

pub struct HarnessCard {
    pub status: HarnessStatus,
    pub defaults: Vec<PathBuf>,
    pub roots: Vec<(PathBuf, bool)>,
    pub custom: bool,
}

pub struct HarnessesData {
    pub cards: Vec<HarnessCard>,
    pub runs: Vec<IngestRunSummary>,
}

impl HarnessesData {
    pub fn load(data: &dyn DataSource) -> anyhow::Result<HarnessesData> {
        let defaults = default_roots();
        let cards = data
            .harness_status()?
            .into_iter()
            .map(|status| {
                let defaults = defaults.get(&status.config.id).cloned().unwrap_or_default();
                let (roots, custom) = effective_roots(&status.config, &defaults);
                HarnessCard {
                    roots: roots
                        .into_iter()
                        .map(|p| {
                            let exists = p.exists();
                            (p, exists)
                        })
                        .collect(),
                    custom,
                    defaults,
                    status,
                }
            })
            .collect();
        Ok(HarnessesData {
            cards,
            runs: data.ingest_runs(RUN_LIMIT)?,
        })
    }
}

struct RootsEditor {
    id: HarnessId,
    input: Entity<TextareaState>,
    issues: Vec<RootIssue>,
    _subscription: Subscription,
}

struct Notice {
    text: String,
    error: bool,
}

pub struct HarnessesPage {
    app_state: Entity<AppState>,
    data: Arc<dyn DataSource>,
    state: Loadable<HarnessesData>,
    editor: Option<RootsEditor>,
    busy: Option<HarnessId>,
    notice: Option<Notice>,
    _load: Task<()>,
    _write: Task<()>,
    _subscription: Subscription,
}

fn home() -> Option<PathBuf> {
    std::env::home_dir()
}

fn local_time(ts: Timestamp, tz: &TimeZone) -> String {
    ts.to_zoned_datetime(tz.clone())
        .strftime("%b %-d, %H:%M")
        .to_string()
}

impl HarnessesPage {
    pub fn new(
        app_state: Entity<AppState>,
        data: Arc<dyn DataSource>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.subscribe(&app_state, |this, _, ev: &AppStateEvent, cx| {
            if *ev == AppStateEvent::DataChanged {
                this.reload(cx);
            }
        });
        let mut page = HarnessesPage {
            app_state,
            data,
            state: Loadable::Loading,
            editor: None,
            busy: None,
            notice: None,
            _load: Task::ready(()),
            _write: Task::ready(()),
            _subscription: subscription,
        };
        page.reload(cx);
        page
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let data = self.data.clone();
        self._load = spawn_load(
            cx,
            move || HarnessesData::load(&*data),
            |this, state, _| this.state = state,
        );
    }

    fn read_only(&self) -> bool {
        self.data.db_path().is_none()
    }

    fn apply(&mut self, id: HarnessId, edit: HarnessEdit, cx: &mut Context<Self>) {
        let Some(db) = self.data.db_path() else {
            return;
        };
        if let (HarnessEdit::Enabled(on), Loadable::Ready(d)) = (&edit, &mut self.state)
            && let Some(card) = d.cards.iter_mut().find(|c| c.status.config.id == id)
        {
            card.status.config.enabled = *on;
        }
        self.busy = Some(id.clone());
        self.notice = None;
        cx.notify();
        let app_state = self.app_state.clone();
        self._write = cx.spawn(async move |this, cx| {
            let job = edit.clone();
            let result = cx
                .background_executor()
                .spawn(async move { apply_edit(&db, &id, &job) })
                .await;
            this.update(cx, |this, cx| {
                this.busy = None;
                this.notice = Some(match result {
                    Ok(()) => {
                        if matches!(edit, HarnessEdit::Roots(_)) {
                            this.editor = None;
                        }
                        Notice {
                            text: SAVED_NOTICE.to_owned(),
                            error: false,
                        }
                    }
                    Err(e) => Notice {
                        text: format!("Could not save: {e}"),
                        error: true,
                    },
                });
                app_state.update(cx, |_, cx| cx.emit(AppStateEvent::DataChanged));
                cx.notify();
            })
            .ok();
        });
    }

    fn open_editor(
        &mut self,
        id: HarnessId,
        roots: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 10)
                .placeholder("One path per line")
                .default_value(roots_text(&roots))
        });
        let subscription = cx.subscribe(&input, |this, input, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change)
                && let Some(editor) = &mut this.editor
            {
                let text = input.read(cx).value();
                editor.issues = check_roots(&parse_roots(&text, home().as_deref()));
                cx.notify();
            }
        });
        self.editor = Some(RootsEditor {
            id,
            input,
            issues: check_roots(&roots),
            _subscription: subscription,
        });
        self.notice = None;
        cx.notify();
    }

    fn save_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &self.editor else {
            return;
        };
        let roots = parse_roots(&editor.input.read(cx).value(), home().as_deref());
        if check_roots(&roots).iter().any(RootIssue::blocks_save) {
            return;
        }
        let id = editor.id.clone();
        self.apply(id, HarnessEdit::Roots(Some(roots)), cx);
    }

    fn render_roots(&self, ix: usize, card: &HarnessCard, cx: &Context<Self>) -> AnyElement {
        let t = cx.theme();
        let (fg, muted, warn, danger) = (t.foreground, t.muted_foreground, t.warning, t.danger);
        let id = card.status.config.id.clone();
        let read_only = self.read_only();
        let busy = self.busy.as_ref() == Some(&id);
        let heading = h_flex()
            .gap_2()
            .items_center()
            .text_xs()
            .text_color(muted)
            .child("Roots")
            .child(
                div()
                    .px_1p5()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(t.border)
                    .child(if card.custom { "custom" } else { "default" }),
            );

        if let Some(editor) = self.editor.as_ref().filter(|e| e.id == id) {
            let blocked = editor.issues.iter().any(RootIssue::blocks_save);
            return v_flex()
                .gap_2()
                .child(heading)
                .child(Textarea::new(&editor.input).disabled(busy))
                .children(editor.issues.iter().map(|issue| {
                    div()
                        .text_xs()
                        .text_color(if issue.blocks_save() { danger } else { warn })
                        .child(format!("⚠ {}", issue.message()))
                }))
                .when(!editor.issues.is_empty() && !blocked, |d| {
                    d.child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("Paths that do not exist yet can still be saved."),
                    )
                })
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new(("roots-save", ix))
                                .small()
                                .primary()
                                .label("Save")
                                .loading(busy)
                                .disabled(blocked || busy)
                                .on_click(cx.listener(|this, _, _, cx| this.save_editor(cx))),
                        )
                        .child(
                            Button::new(("roots-cancel", ix))
                                .small()
                                .ghost()
                                .label("Cancel")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.editor = None;
                                    cx.notify();
                                })),
                        ),
                )
                .into_any_element();
        }

        let edit_roots: Vec<PathBuf> = card.roots.iter().map(|(p, _)| p.clone()).collect();
        let (edit_id, reset_id) = (id.clone(), id.clone());
        v_flex()
            .gap_1()
            .child(heading)
            .when(card.roots.is_empty(), |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(warn)
                        .child("No roots: this harness discovers nothing"),
                )
            })
            .children(card.roots.iter().map(|(path, exists)| {
                h_flex()
                    .gap_2()
                    .text_xs()
                    .child(
                        div()
                            .w(px(14.))
                            .text_color(if *exists { muted } else { warn })
                            .child(if *exists { "✓" } else { "⚠" }),
                    )
                    .child(
                        div()
                            .font_family("monospace")
                            .text_color(fg)
                            .child(path.to_string_lossy().into_owned()),
                    )
                    .when(!*exists, |d| {
                        d.child(
                            div()
                                .text_color(warn)
                                .child(if tbt_core::sandbox::in_flatpak() {
                                    "not found (or outside the Flatpak sandbox)"
                                } else {
                                    "not found"
                                }),
                        )
                    })
            }))
            .child(
                h_flex()
                    .pt_1()
                    .gap_2()
                    .child(
                        Button::new(("roots-edit", ix))
                            .xsmall()
                            .outline()
                            .label("Edit roots")
                            .disabled(read_only || busy)
                            .when(read_only, |b| b.tooltip(READ_ONLY_TIP))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_editor(edit_id.clone(), edit_roots.clone(), window, cx)
                            })),
                    )
                    .child(
                        Button::new(("roots-reset", ix))
                            .xsmall()
                            .ghost()
                            .label("Reset to default")
                            .disabled(read_only || busy || !card.custom)
                            .tooltip(if read_only {
                                READ_ONLY_TIP.to_owned()
                            } else {
                                format!("Default: {}", roots_text(&card.defaults))
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.apply(reset_id.clone(), HarnessEdit::Roots(None), cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_card(&self, ix: usize, card: &HarnessCard, cx: &Context<Self>) -> AnyElement {
        let h = &card.status;
        let tz = self.app_state.read(cx).tz();
        let now = data::now();
        let read_only = self.read_only();
        let id = h.config.id.clone();
        let busy = self.busy.as_ref() == Some(&id);
        let last = h
            .last_read
            .map(|t| format!("{} ({})", relative_time(now, t), local_time(t, &tz)))
            .unwrap_or_else(|| "never".into());
        let toggle = Switch::new(("harness-enabled", ix))
            .checked(h.config.enabled)
            .label(if h.config.enabled {
                "Enabled"
            } else {
                "Disabled"
            })
            .disabled(read_only || busy)
            .when(read_only, |s| s.tooltip(READ_ONLY_TIP))
            .on_click(cx.listener(move |this, on: &bool, _, cx| {
                this.apply(id.clone(), HarnessEdit::Enabled(*on), cx)
            }));
        let roots = self.render_roots(ix, card, cx);
        let t = cx.theme();
        let (fg, muted, warn, danger, border) = (
            t.foreground,
            t.muted_foreground,
            t.warning,
            t.danger,
            t.border,
        );
        let stat = |label: &'static str, value: String| {
            v_flex()
                .min_w(px(96.))
                .child(div().text_xs().text_color(muted).child(label))
                .child(div().text_sm().text_color(fg).child(value))
        };
        let errors = h.last_warnings.iter().filter(|w| is_error(w)).count();
        let warnings =
            v_flex()
                .gap_1()
                .child(div().text_xs().text_color(muted).child(
                    match (h.last_warnings.len(), errors) {
                        (0, _) => "No warnings in the latest run".to_owned(),
                        (n, 0) => format!("Warnings in the latest run ({n})"),
                        (n, e) => format!("Warnings in the latest run ({n}, {e} errors)"),
                    },
                ))
                .when(!h.last_warnings.is_empty(), |d| {
                    d.child(
                        div()
                            .max_h(px(140.))
                            .overflow_y_scrollbar()
                            .id(("warnings", ix))
                            .rounded_md()
                            .border_1()
                            .border_color(border)
                            .p_2()
                            .children(h.last_warnings.iter().map(|w| {
                                let error = is_error(w);
                                div()
                                    .text_xs()
                                    .font_family("monospace")
                                    .text_color(if error { danger } else { warn })
                                    .when(error, |d| d.font_semibold())
                                    .child(warning_text(w))
                            })),
                    )
                });
        v_flex()
            .gap_3()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(border)
            .bg(t.secondary.opacity(0.35))
            .when(!h.config.enabled, |d| d.opacity(0.75))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(ui::swatch(harness_color(&h.config.id)))
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_semibold()
                            .text_color(fg)
                            .child(h.config.display_name.clone()),
                    )
                    .child(toggle),
            )
            .child(
                h_flex()
                    .gap_6()
                    .flex_wrap()
                    .child(stat("Last ingest", last))
                    .child(stat("Files tracked", fmt_count(h.files_tracked)))
                    .child(stat("Files missing", fmt_count(h.files_missing)))
                    .child(stat("Calls", fmt_count(h.events)))
                    .child(stat("Sessions", fmt_count(h.sessions))),
            )
            .child(roots)
            .child(warnings)
            .into_any_element()
    }

    fn render_runs(&self, runs: &[IngestRunSummary], cx: &Context<Self>) -> AnyElement {
        let tz = self.app_state.read(cx).tz();
        let t = cx.theme();
        let (muted, warn, danger) = (t.muted_foreground, t.warning, t.danger);
        let cols = [
            Col::fixed("Started", 130.0),
            Col::fixed("Trigger", 80.0),
            Col::num("Duration", 70.0),
            Col::num("Sources", 70.0),
            Col::num("New events", 90.0),
            Col::num("Warnings", 80.0),
            Col::num("Errors", 60.0),
            Col::flex("Result"),
        ];
        let rows = runs
            .iter()
            .map(|r| {
                let counts = run_counts(r);
                let count_cell = |n: usize, color| {
                    div()
                        .when(n > 0, |d| d.text_color(color))
                        .when(n == 0, |d| d.text_color(muted))
                        .child(n.to_string())
                        .into_any_element()
                };
                let result = match (&r.error, r.finished_at) {
                    (Some(e), _) => div().text_color(danger).child(e.clone()),
                    (None, None) => div().text_color(muted).child("running"),
                    (None, Some(_)) => div().text_color(muted).child("ok"),
                };
                vec![
                    local_time(r.started_at, &tz).into_any_element(),
                    format!("{:?}", r.trigger).into_any_element(),
                    run_duration(r).into_any_element(),
                    fmt_count(r.stats.sources_scanned).into_any_element(),
                    fmt_count(r.stats.events_new).into_any_element(),
                    count_cell(counts.warnings, warn),
                    count_cell(counts.errors, danger),
                    result.into_any_element(),
                ]
            })
            .collect();
        ui::card(format!("Recent ingest runs (last {RUN_LIMIT})"), cx)
            .when(runs.is_empty(), |d| {
                d.child(ui::muted_text("No runs yet", cx))
            })
            .when(!runs.is_empty(), |d| d.child(ui::table(&cols, rows, cx)))
            .into_any_element()
    }

    fn render_ready(&self, d: &HarnessesData, cx: &Context<Self>) -> AnyElement {
        let t = cx.theme();
        let (muted, success, danger) = (t.muted_foreground, t.success, t.danger);
        let header = h_flex()
            .gap_3()
            .items_center()
            .text_xs()
            .child(div().flex_1().text_color(muted).child(if self.read_only() {
                READ_ONLY_TIP.to_owned()
            } else {
                "Changes are saved to the database and apply on the next sync.".to_owned()
            }))
            .children(self.notice.as_ref().map(|n| {
                div()
                    .text_color(if n.error { danger } else { success })
                    .child(n.text.clone())
            }));
        let cards: Vec<AnyElement> = d
            .cards
            .iter()
            .enumerate()
            .map(|(ix, card)| self.render_card(ix, card, cx))
            .collect();
        v_flex()
            .gap_3()
            .p_4()
            .child(header)
            .when(cards.is_empty(), |el| {
                el.child(ui::muted_text(
                    "No harnesses registered yet — run Sync once to register them",
                    cx,
                ))
            })
            .children(cards)
            .child(self.render_runs(&d.runs, cx))
            .into_any_element()
    }
}

impl Render for HarnessesPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.state {
            Loadable::Loading => ui::status_message("Loading…", cx),
            Loadable::Failed(e) => ui::status_message(format!("Could not load: {e}"), cx),
            Loadable::Ready(d) => self.render_ready(d, cx),
        };
        ui::page_scroll("harnesses-scroll", body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tbt_core::RunId;
    use tbt_store::{RunStats, RunTrigger};

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn warning(message: &str, line: Option<u64>) -> ParseWarning {
        ParseWarning {
            harness: HarnessId::CODEX,
            source: "/logs/a.jsonl".into(),
            line,
            byte_offset: None,
            message: message.into(),
        }
    }

    #[test]
    fn parse_roots_trims_dedupes_and_expands_home() {
        let home = Path::new("/home/u");
        let text = "  /a/b  \n\n~/.claude\n/a/b\n~\nrel/x\r\n";
        assert_eq!(
            parse_roots(text, Some(home)),
            vec![
                PathBuf::from("/a/b"),
                PathBuf::from("/home/u/.claude"),
                PathBuf::from("/home/u"),
                PathBuf::from("rel/x"),
            ]
        );
        assert_eq!(parse_roots("~/x", None), vec![PathBuf::from("~/x")]);
        assert!(parse_roots(" \n\t\n", Some(home)).is_empty());
    }

    #[test]
    fn roots_text_round_trips() {
        let roots = vec![PathBuf::from("/a"), PathBuf::from("/b c/d")];
        assert_eq!(roots_text(&roots), "/a\n/b c/d");
        assert_eq!(parse_roots(&roots_text(&roots), None), roots);
    }

    #[test]
    fn check_roots_warns_but_only_empty_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().to_path_buf();
        let missing = dir.path().join("nope");
        let issues = check_roots(&[existing, missing.clone(), PathBuf::from("rel")]);
        assert_eq!(
            issues,
            vec![
                RootIssue::Missing(missing),
                RootIssue::Relative(PathBuf::from("rel"))
            ]
        );
        assert!(!issues.iter().any(RootIssue::blocks_save));
        assert_eq!(check_roots(&[]), vec![RootIssue::Empty]);
        assert!(RootIssue::Empty.blocks_save());
        assert!(check_roots(&[dir.path().to_path_buf()]).is_empty());
    }

    #[test]
    fn effective_roots_prefer_custom() {
        let mut config = HarnessConfig {
            id: HarnessId::CODEX,
            display_name: "Codex".into(),
            enabled: true,
            roots: None,
        };
        let defaults = vec![PathBuf::from("/d")];
        assert_eq!(
            effective_roots(&config, &defaults),
            (defaults.clone(), false)
        );
        config.roots = Some(vec![PathBuf::from("/c")]);
        assert_eq!(
            effective_roots(&config, &defaults),
            (vec![PathBuf::from("/c")], true)
        );
        config.roots = Some(vec![]);
        assert_eq!(effective_roots(&config, &defaults), (vec![], true));
    }

    #[test]
    fn default_roots_cover_builtin_adapters() {
        let d = default_roots();
        for id in [
            HarnessId::CLAUDE_CODE,
            HarnessId::CODEX,
            HarnessId::OPENCODE,
        ] {
            assert!(d.contains_key(&id), "{id}");
        }
    }

    #[test]
    fn warnings_and_errors() {
        let w = warning("unknown field", Some(12));
        let e = warning("error: cannot read file", None);
        assert!(!is_error(&w));
        assert!(is_error(&e));
        assert_eq!(warning_text(&w), "/logs/a.jsonl:12 — unknown field");
        assert_eq!(warning_text(&e), "/logs/a.jsonl — error: cannot read file");
    }

    #[test]
    fn run_summary_cells() {
        let mut run = IngestRunSummary {
            id: RunId(1),
            trigger: RunTrigger::Manual,
            started_at: ts("2026-11-22T20:00:00Z"),
            finished_at: Some(ts("2026-11-22T20:01:05Z")),
            stats: RunStats {
                sources_scanned: 4,
                events_new: 10,
            },
            warnings: vec![
                warning("a", None),
                warning("error: b", None),
                warning("c", Some(1)),
            ],
            error: None,
        };
        assert_eq!(
            run_counts(&run),
            RunCounts {
                warnings: 2,
                errors: 1
            }
        );
        assert_eq!(run_duration(&run), "1m");
        run.error = Some("lock held".into());
        run.finished_at = None;
        assert_eq!(run_counts(&run).errors, 2);
        assert_eq!(run_duration(&run), "running");
    }

    #[test]
    fn apply_edit_refuses_missing_db() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("none.sqlite");
        let err = apply_edit(&db, &HarnessId::CODEX, &HarnessEdit::Enabled(false));
        assert!(matches!(err, Err(StoreError::NotFound(_))));
        assert!(!db.exists());
    }
}
