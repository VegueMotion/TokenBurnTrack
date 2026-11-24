use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use tbt_collector::{CollectorLock, LockAttempt};

const SOURCES: u64 = 26;
const EVENTS: i64 = 640;
const GOLDEN: &[(&str, i64)] = &[
    ("claude_code", 204),
    ("codex", 381),
    ("gemini_cli", 12),
    ("in_house_agent", 4),
    ("llm_proxy", 6),
    ("opencode", 33),
];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

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

fn files_under(root: &Path, out: &mut Vec<PathBuf>) {
    if root.is_dir() {
        for entry in std::fs::read_dir(root).unwrap() {
            files_under(&entry.unwrap().path(), out);
        }
    } else {
        out.push(root.to_path_buf());
    }
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    out.sort();
    out
}

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Sandbox {
        let sb = Sandbox {
            dir: tempfile::tempdir().unwrap(),
        };
        let f = fixtures();
        for version in subdirs(&f.join("claude_code")) {
            copy_tree(&version.join("projects"), &sb.path("claude/projects"));
        }
        for version in subdirs(&f.join("codex")) {
            copy_tree(&version.join("sessions"), &sb.path("codex/sessions"));
        }
        copy_tree(
            &f.join("opencode/1.19.5/opencode.db"),
            &sb.path("home/.local/share/opencode/opencode.db"),
        );
        copy_tree(
            &f.join("gemini_cli/0.71.0-nightly/tmp"),
            &sb.path("gemini/.gemini/tmp"),
        );
        for set in ["openai_proxy", "anthropic_style"] {
            copy_tree(
                &f.join("generic_jsonl").join(set),
                &sb.path(&format!("generic/{set}")),
            );
        }
        let config = std::fs::read_to_string(f.join("generic_jsonl/config.toml"))
            .unwrap()
            .replace(
                "~/.local/share/llm-proxy/logs",
                &sb.path("generic/openai_proxy").to_string_lossy(),
            )
            .replace(
                "~/.in-house-agent",
                &sb.path("generic/anthropic_style").to_string_lossy(),
            );
        std::fs::create_dir_all(sb.path("home/.config/tokenburntrack")).unwrap();
        std::fs::write(sb.path("home/.config/tokenburntrack/config.toml"), config).unwrap();

        let mut files = Vec::new();
        files_under(sb.dir.path(), &mut files);
        files.sort();
        let base = SystemTime::now() - Duration::from_secs(3 * 86_400);
        for (i, file) in files.iter().enumerate() {
            std::fs::File::options()
                .write(true)
                .open(file)
                .unwrap()
                .set_modified(base + Duration::from_secs(i as u64))
                .unwrap();
        }
        sb
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn home_files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        files_under(&self.path("home"), &mut out);
        out.sort();
        out
    }

    fn daemon(&self, args: &[&str]) -> Output {
        let home = self.path("home");
        Command::new(env!("CARGO_BIN_EXE_tbt-daemon"))
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("CLAUDE_CONFIG_DIR", self.path("claude"))
            .env("CODEX_HOME", self.path("codex"))
            .env("GEMINI_CLI_HOME", self.path("gemini"))
            .env("TBT_DATA_DIR", self.path("data"))
            .output()
            .unwrap()
    }

    fn per_harness(&self) -> BTreeMap<String, i64> {
        let db = rusqlite::Connection::open(self.path("data/tbt.sqlite")).unwrap();
        let mut stmt = db
            .prepare("SELECT harness_id, count(*) FROM usage_event GROUP BY harness_id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn golden() -> BTreeMap<String, i64> {
    GOLDEN.iter().map(|(h, n)| (h.to_string(), *n)).collect()
}

#[test]
fn once_twice_backfills_every_harness_then_finds_nothing_new() {
    let sb = Sandbox::new();
    let home_before = sb.home_files();

    let first = sb.daemon(&["--once"]);
    assert_eq!(first.status.code(), Some(0), "{first:?}");
    let line = stdout(&first);
    assert!(line.contains("(hourly)"), "{line}");
    assert!(
        line.contains(&format!(
            "{SOURCES} sources scanned, {SOURCES} read, 0 unchanged, 0 failed, 0 missing; {EVENTS} new events"
        )),
        "{line}"
    );
    assert!(line.contains(" 5 warnings"), "{line}");
    assert_eq!(sb.per_harness(), golden());

    let logs: Vec<String> = std::fs::read_dir(sb.path("data/logs"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(logs.iter().any(|n| n.starts_with("tbt-daemon")), "{logs:?}");

    let second = sb.daemon(&["--once"]);
    assert_eq!(second.status.code(), Some(0), "{second:?}");
    let line = stdout(&second);
    assert!(
        line.contains(&format!(
            "{SOURCES} sources scanned, 0 read, {SOURCES} unchanged, 0 failed, 0 missing; 0 new events, 0 new messages, 0 warnings"
        )),
        "{line}"
    );
    assert_eq!(sb.per_harness(), golden());

    assert_eq!(sb.home_files(), home_before, "the daemon wrote under HOME");
    for dir in [".claude", ".codex", ".gemini"] {
        assert!(!sb.path("home").join(dir).exists(), "{dir}");
    }
    let db = rusqlite::Connection::open(sb.path("data/tbt.sqlite")).unwrap();
    let runs: Vec<(String, i64)> = db
        .prepare("SELECT trigger, events_new FROM ingest_run ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        runs,
        [("hourly".to_owned(), EVENTS), ("hourly".to_owned(), 0)]
    );
}

#[test]
fn second_process_while_the_lock_is_held_is_busy_and_exits_zero() {
    let sb = Sandbox::new();
    assert_eq!(sb.daemon(&["--once"]).status.code(), Some(0));

    let LockAttempt::Acquired(lock) = CollectorLock::try_acquire(&sb.path("data")).unwrap() else {
        panic!("lock should be free");
    };
    let busy = sb.daemon(&["--once", "--trigger", "manual"]);
    assert_eq!(busy.status.code(), Some(0), "{busy:?}");
    assert!(stdout(&busy).contains("busy"), "{}", stdout(&busy));
    assert!(!stdout(&busy).contains("new events"), "{}", stdout(&busy));
    drop(lock);

    let db = rusqlite::Connection::open(sb.path("data/tbt.sqlite")).unwrap();
    let runs: i64 = db
        .query_row("SELECT count(*) FROM ingest_run", [], |r| r.get(0))
        .unwrap();
    assert_eq!(runs, 1);

    let after = sb.daemon(&["--once", "--trigger", "manual"]);
    assert_eq!(after.status.code(), Some(0), "{after:?}");
    assert!(stdout(&after).contains("(manual)"), "{}", stdout(&after));
    assert!(
        stdout(&after).contains(" 0 new events"),
        "{}",
        stdout(&after)
    );
    assert_eq!(sb.per_harness(), golden());
}
