use std::path::Path;

pub const APP_NAME: &str = "TokenBurnTrack";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const LICENSE: &str = env!("CARGO_PKG_LICENSE");
pub const REPO_URL: &str = "https://github.com/VegueMotion/TokenBurnTrack";
pub const ISSUES_URL: &str = "https://github.com/VegueMotion/TokenBurnTrack/issues";
pub const LICENSE_URL: &str = "https://www.gnu.org/licenses/gpl-3.0.html";

pub fn build_kind() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

pub fn platform() -> String {
    format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
}

pub fn rows(db: Option<&Path>, config: Option<&Path>) -> Vec<(&'static str, String)> {
    let show = |p: Option<&Path>, none: &str| {
        p.map(|p| p.display().to_string())
            .unwrap_or_else(|| none.to_owned())
    };
    vec![
        ("Version", format!("{VERSION} ({} build)", build_kind())),
        ("License", LICENSE.to_owned()),
        ("Platform", platform()),
        ("Database", show(db, "none (mock data)")),
        ("Config file", show(config, "unknown")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_show_version_license_and_paths() {
        let rows = rows(
            Some(Path::new("/data/tbt.sqlite")),
            Some(Path::new("/cfg/config.toml")),
        );
        let get = |k: &str| rows.iter().find(|(l, _)| *l == k).unwrap().1.clone();
        assert!(get("Version").starts_with(&format!("{VERSION} (")));
        assert_eq!(get("License"), "GPL-3.0-or-later");
        assert_eq!(get("Database"), "/data/tbt.sqlite");
        assert_eq!(get("Config file"), "/cfg/config.toml");
        assert!(get("Platform").contains(std::env::consts::OS));
    }

    #[test]
    fn rows_without_paths_say_so() {
        let rows = rows(None, None);
        assert_eq!(rows[3].1, "none (mock data)");
        assert_eq!(rows[4].1, "unknown");
    }
}
