//! Store performance harness.
//!
//! Builds a synthetic database through the real write path (`SourceTx::insert_events`,
//! `insert_messages`, session links) and times the dashboard, sessions and detail queries.
//!
//!     scripts/cargo run --release -p tbt-store --example perf -- \
//!         --events 1000000 --dir <scratch dir>
//!
//! Options:
//!   --events N   events to generate (default 1000000; sessions scale as 15 per 1000 events)
//!   --dir D      data dir; the DB is `D/tbt.sqlite` and is reused if it exists
//!   --regen      delete and rebuild the DB even if it exists
//!   --runs R     warm runs per query (default 20)
//!   --seed S     generator seed (default 7)
//!   --gen-only   build the DB and stop
//!   --no-open    skip the writable open (migration + `PRAGMA optimize`) before timing
//!
//! "cold" is the first call on a freshly opened read-only connection; "p50/p95" are over
//! the warm runs on that same connection. Ranges end at the next local midnight after
//! `NOW` in Europe/Berlin, like the app's presets.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use jiff::SignedDuration;
use jiff::tz::TimeZone;
use tbt_core::{
    Cursor, Decimal, HarnessId, LongContextTier, MessageKind, MessageRecord, MessageRole, Micros,
    ModelPrice, PriceId, PriceSource, PricedEvent, RawValue, SessionUpsert, TimeRange, Timestamp,
    UsageEvent,
};
use tbt_store::{BucketGroup, BucketSize, QueryFilter, Store};

