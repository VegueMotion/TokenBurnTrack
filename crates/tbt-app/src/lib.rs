pub mod app_state;
mod assets;
pub mod background;
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

use crate::collector_bridge::SyncSource;
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

fn backend() -> (Arc<dyn DataSource>, SyncSource) {
    if std::env::var("TBT_MOCK").is_ok_and(|v| v == "1") {
        return (
            Arc::new(MockData::new(0x7b7, data::now())),
            SyncSource::Mock,
        );
    }
    let (dir, sync) = collector_bridge::resolve_sync(
        data_dir_arg(std::env::args().skip(1)),
        tbt_collector::CollectorConfig::load_default(),
    );
    (Arc::new(StoreData::from_data_dir(dir)), sync)
}

fn report_error(msg: &str) {
    eprintln!("{msg}");
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
        let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let (text, title) = (wide(msg), wide("TokenBurnTrack"));
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_ICONERROR | MB_OK,
            );
        }
    }
}

pub fn run() {
    let (data, sync) = backend();
    gpui_platform::application()
        .with_assets(assets::AppAssets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            cx.on_window_closed(|cx, _| cx.quit()).detach();
            if let Err(err) =
                workspace::open_main_window(data.clone(), sync.clone(), start_page(), cx)
            {
                report_error(&format!("failed to open window: {err:#}"));
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
