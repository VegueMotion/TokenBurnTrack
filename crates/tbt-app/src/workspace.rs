use std::sync::Arc;

use gpui::{
    AnyView, App, Context, Decorations, Entity, Hsla, IntoElement, MouseButton, MouseMoveEvent,
    ParentElement, Render, SharedString, Styled, Window, WindowControlArea, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::calendar::{Calendar, CalendarEvent, CalendarState, Date, Matcher};
use gpui_component::popover::Popover;
use gpui_component::tooltip::Tooltip;
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, InteractiveElementExt, Selectable, Sizable,
    StyledExt, h_flex, v_flex,
};
use jiff::Timestamp;
use jiff::civil::Weekday;
use tbt_core::{HarnessId, SessionId};

use crate::app_state::{AppState, AppStateEvent, DateRange};
use crate::collector_bridge::{CollectorBridge, SyncSource};
use crate::data::DataSource;
use crate::pages::Page;
use crate::pages::dashboard::DashboardPage;
use crate::pages::harnesses::HarnessesPage;
use crate::pages::models::ModelsPage;
use crate::pages::projects::ProjectsPage;
use crate::pages::session_detail::SessionDetailPage;
use crate::pages::sessions::SessionsPage;
use crate::pages::settings::SettingsPage;
use crate::palette::{harness_color, harness_label};

const REPO_URL: &str = "https://github.com/VegueMotion/TokenBurnTrack";
const TOP_HARNESSES: usize = 3;
const SIDEBAR_WIDTH: f32 = 200.;
const TITLE_BAR_HEIGHT: f32 = 34.;
const TOP_BAR_HEIGHT: f32 = 52.;

pub struct Workspace {
    app_state: Entity<AppState>,
    data: Arc<dyn DataSource>,
    page: Page,
    views: Vec<(Page, AnyView)>,
    detail: Option<(SessionId, AnyView)>,
    sync: Entity<CollectorBridge>,
    calendar: Entity<CalendarState>,
    custom_open: bool,
    title_drag: bool,
}

#[derive(Clone, Copy)]
enum WindowControl {
    Minimize,
    Maximize,
    Close,
}

impl WindowControl {
    const ALL: [WindowControl; 3] = [
        WindowControl::Minimize,
        WindowControl::Maximize,
        WindowControl::Close,
    ];

    fn id(self) -> &'static str {
        match self {
            WindowControl::Minimize => "window-minimize",
            WindowControl::Maximize => "window-maximize",
            WindowControl::Close => "window-close",
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            WindowControl::Minimize => "\u{2014}",
            WindowControl::Maximize => "\u{25C7}",
            WindowControl::Close => "\u{2715}",
        }
    }

    fn area(self) -> WindowControlArea {
        match self {
            WindowControl::Minimize => WindowControlArea::Min,
            WindowControl::Maximize => WindowControlArea::Max,
            WindowControl::Close => WindowControlArea::Close,
        }
    }

    fn run(self, window: &mut Window) {
        match self {
            WindowControl::Minimize => window.minimize_window(),
            WindowControl::Maximize => window.zoom_window(),
            WindowControl::Close => window.remove_window(),
        }
    }
}

impl Workspace {
    pub fn new(
        data: Arc<dyn DataSource>,
        sync: SyncSource,
        start: Page,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let app_state = cx.new(|_| AppState::default());
        crate::pages::session_detail::debug_open_from_env(&app_state, cx);
        cx.observe(&app_state, |_, _, cx| cx.notify()).detach();
        cx.subscribe_in(
            &app_state,
            window,
            |this, _, ev: &AppStateEvent, window, cx| match *ev {
                AppStateEvent::OpenSession(id) => this.open_session(id, window, cx),
                AppStateEvent::CloseSession => {
                    this.detail = None;
                    cx.notify();
                }
                _ => {}
            },
        )
        .detach();
        let sync = {
            let (app_state, data) = (app_state.clone(), data.clone());
            cx.new(|cx| CollectorBridge::new(sync, app_state, data, cx))
        };
        let tz = app_state.read(cx).tz();
        let calendar = cx.new(|cx| {
            let mut c =
                CalendarState::new(window, cx).disabled_matcher(Matcher::custom(move |d| {
                    *d > Timestamp::now().to_zoned_datetime(tz.clone()).date()
                }));
            c.apply_date(Date::Range(None, None));
            c
        });
        cx.subscribe_in(&calendar, window, |this, _, ev: &CalendarEvent, _, cx| {
            let CalendarEvent::Selected(Date::Range(Some(a), Some(b))) = *ev else {
                return;
            };
            this.custom_open = false;
            this.app_state.update(cx, |s, cx| {
                if let Some(r) = DateRange::from_days(a, b, &s.tz()) {
                    s.change_range(r, cx);
                }
            });
            cx.notify();
        })
        .detach();
        Workspace {
            app_state,
            data,
            page: start,
            views: Vec::new(),
            detail: None,
            sync,
            calendar,
            custom_open: false,
            title_drag: false,
        }
    }

