use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyView, App, Context, Entity, Hsla, IntoElement, ParentElement, Render, SharedString, Styled,
    Task, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, Disableable, Selectable, Sizable, StyledExt, h_flex, v_flex};
use jiff::Timestamp;
use tbt_core::HarnessId;

use crate::app_state::{AppState, AppStateEvent, DateRange, HarnessOption, LastSync};
use crate::data::DataSource;
use crate::pages::dashboard::DashboardPage;
use crate::pages::harnesses::HarnessesPage;
use crate::pages::models::ModelsPage;
use crate::pages::projects::ProjectsPage;
use crate::pages::sessions::SessionsPage;
use crate::pages::settings::SettingsPage;
use crate::pages::{Page, spawn_load};
use crate::palette::{harness_color, harness_label};

pub struct Workspace {
    app_state: Entity<AppState>,
    data: Arc<dyn DataSource>,
    page: Page,
    views: Vec<(Page, AnyView)>,
    _tasks: Vec<Task<()>>,
}

impl Workspace {
    pub fn new(
        data: Arc<dyn DataSource>,
        start: Page,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let app_state = cx.new(|_| AppState::default());
        cx.observe(&app_state, |_, _, cx| cx.notify()).detach();
        let load = {
            let data = data.clone();
            let app_state = app_state.clone();
            spawn_load(
                cx,
                move || Ok((data.harness_status()?, data.ingest_runs(1)?)),
                move |_this: &mut Workspace, state, cx| {
                    if let crate::pages::Loadable::Ready((status, runs)) = state {
                        app_state.update(cx, |s, cx| {
                            s.harnesses = status
                                .into_iter()
                                .filter(|h| h.config.enabled)
                                .map(|h| HarnessOption {
                                    id: h.config.id,
                                    name: h.config.display_name,
                                })
                                .collect();
                            s.last_sync = runs.first().and_then(LastSync::from_run);
                            cx.notify();
                        });
                    }
                },
            )
        };
        Workspace {
            app_state,
            data,
            page: start,
            views: Vec::new(),
            _tasks: vec![load],
        }
    }

    fn view(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) -> AnyView {
        if let Some((_, v)) = self.views.iter().find(|(p, _)| *p == page) {
            return v.clone();
        }
        let (s, d) = (self.app_state.clone(), self.data.clone());
        let view: AnyView = match page {
            Page::Dashboard => cx.new(|cx| DashboardPage::new(s, d, window, cx)).into(),
            Page::Sessions => cx.new(|cx| SessionsPage::new(s, d, window, cx)).into(),
            Page::Models => cx.new(|cx| ModelsPage::new(s, d, window, cx)).into(),
            Page::Projects => cx.new(|cx| ProjectsPage::new(s, d, window, cx)).into(),
            Page::Harnesses => cx.new(|cx| HarnessesPage::new(s, d, window, cx)).into(),
            Page::Settings => cx.new(|cx| SettingsPage::new(s, d, window, cx)).into(),
        };
        self.views.push((page, view.clone()));
        view
    }

