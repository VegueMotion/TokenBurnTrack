use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XdgDir {
    Config,
    Data,
}

impl XdgDir {
    fn var(self) -> &'static str {
        match self {
            XdgDir::Config => "XDG_CONFIG_HOME",
            XdgDir::Data => "XDG_DATA_HOME",
        }
    }

    fn fallback(self) -> &'static str {
        match self {
            XdgDir::Config => ".config",
            XdgDir::Data => ".local/share",
        }
    }
}

pub fn flatpak_id() -> Option<&'static str> {
    static ID: OnceLock<Option<String>> = OnceLock::new();
    ID.get_or_init(|| {
        if !cfg!(target_os = "linux") {
            return None;
        }
        flatpak_id_from(
            std::env::var("FLATPAK_ID").ok(),
            std::fs::read_to_string("/.flatpak-info").ok().as_deref(),
        )
    })
    .as_deref()
}

pub fn in_flatpak() -> bool {
    flatpak_id().is_some()
}

pub fn override_command(path: &Path) -> Option<String> {
    let id = flatpak_id().filter(|id| !id.is_empty())?;
    Some(format!(
        "flatpak override --user --filesystem={}:ro {id}",
        path.display()
    ))
}

fn flatpak_id_from(env: Option<String>, info: Option<&str>) -> Option<String> {
    if let Some(id) = env.filter(|v| !v.is_empty()) {
        return Some(id);
    }
    let info = info?;
    let name = info
        .lines()
        .skip_while(|l| l.trim() != "[Application]")
        .skip(1)
        .take_while(|l| !l.trim_start().starts_with('['))
        .find_map(|l| l.trim().strip_prefix("name="))
        .map(str::trim)
        .unwrap_or_default();
    Some(name.to_owned())
}

pub fn host_xdg_var(dir: XdgDir) -> Option<OsString> {
    host_xdg_var_from(dir, in_flatpak(), |k| std::env::var_os(k))
}

pub fn host_xdg_home(dir: XdgDir) -> Option<PathBuf> {
    host_xdg_home_from(dir, host_xdg_var(dir), std::env::home_dir().as_deref())
}

fn host_xdg_var_from(
    dir: XdgDir,
    flatpak: bool,
    var: impl Fn(&str) -> Option<OsString>,
) -> Option<OsString> {
    if flatpak {
        var(&format!("HOST_{}", dir.var()))
    } else {
        var(dir.var())
    }
    .filter(|v| !v.is_empty())
}

fn host_xdg_home_from(
    dir: XdgDir,
    value: Option<OsString>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    value
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.map(|h| h.join(dir.fallback())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatpak_id_prefers_env() {
        assert_eq!(
            flatpak_id_from(Some("a.b.C".into()), Some("[Application]\nname=x.y.Z\n")),
            Some("a.b.C".into())
        );
        assert_eq!(flatpak_id_from(None, None), None);
        assert_eq!(flatpak_id_from(Some(String::new()), None), None);
    }

    #[test]
    fn flatpak_id_reads_info_file() {
        let info = "[Runtime]\nname=org.freedesktop.Platform\n\n[Application]\nruntime=r\nname=io.ex.App\n\n[Instance]\nname=no\n";
        assert_eq!(flatpak_id_from(None, Some(info)), Some("io.ex.App".into()));
        assert_eq!(flatpak_id_from(None, Some("")), Some(String::new()));
    }

    #[test]
    fn host_var_switches_on_flatpak() {
        let env = |k: &str| match k {
            "XDG_DATA_HOME" => Some(OsString::from("/home/u/.var/app/x/data")),
            "HOST_XDG_DATA_HOME" => Some(OsString::from("/home/u/share")),
            _ => None,
        };
        assert_eq!(
            host_xdg_var_from(XdgDir::Data, false, env),
            Some("/home/u/.var/app/x/data".into())
        );
        assert_eq!(
            host_xdg_var_from(XdgDir::Data, true, env),
            Some("/home/u/share".into())
        );
        assert_eq!(host_xdg_var_from(XdgDir::Config, true, env), None);
    }

    #[test]
    fn host_home_falls_back_to_defaults() {
        let home = Path::new("/home/u");
        assert_eq!(
            host_xdg_home_from(XdgDir::Data, None, Some(home)),
            Some(PathBuf::from("/home/u/.local/share"))
        );
        assert_eq!(
            host_xdg_home_from(XdgDir::Config, Some("rel".into()), Some(home)),
            Some(PathBuf::from("/home/u/.config"))
        );
        assert_eq!(
            host_xdg_home_from(XdgDir::Config, Some("/x/cfg".into()), Some(home)),
            Some(PathBuf::from("/x/cfg"))
        );
        assert_eq!(host_xdg_home_from(XdgDir::Data, None, None), None);
    }
}