    pub fn app_state(&self) -> &Entity<AppState> {
        &self.app_state
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

    fn open_session(&mut self, id: SessionId, window: &mut Window, cx: &mut Context<Self>) {
        let (s, d) = (self.app_state.clone(), self.data.clone());
        let view: AnyView = cx
            .new(|cx| SessionDetailPage::new(id, s, d, window, cx))
            .into();
        self.detail = Some((id, view));
        cx.notify();
    }

    fn set_custom_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if open == self.custom_open {
            return;
        }
        self.custom_open = open;
        if open {
            let state = self.app_state.read(cx);
            let date = match state.range.custom_days(&state.tz()) {
                Some((a, b)) => Date::Range(Some(a), Some(b)),
                None => Date::Range(None, None),
            };
            self.calendar
                .update(cx, |c, cx| c.set_date(date, window, cx));
        }
        cx.notify();
    }

    fn render_custom_range(&self, state: &AppState, cx: &mut Context<Self>) -> impl IntoElement {
        let label = match state.range.custom_days(&state.tz()) {
            Some((a, b)) if a == b => a.strftime("%b %-d").to_string(),
            Some((a, b)) if a.year() == b.year() => {
                format!("{} – {}", a.strftime("%b %-d"), b.strftime("%b %-d"))
            }
            Some((a, b)) => format!(
                "{} – {}",
                a.strftime("%b %-d, %Y"),
                b.strftime("%b %-d, %Y")
            ),
            None => "Custom".to_owned(),
        };
        let this = cx.entity().downgrade();
        Popover::new("range-custom-popover")
            .open(self.custom_open)
            .on_open_change(move |open, window, cx| {
                let open = *open;
                this.update(cx, |w, cx| w.set_custom_open(open, window, cx))
                    .ok();
            })
            .trigger(
                Button::new("range-custom")
                    .small()
                    .ghost()
                    .selected(matches!(state.range, DateRange::Custom(_)))
                    .label(label),
            )
            .p_2()
            .when(self.custom_open, |p| {
                p.child(
                    Calendar::new(&self.calendar)
                        .small()
                        .first_day_of_week(Weekday::Monday),
                )
            })
    }

    fn sync_now(&mut self, cx: &mut Context<Self>) {
        self.sync.update(cx, |b, cx| b.sync_now(cx));
    }

    fn draws_title_bar(window: &Window) -> bool {
        cfg!(target_os = "windows")
            || matches!(window.window_decorations(), Decorations::Client { .. })
    }

