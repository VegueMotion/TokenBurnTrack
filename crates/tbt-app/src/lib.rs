pub mod app_state;
pub mod charts;
pub mod collector_bridge;
pub mod data;
pub mod export;
pub mod pages;
pub mod palette;
pub mod ui;
pub mod workspace;

use std::path::PathBuf;
use std::sync::Arc;

use gpui::App;
use gpui_component::{Theme, ThemeMode};

use crate::data::{DataSource, MockData, StoreData};
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

fn data_dir_arg(args: impl IntoIterator<Item = String>) -> Option<PathBuf> {
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--data-dir" {
            return args.next().map(PathBuf::from);
        }
        if let Some(dir) = arg.strip_prefix("--data-dir=") {
            return Some(PathBuf::from(dir));
        }
    }
    None
}

fn data_source() -> Arc<dyn DataSource> {
    if std::env::var("TBT_MOCK").is_ok_and(|v| v == "1") {
        return Arc::new(MockData::new(0x7b7, data::now()));
    }
    let dir = data_dir_arg(std::env::args().skip(1)).or_else(tbt_store::paths::default_data_dir);
    Arc::new(StoreData::from_data_dir(dir))
}

pub fn run() {
    let data = data_source();
    gpui_platform::application().run(move |cx: &mut App| {
        gpui_component::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        if let Err(err) = workspace::open_main_window(data.clone(), start_page(), cx) {
            eprintln!("failed to open window: {err:#}");
            cx.quit();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_data_dir_flag() {
        assert_eq!(data_dir_arg(args(&[])), None);
        assert_eq!(
            data_dir_arg(args(&["--data-dir", "/tmp/x"])),
            Some(PathBuf::from("/tmp/x"))
        );
        assert_eq!(
            data_dir_arg(args(&["-v", "--data-dir=/tmp/y"])),
            Some(PathBuf::from("/tmp/y"))
        );
        assert_eq!(data_dir_arg(args(&["--data-dir"])), None);
    }
}
