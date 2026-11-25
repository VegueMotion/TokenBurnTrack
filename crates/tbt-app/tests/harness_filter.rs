use std::sync::Arc;

use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};
use tbt_app::app_state::AppState;
use tbt_app::collector_bridge::SyncSource;
use tbt_app::data::{self, MockData};
use tbt_app::pages::Page;
use tbt_app::workspace::Workspace;
use tbt_core::HarnessId;

fn drawn(cx: &mut VisualTestContext, selector: String) -> bool {
    cx.debug_bounds(selector.leak()).is_some()
}

fn click(cx: &mut VisualTestContext, selector: String) {
    let bounds = cx
        .debug_bounds(selector.clone().leak())
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

fn selected(cx: &mut VisualTestContext, state: &Entity<AppState>) -> Vec<HarnessId> {
    cx.update(|_, cx| state.read(cx).selected.iter().cloned().collect())
}

#[gpui::test]
fn top_bar_shows_the_three_most_used_harnesses_and_a_menu_with_all(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let mut workspace = None;
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |window, cx| {
            let data = Arc::new(MockData::new(7, data::now()));
            let view =
                cx.new(|cx| Workspace::new(data, SyncSource::Mock, Page::Dashboard, window, cx));
            workspace = Some(view.clone());
            cx.new(|cx| gpui_component::Root::new(view, window, cx))
        })
        .unwrap()
    });
    let workspace = workspace.unwrap();
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let state = cx.update(|_, cx| workspace.read(cx).app_state().clone());

    let order: Vec<(HarnessId, u64)> = cx.update(|_, cx| {
        state
            .read(cx)
            .harnesses
            .iter()
            .map(|h| (h.id.clone(), h.events))
            .collect()
    });
    assert_eq!(order.len(), 4);
    assert!(order.windows(2).all(|w| w[0].1 >= w[1].1));
    let (top, rest) = order.split_at(3);

    assert!(drawn(&mut cx, "harness-all".into()));
    for (id, _) in top {
        assert!(drawn(&mut cx, format!("harness-{id}")));
    }
    assert!(!drawn(&mut cx, format!("harness-{}", rest[0].0)));
    assert!(!drawn(&mut cx, format!("harness-menu-{}", rest[0].0)));

    click(&mut cx, "harness-menu".into());
    for (id, _) in &order {
        assert!(drawn(&mut cx, format!("harness-menu-{id}")));
    }

    click(&mut cx, format!("harness-menu-{}", rest[0].0));
    assert_eq!(selected(&mut cx, &state), vec![rest[0].0.clone()]);
    assert!(drawn(&mut cx, format!("harness-menu-{}", top[0].0)));

    click(&mut cx, format!("harness-menu-{}", top[0].0));
    let mut both = vec![rest[0].0.clone(), top[0].0.clone()];
    both.sort();
    assert_eq!(selected(&mut cx, &state), both);

    click(&mut cx, "harness-all".into());
    assert!(selected(&mut cx, &state).is_empty());
}