    fn draggable(
        &self,
        el: gpui::Stateful<gpui::Div>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if cfg!(target_os = "windows") {
            return el.window_control_area(WindowControlArea::Drag);
        }
        el.on_double_click(|_, window, _| window.zoom_window())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, _| this.title_drag = !window.default_prevented()),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.title_drag = false),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, _| this.title_drag = false))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, window, _| {
                if ev.pressed_button != Some(MouseButton::Left) {
                    this.title_drag = false;
                } else if this.title_drag {
                    this.title_drag = false;
                    window.start_window_move();
                }
            }))
            .on_mouse_down(MouseButton::Right, |ev, window, _| {
                window.show_window_menu(ev.position)
            })
    }

    /// Replaces the system title bar: window controls on the right, the rest of the
    /// strip drags the window. Only drawn on Windows or when the window got client-side
    /// decorations; otherwise the compositor already draws its own bar.
    fn render_title_bar(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if !Self::draws_title_bar(window) {
            return None;
        }
        let t = cx.theme();
        let (fg, hover, danger, border) = (t.foreground, t.secondary_hover, t.danger, t.border);
        let supported = window.window_controls();
        let controls = WindowControl::ALL.into_iter().filter(move |c| match c {
            WindowControl::Minimize => supported.minimize,
            WindowControl::Maximize => supported.maximize,
            WindowControl::Close => true,
        });
        let bar = h_flex()
            .id("title-bar")
            .debug_selector(|| "title-bar".into())
            .w_full()
            .flex_none()
            .h(px(TITLE_BAR_HEIGHT))
            .items_center()
            .justify_end()
            .border_b_1()
            .border_color(border);
        Some(
            self.draggable(bar, cx)
                .child(h_flex().h_full().pr_1().children(controls.map(|c| {
                    let close = matches!(c, WindowControl::Close);
                    div()
                        .id(c.id())
                        .debug_selector(|| c.id().into())
                        .h_full()
                        .w(px(34.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .text_color(fg)
                        .hover(move |s| if close { s.bg(danger) } else { s.bg(hover) })
                        .map(|d| {
                            if cfg!(target_os = "windows") {
                                return d.occlude().window_control_area(c.area());
                            }
                            d.on_mouse_down(MouseButton::Left, |_, window, cx| {
                                window.prevent_default();
                                cx.stop_propagation();
                            })
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                c.run(window);
                            })
                        })
                        .child(c.glyph())
                })))
                .into_any_element(),
        )
    }

    fn render_brand(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme();
        let (fg, muted) = (t.sidebar_foreground, t.muted_foreground);
        let height = if Self::draws_title_bar(window) {
            TITLE_BAR_HEIGHT + TOP_BAR_HEIGHT
        } else {
            TOP_BAR_HEIGHT
        };
        let brand = h_flex()
            .id("brand")
            .debug_selector(|| "brand".into())
            .flex_none()
            .h(px(height))
            .px_5()
            .gap_2()
            .items_center();
        let brand = if Self::draws_title_bar(window) {
            self.draggable(brand, cx)
        } else {
            brand
        };
        brand
            .child(gpui::img(crate::assets::APP_ICON).size_8().flex_none())
            .child(
                v_flex()
                    .child(
                        div()
                            .text_base()
                            .font_semibold()
                            .text_color(fg)
                            .child("TokenBurnTrack"),
                    )
                    .child(div().text_xs().text_color(muted).child("AI usage & cost")),
            )
    }

    fn render_sidebar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme();
        let (bg, border, fg, muted, accent) = (
            t.sidebar,
            t.sidebar_border,
            t.sidebar_foreground,
            t.muted_foreground,
            t.sidebar_accent,
        );
        let nav = v_flex()
            .flex_1()
            .min_h_0()
            .p_3()
            .gap_1()
            .children(Page::ALL.into_iter().map(|page| {
                let active = page == self.page;
                h_flex()
                    .id(page.label())
                    .gap_2()
                    .items_center()
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
                        this.detail = None;
                        cx.notify();
                    }))
                    .child(Icon::new(page.icon()).size_4())
                    .child(page.label())
            }))
            .child(div().flex_1())
            .child(
                h_flex()
                    .id("github-link")
                    .debug_selector(|| "github-link".into())
                    .gap_2()
                    .items_center()
                    .px_3()
                    .py_1p5()
                    .rounded_md()
                    .text_xs()
                    .cursor_pointer()
                    .text_color(muted)
                    .hover(move |s| s.bg(accent).text_color(fg))
                    .on_click(|_, _, cx| cx.open_url(REPO_URL))
                    .child(Icon::empty().path(crate::assets::GITHUB_ICON).size_3p5())
                    .child("GitHub"),
            );
        v_flex()
            .id("sidebar")
            .debug_selector(|| "sidebar".into())
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_none()
            .bg(bg)
            .border_r_1()
            .border_color(border)
            .child(self.render_brand(window, cx))
            .child(nav)
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
        let selector = id.to_string();
        h_flex()
            .id(id)
            .debug_selector(move || selector)
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

    fn render_harness_menu(&self, state: &AppState, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme();
        let (fg, muted, hover) = (t.foreground, t.muted_foreground, t.secondary_hover);
        let hidden = state
            .harnesses
            .iter()
            .skip(TOP_HARNESSES)
            .filter(|h| state.is_selected(&h.id))
            .count();
        let label = if hidden > 0 {
            format!("More · {hidden}")
        } else {
            "More".to_owned()
        };
        let rows = state.harnesses.iter().map(|h| {
            let selected = state.is_selected(&h.id);
            let color = harness_color(&h.id);
            let id = h.id.clone();
            let app_state = self.app_state.clone();
            let selector = format!("harness-menu-{}", h.id);
            h_flex()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector)
                .gap_2()
                .items_center()
                .px_2()
                .py_1()
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .text_color(if selected { fg } else { muted })
                .hover(move |s| s.bg(hover).text_color(fg))
                .child(div().size(px(8.)).rounded_full().bg(if selected {
                    color
                } else {
                    color.opacity(0.45)
                }))
                .child(div().flex_1().child(harness_label(&h.id)))
                .child(
                    div()
                        .w(px(14.))
                        .when(selected, |d| d.child(Icon::new(IconName::Check).size_3p5())),
                )
                .on_click(move |_, _, cx| {
                    app_state.update(cx, |s, cx| s.change_harness(Some(&id), cx));
                })
        });
        Popover::new("harness-menu-popover")
            .trigger(
                Button::new("harness-menu")
                    .debug_selector(|| "harness-menu".into())
                    .small()
                    .outline()
                    .selected(hidden > 0)
                    .dropdown_caret(true)
                    .label(label),
            )
            .p_1()
            .child(v_flex().min_w(px(180.)).gap_0p5().children(rows))
    }

    fn render_top_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.app_state.read(cx).clone();
        let t = cx.theme();
        let (border, muted, danger) = (t.border, t.muted_foreground, t.danger);
        let now = Timestamp::now();
        let failed = state.sync_failed();
        let ranges = h_flex()
            .id("date-ranges")
            .debug_selector(|| "date-ranges".into())
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
            .child(self.render_custom_range(&state, cx));
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
        for h in state.harnesses.iter().take(TOP_HARNESSES) {
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
        if state.harnesses.len() > TOP_HARNESSES {
            chips.push(self.render_harness_menu(&state, cx).into_any_element());
        }
        let filters_apply = self.page.uses_filters();
        h_flex()
            .w_full()
            .flex_none()
            .h(px(TOP_BAR_HEIGHT))
            .px_4()
            .gap_4()
            .items_center()
            .border_b_1()
            .border_color(border)
            .child(
                h_flex()
                    .flex_1()
                    .flex_basis(px(0.))
                    .items_center()
                    .when(!filters_apply, |d| d.opacity(0.4))
                    .child(ranges),
            )
            .child(
                h_flex()
                    .id("harness-chips")
                    .debug_selector(|| "harness-chips".into())
                    .flex_none()
                    .gap_1p5()
                    .items_center()
                    .when(!filters_apply, |d| d.opacity(0.4))
                    .children(chips),
            )
            .child(
                h_flex()
                    .flex_1()
                    .flex_basis(px(0.))
                    .min_w_0()
                    .gap_4()
                    .items_center()
                    .justify_end()
                    .child(
                        div()
                            .id("sync-status")
                            .min_w_0()
                            .max_w(px(320.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_xs()
                            .text_color(if failed { danger } else { muted })
                            .child(state.sync_status(now))
                            .when_some(state.sync_tooltip(now), |d, tip| {
                                d.tooltip(move |window, cx| {
                                    Tooltip::new(tip.clone()).build(window, cx)
                                })
                            }),
                    )
                    .child(
                        Button::new("sync-now")
                            .debug_selector(|| "sync-now".into())
                            .flex_none()
                            .small()
                            .primary()
                            .loading(state.syncing || state.daemon_busy)
                            .disabled(!state.sync_available)
                            .when(!state.sync_available, |b| {
                                b.tooltip("Sync is off: no collector is available")
                            })
                            .label("Sync now")
                            .on_click(cx.listener(|this, _, _, cx| this.sync_now(cx))),
                    ),
            )
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = match &self.detail {
            Some((_, view)) => view.clone(),
            None => self.view(self.page, window, cx),
        };
        let t = cx.theme();
        let (bg, fg) = (t.background, t.foreground);
        h_flex()
            .size_full()
            .bg(bg)
            .text_color(fg)
            .child(self.render_sidebar(window, cx))
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .children(self.render_title_bar(window, cx))
                    .child(self.render_top_bar(cx))
                    .child(div().flex_1().min_h_0().child(page)),
            )
    }
}