    fn sync_now(&mut self, cx: &mut Context<Self>) {
        let state = self.app_state.clone();
        if state.read(cx).syncing {
            return;
        }
        state.update(cx, |s, cx| {
            s.syncing = true;
            cx.notify();
        });
        let task = cx.spawn(async move |_, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1200))
                .await;
            state.update(cx, |s, cx| {
                s.syncing = false;
                s.last_sync = Some(LastSync {
                    finished_at: Timestamp::now(),
                    events_new: 0,
                    error: None,
                });
                cx.emit(AppStateEvent::DataChanged);
                cx.notify();
            });
        });
        self._tasks.push(task);
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme();
        let (bg, border, fg, muted, accent) = (
            t.sidebar,
            t.sidebar_border,
            t.sidebar_foreground,
            t.muted_foreground,
            t.sidebar_accent,
        );
        v_flex()
            .w(px(200.))
            .h_full()
            .flex_none()
            .bg(bg)
            .border_r_1()
            .border_color(border)
            .p_3()
            .gap_1()
            .child(
                v_flex()
                    .px_2()
                    .pb_4()
                    .pt_1()
                    .child(
                        div()
                            .text_base()
                            .font_semibold()
                            .text_color(fg)
                            .child("TokenBurnTrack"),
                    )
                    .child(div().text_xs().text_color(muted).child("AI usage & cost")),
            )
            .children(Page::ALL.into_iter().map(|page| {
                let active = page == self.page;
                div()
                    .id(page.label())
                    .px_3()
                    .py_1p5()
                    .rounded_md()
                    .text_sm()
                    .cursor_pointer()
                    .text_color(if active { fg } else { muted })
                    .when(active, |d| d.bg(accent).font_medium())
                    .hover(move |s| s.bg(accent).text_color(fg))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.page = page;
                        cx.notify();
                    }))
                    .child(page.label())
            }))
    }

    fn chip(
        &self,
        id: SharedString,
        label: String,
        color: Option<Hsla>,
        selected: bool,
        harness: Option<HarnessId>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = cx.theme();
        let (fg, muted, border, sel) = (
            t.foreground,
            t.muted_foreground,
            t.border,
            t.secondary_active,
        );
        let state = self.app_state.clone();
        h_flex()
            .id(id)
            .gap_1p5()
            .items_center()
            .px_2p5()
            .h(px(26.))
            .rounded_md()
            .border_1()
            .border_color(border)
            .text_xs()
            .cursor_pointer()
            .text_color(if selected { fg } else { muted })
            .when(selected, |d| d.bg(sel))
            .hover(move |s| s.text_color(fg))
            .when_some(color, |d, c| {
                d.child(div().size(px(8.)).rounded_full().bg(if selected {
                    c
                } else {
                    c.opacity(0.45)
                }))
            })
            .child(label)
            .on_click(move |_, _, cx| {
                state.update(cx, |s, cx| s.change_harness(harness.as_ref(), cx));
            })
    }

    fn render_top_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.app_state.read(cx).clone();
        let t = cx.theme();
        let (border, muted, fg) = (t.border, t.muted_foreground, t.foreground);
        let ranges = h_flex()
            .gap_1()
            .children(DateRange::PRESETS.into_iter().map(|r| {
                let app_state = self.app_state.clone();
                Button::new(SharedString::from(format!("range-{}", r.label())))
                    .small()
                    .ghost()
                    .selected(state.range == r)
                    .label(r.label())
                    .on_click(move |_, _, cx| {
                        app_state.update(cx, |s, cx| s.change_range(r, cx));
                    })
            }))
            .child(
                Button::new("range-custom")
                    .small()
                    .ghost()
                    .disabled(true)
                    .selected(matches!(state.range, DateRange::Custom(_)))
                    .label("Custom")
                    .tooltip("Custom ranges are not available yet"),
            );
        let mut chips = vec![
            self.chip(
                "harness-all".into(),
                "All".into(),
                None,
                state.is_all(),
                None,
                cx,
            )
            .into_any_element(),
        ];
        for h in &state.harnesses {
            chips.push(
                self.chip(
                    format!("harness-{}", h.id).into(),
                    harness_label(&h.id),
                    Some(harness_color(&h.id)),
                    state.is_selected(&h.id),
                    Some(h.id.clone()),
                    cx,
                )
                .into_any_element(),
            );
        }
        let filters_apply = self.page.uses_filters();
        h_flex()
            .w_full()
            .flex_none()
            .h(px(52.))
            .px_4()
            .gap_4()
            .items_center()
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .text_lg()
                    .font_semibold()
                    .text_color(fg)
                    .min_w(px(110.))
                    .child(self.page.label()),
            )
            .child(
                h_flex()
                    .gap_4()
                    .items_center()
                    .when(!filters_apply, |d| d.opacity(0.4))
                    .child(ranges)
                    .child(h_flex().gap_1p5().children(chips)),
            )
            .child(div().flex_1().min_w(px(8.)))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_xs()
                    .text_color(muted)
                    .child(state.sync_status(Timestamp::now())),
            )
            .child(
                Button::new("sync-now")
                    .flex_none()
                    .small()
                    .primary()
                    .loading(state.syncing)
                    .label("Sync now")
                    .on_click(cx.listener(|this, _, _, cx| this.sync_now(cx))),
            )
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = self.view(self.page, window, cx);
        let t = cx.theme();
        let (bg, fg) = (t.background, t.foreground);
        h_flex()
            .size_full()
            .bg(bg)
            .text_color(fg)
            .child(self.render_sidebar(cx))
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .child(self.render_top_bar(cx))
                    .child(div().flex_1().min_h_0().child(page)),
            )
    }
}

pub fn open_main_window(
    data: Arc<dyn DataSource>,
    start: Page,
    cx: &mut App,
) -> anyhow::Result<()> {
    use gpui::{Bounds, WindowBounds, WindowOptions, size};
    let bounds = Bounds::centered(None, size(px(1320.), px(880.)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            app_id: Some("tbt-app".into()),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("TokenBurnTrack".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| {
            let view = cx.new(|cx| Workspace::new(data, start, window, cx));
            cx.new(|cx| gpui_component::Root::new(view, window, cx))
        },
    )?;
    cx.activate(true);
    Ok(())
}
