use std::sync::Arc;

use gpui::{AppContext as _, Modifiers, TestAppContext, VisualTestContext, px};
use tbt_app::collector_bridge::SyncSource;
use tbt_app::data::{self, MockData};
use tbt_app::pages::Page;
use tbt_app::workspace::Workspace;

#[gpui::test]
fn gitlab_link_sits_bottom_left_and_opens_the_repo(cx: &mut TestAppContext) {
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
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, cx| window.draw(cx).clear(cx));

    let link = cx.debug_bounds("gitlab-link").expect("link is drawn");
    let viewport = cx.update(|window, _| window.viewport_size());
    assert!(link.right() < px(200.));
    assert!(link.bottom() > viewport.height - px(40.));

    cx.simulate_click(link.center(), Modifiers::default());
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://gitlab.com/vaguemotion/tokenburntrack")
    );
}
