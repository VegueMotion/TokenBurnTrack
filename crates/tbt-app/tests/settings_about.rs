use std::sync::Arc;

use gpui::{AppContext as _, Modifiers, TestAppContext, VisualTestContext};
use tbt_app::collector_bridge::SyncSource;
use tbt_app::data::{self, MockData};
use tbt_app::pages::Page;
use tbt_app::workspace::Workspace;

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

#[gpui::test]
fn about_tab_links_to_source_issues_and_license(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |window, cx| {
            let data = Arc::new(MockData::new(7, data::now()));
            let view =
                cx.new(|cx| Workspace::new(data, SyncSource::Mock, Page::Settings, window, cx));
            cx.new(|cx| gpui_component::Root::new(view, window, cx))
        })
        .unwrap()
    });
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, cx| window.draw(cx).clear(cx));

    assert!(cx.debug_bounds("about-source").is_none());
    click(&mut cx, "settings-tab-About");

    for (selector, url) in [
        (
            "about-source",
            "https://github.com/VegueMotion/TokenBurnTrack",
        ),
        (
            "about-issues",
            "https://github.com/VegueMotion/TokenBurnTrack/issues",
        ),
        ("about-license", "https://www.gnu.org/licenses/gpl-3.0.html"),
    ] {
        click(&mut cx, selector);
        assert_eq!(cx.opened_url().as_deref(), Some(url));
    }
}
