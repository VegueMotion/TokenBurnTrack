use std::sync::Arc;

use gpui::{
    AnyElement, Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Task,
    Window, div,
};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use jiff::Timestamp;
use tbt_store::ProjectStats;

use super::{Loadable, spawn_load};
use crate::app_state::{AppState, AppStateEvent, relative_time};
use crate::charts::format::{fmt_micros, fmt_tokens, micros_to_usd};
use crate::data::DataSource;
use crate::palette::categorical;
use crate::ui;

pub struct ProjectsPage {
    app_state: Entity<AppState>,
    data: Arc<dyn DataSource>,
    state: Loadable<Vec<ProjectStats>>,
    _load: Task<()>,
    _subscription: Subscription,
}

impl ProjectsPage {
    pub fn new(
        app_state: Entity<AppState>,
        data: Arc<dyn DataSource>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription =
            cx.subscribe(&app_state, |this, _, _: &AppStateEvent, cx| this.reload(cx));
        let mut page = ProjectsPage {
            app_state,
            data,
            state: Loadable::Loading,
            _load: Task::ready(()),
            _subscription: subscription,
        };
        page.reload(cx);
        page
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let filter = self.app_state.read(cx).query_filter(Timestamp::now());
        let data = self.data.clone();
        self._load = spawn_load(
            cx,
            move || Ok(data.project_stats(&filter)?),
            |this, state, _| this.state = state,
        );
    }

    fn render_ready(&self, projects: &[ProjectStats], cx: &Context<Self>) -> AnyElement {
        let t = cx.theme();
        let (fg, muted, track) = (t.foreground, t.muted_foreground, t.border.opacity(0.4));
        let max = projects
            .iter()
            .map(|p| micros_to_usd(p.cost))
            .fold(0.0, f64::max);
        let now = Timestamp::now();
        v_flex()
            .gap_3()
            .p_4()
            .children(projects.iter().map(|p| {
                let title = p.name.clone().unwrap_or_else(|| "(no project)".into());
                let sub = format!(
                    "{} · {} sessions · {} tokens{}",
                    p.path.clone().unwrap_or_default(),
                    p.sessions,
                    fmt_tokens(p.tokens.total()),
                    p.last_active
                        .map(|t| format!(" · active {}", relative_time(now, t)))
                        .unwrap_or_default()
                );
                ui::card(title, cx)
                    .child(
                        h_flex()
                            .gap_3()
                            .child(div().flex_1().text_xs().text_color(muted).child(sub))
                            .child(div().text_color(fg).child(fmt_micros(p.cost))),
                    )
                    .child(ui::bar_cell(
                        if max > 0.0 {
                            micros_to_usd(p.cost) / max
                        } else {
                            0.0
                        },
                        categorical(0),
                        track,
                    ))
                    .child(
                        h_flex()
                            .gap_4()
                            .flex_wrap()
                            .children(p.branches.iter().map(|b| {
                                div().text_xs().text_color(muted).child(format!(
                                    "{}: {} ({} sessions)",
                                    b.branch.clone().unwrap_or_else(|| "–".into()),
                                    fmt_micros(b.cost),
                                    b.sessions
                                ))
                            })),
                    )
            }))
            .into_any_element()
    }
}

impl Render for ProjectsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.state {
            Loadable::Loading => ui::status_message("Loading…", cx),
            Loadable::Failed(e) => ui::status_message(format!("Could not load: {e}"), cx),
            Loadable::Ready(p) if p.is_empty() => ui::status_message("No usage in range", cx),
            Loadable::Ready(p) => self.render_ready(p, cx),
        };
        ui::page_scroll("projects-scroll", body)
    }
}
