use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

pub const APP_ICON: &str = "app-icon.svg";
pub const GITHUB_ICON: &str = "github.svg";

const APP_ICON_SVG: &[u8] =
    include_bytes!("../../../packaging/icons/io.VagueMotion.TokenBurnTrack.svg");
const GITHUB_ICON_SVG: &[u8] = include_bytes!("../../../assets/icons/github.svg");

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if path == APP_ICON {
            return Ok(Some(Cow::Borrowed(APP_ICON_SVG)));
        }
        if path == GITHUB_ICON {
            return Ok(Some(Cow::Borrowed(GITHUB_ICON_SVG)));
        }
        gpui_kit_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        gpui_kit_assets::Assets.list(path)
    }
}