const NOW: &str = "2026-11-24T16:30:00Z";
const SPAN_DAYS: i64 = 548;
const TZ: &str = "Europe/Berlin";
const CHUNK: usize = 500;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn f64(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + self.below((hi - lo).max(1) as u64) as i64
    }

    fn chance(&mut self, p: f64) -> bool {
        self.f64() < p
    }

    fn normal(&mut self) -> f64 {
        let u1 = self.f64().max(1e-12);
        let u2 = self.f64();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }

    fn weighted(&mut self, weights: &[f64]) -> usize {
        let total: f64 = weights.iter().sum();
        let mut x = self.f64() * total;
        for (i, w) in weights.iter().enumerate() {
            if x < *w {
                return i;
            }
            x -= w;
        }
        weights.len() - 1
    }

    fn hex(&mut self, len: usize) -> String {
        let mut s = String::with_capacity(len);
        while s.len() < len {
            s.push_str(&format!("{:016x}", self.next()));
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

struct Model {
    provider: &'static str,
    name: &'static str,
    rates: Option<[f64; 4]>,
    tiered: bool,
}

const fn m(provider: &'static str, name: &'static str, rates: Option<[f64; 4]>) -> Model {
    Model {
        provider,
        name,
        rates,
        tiered: false,
    }
}

const MODELS: [Model; 20] = [
    Model {
        tiered: true,
        ..m(
            "anthropic",
            "claude-sonnet-4-5",
            Some([3.0, 15.0, 3.75, 0.3]),
        )
    },
    m("anthropic", "claude-opus-4-5", Some([5.0, 25.0, 6.25, 0.5])),
    m("anthropic", "claude-haiku-4-5", Some([1.0, 5.0, 1.25, 0.1])),
    m(
        "anthropic",
        "claude-opus-4-1",
        Some([15.0, 75.0, 18.75, 1.5]),
    ),
    Model {
        tiered: true,
        ..m("anthropic", "claude-sonnet-4", Some([3.0, 15.0, 3.75, 0.3]))
    },
    m(
        "anthropic",
        "claude-3-7-sonnet",
        Some([3.0, 15.0, 3.75, 0.3]),
    ),
    Model {
        tiered: true,
        ..m(
            "anthropic",
            "claude-sonnet-4-6",
            Some([3.0, 15.0, 3.75, 0.3]),
        )
    },
    m("anthropic", "claude-opus-4-6", Some([5.0, 25.0, 6.25, 0.5])),
    m("openai", "gpt-5-codex", Some([1.25, 10.0, 0.0, 0.125])),
    m("openai", "gpt-5", Some([1.25, 10.0, 0.0, 0.125])),
    m("openai", "gpt-5.1-codex", Some([1.25, 10.0, 0.0, 0.125])),
    m(
        "openai",
        "gpt-5.1-codex-max",
        Some([1.25, 10.0, 0.0, 0.125]),
    ),
    m("openai", "gpt-5-mini", Some([0.25, 2.0, 0.0, 0.025])),
    m("openai", "o4-mini", Some([1.1, 4.4, 0.0, 0.275])),
    m("google", "gemini-2.5-pro", Some([1.25, 10.0, 0.0, 0.31])),
    m("google", "gemini-2.5-flash", Some([0.3, 2.5, 0.0, 0.075])),
    m("google", "gemini-3-pro-preview", None),
    m("zhipu", "glm-4.6", None),
    m("alibaba", "qwen3-coder", None),
    m("moonshot", "kimi-k2", None),
];

struct HarnessShape {
    id: HarnessId,
    weight: f64,
    models: &'static [(usize, f64)],
    version: &'static str,
}

fn harnesses() -> Vec<HarnessShape> {
    vec![
        HarnessShape {
            id: HarnessId::CLAUDE_CODE,
            weight: 0.55,
            models: &[
                (0, 4.0),
                (1, 3.0),
                (2, 1.5),
                (3, 0.4),
                (4, 1.0),
                (5, 0.3),
                (6, 1.5),
                (7, 1.0),
            ],
            version: "2.1.330",
        },
        HarnessShape {
            id: HarnessId::CODEX,
            weight: 0.30,
            models: &[
                (8, 4.0),
                (9, 2.0),
                (10, 2.0),
                (11, 1.0),
                (12, 0.5),
                (13, 0.3),
            ],
            version: "0.171.0",
        },
        HarnessShape {
            id: HarnessId::OPENCODE,
            weight: 0.10,
            models: &[(0, 2.0), (9, 1.0), (17, 0.6), (18, 0.5), (19, 0.3)],
            version: "1.0.180",
        },
        HarnessShape {
            id: HarnessId::GEMINI_CLI,
            weight: 0.05,
            models: &[(14, 3.0), (15, 2.0), (16, 0.6)],
            version: "0.20.0",
        },
    ]
}

struct Project {
    path: String,
    branches: Vec<String>,
}

struct Spec {
    harness: usize,
    ext: String,
    project: usize,
    branch: String,
    primary: usize,
    secondary: usize,
    start: Timestamp,
    offsets: Vec<i64>,
    parent: Option<String>,
    resumed_from: Option<String>,
    messages: bool,
    title: Option<String>,
}

impl Spec {
    fn end(&self) -> Timestamp {
        self.start + SignedDuration::from_secs(*self.offsets.last().unwrap_or(&0))
    }
}

struct Args {
    events: usize,
    dir: PathBuf,
    runs: usize,
    seed: u64,
    regen: bool,
    gen_only: bool,
    no_open: bool,
}

fn parse_args() -> Args {
    let mut args = Args {
        events: 1_000_000,
        dir: std::env::temp_dir().join("tbt-perf"),
        runs: 20,
        seed: 7,
        regen: false,
        gen_only: false,
        no_open: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--events" => args.events = it.next().and_then(|v| v.parse().ok()).expect("--events N"),
            "--dir" => args.dir = it.next().map(PathBuf::from).expect("--dir D"),
            "--runs" => args.runs = it.next().and_then(|v| v.parse().ok()).expect("--runs R"),
            "--seed" => args.seed = it.next().and_then(|v| v.parse().ok()).expect("--seed S"),
            "--regen" => args.regen = true,
            "--gen-only" => args.gen_only = true,
            "--no-open" => args.no_open = true,
            other => panic!("unknown argument {other}"),
        }
    }
    args
}

fn now() -> Timestamp {
    NOW.parse().unwrap()
}

fn lognormal(rng: &mut Rng, mu: f64, sigma: f64) -> f64 {
    (mu + sigma * rng.normal()).exp()
}

fn session_offsets(rng: &mut Rng, n: usize) -> Vec<i64> {
    let mut t = 0;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        if i > 0 {
            t += if rng.chance(0.015) {
                rng.range(1_800, 4 * 3_600)
            } else {
                lognormal(rng, 2.8, 0.9).clamp(1.0, 600.0) as i64
            };
        }
        out.push(t);
    }
    out
}

