use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

use crate::templates;

pub const SERVICE_FILE: &str = "tbt-daemon.service";
pub const TIMER_FILE: &str = "tbt-daemon.timer";
pub const DESKTOP_FILE: &str = "io.VagueMotion.TokenBurnTrack.desktop";
pub const ICON_FILE: &str = "io.VagueMotion.TokenBurnTrack.svg";
const LEGACY_DESKTOP_FILE: &str = "tbt-app.desktop";
const LEGACY_ICON_FILE: &str = "tbt-app.svg";
pub const LAUNCHD_LABEL: &str = "com.tokenburntrack.daemon";
pub const TASK_NAME: &str = r"TokenBurnTrack\tbt-daemon";
pub const TASK_FILE: &str = "tbt-daemon-task.xml";
const APP_DIR: &str = "tokenburntrack";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Systemd,
    Launchd,
    TaskScheduler,
}

impl Platform {
    pub fn current() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::Launchd
        } else if cfg!(windows) {
            Platform::TaskScheduler
        } else {
            Platform::Systemd
        }
    }
}

#[derive(Debug, Clone)]
pub struct Target {
    pub platform: Platform,
    pub home: PathBuf,
    pub config_home: PathBuf,
    pub data_home: PathBuf,
    pub launchd_domain: String,
}

impl Target {
    pub fn detect() -> anyhow::Result<Target> {
        let base = directories::BaseDirs::new().context("cannot determine the home directory")?;
        let platform = Platform::current();
        let home = base.home_dir().to_path_buf();
        let launchd_domain = match platform {
            Platform::Launchd => format!("gui/{}", user_id(&home)?),
            _ => String::new(),
        };
        Ok(Target {
            platform,
            config_home: base.config_dir().to_path_buf(),
            data_home: base.data_dir().to_path_buf(),
            home,
            launchd_domain,
        })
    }

    fn systemd_dir(&self) -> PathBuf {
        self.config_home.join("systemd").join("user")
    }

    fn applications_dir(&self) -> PathBuf {
        self.data_home.join("applications")
    }

    fn icon_dir(&self) -> PathBuf {
        self.data_home.join("icons/hicolor/scalable/apps")
    }

    fn plist_path(&self) -> PathBuf {
        self.home
            .join("Library/LaunchAgents")
            .join(format!("{LAUNCHD_LABEL}.plist"))
    }

    fn task_xml_path(&self) -> PathBuf {
        self.config_home.join(APP_DIR).join(TASK_FILE)
    }
}

#[cfg(unix)]
fn user_id(home: &Path) -> anyhow::Result<u32> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(home)
        .with_context(|| format!("cannot stat home directory {}", home.display()))?;
    Ok(meta.uid())
}

#[cfg(not(unix))]
fn user_id(_home: &Path) -> anyhow::Result<u32> {
    bail!("launchd is only available on macOS")
}

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub bin: PathBuf,
    pub app: Option<PathBuf>,
    pub data_dir: PathBuf,
    pub daemon_args: Vec<String>,
    pub enable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
}

impl Cmd {
    fn new(program: &str, args: &[&str]) -> Cmd {
        Cmd {
            program: program.to_owned(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
        }
    }
}

impl fmt::Display for Cmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, word) in std::iter::once(&self.program).chain(&self.args).enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            if word.is_empty() || word.chars().any(|c| c.is_whitespace() || c == '"') {
                write!(f, "\"{}\"", word.replace('"', "\\\""))?;
            } else {
                f.write_str(word)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    CreateDir(PathBuf),
    Write { path: PathBuf, contents: String },
    Remove(PathBuf),
    Run { cmd: Cmd, must_succeed: bool },
}

impl Action {
    fn write(path: PathBuf, contents: impl Into<String>) -> Action {
        Action::Write {
            path,
            contents: contents.into(),
        }
    }

    fn run(cmd: Cmd) -> Action {
        Action::Run {
            cmd,
            must_succeed: true,
        }
    }

    fn try_run(cmd: Cmd) -> Action {
        Action::Run {
            cmd,
            must_succeed: false,
        }
    }
}

pub trait Runner {
    fn run(&mut self, cmd: &Cmd) -> std::io::Result<bool>;
}

pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&mut self, cmd: &Cmd) -> std::io::Result<bool> {
        let status = std::process::Command::new(&cmd.program)
            .args(&cmd.args)
            .status()?;
        Ok(status.success())
    }
}

fn utf8(path: &Path) -> anyhow::Result<&str> {
    match path.to_str() {
        Some(s) if !s.chars().any(char::is_control) => Ok(s),
        _ => bail!("unsupported characters in path {}", path.display()),
    }
}

fn systemctl(args: &[&str]) -> Cmd {
    let mut all = vec!["--user"];
    all.extend_from_slice(args);
    Cmd::new("systemctl", &all)
}

pub fn install_plan(t: &Target, o: &InstallOptions) -> anyhow::Result<Vec<Action>> {
    let bin = utf8(&o.bin)?;
    let mut plan = Vec::new();
    match t.platform {
        Platform::Systemd => {
            let unit_dir = t.systemd_dir();
            plan.push(Action::CreateDir(o.data_dir.clone()));
            plan.push(Action::CreateDir(unit_dir.clone()));
            plan.push(Action::write(
                unit_dir.join(SERVICE_FILE),
                templates::systemd_service(bin, &o.daemon_args, utf8(&o.data_dir)?),
            ));
            plan.push(Action::write(unit_dir.join(TIMER_FILE), templates::TIMER));
            if let Some(app) = &o.app {
                plan.push(Action::Remove(
                    t.applications_dir().join(LEGACY_DESKTOP_FILE),
                ));
                plan.push(Action::Remove(t.icon_dir().join(LEGACY_ICON_FILE)));
                plan.push(Action::CreateDir(t.applications_dir()));
                plan.push(Action::write(
                    t.applications_dir().join(DESKTOP_FILE),
                    templates::desktop_entry(Some(utf8(app)?)),
                ));
                plan.push(Action::CreateDir(t.icon_dir()));
                plan.push(Action::write(
                    t.icon_dir().join(ICON_FILE),
                    templates::ICON_SVG,
                ));
            }
            plan.push(Action::run(systemctl(&["daemon-reload"])));
            if o.enable {
                plan.push(Action::run(systemctl(&["enable", "--now", TIMER_FILE])));
            }
        }
        Platform::Launchd => {
            let plist = t.plist_path();
            let log_dir = o.data_dir.join(crate::LOG_DIR);
            plan.push(Action::CreateDir(log_dir.clone()));
            plan.push(Action::CreateDir(
                plist.parent().expect("plist has a parent").to_path_buf(),
            ));
            plan.push(Action::write(
                plist.clone(),
                templates::launchd_plist(bin, &o.daemon_args, utf8(&log_dir)?),
            ));
            if o.enable {
                let service = format!("{}/{LAUNCHD_LABEL}", t.launchd_domain);
                plan.push(Action::try_run(Cmd::new(
                    "launchctl",
                    &["bootout", &service],
                )));
                plan.push(Action::run(Cmd::new(
                    "launchctl",
                    &["bootstrap", &t.launchd_domain, utf8(&plist)?],
                )));
            }
        }
        Platform::TaskScheduler => {
            let xml = t.task_xml_path();
            plan.push(Action::CreateDir(
                xml.parent().expect("task file has a parent").to_path_buf(),
            ));
            plan.push(Action::write(
                xml.clone(),
                templates::task_xml(bin, &o.daemon_args),
            ));
            if o.enable {
                plan.push(Action::run(Cmd::new(
                    "schtasks",
                    &["/Create", "/TN", TASK_NAME, "/XML", utf8(&xml)?, "/F"],
                )));
            }
        }
    }
    Ok(plan)
}

