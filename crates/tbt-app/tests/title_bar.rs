//! The custom title bar that replaces the system one. gpui's test platform
//! leaves minimize, zoom and window moves `unimplemented!()`, so a control
//! reaching the platform shows up as a "not implemented" panic.

#![cfg_attr(target_os = "windows", allow(dead_code))]

use std::sync::Arc;

use gpui::{
    AppContext as _, Decorations, Modifiers, MouseButton, Pixels, Point, TestAppContext, Tiling,
    VisualTestContext, point, px,
};
use tbt_app::collector_bridge::SyncSource;
use tbt_app::data::{self, MockData};
use tbt_app::pages::Page;
use tbt_app::workspace::Workspace;

fn open(cx: &mut TestAppContext, client: bool) -> VisualTestContext {
    cx.update(gpui_component::init);
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |window, cx| {
            let data = Arc::new(MockData::new(7, data::now()));
            let view =
                cx.new(|cx| Workspace::new(data, SyncSource::Mock, Page::Dashboard, window, cx));
            cx.new(|cx| gpui_component::Root::new(view, window, cx))
        })
        .unwrap()
    });
    if client {
        cx.simulate_window_decorations_change(
            window.into(),
            Decorations::Client {
                tiling: Tiling::default(),
            },
        );
    }
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx
}

fn center(cx: &mut VisualTestContext, selector: &'static str) -> Point<Pixels> {
    cx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"))
        .center()
}

fn click(cx: &mut VisualTestContext, at: Point<Pixels>) {
    cx.simulate_click(at, Modifiers::default());
}

/// Press, move one pixel, release, as a hand often does.
fn press_and_drift(cx: &mut VisualTestContext, at: Point<Pixels>) {
    let drift = point(at.x + px(1.), at.y);
    cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(drift, Some(MouseButton::Left), Modifiers::default());
    cx.simulate_mouse_up(drift, MouseButton::Left, Modifiers::default());
}

fn window_count(cx: &mut VisualTestContext) -> usize {
    let app: &mut TestAppContext = cx;
    app.update(|cx| cx.windows().len())
}

#[cfg(not(target_os = "windows"))]
#[gpui::test]
fn no_custom_bar_under_server_decorations(cx: &mut TestAppContext) {
    let mut cx = open(cx, false);
    assert!(cx.debug_bounds("title-bar").is_none());
    assert!(cx.debug_bounds("window-close").is_none());
}

#[cfg(target_os = "windows")]
#[gpui::test]
fn windows_draws_the_bar_under_server_decorations(cx: &mut TestAppContext) {
    let mut cx = open(cx, false);
    assert!(cx.debug_bounds("title-bar").is_some());
    assert!(cx.debug_bounds("window-close").is_some());
}

#[gpui::test]
fn controls_sit_on_the_right_in_order(cx: &mut TestAppContext) {
    let mut cx = open(cx, true);
    let bar = cx.debug_bounds("title-bar").expect("title bar is drawn");
    let min = center(&mut cx, "window-minimize");
    let max = center(&mut cx, "window-maximize");
    let close = center(&mut cx, "window-close");
    assert!(min.x < max.x && max.x < close.x);
    assert!(close.x > bar.right() - px(150.));
}

#[cfg(not(target_os = "windows"))]
#[gpui::test]
fn close_removes_the_window_even_with_drift(cx: &mut TestAppContext) {
    let mut cx = open(cx, true);
    assert_eq!(window_count(&mut cx), 1);
    // A drag that starts on a control must click it, not move the window
    // (which would panic here).
    let at = center(&mut cx, "window-close");
    press_and_drift(&mut cx, at);
    assert_eq!(window_count(&mut cx), 0);
}

#[cfg(not(target_os = "windows"))]
#[gpui::test]
#[should_panic(expected = "not implemented")]
fn minimize_reaches_the_platform(cx: &mut TestAppContext) {
    let mut cx = open(cx, true);
    let at = center(&mut cx, "window-minimize");
    click(&mut cx, at);
}

#[cfg(not(target_os = "windows"))]
#[gpui::test]
#[should_panic(expected = "not implemented")]
fn maximize_reaches_the_platform(cx: &mut TestAppContext) {
    let mut cx = open(cx, true);
    let at = center(&mut cx, "window-maximize");
    click(&mut cx, at);
}

#[cfg(not(target_os = "windows"))]
#[gpui::test]
#[should_panic(expected = "not implemented")]
fn dragging_the_bar_moves_the_window(cx: &mut TestAppContext) {
    let mut cx = open(cx, true);
    let bar = cx.debug_bounds("title-bar").expect("title bar is drawn");
    press_and_drift(&mut cx, bar.center());
}