pub fn open_main_window(
    data: Arc<dyn DataSource>,
    sync: SyncSource,
    start: Page,
    cx: &mut App,
) -> anyhow::Result<()> {
    use gpui::{Bounds, WindowBounds, WindowOptions};
    let bounds = Bounds::centered(None, initial_window_size(cx), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            app_id: Some(tbt_core::APP_ID.into()),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("TokenBurnTrack".into()),
                appears_transparent: true,
                ..Default::default()
            }),
            window_decorations: Some(gpui::WindowDecorations::Client),
            ..Default::default()
        },
        |window, cx| {
            let view = cx.new(|cx| Workspace::new(data, sync, start, window, cx));
            cx.new(|cx| gpui_component::Root::new(view, window, cx))
        },
    )?;
    cx.activate(true);
    Ok(())
}

fn initial_window_size(cx: &App) -> gpui::Size<gpui::Pixels> {
    let screen = cx.primary_display().map(|d| d.bounds().size).or_else(|| {
        cx.displays()
            .iter()
            .map(|d| d.bounds().size)
            .max_by(|a, b| area(a).total_cmp(&area(b)))
    });
    window_size_for(screen)
}

pub fn window_size_for(screen: Option<gpui::Size<gpui::Pixels>>) -> gpui::Size<gpui::Pixels> {
    let (w, h) = (px(1600.), px(1040.));
    match screen {
        Some(s) => gpui::size(w.min(s.width * 0.9), h.min(s.height * 0.9)),
        None => gpui::size(w, h),
    }
}

fn area(s: &gpui::Size<gpui::Pixels>) -> f32 {
    s.width.as_f32() * s.height.as_f32()
}
