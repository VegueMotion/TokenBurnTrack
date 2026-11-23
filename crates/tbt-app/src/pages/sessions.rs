use std::cmp::Ordering;
use std::ops::Range;
use std::sync::Arc;

use gpui::{
    Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Task,
    UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use jiff::Timestamp;
use tbt_core::Micros;
use tbt_store::SessionRow;

use super::{Loadable, spawn_load};
use crate::app_state::{AppState, AppStateEvent};
use crate::charts::format::{fmt_duration, fmt_micros, fmt_tokens};
use crate::data::DataSource;
use crate::palette::{harness_color, harness_label};
use crate::ui::{self, Col};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Start,
    Harness,
    Project,
    Models,
    Duration,
    Tokens,
    Cost,
}

const COLUMNS: [(SortKey, Col); 7] = [
    (SortKey::Start, Col::fixed("Start", 130.0)),
    (SortKey::Harness, Col::fixed("Harness", 120.0)),
    (SortKey::Project, Col::flex("Project")),
    (SortKey::Models, Col::fixed("Model(s)", 210.0)),
    (SortKey::Duration, Col::num("Duration", 80.0)),
    (SortKey::Tokens, Col::num("Tokens", 80.0)),
    (SortKey::Cost, Col::num("Cost", 80.0)),
];

const ROW_HEIGHT: f32 = 30.0;

pub fn wall_secs(row: &SessionRow) -> u64 {
    row.ended_at
        .map(|e| (e.as_second() - row.started_at.as_second()).max(0) as u64)
        .unwrap_or(0)
}

fn project_label(row: &SessionRow) -> &str {
    row.project_name
        .as_deref()
        .or(row.project_path.as_deref())
        .unwrap_or("")
}

pub fn cost_label(row: &SessionRow) -> String {
    if row.unpriced_events > 0 && row.cost == Micros::ZERO {
        "unpriced".to_owned()
    } else {
        fmt_micros(row.cost)
    }
}

pub fn compare(a: &SessionRow, b: &SessionRow, key: SortKey) -> Ordering {
    match key {
        SortKey::Start => a.started_at.cmp(&b.started_at),
        SortKey::Harness => a.harness.cmp(&b.harness),
        SortKey::Project => project_label(a)
            .to_lowercase()
            .cmp(&project_label(b).to_lowercase()),
        SortKey::Models => a.models.cmp(&b.models),
        SortKey::Duration => wall_secs(a).cmp(&wall_secs(b)),
        SortKey::Tokens => a.tokens.total().cmp(&b.tokens.total()),
        SortKey::Cost => a.cost.cmp(&b.cost),
    }
    .then_with(|| a.id.cmp(&b.id))
}

pub fn matches(row: &SessionRow, query: &str) -> bool {
    let hay = [
        Some(row.harness.as_str().to_owned()),
        Some(harness_label(&row.harness)),
        row.project_path.clone(),
        row.project_name.clone(),
        row.git_branch.clone(),
        row.title.clone(),
        Some(row.ext_id.clone()),
        Some(row.models.join(" ")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n")
    .to_lowercase();
    query
        .split_whitespace()
        .all(|term| hay.contains(&term.to_lowercase()))
}

pub fn view_indices(rows: &[SessionRow], query: &str, key: SortKey, desc: bool) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..rows.len())
        .filter(|i| matches(&rows[*i], query))
        .collect();
    idx.sort_by(|a, b| {
        let o = compare(&rows[*a], &rows[*b], key);
        if desc { o.reverse() } else { o }
    });
    idx
}

pub fn default_desc(key: SortKey) -> bool {
    matches!(
        key,
        SortKey::Start | SortKey::Duration | SortKey::Tokens | SortKey::Cost
    )
}

