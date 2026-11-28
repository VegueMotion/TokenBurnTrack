use std::sync::Arc;

use gpui::{AppContext as _, TestAppContext, VisualTestContext, px, size};
use tbt_app::collector_bridge::SyncSource;
use tbt_app::data::{self, MockData};
use tbt_app::pages::Page;
use tbt_app::workspace::Workspace;

fn open(cx: &mut TestAppContext, page: Page) -> VisualTestContext {
    cx.update(gpui_component::init);
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |window, cx| {
            let data = Arc::new(MockData::new(7, data::now()));
            let view = cx.new(|cx| Workspace::new(data, SyncSource::Mock, page, window, cx));
            cx.new(|cx| gpui_component::Root::new(view, window, cx))
        })
        .unwrap()
    });
    cx.simulate_window_resize(window.into(), size(px(1000.), px(600.)));
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx
}

#[gpui::test]
fn every_page_draws_a_scrollbar(cx: &mut TestAppContext) {
    for page in Page::ALL {
        let mut cx = open(cx, page);
        let overlay = cx.debug_bounds("scrollbar-overlay");
        assert!(overlay.is_some(), "{page:?} has no scrollbar");
    }
}
