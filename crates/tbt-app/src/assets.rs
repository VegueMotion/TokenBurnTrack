use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

pub const APP_ICON: &str = "app-icon.svg";
pub const GITLAB_ICON: &str = "gitlab.svg";

const APP_ICON_SVG: &[u8] =
    include_bytes!("../../../packaging/icons/io.VagueMotion.TokenBurnTrack.svg");
const GITLAB_ICON_SVG: &[u8] = include_bytes!("../../../assets/icons/gitlab.svg");

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if path == APP_ICON {
            return Ok(Some(Cow::Borrowed(APP_ICON_SVG)));
        }
        if path == GITLAB_ICON {
            return Ok(Some(Cow::Borrowed(GITLAB_ICON_SVG)));
        }
        gpui_kit_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        gpui_kit_assets::Assets.list(path)
    }
}