fn start_time(rng: &mut Rng, earliest: i64) -> Timestamp {
    const HOURS: [f64; 24] = [
        0.3, 0.2, 0.1, 0.1, 0.1, 0.2, 0.6, 1.5, 3.0, 4.0, 4.2, 3.8, 2.5, 3.5, 4.0, 4.0, 3.6, 3.0,
        2.0, 1.6, 1.8, 1.6, 1.0, 0.6,
    ];
    let end = now().as_second();
    loop {
        let day = rng.range(0, SPAN_DAYS);
        let day_start = (end / 86_400 - day) * 86_400;
        let weekday = (day_start / 86_400 + 3) % 7;
        if weekday >= 5 && rng.chance(0.75) {
            continue;
        }
        let hour = rng.weighted(&HOURS) as i64;
        let secs = day_start + hour * 3_600 + rng.range(0, 3_600);
        if secs >= earliest && secs < end {
            return Timestamp::from_second(secs).unwrap();
        }
    }
}

fn plan(rng: &mut Rng, total_events: usize) -> (Vec<Project>, Vec<Spec>) {
    let hs = harnesses();
    let projects: Vec<Project> = (0..50)
        .map(|i| {
            let n = 1 + rng.below(6) as usize;
            let mut branches = vec!["main".to_owned()];
            branches.extend((1..n).map(|b| format!("feature/p{i}-{b}")));
            Project {
                path: format!("/home/dev/src/project-{i:02}-{}", rng.hex(4)),
                branches,
            }
        })
        .collect();
    let project_weights: Vec<f64> = (0..projects.len())
        .map(|i| 1.0 / (i as f64 + 1.5))
        .collect();
    let n_sessions = ((total_events as f64) * 0.015).round().max(10.0) as usize;
    let n_top = n_sessions * 85 / 100;
    let earliest = now().as_second() - SPAN_DAYS * 86_400;

    let mut sizes: Vec<f64> = Vec::new();
    let mut specs: Vec<Spec> = Vec::new();
    let mut last_by: HashMap<(usize, usize), usize> = HashMap::new();
    while specs.len() < n_sessions {
        let top = specs.len() < n_top || specs.is_empty();
        if top {
            let h = rng.weighted(&hs.iter().map(|h| h.weight).collect::<Vec<_>>());
            let project = rng.weighted(&project_weights);
            let p = &projects[project];
            let branch = if rng.chance(0.6) {
                p.branches[0].clone()
            } else {
                p.branches[rng.below(p.branches.len() as u64) as usize].clone()
            };
            let mw: Vec<f64> = hs[h].models.iter().map(|(_, w)| *w).collect();
            let primary = hs[h].models[rng.weighted(&mw)].0;
            let secondary = hs[h].models[rng.weighted(&mw)].0;
            let mut start = start_time(rng, earliest);
            let mut resumed_from = None;
            if let Some(prev) = last_by.get(&(h, project))
                && rng.chance(0.08)
            {
                let prev = &specs[*prev];
                resumed_from = Some(prev.ext.clone());
                let after = prev.start.as_second() + 3_600 * rng.range(2, 24 * 20);
                start = Timestamp::from_second(after.min(now().as_second() - 3_600)).unwrap();
            }
            last_by.insert((h, project), specs.len());
            sizes.push(lognormal(rng, 3.0, 1.45));
            specs.push(Spec {
                harness: h,
                ext: rng.uuid(),
                project,
                branch,
                primary,
                secondary,
                start,
                offsets: Vec::new(),
                parent: None,
                resumed_from,
                messages: rng.chance(0.2),
                title: rng.chance(0.7).then(|| format!("Task {}", rng.hex(6))),
            });
        } else {
            let parent = loop {
                let i = rng.below(n_top as u64) as usize;
                if specs[i].harness == 0 {
                    break i;
                }
            };
            let p = &specs[parent];
            let ext = format!("{}/agent-{}", p.ext, rng.hex(17));
            let spec = Spec {
                harness: 0,
                ext,
                project: p.project,
                branch: p.branch.clone(),
                primary: if rng.chance(0.5) { 2 } else { p.primary },
                secondary: p.primary,
                start: p.start + SignedDuration::from_secs(rng.range(30, 900)),
                offsets: Vec::new(),
                parent: Some(p.ext.clone()),
                resumed_from: None,
                messages: p.messages,
                title: None,
            };
            sizes.push(lognormal(rng, 2.3, 1.0));
            specs.push(spec);
        }
    }

    let sum: f64 = sizes.iter().sum();
    let mut counts: Vec<usize> = sizes
        .iter()
        .map(|s| ((s / sum) * total_events as f64).round().max(1.0) as usize)
        .collect();
    let long: Vec<usize> = (0..specs.len())
        .filter(|i| specs[*i].parent.is_none())
        .take(8)
        .collect();
    for (k, i) in long.iter().enumerate() {
        counts[*i] = if k < 3 { 5_000 } else { 2_000 + 250 * (k - 3) };
    }
    let mut diff = total_events as i64 - counts.iter().sum::<usize>() as i64;
    let mut i = 0;
    while diff != 0 {
        let idx = i % counts.len();
        if !long.contains(&idx) {
            if diff > 0 {
                let add = (diff as usize).min(counts[idx]);
                counts[idx] += add;
                diff -= add as i64;
            } else if counts[idx] > 1 {
                let take = ((-diff) as usize).min(counts[idx] - 1);
                counts[idx] -= take;
                diff += take as i64;
            }
        }
        i += 1;
    }
    for (spec, n) in specs.iter_mut().zip(counts) {
        spec.offsets = session_offsets(rng, n);
        let span = *spec.offsets.last().unwrap();
        let latest = now().as_second() - span - 60;
        if spec.start.as_second() > latest {
            spec.start = Timestamp::from_second(latest.max(earliest)).unwrap();
        }
    }
    (projects, specs)
}

