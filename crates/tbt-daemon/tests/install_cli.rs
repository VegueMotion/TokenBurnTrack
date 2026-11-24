use std::path::Path;
use std::process::{Command, Output};

fn daemon(home: &Path, args: &[&str], envs: &[(&str, &Path)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tbt-daemon"));
    cmd.args(args)
        .env_clear()
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("PATH", "/nonexistent");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn is_empty_dir(dir: &Path) -> bool {
    std::fs::read_dir(dir).unwrap().next().is_none()
}

#[test]
fn help_lists_subcommands() {
    let home = tempfile::tempdir().unwrap();
    let out = daemon(home.path(), &["--help"], &[]);
    assert!(out.status.success());
    let help = text(&out.stdout);
    for word in ["install", "uninstall", "--once", "--data-dir", "--trigger"] {
        assert!(help.contains(word), "{word} missing from:\n{help}");
    }
}

#[test]
fn run_flags_conflict_with_subcommands() {
    let home = tempfile::tempdir().unwrap();
    let out = daemon(home.path(), &["--once", "install", "--dry-run"], &[]);
    assert!(!out.status.success());
    assert!(is_empty_dir(home.path()));
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn install_dry_run_prints_plan_and_touches_nothing() {
    let home = tempfile::tempdir().unwrap();
    let out = daemon(
        home.path(),
        &[
            "install",
            "--dry-run",
            "--bin",
            "/opt/tbt/tbt-daemon",
            "--app",
            "/opt/tbt/tbt-app",
            "--data-dir",
            "/srv/tbt data",
        ],
        &[],
    );
    assert!(out.status.success(), "{out:?}");
    let stdout = text(&out.stdout);
    let units = home.path().join(".config/systemd/user");
    assert!(stdout.contains(&format!(
        "write {}",
        units.join("tbt-daemon.service").display()
    )));
    assert!(
        stdout.contains("    ExecStart=/opt/tbt/tbt-daemon --once --data-dir \"/srv/tbt data\"\n")
    );
    assert!(stdout.contains("    ReadWritePaths=\"/srv/tbt data\"\n"));
    assert!(stdout.contains("    OnCalendar=hourly\n"));
    assert!(stdout.contains("    Exec=/opt/tbt/tbt-app\n"));
    assert!(stdout.contains(&format!(
        "write {}",
        home.path()
            .join(".local/share/icons/hicolor/scalable/apps/io.VagueMotion.TokenBurnTrack.svg")
            .display()
    )));
    assert!(stdout.contains("run systemctl --user daemon-reload\n"));
    assert!(stdout.contains("run systemctl --user enable --now tbt-daemon.timer\n"));
    assert!(is_empty_dir(home.path()));
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn install_dry_run_defaults_and_env_data_dir() {
    let home = tempfile::tempdir().unwrap();
    let out = daemon(home.path(), &["install", "--dry-run", "--no-enable"], &[]);
    assert!(out.status.success(), "{out:?}");
    let stdout = text(&out.stdout);
    assert!(stdout.contains(&format!(
        "    ExecStart={} --once\n",
        env!("CARGO_BIN_EXE_tbt-daemon")
    )));
    assert!(stdout.contains(&format!(
        "    ReadWritePaths={}\n",
        home.path().join(".local/share/tokenburntrack").display()
    )));
    assert!(!stdout.contains("enable --now"));
    assert!(!stdout.contains(".desktop"));

    let data = home.path().join("data");
    let out = daemon(
        home.path(),
        &["install", "--dry-run"],
        &[("TBT_DATA_DIR", &data)],
    );
    assert!(out.status.success(), "{out:?}");
    assert!(text(&out.stdout).contains(&format!(" --once --data-dir {}\n", data.display())));
    assert!(is_empty_dir(home.path()));
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn uninstall_dry_run_touches_nothing() {
    let home = tempfile::tempdir().unwrap();
    let out = daemon(home.path(), &["uninstall", "--dry-run"], &[]);
    assert!(out.status.success(), "{out:?}");
    let stdout = text(&out.stdout);
    assert!(stdout.contains("run systemctl --user disable --now tbt-daemon.timer\n"));
    assert!(stdout.contains(&format!(
        "rm -f {}",
        home.path()
            .join(".config/systemd/user/tbt-daemon.timer")
            .display()
    )));
    assert!(is_empty_dir(home.path()));
}

#[test]
fn install_rejects_missing_binary() {
    let home = tempfile::tempdir().unwrap();
    let out = daemon(
        home.path(),
        &["install", "--no-enable", "--bin", "/nonexistent/tbt-daemon"],
        &[],
    );
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("is not a file"), "{out:?}");
    assert!(is_empty_dir(home.path()));
}
