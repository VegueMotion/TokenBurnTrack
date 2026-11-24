//! Incremental-run timing on a large database.
//!
//! First build the synthetic DB with the store harness, then point this at the same dir:
//!
//!     scripts/cargo run --release -p tbt-store --example perf -- --events 1000000 --dir <D>
//!     scripts/cargo run --release -p tbt-collector --example perf_ingest -- --dir <D>
//!
//! It copies `<D>/tbt.sqlite` to `<D>/collector/data`, copies all fixture sources plus
//! `--files N` (default 300) synthetic Claude Code JSONL files and one 5,000-call session
//! into `<D>/collector/roots`, then times:
//!   1. the first run (ingests the fixtures and synthetic files),
//!   2. a second run with nothing changed,
//!   3. a run after appending `--append K` (default 20) calls to the 5,000-call session,
//!   4. with `raw_json_retention_days = 30`: the first run (drops old raw JSON) and a
//!      run after it with nothing changed.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use jiff::{SignedDuration, Timestamp};
use tbt_collector::{Collector, CollectorConfig, Registry, RunOutcome, RunTrigger};
use tbt_core::HarnessId;
use tbt_store::Store;

const NOW: &str = "2026-11-24T16:30:00Z";

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn hex(&mut self, len: usize) -> String {
        let mut s = String::with_capacity(len);
        while s.len() < len {
            let _ = write!(s, "{:016x}", self.next());
        }
        s.truncate(len);
        s
    }

    fn uuid(&mut self) -> String {
        let h = self.hex(32);
        format!(
            "{}-{}-{}-{}-{}",
            &h[0..8],
            &h[8..12],
            &h[12..16],
            &h[16..20],
            &h[20..32]
        )
    }
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

fn claude_lines(rng: &mut Rng, session: &str, cwd: &str, start: Timestamp, n: usize) -> String {
    let mut out = String::new();
    let mut ts = start;
    for i in 0..n {
        let user = rng.uuid();
        let asst = rng.uuid();
        let tool = rng.hex(24);
        let t = ts.strftime("%Y-%m-%dT%H:%M:%S%.3fZ");
        let _ = writeln!(
            out,
            r#"{{"parentUuid":null,"isSidechain":false,"type":"user","message":{{"role":"user","content":[{{"tool_use_id":"toolu_{tool}","type":"tool_result","content":"ok"}}]}},"uuid":"{user}","timestamp":"{t}","sessionId":"{session}","cwd":"{cwd}","version":"2.1.330","gitBranch":"main","userType":"external"}}"#
        );
        let cache_read = 20_000 + rng.below(150_000);
        let _ = writeln!(
            out,
            r#"{{"parentUuid":"{user}","isSidechain":false,"message":{{"model":"claude-sonnet-4-5","id":"msg_{}","type":"message","role":"assistant","content":[{{"type":"tool_use","id":"toolu_{}","name":"Bash","input":{{}}}}],"stop_reason":"tool_use","usage":{{"input_tokens":{},"cache_creation_input_tokens":{},"cache_read_input_tokens":{cache_read},"output_tokens":{},"service_tier":"standard","cache_creation":{{"ephemeral_1h_input_tokens":0,"ephemeral_5m_input_tokens":0}}}}}},"requestId":"req_{}","type":"assistant","uuid":"{asst}","timestamp":"{t}","sessionId":"{session}","cwd":"{cwd}","version":"2.1.330","gitBranch":"main","userType":"external"}}"#,
            rng.hex(24),
            rng.hex(24),
            1 + rng.below(3000),
            rng.below(4000),
            50 + rng.below(2000),
            rng.hex(24),
        );
        ts += SignedDuration::from_secs(5 + rng.below(90) as i64);
        if i % 400 == 399 {
            ts += SignedDuration::from_secs(3_600);
        }
    }
    out
}

fn write_session(dir: &Path, rng: &mut Rng, start: Timestamp, n: usize) -> PathBuf {
    let session = rng.uuid();
    let cwd = format!("/home/dev/src/synthetic-{}", rng.below(30));
    let project_dir = dir.join(cwd.replace('/', "-"));
    std::fs::create_dir_all(&project_dir).unwrap();
    let path = project_dir.join(format!("{session}.jsonl"));
    std::fs::write(&path, claude_lines(rng, &session, &cwd, start, n)).unwrap();
    path
}