fn decimal(v: f64) -> Decimal {
    format!("{v}").parse().unwrap()
}

fn seed_prices(store: &mut Store) -> Vec<Option<PriceId>> {
    MODELS
        .iter()
        .map(|m| {
            let rates = m.rates?;
            let price = ModelPrice {
                id: None,
                provider: m.provider.to_owned(),
                model_pattern: m.name.to_owned(),
                effective_from: "2024-01-01T00:00:00Z".parse().unwrap(),
                effective_to: None,
                input_per_mtok: decimal(rates[0]),
                output_per_mtok: decimal(rates[1]),
                cache_write_5m_per_mtok: (rates[2] > 0.0).then(|| decimal(rates[2])),
                cache_write_1h_per_mtok: (rates[2] > 0.0).then(|| decimal(rates[2] * 1.6)),
                cache_read_per_mtok: Some(decimal(rates[3])),
                long_context: m.tiered.then(|| LongContextTier {
                    threshold_tokens: 200_000,
                    input: Decimal::TWO,
                    output: decimal(1.5),
                    cache_write: Decimal::TWO,
                    cache_read: Decimal::TWO,
                }),
                source: PriceSource::Bundled,
            };
            Some(store.insert_price(&price).unwrap())
        })
        .collect()
}

const TOOLS: [&str; 8] = [
    "Bash", "Read", "Edit", "Grep", "Write", "Glob", "Task", "WebFetch",
];

fn make_event(
    rng: &mut Rng,
    harness: &HarnessId,
    spec: &Spec,
    project: &Project,
    i: usize,
    prices: &[Option<PriceId>],
) -> PricedEvent {
    let model = if rng.chance(0.85) {
        spec.primary
    } else {
        spec.secondary
    };
    let mdl = &MODELS[model];
    let ts = spec.start + SignedDuration::from_secs(spec.offsets[i]);
    let ts = ts + SignedDuration::from_millis(rng.range(0, 1000));
    let growth = (i as f64 / 40.0).min(1.0);
    let cache_read = if i == 0 {
        0
    } else {
        (15_000.0 + growth * lognormal(rng, 11.2, 0.6)) as u64
    };
    let cache_write = if rng.chance(0.3) {
        rng.range(200, 25_000) as u64
    } else {
        rng.range(0, 800) as u64
    };
    let input = rng.range(1, 4_000) as u64;
    let output = lognormal(rng, 5.5, 1.0).min(30_000.0) as u64;
    let reasoning = if mdl.provider == "openai" {
        output / 3
    } else {
        0
    };
    let (cw5, cw1) = if rng.chance(0.2) {
        (0, cache_write)
    } else {
        (cache_write, 0)
    };
    let price_id = prices[model];
    let cost = match mdl.rates {
        Some(r) => Micros(
            (input as f64 * r[0]
                + output as f64 * r[1]
                + cw5 as f64 * r[2]
                + cw1 as f64 * r[2] * 1.6
                + cache_read as f64 * r[3])
                .round() as i64,
        ),
        None => Micros::ZERO,
    };
    let raw = format!(
        r#"{{"input_tokens":{input},"cache_creation_input_tokens":{cache_write},"cache_read_input_tokens":{cache_read},"output_tokens":{output},"service_tier":"standard","cache_creation":{{"ephemeral_5m_input_tokens":{cw5},"ephemeral_1h_input_tokens":{cw1}}}}}"#
    );
    PricedEvent {
        event: UsageEvent {
            harness: harness.clone(),
            dedup_key: format!("msg_{}", rng.hex(24)),
            session_ext_id: spec.ext.clone(),
            ts,
            provider: mdl.provider.to_owned(),
            model: mdl.name.to_owned(),
            input_tokens: input,
            output_tokens: output,
            cache_write_5m_tokens: cw5,
            cache_write_1h_tokens: cw1,
            cache_read_tokens: cache_read,
            reasoning_tokens: reasoning,
            reported_cost: None,
            project_path: Some(project.path.clone()),
            git_branch: Some(spec.branch.clone()),
            raw: RawValue::from_string(raw).ok(),
        },
        cost: if price_id.is_some() {
            cost
        } else {
            Micros::ZERO
        },
        price_id,
    }
}

