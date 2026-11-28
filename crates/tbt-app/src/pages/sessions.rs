use std::cmp::Ordering;
use std::ops::Range;
use std::sync::Arc;

use gpui::{
    AnyElement, Context, Entity, IntoElement, ParentElement, Render, ScrollStrategy, SharedString,
    Styled, Subscription, Task, UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, h_flex, v_flex};
use jiff::tz::TimeZone;
use tbt_core::Micros;
use tbt_store::{QueryFilter, SessionRow};

use super::{Loadable, spawn_load};
use crate::app_state::{AppState, AppStateEvent};
use crate::charts::format::{fmt_duration, fmt_micros, fmt_tokens};
use crate::charts::time::time_zone;
use crate::data::{self, DataSource, NO_DATA_MESSAGE};
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
    (SortKey::Cost, Col::num("Cost", 130.0)),
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
    fmt_micros(row.cost)
}

fn haystack(row: &SessionRow) -> String {
    [
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
    .to_lowercase()
}

pub fn default_desc(key: SortKey) -> bool {
    matches!(
        key,
        SortKey::Start | SortKey::Duration | SortKey::Tokens | SortKey::Cost
    )
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableView {
    pub indices: Vec<usize>,
    pub cost: Micros,
}

pub struct SessionTable {
    pub rows: Vec<SessionRow>,
    pub tz: TimeZone,
    pub no_data: bool,
    hay: Vec<String>,
    project_keys: Vec<String>,
}

impl SessionTable {
    pub fn new(rows: Vec<SessionRow>, tz: TimeZone, no_data: bool) -> SessionTable {
        SessionTable {
            hay: rows.iter().map(haystack).collect(),
            project_keys: rows
                .iter()
                .map(|r| project_label(r).to_lowercase())
                .collect(),
            rows,
            tz,
            no_data,
        }
    }

    pub fn load(data: &dyn DataSource, filter: &QueryFilter) -> anyhow::Result<SessionTable> {
        let rows = data.sessions(filter)?;
        let no_data = rows.is_empty() && !data.has_data()?;
        Ok(SessionTable::new(
            rows,
            time_zone(&filter.time_zone),
            no_data,
        ))
    }

    fn compare(&self, a: usize, b: usize, key: SortKey) -> Ordering {
        let (ra, rb) = (&self.rows[a], &self.rows[b]);
        match key {
            SortKey::Start => ra.started_at.cmp(&rb.started_at),
            SortKey::Harness => ra.harness.cmp(&rb.harness),
            SortKey::Project => self.project_keys[a].cmp(&self.project_keys[b]),
            SortKey::Models => ra.models.cmp(&rb.models),
            SortKey::Duration => wall_secs(ra).cmp(&wall_secs(rb)),
            SortKey::Tokens => ra.tokens.total().cmp(&rb.tokens.total()),
            SortKey::Cost => ra.cost.cmp(&rb.cost),
        }
        .then_with(|| ra.id.cmp(&rb.id))
    }

    pub fn view(&self, query: &str, key: SortKey, desc: bool) -> TableView {
        let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let mut indices: Vec<usize> = (0..self.rows.len())
            .filter(|i| terms.iter().all(|t| self.hay[*i].contains(t.as_str())))
            .collect();
        indices.sort_unstable_by(|a, b| {
            let o = self.compare(*a, *b, key);
            if desc { o.reverse() } else { o }
        });
        let cost = indices.iter().map(|i| self.rows[*i].cost).sum();
        TableView { indices, cost }
    }
}

pub struct SessionsPage {
    app_state: Entity<AppState>,
    data: Arc<dyn DataSource>,
    table: Loadable<Arc<SessionTable>>,
    view: TableView,
    sort: SortKey,
    desc: bool,
    query: String,
    search: Entity<InputState>,
    scroll: UniformListScrollHandle,
    _load: Task<()>,
    _view_task: Task<()>,
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
                    this.refresh_view(cx);
                }
            }),
        ];
        let mut page = SessionsPage {
            app_state,
            data,
            table: Loadable::Loading,
            view: TableView::default(),
            sort: SortKey::Start,
            desc: true,
            query: String::new(),
            search,
            scroll: UniformListScrollHandle::new(),
            _load: Task::ready(()),
            _view_task: Task::ready(()),
            _subscriptions: subscriptions,
        };
        page.reload(cx);
        page
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let filter = self.app_state.read(cx).query_filter(data::now());
        let data = self.data.clone();
        let (query, sort, desc) = (self.query.clone(), self.sort, self.desc);
        self._view_task = Task::ready(());
        self._load = spawn_load(
            cx,
            move || {
                let table = SessionTable::load(&*data, &filter)?;
                let view = table.view(&query, sort, desc);
                Ok((Arc::new(table), view))
            },
            |this, state, _| match state {
                Loadable::Ready((table, view)) => {
                    this.table = Loadable::Ready(table);
                    this.view = view;
                }
                Loadable::Failed(e) => {
                    this.table = Loadable::Failed(e);
                    this.view = TableView::default();
                }
                Loadable::Loading => this.table = Loadable::Loading,
            },
        );
    }

    fn refresh_view(&mut self, cx: &mut Context<Self>) {
        let Loadable::Ready(table) = &self.table else {
            return;
        };
        let table = table.clone();
        let (query, sort, desc) = (self.query.clone(), self.sort, self.desc);
        self._view_task = spawn_load(
            cx,
            move || Ok(table.view(&query, sort, desc)),
            |this, state, _| {
                if let Loadable::Ready(view) = state {
                    this.view = view;
                    this.scroll.scroll_to_item(0, ScrollStrategy::Top);
                }
            },
        );
    }

    fn sort_by(&mut self, key: SortKey, cx: &mut Context<Self>) {
        if self.sort == key {
            self.desc = !self.desc;
        } else {
            self.sort = key;
            self.desc = default_desc(key);
        }
        self.refresh_view(cx);
        cx.notify();
    }

    fn cost_cell(row: &SessionRow) -> AnyElement {
        cost_label(row).into_any_element()
    }

    fn render_row(&self, ix: usize, cx: &Context<Self>) -> AnyElement {
        let t = cx.theme();
        let Loadable::Ready(table) = &self.table else {
            return div().into_any_element();
        };
        let Some(row) = self.view.indices.get(ix).and_then(|i| table.rows.get(*i)) else {
            return div().into_any_element();
        };
        let start: SharedString = row
            .started_at
            .to_zoned_datetime(table.tz.clone())
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
        let cells: [AnyElement; 7] = [
            start.into_any_element(),
            harness.into_any_element(),
            project.into_any_element(),
            row.models.join(", ").into_any_element(),
            fmt_duration(wall_secs(row)).into_any_element(),
            fmt_tokens(row.tokens.total()).into_any_element(),
            Self::cost_cell(row),
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
            .cursor_pointer()
            .on_click({
                let (app_state, id) = (self.app_state.clone(), row.id);
                move |_, _, cx| app_state.update(cx, |s, cx| s.open_session(id, cx))
            })
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
        let summary: SharedString = match &self.table {
            Loadable::Ready(table) => format!(
                "{} of {} sessions · {}",
                self.view.indices.len(),
                table.rows.len(),
                fmt_micros(self.view.cost)
            )
            .into(),
            _ => "".into(),
        };
        let body = match &self.table {
            Loadable::Loading => ui::status_message("Loading…", cx),
            Loadable::Failed(e) => ui::status_message(format!("Could not load: {e}"), cx),
            Loadable::Ready(table) if table.no_data => ui::status_message(NO_DATA_MESSAGE, cx),
            Loadable::Ready(table) if table.rows.is_empty() => {
                ui::status_message("No sessions in this range", cx)
            }
            Loadable::Ready(_) if self.view.indices.is_empty() => {
                ui::status_message("No sessions match", cx)
            }
            Loadable::Ready(_) => div()
                .relative()
                .size_full()
                .child(
                    uniform_list(
                        "sessions",
                        self.view.indices.len(),
                        cx.processor(|this, range: Range<usize>, _window, cx| {
                            range.map(|ix| this.render_row(ix, cx)).collect()
                        }),
                    )
                    .track_scroll(&self.scroll)
                    .size_full(),
                )
                .vertical_scrollbar(&self.scroll)
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
    use jiff::Timestamp;
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

    fn table() -> SessionTable {
        SessionTable::new(
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
            ],
            TimeZone::UTC,
            false,
        )
    }

    fn idx(t: &SessionTable, query: &str, key: SortKey, desc: bool) -> Vec<usize> {
        t.view(query, key, desc).indices
    }

    #[test]
    fn sorts_by_column_and_direction() {
        let t = table();
        assert_eq!(idx(&t, "", SortKey::Start, true), vec![1, 2, 0]);
        assert_eq!(idx(&t, "", SortKey::Cost, true), vec![2, 0, 1]);
        assert_eq!(idx(&t, "", SortKey::Cost, false), vec![1, 0, 2]);
        assert_eq!(idx(&t, "", SortKey::Project, false), vec![0, 2, 1]);
        assert_eq!(idx(&t, "", SortKey::Duration, true), vec![2, 1, 0]);
        assert_eq!(idx(&t, "", SortKey::Harness, false), vec![1, 2, 0]);
    }

    #[test]
    fn search_matches_all_terms_case_insensitively() {
        let t = table();
        assert_eq!(idx(&t, "API", SortKey::Start, true), vec![2, 0]);
        assert_eq!(idx(&t, "api claude", SortKey::Start, true), vec![2]);
        assert_eq!(idx(&t, "codex cli", SortKey::Start, true), vec![0]);
        assert_eq!(idx(&t, "sonnet", SortKey::Start, true).len(), 2);
        assert!(idx(&t, "gemini", SortKey::Start, true).is_empty());
    }

    #[test]
    fn view_sums_cost_of_visible_rows() {
        let t = table();
        let v = t.view("api", SortKey::Start, true);
        assert_eq!(v.cost, Micros(1400));
        let v = t.view("", SortKey::Start, true);
        assert_eq!(v.cost, Micros(1500));
    }

    #[test]
    fn unpriced_calls_count_as_zero_cost() {
        let mut r = table().rows.remove(0);
        assert_eq!(cost_label(&r), "<$0.01");
        r.unpriced_events = 2;
        assert_eq!(cost_label(&r), "<$0.01");
        r.cost = Micros::ZERO;
        r.unpriced_events = 1;
        assert_eq!(cost_label(&r), "$0.00");
    }

    #[test]
    fn default_directions() {
        assert!(default_desc(SortKey::Cost));
        assert!(!default_desc(SortKey::Project));
    }

    #[test]
    fn large_tables_filter_and_sort() {
        let rows: Vec<SessionRow> = (0..50_000)
            .map(|i| {
                let mut r = row(
                    i,
                    if i % 3 == 0 {
                        HarnessId::CODEX
                    } else {
                        HarnessId::CLAUDE_CODE
                    },
                    &format!("p{}", i % 97),
                    "2026-11-20T10:00:00Z",
                    i,
                );
                r.started_at += jiff::SignedDuration::from_secs(i);
                r
            })
            .collect();
        let t = SessionTable::new(rows, TimeZone::UTC, false);
        let v = t.view("codex", SortKey::Cost, true);
        assert_eq!(v.indices.len(), 16_667);
        assert_eq!(v.indices[0], 49_998);
        assert_eq!(t.view("", SortKey::Start, true).indices[0], 49_999);
    }
}
