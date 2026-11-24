use std::path::PathBuf;

use tbt_core::sandbox::{self, XdgDir};

pub const DATA_DIR_ENV: &str = "TBT_DATA_DIR";
pub const DB_FILE: &str = "tbt.sqlite";
pub const APP_DIR: &str = "tokenburntrack";

pub fn default_data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(DATA_DIR_ENV).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    app_dir(XdgDir::Data)
}

pub fn app_dir(dir: XdgDir) -> Option<PathBuf> {
    if sandbox::in_flatpak() {
        return sandbox::host_xdg_home(dir).map(|d| d.join(APP_DIR));
    }
    let dirs = directories::ProjectDirs::from("", "", APP_DIR)?;
    Some(
        match dir {
            XdgDir::Config => dirs.config_dir(),
            XdgDir::Data => dirs.data_dir(),
        }
        .to_path_buf(),
    )
}

pub fn db_path(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join(DB_FILE)
}