fn messages_for(rng: &mut Rng, events: &[PricedEvent]) -> Vec<MessageRecord> {
    let mut out = Vec::with_capacity(events.len() * 2);
    for e in events {
        let e = &e.event;
        let tool = TOOLS[rng.below(TOOLS.len() as u64) as usize];
        out.push(MessageRecord {
            session_ext_id: e.session_ext_id.clone(),
            key: format!("{}-a", e.dedup_key),
            ts: e.ts,
            role: MessageRole::Assistant,
            kind: Some(MessageKind::ToolUse),
            tool_name: Some(tool.to_owned()),
            preview: None,
            event_dedup_key: Some(e.dedup_key.clone()),
        });
        out.push(MessageRecord {
            session_ext_id: e.session_ext_id.clone(),
            key: format!("{}-u", e.dedup_key),
            ts: e.ts + SignedDuration::from_millis(900),
            role: MessageRole::User,
            kind: Some(MessageKind::ToolResult),
            tool_name: Some(tool.to_owned()),
            preview: None,
            event_dedup_key: None,
        });
    }
    out
}

fn generate(db: &Path, total: usize, seed: u64) {
    let started = Instant::now();
    let mut rng = Rng(seed);
    let (projects, mut specs) = plan(&mut rng, total);
    specs.sort_by_key(|s| s.end());
    let hs = harnesses();
    let mut store = Store::open(db).unwrap();
    for h in &hs {
        store.register_harness(&h.id, h.id.as_str()).unwrap();
    }
    let prices = seed_prices(&mut store);
    println!(
        "planned {} sessions, {} events in {:.1?}",
        specs.len(),
        specs.iter().map(|s| s.offsets.len()).sum::<usize>(),
        started.elapsed()
    );

    let mut done = 0usize;
    let mut last_report = Instant::now();
    let write_started = Instant::now();
    for spec in &specs {
        let harness = &hs[spec.harness].id;
        let project = &projects[spec.project];
        let path = PathBuf::from(format!("/synthetic/{}/{}.jsonl", harness, spec.ext));
        let n = spec.offsets.len();
        let mut from = 0;
        while from < n {
            let to = (from + CHUNK).min(n);
            let events: Vec<PricedEvent> = (from..to)
                .map(|i| make_event(&mut rng, harness, spec, project, i, &prices))
                .collect();
            let mut tx = store.begin_source_tx(harness, &path).unwrap();
            let pid = tx.upsert_project(Some(&project.path)).unwrap();
            let upsert = SessionUpsert {
                project_path: Some(project.path.clone()),
                title: spec.title.clone(),
                git_branch: Some(spec.branch.clone()),
                harness_version: Some(hs[spec.harness].version.to_owned()),
                started_at: Some(events[0].event.ts),
                ended_at: Some(events[events.len() - 1].event.ts),
                ..SessionUpsert::new(harness.clone(), spec.ext.clone())
            };
            let sid = tx.upsert_session(&upsert, pid).unwrap();
            if from == 0 {
                for (target, parent) in [(&spec.parent, true), (&spec.resumed_from, false)] {
                    let Some(ext) = target else { continue };
                    let other = match tx.session_id(harness, ext).unwrap() {
                        Some(id) => id,
                        None => tx
                            .upsert_session(&SessionUpsert::new(harness.clone(), ext.clone()), pid)
                            .unwrap(),
                    };
                    if parent {
                        tx.set_parent_session(sid, other).unwrap();
                    } else {
                        tx.link_resumed_session(sid, other).unwrap();
                    }
                }
            }
            tx.insert_events(&events).unwrap();
            if spec.messages {
                tx.insert_messages(&messages_for(&mut rng, &events))
                    .unwrap();
            }
            tx.save_cursor(
                &Cursor::from_json(serde_json::json!({ "offset": to })),
                events[events.len() - 1].event.ts,
            )
            .unwrap();
            tx.commit().unwrap();
            done += to - from;
            from = to;
        }
        if last_report.elapsed() > Duration::from_secs(10) {
            let secs = write_started.elapsed().as_secs_f64();
            println!(
                "  {done} events ({:.0}/s)",
                done as f64 / secs.max(f64::EPSILON)
            );
            last_report = Instant::now();
        }
    }
    let write_secs = write_started.elapsed();
    drop(store);
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .unwrap();
    drop(conn);
    println!(
        "wrote {done} events via SourceTx in {:.1?} ({:.0} events/s); total {:.1?}",
        write_secs,
        done as f64 / write_secs.as_secs_f64(),
        started.elapsed()
    );
}