fn append_session(path: &Path, rng: &mut Rng, n: usize) {
    let text = std::fs::read_to_string(path).unwrap();
    let last = text.lines().last().unwrap();
    let v: serde_json::Value = serde_json::from_str(last).unwrap();
    let ts: Timestamp = v["timestamp"].as_str().unwrap().parse().unwrap();
    let session = v["sessionId"].as_str().unwrap();
    let cwd = v["cwd"].as_str().unwrap();
    let lines = claude_lines(rng, session, cwd, ts + SignedDuration::from_secs(30), n);
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(lines.as_bytes()).unwrap();
}

fn run(collector: &Collector, label: &str) {
    let t = Instant::now();
    let outcome = collector.ingest_run(RunTrigger::Manual).unwrap();
    let elapsed = t.elapsed();
    let RunOutcome::Finished(r) = outcome else {
        panic!("collector busy")
    };
    println!(
        "| {label} | {:.1} | {} | {} | {} | {} |",
        elapsed.as_secs_f64() * 1000.0,
        r.sources_scanned,
        r.sources_read,
        r.events_new,
        r.errors.len()
    );
    for e in &r.errors {
        eprintln!("  error: {e}");
    }
}

fn main() {
    let mut dir: Option<PathBuf> = None;
    let mut files = 300usize;
    let mut append = 20usize;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dir" => dir = it.next().map(PathBuf::from),
            "--files" => files = it.next().and_then(|v| v.parse().ok()).expect("--files N"),
            "--append" => append = it.next().and_then(|v| v.parse().ok()).expect("--append K"),
            other => panic!("unknown argument {other}"),
        }
    }
    let dir = dir.expect("--dir D");
    let base_db = tbt_store::paths::db_path(&dir);
    assert!(
        base_db.exists(),
        "{} missing; run the store perf example first",
        base_db.display()
    );

    let work = dir.join("collector");
    let _ = std::fs::remove_dir_all(&work);
    let data = work.join("data");
    let roots = work.join("roots");
    std::fs::create_dir_all(&data).unwrap();
    let t = Instant::now();
    std::fs::copy(&base_db, tbt_store::paths::db_path(&data)).unwrap();
    println!("copied DB in {:.1?}", t.elapsed());

    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    for set in ["claude_code", "codex", "opencode", "gemini_cli"] {
        copy_tree(&fixtures.join(set), &roots.join(set));
    }
    let synthetic = roots.join("claude_code/synthetic/projects");
    let mut rng = Rng(11);
    let now: Timestamp = NOW.parse().unwrap();
    for i in 0..files {
        let start =
            now - SignedDuration::from_hours(24 * 25) + SignedDuration::from_hours(2 * i as i64);
        let n = 5 + rng.below(120) as usize;
        write_session(&synthetic, &mut rng, start, n);
    }
    let long = write_session(
        &synthetic,
        &mut rng,
        now - SignedDuration::from_hours(24 * 6),
        5_000,
    );

    let harness_roots: [(HarnessId, Vec<PathBuf>); 4] = [
        (HarnessId::CLAUDE_CODE, vec![roots.join("claude_code")]),
        (HarnessId::CODEX, vec![roots.join("codex")]),
        (
            HarnessId::OPENCODE,
            std::fs::read_dir(roots.join("opencode"))
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect(),
        ),
        (HarnessId::GEMINI_CLI, vec![roots.join("gemini_cli")]),
    ];
    {
        let mut store = Store::open(&tbt_store::paths::db_path(&data)).unwrap();
        for (id, r) in &harness_roots {
            store.register_harness(id, id.as_str()).unwrap();
            store.set_harness_roots(id, Some(r)).unwrap();
        }
    }

    let config = CollectorConfig::default();
    let collector = Collector::new(data.clone(), Registry::builtin(&config), config);
    println!("\n| run | ms | sources scanned | sources read | new events | errors |");
    println!("|---|---|---|---|---|---|");
    run(&collector, "1st run (fixtures + synthetic files)");
    run(&collector, "2nd run, nothing changed");
    run(&collector, "3rd run, nothing changed");
    append_session(&long, &mut rng, append);
    run(
        &collector,
        &format!("append {append} calls to 5,000-call session"),
    );
    run(&collector, "nothing changed");

    let config = CollectorConfig {
        raw_json_retention_days: Some(30),
        ..CollectorConfig::default()
    };
    let collector = Collector::new(data.clone(), Registry::builtin(&config), config);
    run(&collector, "retention 30d, first drop");
    run(&collector, "retention 30d, nothing changed");
    append_session(&long, &mut rng, append);
    run(&collector, &format!("retention 30d, append {append} calls"));
}