pub fn uninstall_plan(t: &Target) -> Vec<Action> {
    match t.platform {
        Platform::Systemd => vec![
            Action::try_run(systemctl(&["disable", "--now", TIMER_FILE])),
            Action::try_run(systemctl(&["stop", SERVICE_FILE])),
            Action::Remove(t.systemd_dir().join(TIMER_FILE)),
            Action::Remove(t.systemd_dir().join(SERVICE_FILE)),
            Action::Remove(t.applications_dir().join(DESKTOP_FILE)),
            Action::Remove(t.icon_dir().join(ICON_FILE)),
            Action::Remove(t.applications_dir().join(LEGACY_DESKTOP_FILE)),
            Action::Remove(t.icon_dir().join(LEGACY_ICON_FILE)),
            Action::try_run(systemctl(&["daemon-reload"])),
        ],
        Platform::Launchd => vec![
            Action::try_run(Cmd::new(
                "launchctl",
                &["bootout", &format!("{}/{LAUNCHD_LABEL}", t.launchd_domain)],
            )),
            Action::Remove(t.plist_path()),
        ],
        Platform::TaskScheduler => vec![
            Action::try_run(Cmd::new("schtasks", &["/Delete", "/TN", TASK_NAME, "/F"])),
            Action::Remove(t.task_xml_path()),
        ],
    }
}