fn file_size(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

fn print_db_shape(db: &Path) {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    println!(
        "DB {} — {:.1} MiB; events {}, sessions {}, messages {}, hourly_usage rows {}, \
         projects {}, unpriced events {}, max events/session {}",
        db.display(),
        file_size(db) as f64 / (1024.0 * 1024.0),
        count("SELECT COUNT(*) FROM usage_event"),
        count("SELECT COUNT(*) FROM session"),
        count("SELECT COUNT(*) FROM message"),
        count("SELECT COUNT(*) FROM hourly_usage"),
        count("SELECT COUNT(*) FROM project"),
        count("SELECT COUNT(*) FROM usage_event WHERE price_id IS NULL"),
        count("SELECT MAX(events) FROM session"),
    );
}

struct Timing {
    cold: Duration,
    p50: Duration,
    p95: Duration,
}

fn time<T>(db: &Path, runs: usize, f: impl Fn(&Store) -> T) -> (Timing, T) {
    let store = Store::open_read_only(db).unwrap();
    let t0 = Instant::now();
    let out = f(&store);
    let cold = t0.elapsed();
    let mut warm: Vec<Duration> = (0..runs)
        .map(|_| {
            let t = Instant::now();
            std::hint::black_box(f(&store));
            t.elapsed()
        })
        .collect();
    warm.sort();
    let pick = |q: f64| warm[((warm.len() as f64 - 1.0) * q).round() as usize];
    (
        Timing {
            cold,
            p50: pick(0.5),
            p95: pick(0.95),
        },
        out,
    )
}

fn ms(d: Duration) -> String {
    format!("{:.2}", d.as_secs_f64() * 1000.0)
}

fn row(name: &str, range: &str, filter: &str, t: &Timing, note: &str) {
    println!(
        "| {name} | {range} | {filter} | {} | {} | {} | {note} |",
        ms(t.cold),
        ms(t.p50),
        ms(t.p95)
    );
}

fn ranges() -> Vec<(&'static str, i64, BucketSize)> {
    vec![
        ("Today", 1, BucketSize::Hour),
        ("7d", 7, BucketSize::Day),
        ("30d", 30, BucketSize::Day),
        ("1y", 365, BucketSize::Day),
    ]
}

fn filter(days: i64, bucket: BucketSize, harness: Option<HarnessId>) -> QueryFilter {
    let tz = TimeZone::get(TZ).unwrap();
    let today = now().to_zoned_datetime(tz).start_of_day().unwrap();
    let end = today.checked_add(jiff::Span::new().days(1)).unwrap();
    let start = end
        .checked_sub(jiff::Span::new().days(days))
        .unwrap()
        .timestamp();
    let mut f = QueryFilter::new(TimeRange::new(start, end.timestamp()), bucket);
    f.time_zone = TZ.to_owned();
    f.harnesses = harness.into_iter().collect();
    f
}

fn bench(db: &Path, runs: usize) {
    println!("\n| query | range | harness | cold ms | p50 ms | p95 ms | rows |");
    println!("|---|---|---|---|---|---|---|");
    for (label, days, bucket) in ranges() {
        for h in [None, Some(HarnessId::CLAUDE_CODE)] {
            let hl = if h.is_some() { "claude_code" } else { "all" };
            let f = filter(days, bucket, h);
            let (t, k) = time(db, runs, |s| s.kpi_summary(&f).unwrap());
            row(
                "kpi_summary",
                label,
                hl,
                &t,
                &format!("{} sessions", k.sessions),
            );
            for (g, name) in [
                (BucketGroup::Harness, "cost_buckets/harness"),
                (BucketGroup::Model, "cost_buckets/model"),
                (BucketGroup::Project, "cost_buckets/project"),
            ] {
                let (t, n) = time(db, runs, |s| s.cost_buckets(&f, g).unwrap().len());
                row(name, label, hl, &t, &n.to_string());
            }
            let (t, n) = time(db, runs, |s| s.heatmap(&f).unwrap().len());
            row("heatmap", label, hl, &t, &n.to_string());
            let (t, n) = time(db, runs, |s| s.model_stats(&f).unwrap().len());
            row("model_stats", label, hl, &t, &n.to_string());
            let (t, n) = time(db, runs, |s| s.project_stats(&f).unwrap().len());
            row("project_stats", label, hl, &t, &n.to_string());
        }
    }
    let (t, n) = time(db, runs, |s| s.unpriced_models().unwrap().len());
    row("unpriced_models", "all", "-", &t, &n.to_string());
    let (t, n) = time(db, runs, |s| s.harness_status().unwrap().len());
    row("harness_status", "-", "-", &t, &n.to_string());

    println!("\n| query | range | harness | cold ms | p50 ms | p95 ms | rows |");
    println!("|---|---|---|---|---|---|---|");
    for (label, days) in [
        ("Today", 1),
        ("7d", 7),
        ("30d", 30),
        ("1y", 365),
        ("all", 600),
    ] {
        for h in [None, Some(HarnessId::CLAUDE_CODE)] {
            let hl = if h.is_some() { "claude_code" } else { "all" };
            let f = filter(days, BucketSize::Day, h);
            let (t, n) = time(db, runs, |s| s.sessions(&f).unwrap().len());
            row("sessions", label, hl, &t, &n.to_string());
        }
    }

    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let pick = |sql: &str| -> (i64, i64) {
        conn.query_row(sql, [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
    };
    let targets = [
        (
            "session_detail ~2000 calls",
            pick("SELECT id, events FROM session ORDER BY abs(events - 2000) LIMIT 1"),
        ),
        (
            "session_detail 5000 calls",
            pick("SELECT id, events FROM session ORDER BY events DESC LIMIT 1"),
        ),
        (
            "session_detail w/ messages",
            pick(
                "SELECT s.id, s.events FROM session s
                 WHERE EXISTS (SELECT 1 FROM message m WHERE m.session_id = s.id)
                 ORDER BY s.events DESC LIMIT 1",
            ),
        ),
        (
            "session_detail resumed",
            pick(
                "SELECT id, events FROM session WHERE resumed_from_session_id IS NOT NULL
                 ORDER BY events DESC LIMIT 1",
            ),
        ),
        (
            "session_detail w/ subagents",
            pick(
                "SELECT p.id, p.events FROM session p
                 WHERE EXISTS (SELECT 1 FROM session c WHERE c.parent_session_id = p.id)
                 ORDER BY p.events DESC LIMIT 1",
            ),
        ),
    ];
    for (name, (id, events)) in targets {
        let (t, d) = time(db, runs, |s| {
            s.session_detail(tbt_core::SessionId(id)).unwrap().unwrap()
        });
        row(
            name,
            "-",
            "-",
            &t,
            &format!(
                "{events} events, {} messages, chain {}, subagents {}",
                d.messages.len(),
                d.resume_chain.len(),
                d.subagents.len()
            ),
        );
    }
}

fn main() {
    let args = parse_args();
    std::fs::create_dir_all(&args.dir).unwrap();
    let db = tbt_store::paths::db_path(&args.dir);
    if args.regen || !db.exists() {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
        generate(&db, args.events, args.seed);
    }
    if !args.no_open {
        let t = Instant::now();
        drop(Store::open(&db).unwrap());
        println!("open + migrate + optimize: {:.1?}", t.elapsed());
    }
    print_db_shape(&db);
    if !args.gen_only {
        bench(&db, args.runs);
    }
}
