use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tbt_collector::{CollectorLock, LockAttempt};

fn copy_tree(from: &Path, to: &Path) {
    if from.is_dir() {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            copy_tree(&entry.path(), &to.join(entry.file_name()));
        }
    } else {
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(from, to).unwrap();
    }
}

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Sandbox {
        let dir = tempfile::tempdir().unwrap();
        let fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/claude_code");
        for version in std::fs::read_dir(&fixtures).unwrap() {
            let projects = version.unwrap().path().join("projects");
            copy_tree(&projects, &dir.path().join("claude/projects"));
        }
        Sandbox { dir }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn daemon(&self, args: &[&str]) -> Output {
        let home = self.path("home");
        std::fs::create_dir_all(&home).unwrap();
        Command::new(env!("CARGO_BIN_EXE_tbt-daemon"))
            .args(args)
            .arg("--config")
            .arg(self.path("config/config.toml"))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("CLAUDE_CONFIG_DIR", self.path("claude"))
            .env("TBT_DATA_DIR", self.path("data"))
            .output()
            .unwrap()
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn once_ingests_fixtures_into_the_data_dir() {
    let sb = Sandbox::new();
    let out = sb.daemon(&["--once"]);
    assert!(out.status.success(), "{out:?}");
    let line = stdout(&out);
    assert!(line.contains("(hourly)"), "{line}");
    assert!(line.contains("204 new events"), "{line}");

    let db = rusqlite::Connection::open(sb.path("data/tbt.sqlite")).unwrap();
    let events: i64 = db
        .query_row("SELECT count(*) FROM usage_event", [], |r| r.get(0))
        .unwrap();
    assert_eq!(events, 204);
    let priced: i64 = db
        .query_row("SELECT count(*) FROM model_price", [], |r| r.get(0))
        .unwrap();
    assert!(priced > 0);
    let logs: Vec<_> = std::fs::read_dir(sb.path("data/logs"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(logs.iter().any(|n| n.starts_with("tbt-daemon")), "{logs:?}");
    assert!(!sb.path("home/.claude").exists());

    let again = sb.daemon(&["--once", "--trigger", "manual"]);
    assert!(again.status.success(), "{again:?}");
    let line = stdout(&again);
    assert!(
        line.contains("(manual)") && line.contains(" 0 new events"),
        "{line}"
    );
}

#[test]
fn once_with_busy_lock_exits_zero_and_skips() {
    let sb = Sandbox::new();
    let LockAttempt::Acquired(lock) = CollectorLock::try_acquire(&sb.path("data")).unwrap() else {
        panic!("lock should be free");
    };
    let out = sb.daemon(&["--once"]);
    assert!(out.status.success(), "{out:?}");
    assert!(stdout(&out).contains("busy"), "{}", stdout(&out));
    assert!(!sb.path("data/tbt.sqlite").exists());
    drop(lock);
}

#[test]
fn data_dir_flag_overrides_the_environment() {
    let sb = Sandbox::new();
    let other = sb.path("other");
    let LockAttempt::Acquired(_lock) = CollectorLock::try_acquire(&other).unwrap() else {
        panic!("lock should be free");
    };
    let out = sb.daemon(&["--once", "--data-dir", other.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    assert!(stdout(&out).contains("busy"));
    assert!(other.join("logs").is_dir());
    assert!(!sb.path("data").exists());
}

#[test]
fn invalid_config_fails() {
    let sb = Sandbox::new();
    std::fs::create_dir_all(sb.path("config")).unwrap();
    std::fs::write(sb.path("config/config.toml"), "privacy = \"loud\"").unwrap();
    let out = sb.daemon(&["--once"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("config.toml"));
}
