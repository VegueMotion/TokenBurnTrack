use std::sync::Arc;

use gpui::{
    AppContext as _, Decorations, Pixels, Size, TestAppContext, Tiling, VisualTestContext, px, size,
};
use tbt_app::collector_bridge::SyncSource;
use tbt_app::data::{self, MockData};
use tbt_app::pages::Page;
use tbt_app::workspace::{Workspace, window_size_for};

fn open(cx: &mut TestAppContext, client: bool, at: Option<Size<Pixels>>) -> VisualTestContext {
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
    if let Some(s) = at {
        cx.simulate_window_resize(window.into(), s);
    }
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx
}

#[gpui::test]
fn sidebar_spans_the_window_with_the_app_name_on_top(cx: &mut TestAppContext) {
    let mut cx = open(cx, true, None);
    let viewport = cx.update(|window, _| window.viewport_size());
    let sidebar = cx.debug_bounds("sidebar").expect("sidebar is drawn");
    let brand = cx.debug_bounds("brand").expect("app name is drawn");
    let title = cx.debug_bounds("title-bar").expect("title bar is drawn");

    let inset = sidebar.left();
    assert_eq!(sidebar.top(), inset);
    assert_eq!(sidebar.bottom(), viewport.height - inset);
    assert_eq!(brand.origin, sidebar.origin);
    assert_eq!(title.top(), inset);
    assert!(title.left() >= sidebar.right());
}

#[cfg(not(target_os = "windows"))]
#[gpui::test]
fn app_name_stays_in_the_sidebar_without_a_custom_title_bar(cx: &mut TestAppContext) {
    let mut cx = open(cx, false, None);
    let brand = cx.debug_bounds("brand").expect("app name is drawn");
    assert_eq!((brand.left(), brand.top()), (px(0.), px(0.)));
    assert!(cx.debug_bounds("title-bar").is_none());
}

#[gpui::test]
fn harness_chips_are_centered_in_the_top_bar(cx: &mut TestAppContext) {
    let mut cx = open(cx, true, Some(size(px(1600.), px(1000.))));
    let sidebar = cx.debug_bounds("sidebar").unwrap();
    let chips = cx.debug_bounds("harness-chips").expect("chips are drawn");
    let viewport = cx.update(|window, _| window.viewport_size());
    let content_center = (sidebar.right() + viewport.width - sidebar.left()) / 2.;
    assert!(
        (chips.center().x - content_center).abs() <= px(1.),
        "chips at {:?}, content center {:?}",
        chips.center().x,
        content_center
    );
}

#[gpui::test]
fn narrow_window_keeps_top_bar_groups_apart(cx: &mut TestAppContext) {
    let mut cx = open(cx, true, Some(size(px(1100.), px(800.))));
    let ranges = cx.debug_bounds("date-ranges").unwrap();
    let sidebar = cx.debug_bounds("sidebar").unwrap();
    let chips = cx.debug_bounds("harness-chips").unwrap();
    let sync = cx.debug_bounds("sync-now").unwrap();
    assert!(ranges.right() <= chips.left());
    assert!(chips.right() <= sync.left());
    let viewport = cx.update(|window, _| window.viewport_size());
    assert!(sync.right() <= viewport.width - sidebar.left());
}

#[test]
fn starting_window_is_bigger_but_fits_the_screen() {
    assert_eq!(window_size_for(None), size(px(1600.), px(1040.)));
    assert_eq!(
        window_size_for(Some(size(px(2560.), px(1440.)))),
        size(px(1600.), px(1040.))
    );
    assert_eq!(
        window_size_for(Some(size(px(1366.), px(768.)))),
        size(px(1366. * 0.9), px(768. * 0.9))
    );
}

#[cfg(not(target_os = "windows"))]
#[gpui::test]
#[should_panic(expected = "not implemented")]
fn dragging_the_app_name_moves_the_window(cx: &mut TestAppContext) {
    let mut cx = open(cx, true, None);
    let at = cx.debug_bounds("brand").unwrap().center();
    let drift = gpui::point(at.x + px(1.), at.y);
    cx.simulate_mouse_down(at, gpui::MouseButton::Left, gpui::Modifiers::default());
    cx.simulate_mouse_move(
        drift,
        Some(gpui::MouseButton::Left),
        gpui::Modifiers::default(),
    );
}