pub fn execute(
    plan: &[Action],
    dry_run: bool,
    runner: &mut dyn Runner,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    if dry_run {
        writeln!(out, "dry run: nothing is written or run")?;
    }
    for action in plan {
        match action {
            Action::CreateDir(dir) => {
                writeln!(out, "mkdir -p {}", dir.display())?;
                if !dry_run {
                    std::fs::create_dir_all(dir)
                        .with_context(|| format!("cannot create {}", dir.display()))?;
                }
            }
            Action::Write { path, contents } => {
                writeln!(out, "write {}", path.display())?;
                if dry_run {
                    for line in contents.lines() {
                        if line.is_empty() {
                            writeln!(out)?;
                        } else {
                            writeln!(out, "    {line}")?;
                        }
                    }
                } else {
                    std::fs::write(path, contents)
                        .with_context(|| format!("cannot write {}", path.display()))?;
                }
            }
            Action::Remove(path) => {
                if dry_run {
                    writeln!(out, "rm -f {}", path.display())?;
                    continue;
                }
                match std::fs::remove_file(path) {
                    Ok(()) => writeln!(out, "rm -f {}", path.display())?,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => {
                        return Err(e).with_context(|| format!("cannot remove {}", path.display()));
                    }
                }
            }
            Action::Run { cmd, must_succeed } => {
                writeln!(out, "run {cmd}")?;
                if dry_run {
                    continue;
                }
                let result = runner.run(cmd);
                match (result, must_succeed) {
                    (Ok(true), _) => {}
                    (Ok(false), true) => bail!("command failed: {cmd}"),
                    (Err(e), true) => {
                        return Err(e).with_context(|| format!("cannot run {cmd}"));
                    }
                    (Ok(false), false) => writeln!(out, "  (failed, continuing)")?,
                    (Err(e), false) => writeln!(out, "  (cannot run: {e}; continuing)")?,
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeRunner {
        calls: Vec<String>,
        fail: bool,
    }

    impl Runner for FakeRunner {
        fn run(&mut self, cmd: &Cmd) -> std::io::Result<bool> {
            self.calls.push(cmd.to_string());
            Ok(!self.fail)
        }
    }

    struct Sandbox {
        dir: tempfile::TempDir,
    }

    impl Sandbox {
        fn new() -> Sandbox {
            Sandbox {
                dir: tempfile::tempdir().unwrap(),
            }
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.dir.path().join(rel)
        }

        fn target(&self, platform: Platform) -> Target {
            Target {
                platform,
                home: self.path("home"),
                config_home: self.path("home/.config"),
                data_home: self.path("home/.local/share"),
                launchd_domain: "gui/501".into(),
            }
        }

        fn options(&self, app: bool, enable: bool) -> InstallOptions {
            InstallOptions {
                bin: self.path("bin/tbt-daemon"),
                app: app.then(|| self.path("bin/tbt-app")),
                data_dir: self.path("home/.local/share/tokenburntrack"),
                daemon_args: Vec::new(),
                enable,
            }
        }

        fn files(&self) -> Vec<String> {
            fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
                for entry in std::fs::read_dir(dir).unwrap() {
                    let path = entry.unwrap().path();
                    if path.is_dir() {
                        walk(&path, root, out);
                    } else {
                        out.push(path.strip_prefix(root).unwrap().display().to_string());
                    }
                }
            }
            let mut out = Vec::new();
            walk(self.dir.path(), self.dir.path(), &mut out);
            out.sort();
            out
        }

        fn read(&self, rel: &str) -> String {
            std::fs::read_to_string(self.path(rel)).unwrap()
        }
    }

    fn apply(plan: &[Action], dry_run: bool, runner: &mut FakeRunner) -> String {
        let mut out = Vec::new();
        execute(plan, dry_run, runner, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn systemd_install_and_uninstall() {
        let sb = Sandbox::new();
        let t = sb.target(Platform::Systemd);
        let o = sb.options(true, false);
        let mut runner = FakeRunner::default();
        apply(&install_plan(&t, &o).unwrap(), false, &mut runner);

        assert_eq!(
            sb.files(),
            [
                "home/.config/systemd/user/tbt-daemon.service",
                "home/.config/systemd/user/tbt-daemon.timer",
                "home/.local/share/applications/io.VagueMotion.TokenBurnTrack.desktop",
                "home/.local/share/icons/hicolor/scalable/apps/io.VagueMotion.TokenBurnTrack.svg",
            ]
        );
        assert!(sb.path("home/.local/share/tokenburntrack").is_dir());
        let service = sb.read("home/.config/systemd/user/tbt-daemon.service");
        let bin = sb.path("bin/tbt-daemon");
        assert!(service.contains(&format!("\nExecStart={} --once\n", bin.display())));
        assert!(service.contains(&format!(
            "\nReadWritePaths={}\n",
            sb.path("home/.local/share/tokenburntrack").display()
        )));
        assert_eq!(
            sb.read("home/.config/systemd/user/tbt-daemon.timer"),
            templates::TIMER
        );
        let desktop =
            sb.read("home/.local/share/applications/io.VagueMotion.TokenBurnTrack.desktop");
        assert!(desktop.contains(&format!("\nExec={}\n", sb.path("bin/tbt-app").display())));
        assert_eq!(
            sb.read(
                "home/.local/share/icons/hicolor/scalable/apps/io.VagueMotion.TokenBurnTrack.svg"
            ),
            templates::ICON_SVG
        );
        assert_eq!(runner.calls, ["systemctl --user daemon-reload"]);

        let mut runner = FakeRunner::default();
        apply(&uninstall_plan(&t), false, &mut runner);
        assert!(sb.files().is_empty(), "{:?}", sb.files());
        assert!(sb.path("home/.local/share/tokenburntrack").is_dir());
        assert_eq!(
            runner.calls,
            [
                "systemctl --user disable --now tbt-daemon.timer",
                "systemctl --user stop tbt-daemon.service",
                "systemctl --user daemon-reload",
            ]
        );
    }

    #[test]
    fn systemd_install_enables_timer_and_passes_daemon_args() {
        let sb = Sandbox::new();
        let t = sb.target(Platform::Systemd);
        let mut o = sb.options(false, true);
        o.daemon_args = vec!["--config".into(), "/etc/tbt.toml".into()];
        let mut runner = FakeRunner::default();
        apply(&install_plan(&t, &o).unwrap(), false, &mut runner);
        assert_eq!(
            runner.calls,
            [
                "systemctl --user daemon-reload",
                "systemctl --user enable --now tbt-daemon.timer",
            ]
        );
        assert!(!sb.path("home/.local/share/applications").exists());
        let service = sb.read("home/.config/systemd/user/tbt-daemon.service");
        assert!(
            service.contains(" --once --config /etc/tbt.toml\n"),
            "{service}"
        );
    }

    #[test]
    fn failing_required_command_stops_install() {
        let sb = Sandbox::new();
        let t = sb.target(Platform::Systemd);
        let o = sb.options(false, true);
        let mut runner = FakeRunner {
            fail: true,
            ..FakeRunner::default()
        };
        let mut out = Vec::new();
        let err =
            execute(&install_plan(&t, &o).unwrap(), false, &mut runner, &mut out).unwrap_err();
        assert!(err.to_string().contains("daemon-reload"), "{err}");
        assert_eq!(runner.calls.len(), 1);
    }

    #[test]
    fn uninstall_tolerates_missing_install() {
        let sb = Sandbox::new();
        let mut runner = FakeRunner {
            fail: true,
            ..FakeRunner::default()
        };
        let out = apply(
            &uninstall_plan(&sb.target(Platform::Systemd)),
            false,
            &mut runner,
        );
        assert_eq!(runner.calls.len(), 3);
        assert!(out.contains("failed, continuing"));
        assert!(!out.contains("rm -f"), "{out}");
    }

    #[test]
    fn dry_run_writes_and_runs_nothing() {
        let sb = Sandbox::new();
        let t = sb.target(Platform::Systemd);
        let o = sb.options(true, true);
        let mut runner = FakeRunner::default();
        let out = apply(&install_plan(&t, &o).unwrap(), true, &mut runner);
        assert!(runner.calls.is_empty());
        assert!(sb.files().is_empty());
        assert!(!sb.path("home").exists());
        assert!(out.starts_with("dry run"));
        assert!(out.contains(&format!(
            "write {}",
            sb.path("home/.config/systemd/user/tbt-daemon.timer").display()
        )));
        assert!(out.contains("    OnCalendar=hourly\n"));
        assert!(out.contains("    Type=oneshot\n"));
        assert!(out.contains("    StartupWMClass=io.VagueMotion.TokenBurnTrack\n"));
        assert!(out.contains("run systemctl --user daemon-reload\n"));
        assert!(out.contains("run systemctl --user enable --now tbt-daemon.timer\n"));

        let out = apply(&uninstall_plan(&t), true, &mut runner);
        assert!(runner.calls.is_empty());
        assert!(out.contains("run systemctl --user disable --now tbt-daemon.timer\n"));
        assert!(out.contains("rm -f "));
    }

    #[test]
    fn launchd_install_and_uninstall() {
        let sb = Sandbox::new();
        let t = sb.target(Platform::Launchd);
        let o = sb.options(false, true);
        let mut runner = FakeRunner::default();
        apply(&install_plan(&t, &o).unwrap(), false, &mut runner);
        let plist_rel = "home/Library/LaunchAgents/com.tokenburntrack.daemon.plist";
        assert_eq!(sb.files(), [plist_rel]);
        assert!(sb.path("home/.local/share/tokenburntrack/logs").is_dir());
        let plist = sb.read(plist_rel);
        assert!(plist.contains(&format!(
            "<string>{}</string>",
            sb.path("bin/tbt-daemon").display()
        )));
        assert!(plist.contains("/tokenburntrack/logs/launchd-stdout.log</string>"));
        assert_eq!(
            runner.calls,
            [
                "launchctl bootout gui/501/com.tokenburntrack.daemon".to_owned(),
                format!(
                    "launchctl bootstrap gui/501 {}",
                    sb.path(plist_rel).display()
                ),
            ]
        );

        let mut runner = FakeRunner::default();
        apply(&uninstall_plan(&t), false, &mut runner);
        assert!(sb.files().is_empty());
        assert_eq!(
            runner.calls,
            ["launchctl bootout gui/501/com.tokenburntrack.daemon"]
        );
    }

    #[test]
    fn task_scheduler_install_and_uninstall() {
        let sb = Sandbox::new();
        let t = sb.target(Platform::TaskScheduler);
        let o = sb.options(false, true);
        let mut runner = FakeRunner::default();
        apply(&install_plan(&t, &o).unwrap(), false, &mut runner);
        let xml_rel = "home/.config/tokenburntrack/tbt-daemon-task.xml";
        assert_eq!(sb.files(), [xml_rel]);
        assert!(sb.read(xml_rel).contains(&format!(
            "<Command>{}</Command>",
            sb.path("bin/tbt-daemon").display()
        )));
        assert_eq!(
            runner.calls,
            [format!(
                r"schtasks /Create /TN TokenBurnTrack\tbt-daemon /XML {} /F",
                sb.path(xml_rel).display()
            )]
        );

        let mut runner = FakeRunner::default();
        apply(&uninstall_plan(&t), false, &mut runner);
        assert!(sb.files().is_empty());
        assert_eq!(
            runner.calls,
            [r"schtasks /Delete /TN TokenBurnTrack\tbt-daemon /F"]
        );
    }

    #[test]
    fn cmd_display_quotes_spaces() {
        let cmd = Cmd::new("launchctl", &["bootstrap", "gui/501", "/Users/a b/x.plist"]);
        assert_eq!(
            cmd.to_string(),
            "launchctl bootstrap gui/501 \"/Users/a b/x.plist\""
        );
    }
}
