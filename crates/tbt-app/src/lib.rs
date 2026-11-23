pub mod app_state;
pub mod charts;
pub mod data;
pub mod pages;
pub mod palette;
pub mod ui;
pub mod workspace;

use std::sync::Arc;

use gpui::App;
use gpui_component::{Theme, ThemeMode};
use jiff::Timestamp;

use crate::data::{DataSource, MockData};
use crate::pages::Page;

fn start_page() -> Page {
    if cfg!(debug_assertions) {
        std::env::var("TBT_START_PAGE")
            .ok()
            .and_then(|s| Page::parse(&s))
            .unwrap_or(Page::Dashboard)
    } else {
        Page::Dashboard
    }
}

pub fn run() {
    gpui_platform::application().run(|cx: &mut App| {
        gpui_component::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        let data: Arc<dyn DataSource> = Arc::new(MockData::new(0x7b7, Timestamp::now()));
        if let Err(err) = workspace::open_main_window(data, start_page(), cx) {
            eprintln!("failed to open window: {err:#}");
            cx.quit();
        }
    });
}