pub struct SessionsPage {
    app_state: Entity<AppState>,
    data: Arc<dyn DataSource>,
    rows: Loadable<Vec<SessionRow>>,
    view: Vec<usize>,
    sort: SortKey,
    desc: bool,
    query: String,
    search: Entity<InputState>,
    scroll: UniformListScrollHandle,
    _load: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl SessionsPage {
    pub fn new(
        app_state: Entity<AppState>,
        data: Arc<dyn DataSource>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search project, model, harness, branch…")
        });
        let subscriptions = vec![
            cx.subscribe(&app_state, |this, _, _: &AppStateEvent, cx| this.reload(cx)),
            cx.subscribe(&search, |this, input, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.query = input.read(cx).value().to_string();
                    this.refresh_view();
                    cx.notify();
                }
            }),
        ];
        let mut page = SessionsPage {
            app_state,
            data,
            rows: Loadable::Loading,
            view: Vec::new(),
            sort: SortKey::Start,
            desc: true,
            query: String::new(),
            search,
            scroll: UniformListScrollHandle::new(),
            _load: Task::ready(()),
            _subscriptions: subscriptions,
        };
        page.reload(cx);
        page
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let filter = self.app_state.read(cx).query_filter(Timestamp::now());
        let data = self.data.clone();
        self._load = spawn_load(
            cx,
            move || Ok(data.sessions(&filter)?),
            |this, rows, _| {
                this.rows = rows;
                this.refresh_view();
            },
        );
    }

    fn refresh_view(&mut self) {
        self.view = match &self.rows {
            Loadable::Ready(rows) => view_indices(rows, &self.query, self.sort, self.desc),
            _ => Vec::new(),
        };
    }

    fn sort_by(&mut self, key: SortKey, cx: &mut Context<Self>) {
        if self.sort == key {
            self.desc = !self.desc;
        } else {
            self.sort = key;
            self.desc = default_desc(key);
        }
        self.refresh_view();
        cx.notify();
    }

    fn render_row(&self, ix: usize, cx: &Context<Self>) -> gpui::AnyElement {
        let t = cx.theme();
        let Loadable::Ready(rows) = &self.rows else {
            return div().into_any_element();
        };
        let row = &rows[self.view[ix]];
        let tz = self.app_state.read(cx).tz();
        let start: SharedString = row
            .started_at
            .to_zoned_datetime(tz)
            .strftime("%b %-d, %H:%M")
            .to_string()
            .into();
        let harness = h_flex()
            .gap_2()
            .items_center()
            .child(ui::swatch(harness_color(&row.harness)))
            .child(harness_label(&row.harness));
        let project = h_flex()
            .gap_2()
            .overflow_hidden()
            .child(project_label(row).to_owned())
            .when_some(row.git_branch.clone(), |d, b| {
                d.child(div().text_xs().text_color(t.muted_foreground).child(b))
            });
        let cells: [gpui::AnyElement; 7] = [
            start.into_any_element(),
            harness.into_any_element(),
            project.into_any_element(),
            row.models.join(", ").into_any_element(),
            fmt_duration(wall_secs(row)).into_any_element(),
            fmt_tokens(row.tokens.total()).into_any_element(),
            cost_label(row).into_any_element(),
        ];
        h_flex()
            .id(ix)
            .w_full()
            .h(px(ROW_HEIGHT))
            .gap_3()
            .px_3()
            .text_sm()
            .border_b_1()
            .border_color(t.border.opacity(0.4))
            .hover(|s| s.bg(t.foreground.opacity(0.04)))
            .children(COLUMNS.iter().zip(cells).map(|((_, c), cell)| c.cell(cell)))
            .into_any_element()
    }

    fn render_header(&self, cx: &Context<Self>) -> impl IntoElement {
        let t = cx.theme();
        h_flex()
            .w_full()
            .gap_3()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(t.border)
            .text_xs()
            .text_color(t.muted_foreground)
            .children(COLUMNS.iter().enumerate().map(|(i, (key, col))| {
                let key = *key;
                let arrow = if self.sort == key {
                    if self.desc { " ▼" } else { " ▲" }
                } else {
                    ""
                };
                col.cell(
                    div()
                        .id(("sort", i))
                        .cursor_pointer()
                        .when(self.sort == key, |d| d.text_color(t.foreground))
                        .hover(|s| s.text_color(t.foreground))
                        .on_click(cx.listener(move |this, _, _, cx| this.sort_by(key, cx)))
                        .child(format!("{}{arrow}", col.title)),
                )
            }))
    }
}

impl Render for SessionsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme();
        let muted = t.muted_foreground;
        let summary: SharedString = match &self.rows {
            Loadable::Ready(rows) => {
                let cost: Micros = self.view.iter().map(|i| rows[*i].cost).sum();
                format!(
                    "{} of {} sessions · {}",
                    self.view.len(),
                    rows.len(),
                    fmt_micros(cost)
                )
                .into()
            }
            _ => "".into(),
        };
        let body = match &self.rows {
            Loadable::Loading => ui::status_message("Loading…", cx),
            Loadable::Failed(e) => ui::status_message(format!("Could not load: {e}"), cx),
            Loadable::Ready(_) if self.view.is_empty() => {
                ui::status_message("No sessions match", cx)
            }
            Loadable::Ready(_) => uniform_list(
                "sessions",
                self.view.len(),
                cx.processor(|this, range: Range<usize>, _window, cx| {
                    range.map(|ix| this.render_row(ix, cx)).collect()
                }),
            )
            .track_scroll(&self.scroll)
            .size_full()
            .into_any_element(),
        };
        v_flex()
            .size_full()
            .p_4()
            .gap_3()
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .w(px(360.))
                            .child(Input::new(&self.search).cleanable(true)),
                    )
                    .child(div().text_sm().text_color(muted).child(summary)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .rounded_lg()
                    .border_1()
                    .border_color(t.border)
                    .overflow_hidden()
                    .child(self.render_header(cx))
                    .child(div().flex_1().min_h_0().child(body)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tbt_core::{HarnessId, SessionId};
    use tbt_store::TokenSums;

    fn row(id: i64, harness: HarnessId, project: &str, start: &str, cost: i64) -> SessionRow {
        let started_at: Timestamp = start.parse().unwrap();
        SessionRow {
            id: SessionId(id),
            ext_id: format!("ext-{id}"),
            title: None,
            project_path: Some(format!("/src/{project}")),
            project_name: Some(project.to_owned()),
            git_branch: Some("main".into()),
            models: vec![if harness == HarnessId::CODEX {
                "gpt-5-codex".into()
            } else {
                "claude-sonnet-4-5".into()
            }],
            harness,
            started_at,
            ended_at: Some(started_at + jiff::SignedDuration::from_secs(id * 60)),
            active_seconds: 0,
            events: 1,
            tokens: TokenSums {
                input: (id * 100) as u64,
                ..Default::default()
            },
            cost: Micros(cost),
            unpriced_events: 0,
            parent: None,
            resumed_from: None,
        }
    }

    fn rows() -> Vec<SessionRow> {
        vec![
            row(1, HarnessId::CODEX, "api", "2026-11-20T10:00:00Z", 500),
            row(
                2,
                HarnessId::CLAUDE_CODE,
                "Website",
                "2026-11-22T10:00:00Z",
                100,
            ),
            row(
                3,
                HarnessId::CLAUDE_CODE,
                "api",
                "2026-11-21T10:00:00Z",
                900,
            ),
        ]
    }

    #[test]
    fn sorts_by_column_and_direction() {
        let r = rows();
        assert_eq!(view_indices(&r, "", SortKey::Start, true), vec![1, 2, 0]);
        assert_eq!(view_indices(&r, "", SortKey::Cost, true), vec![2, 0, 1]);
        assert_eq!(view_indices(&r, "", SortKey::Cost, false), vec![1, 0, 2]);
        assert_eq!(view_indices(&r, "", SortKey::Project, false), vec![0, 2, 1]);
        assert_eq!(view_indices(&r, "", SortKey::Duration, true), vec![2, 1, 0]);
        assert_eq!(view_indices(&r, "", SortKey::Harness, false), vec![1, 2, 0]);
    }

    #[test]
    fn search_matches_all_terms_case_insensitively() {
        let r = rows();
        assert_eq!(view_indices(&r, "API", SortKey::Start, true), vec![2, 0]);
        assert_eq!(
            view_indices(&r, "api claude", SortKey::Start, true),
            vec![2]
        );
        assert_eq!(view_indices(&r, "codex cli", SortKey::Start, true), vec![0]);
        assert_eq!(view_indices(&r, "sonnet", SortKey::Start, true).len(), 2);
        assert!(view_indices(&r, "gemini", SortKey::Start, true).is_empty());
    }

    #[test]
    fn unpriced_sessions_have_no_dollar_amount() {
        let mut r = rows().remove(0);
        assert_eq!(cost_label(&r), "<$0.01");
        r.cost = Micros::ZERO;
        r.unpriced_events = 1;
        assert_eq!(cost_label(&r), "unpriced");
    }

    #[test]
    fn default_directions() {
        assert!(default_desc(SortKey::Cost));
        assert!(!default_desc(SortKey::Project));
    }
}
