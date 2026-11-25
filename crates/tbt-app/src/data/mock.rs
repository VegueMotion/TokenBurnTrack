use std::collections::{BTreeMap, BTreeSet};

use jiff::{SignedDuration, Timestamp};
use tbt_core::{
    EventId, HarnessId, Hour, Micros, ParseWarning, ProjectId, RunId, SessionId, TokenCounts,
};
use tbt_store::{
    BranchStats, BucketGroup, CostBucket, EventRow, HarnessConfig, HarnessStatus, IngestRunSummary,
    KpiSummary, ModelStats, ProjectStats, QueryFilter, Result, RunStats, RunTrigger, SessionDetail,
    SessionRow, TokenSums, UnpricedModel,
};

use super::DataSource;
use crate::charts::time::{bucket_start, time_zone};

const DAYS: i64 = 30;

struct ModelSpec {
    provider: &'static str,
    model: &'static str,
    price: Option<[f64; 4]>,
}

static MODELS: [ModelSpec; 8] = [
    ModelSpec {
        provider: "anthropic",
        model: "claude-opus-4-1",
        price: Some([15.0, 75.0, 18.75, 1.5]),
    },
    ModelSpec {
        provider: "anthropic",
        model: "claude-sonnet-4-5",
        price: Some([3.0, 15.0, 3.75, 0.3]),
    },
    ModelSpec {
        provider: "anthropic",
        model: "claude-haiku-4-5",
        price: Some([1.0, 5.0, 1.25, 0.1]),
    },
    ModelSpec {
        provider: "openai",
        model: "gpt-5-codex",
        price: Some([1.25, 10.0, 0.0, 0.125]),
    },
    ModelSpec {
        provider: "openai",
        model: "gpt-5",
        price: Some([1.25, 10.0, 0.0, 0.125]),
    },
    ModelSpec {
        provider: "google",
        model: "gemini-2.5-pro",
        price: Some([1.25, 10.0, 0.0, 0.31]),
    },
    ModelSpec {
        provider: "google",
        model: "gemini-2.5-flash",
        price: Some([0.3, 2.5, 0.0, 0.075]),
    },
    ModelSpec {
        provider: "ollama",
        model: "qwen3-coder",
        price: None,
    },
];

struct HarnessSpec {
    id: HarnessId,
    name: &'static str,
    version: &'static str,
    weight: f64,
    models: &'static [(usize, f64)],
}

static HARNESSES: [HarnessSpec; 4] = [
    HarnessSpec {
        id: HarnessId::CLAUDE_CODE,
        name: "Claude Code",
        version: "2.0.51",
        weight: 0.5,
        models: &[(1, 0.65), (0, 0.25), (2, 0.10)],
    },
    HarnessSpec {
        id: HarnessId::CODEX,
        name: "Codex CLI",
        version: "0.63.0",
        weight: 0.25,
        models: &[(3, 0.8), (4, 0.2)],
    },
    HarnessSpec {
        id: HarnessId::OPENCODE,
        name: "OpenCode",
        version: "1.0.85",
        weight: 0.13,
        models: &[(1, 0.5), (4, 0.3), (7, 0.2)],
    },
    HarnessSpec {
        id: HarnessId::GEMINI_CLI,
        name: "Gemini CLI",
        version: "0.17.1",
        weight: 0.12,
        models: &[(5, 0.7), (6, 0.3)],
    },
];

const PROJECTS: [(&str, &[&str]); 6] = [
    (
        "/home/dev/src/tokenburntrack",
        &["main", "feat/charts", "fix/dedup"],
    ),
    ("/home/dev/src/acme-api", &["main", "perf/cache-layer"]),
    ("/home/dev/src/marketing-site", &["main"]),
    ("/home/dev/src/ml-pipeline", &["main", "exp/tokenizer"]),
    ("/home/dev/src/infra", &["main", "terraform-upgrade"]),
    ("/home/dev/dotfiles", &["master"]),
];

const TITLES: [&str; 8] = [
    "Refactor ingest pipeline",
    "Fix flaky integration test",
    "Add CSV export",
    "Investigate memory spike",
    "Write migration for v3 schema",
    "Review PR feedback",
    "Speed up cold start",
    "Explain auth middleware",
];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }

    fn weighted<T: Copy>(&mut self, items: &[(T, f64)]) -> T {
        let total: f64 = items.iter().map(|(_, w)| w).sum();
        let mut r = self.unit() * total;
        for (item, w) in items {
            if r < *w {
                return *item;
            }
            r -= w;
        }
        items[items.len() - 1].0
    }
}

struct MockSession {
    harness: usize,
    ext_id: String,
    title: Option<String>,
    project: Option<usize>,
    branch: Option<&'static str>,
    parent: Option<usize>,
}

struct MockEvent {
    session: usize,
    ts: Timestamp,
    model: usize,
    tokens: TokenCounts,
    cost: Micros,
}

pub struct MockData {
    now: Timestamp,
    sessions: Vec<MockSession>,
    session_events: Vec<Vec<usize>>,
    events: Vec<MockEvent>,
    runs: Vec<IngestRunSummary>,
}

fn add_secs(ts: Timestamp, secs: i64) -> Timestamp {
    ts.checked_add(SignedDuration::from_secs(secs))
        .unwrap_or(ts)
}

fn cost_of(model: usize, t: &TokenCounts) -> Micros {
    match MODELS[model].price {
        Some([i, o, cw, cr]) => Micros(
            (t.input as f64 * i
                + t.output as f64 * o
                + t.cache_write() as f64 * cw
                + t.cache_read as f64 * cr)
                .round() as i64,
        ),
        None => Micros::ZERO,
    }
}

fn activity(hour_of_day: i8, weekend: bool) -> f64 {
    let base = match hour_of_day {
        9..=12 | 14..=17 => 0.6,
        13 | 18 => 0.4,
        19..=22 => 0.3,
        7 | 8 => 0.2,
        _ => 0.03,
    };
    if weekend { base * 0.35 } else { base }
}

impl MockData {
    pub fn new(seed: u64, now: Timestamp) -> MockData {
        let mut rng = Rng(seed);
        let mut sessions = Vec::new();
        let mut events = Vec::new();
        let last_hour = Hour::containing(now).start();
        for h in (0..DAYS * 24).rev() {
            let hour = add_secs(last_hour, -h * 3600);
            let z = hour.to_zoned_datetime(jiff::tz::TimeZone::UTC);
            let weekend = z.weekday().to_monday_zero_offset() >= 5;
            let rate = activity(z.hour(), weekend);
            let mut starts = 0;
            while starts < 3 && rng.unit() < rate * if starts == 0 { 1.0 } else { 0.35 } {
                starts += 1;
                let start = add_secs(hour, rng.range(0, 3599) as i64);
                gen_session(&mut rng, start, now, None, &mut sessions, &mut events);
            }
        }
        events.sort_by_key(|e: &MockEvent| (e.ts, e.session));
        let mut session_events = vec![Vec::new(); sessions.len()];
        for (i, e) in events.iter().enumerate() {
            session_events[e.session].push(i);
        }
        let mut data = MockData {
            now,
            sessions,
            session_events,
            events,
            runs: Vec::new(),
        };
        data.runs = data.gen_runs();
        data
    }

    pub fn now(&self) -> Timestamp {
        self.now
    }

    fn gen_runs(&self) -> Vec<IngestRunSummary> {
        let last = Hour::containing(self.now).start();
        (0..48i64)
            .map(|i| {
                let started = add_secs(last, -i * 3600 + 4);
                let window_start = add_secs(started, -3600);
                let new = self
                    .events
                    .iter()
                    .filter(|e| e.ts >= window_start && e.ts < started)
                    .count() as u64;
                IngestRunSummary {
                    id: RunId(48 - i),
                    trigger: if i == 47 {
                        RunTrigger::Startup
                    } else {
                        RunTrigger::Hourly
                    },
                    started_at: started,
                    finished_at: Some(add_secs(started, 2 + (i % 5))),
                    stats: RunStats {
                        sources_scanned: 120 + (i as u64 * 7) % 40,
                        events_new: new,
                    },
                    warnings: Vec::new(),
                    error: None,
                }
            })
            .collect()
    }

    fn harness_id(&self, session: usize) -> &HarnessId {
        &HARNESSES[self.sessions[session].harness].id
    }

    fn in_filter<'a>(&'a self, f: &'a QueryFilter) -> impl Iterator<Item = &'a MockEvent> + 'a {
        let lo = self.events.partition_point(|e| e.ts < f.range.start);
        let hi = self.events.partition_point(|e| e.ts < f.range.end);
        self.events[lo..hi.max(lo)].iter().filter(move |e| {
            f.harnesses.is_empty() || f.harnesses.contains(self.harness_id(e.session))
        })
    }

    fn session_row(&self, s: usize) -> SessionRow {
        let sess = &self.sessions[s];
        let evs: Vec<&MockEvent> = self.session_events[s]
            .iter()
            .map(|i| &self.events[*i])
            .collect();
        let mut models: Vec<String> = Vec::new();
        let mut tokens = TokenSums::default();
        let mut cost = Micros::ZERO;
        let mut unpriced = 0;
        let mut active = 0u64;
        for (k, e) in evs.iter().enumerate() {
            let name = MODELS[e.model].model;
            if !models.iter().any(|m| m == name) {
                models.push(name.to_owned());
            }
            add_tokens(&mut tokens, &e.tokens);
            cost += e.cost;
            if MODELS[e.model].price.is_none() {
                unpriced += 1;
            }
            if k > 0 {
                let gap = e.ts.as_second() - evs[k - 1].ts.as_second();
                if gap <= 1800 {
                    active += gap.max(0) as u64;
                }
            }
        }
        let project = sess.project.map(|p| PROJECTS[p].0);
        SessionRow {
            id: SessionId(s as i64 + 1),
            harness: HARNESSES[sess.harness].id.clone(),
            ext_id: sess.ext_id.clone(),
            title: sess.title.clone(),
            project_path: project.map(str::to_owned),
            project_name: project
                .and_then(|p| p.rsplit('/').next())
                .map(str::to_owned),
            git_branch: sess.branch.map(str::to_owned),
            models,
            started_at: evs.first().map(|e| e.ts).unwrap_or(self.now),
            ended_at: evs.last().map(|e| e.ts),
            active_seconds: active,
            events: evs.len() as u64,
            tokens,
            cost,
            unpriced_events: unpriced,
            parent: sess.parent.map(|p| SessionId(p as i64 + 1)),
            resumed_from: None,
        }
    }
}

fn add_tokens(sum: &mut TokenSums, t: &TokenCounts) {
    sum.input += t.input;
    sum.output += t.output;
    sum.cache_write += t.cache_write();
    sum.cache_read += t.cache_read;
}

fn gen_session(
    rng: &mut Rng,
    start: Timestamp,
    now: Timestamp,
    parent: Option<(usize, usize, Option<usize>, Option<&'static str>)>,
    sessions: &mut Vec<MockSession>,
    events: &mut Vec<MockEvent>,
) {
    let harness = match parent {
        Some((_, h, _, _)) => h,
        None => {
            let weights: Vec<(usize, f64)> = HARNESSES
                .iter()
                .enumerate()
                .map(|(i, h)| (i, h.weight))
                .collect();
            rng.weighted(&weights)
        }
    };
    let spec = &HARNESSES[harness];
    let main_model = if parent.is_some() {
        2
    } else {
        rng.weighted(spec.models)
    };
    let (project, branch) = match parent {
        Some((_, _, p, b)) => (p, b),
        None if rng.unit() < 0.08 => (None, None),
        None => {
            let p = rng.weighted(&[
                (0usize, 0.35),
                (1, 0.25),
                (2, 0.1),
                (3, 0.15),
                (4, 0.1),
                (5, 0.05),
            ]);
            let branches = PROJECTS[p].1;
            let b = branches[rng.range(0, branches.len() as u64 - 1) as usize];
            (Some(p), Some(b))
        }
    };
    let duration = if parent.is_some() {
        rng.range(60, 600)
    } else {
        rng.range(4, 150) * 60
    };
    let n = if parent.is_some() {
        rng.range(3, 12)
    } else {
        (duration / 150).clamp(3, 70) + rng.range(0, 6)
    };
    let caches = MODELS[main_model].provider == "anthropic";
    let idx = sessions.len();
    let mut new_events = Vec::new();
    let mut ctx = rng.range(6_000, 14_000);
    for k in 0..n {
        let offset = duration * k / n + rng.range(0, duration / n / 2 + 1);
        let ts = add_secs(start, offset as i64);
        if ts > now {
            break;
        }
        let model = if harness == 0 && parent.is_none() && rng.unit() < 0.12 {
            2
        } else {
            main_model
        };
        let input = if caches {
            rng.range(20, 2_500)
        } else {
            rng.range(800, 18_000)
        };
        let fresh = rng.range(400, 9_000);
        let tokens = TokenCounts {
            input,
            output: rng.range(80, 3_500),
            cache_write_5m: if caches { fresh } else { 0 },
            cache_write_1h: if caches && k == 0 {
                rng.range(2_000, 12_000)
            } else {
                0
            },
            cache_read: ctx.min(170_000),
            reasoning: 0,
        };
        ctx += fresh;
        new_events.push(MockEvent {
            session: idx,
            ts,
            model,
            cost: cost_of(model, &tokens),
            tokens,
        });
    }
    if new_events.is_empty() {
        return;
    }
    let first_ts = new_events[0].ts;
    let span = new_events.last().map(|e| e.ts).unwrap_or(first_ts);
    sessions.push(MockSession {
        harness,
        ext_id: format!("{:08x}-{:04x}", rng.next() as u32, rng.next() as u16),
        title: (harness == 0 && parent.is_none())
            .then(|| TITLES[rng.range(0, TITLES.len() as u64 - 1) as usize].to_owned()),
        project,
        branch,
        parent: parent.map(|(p, _, _, _)| p),
    });
    events.extend(new_events);
    if harness == 0 && parent.is_none() && rng.unit() < 0.18 {
        let secs = (span.as_second() - first_ts.as_second()).max(1) as u64;
        let at = add_secs(first_ts, rng.range(0, secs) as i64);
        gen_session(
            rng,
            at,
            now,
            Some((idx, harness, project, branch)),
            sessions,
            events,
        );
    }
}

impl DataSource for MockData {
    fn kpi_summary(&self, filter: &QueryFilter) -> Result<KpiSummary> {
        let mut k = KpiSummary::default();
        let mut sessions = BTreeSet::new();
        let mut hours = BTreeSet::new();
        for e in self.in_filter(filter) {
            k.cost += e.cost;
            add_tokens(&mut k.tokens, &e.tokens);
            k.events += 1;
            if MODELS[e.model].price.is_none() {
                k.unpriced_events += 1;
            }
            sessions.insert(e.session);
            hours.insert(Hour::containing(e.ts));
        }
        k.sessions = sessions.len() as u64;
        k.active_hours = hours.len() as u64;
        Ok(k)
    }

    fn cost_buckets(&self, filter: &QueryFilter, group: BucketGroup) -> Result<Vec<CostBucket>> {
        let tz = time_zone(&filter.time_zone);
        let mut map: BTreeMap<(Timestamp, String), CostBucket> = BTreeMap::new();
        for e in self.in_filter(filter) {
            let start = bucket_start(e.ts, filter.bucket, &tz);
            let key = match group {
                BucketGroup::Harness => self.harness_id(e.session).to_string(),
                BucketGroup::Model => MODELS[e.model].model.to_owned(),
                BucketGroup::Project => self.sessions[e.session]
                    .project
                    .map(|p| PROJECTS[p].0.to_owned())
                    .unwrap_or_default(),
            };
            let b = map
                .entry((start, key.clone()))
                .or_insert_with(|| CostBucket {
                    start,
                    key,
                    cost: Micros::ZERO,
                    tokens: TokenSums::default(),
                    events: 0,
                });
            b.cost += e.cost;
            add_tokens(&mut b.tokens, &e.tokens);
            b.events += 1;
        }
        Ok(map.into_values().collect())
    }

    fn sessions(&self, filter: &QueryFilter) -> Result<Vec<SessionRow>> {
        let ids: BTreeSet<usize> = self.in_filter(filter).map(|e| e.session).collect();
        let mut rows: Vec<SessionRow> = ids.into_iter().map(|s| self.session_row(s)).collect();
        rows.sort_by(|a, b| b.started_at.cmp(&a.started_at).then(b.id.cmp(&a.id)));
        Ok(rows)
    }

    fn session_detail(&self, id: SessionId) -> Result<Option<SessionDetail>> {
        let Some(s) = usize::try_from(id.0 - 1)
            .ok()
            .filter(|s| *s < self.sessions.len())
        else {
            return Ok(None);
        };
        let row = self.session_row(s);
        let events: Vec<EventRow> = self.session_events[s]
            .iter()
            .map(|i| {
                let e = &self.events[*i];
                EventRow {
                    id: EventId(*i as i64 + 1),
                    ts: e.ts,
                    provider: MODELS[e.model].provider.to_owned(),
                    model: MODELS[e.model].model.to_owned(),
                    tokens: e.tokens,
                    cost: e.cost,
                    reported_cost: None,
                    price_id: None,
                }
            })
            .collect();
        let mut rng = Rng(id.0 as u64);
        let n = events.len() as u64;
        let mut tool_counts: Vec<(String, u64)> = ["Read", "Edit", "Bash", "Grep", "Write"]
            .iter()
            .map(|t| (t.to_string(), rng.range(0, n)))
            .filter(|(_, c)| *c > 0)
            .collect();
        tool_counts.sort_by_key(|a| std::cmp::Reverse(a.1));
        let subagents = (0..self.sessions.len())
            .filter(|c| self.sessions[*c].parent == Some(s))
            .map(|c| self.session_row(c))
            .collect();
        let reported_cost = (self.sessions[s].harness == 2).then(|| Micros(row.cost.0 * 98 / 100));
        Ok(Some(SessionDetail {
            harness_version: Some(HARNESSES[self.sessions[s].harness].version.to_owned()),
            events,
            messages: Vec::new(),
            tool_counts,
            reported_cost,
            resume_chain: vec![row.clone()],
            chain_cost: row.cost,
            subagents,
            session: row,
        }))
    }

    fn model_stats(&self, filter: &QueryFilter) -> Result<Vec<ModelStats>> {
        let mut map: BTreeMap<usize, ModelStats> = BTreeMap::new();
        for e in self.in_filter(filter) {
            let spec = &MODELS[e.model];
            let m = map.entry(e.model).or_insert_with(|| ModelStats {
                provider: spec.provider.to_owned(),
                model: spec.model.to_owned(),
                events: 0,
                tokens: TokenSums::default(),
                cost: Micros::ZERO,
                unpriced_events: 0,
                cache_savings: Micros::ZERO,
            });
            m.events += 1;
            add_tokens(&mut m.tokens, &e.tokens);
            m.cost += e.cost;
            match spec.price {
                Some([i, _, _, cr]) => {
                    m.cache_savings +=
                        Micros((e.tokens.cache_read as f64 * (i - cr)).round() as i64)
                }
                None => m.unpriced_events += 1,
            }
        }
        let mut out: Vec<ModelStats> = map.into_values().collect();
        out.sort_by(|a, b| b.cost.cmp(&a.cost).then(b.events.cmp(&a.events)));
        Ok(out)
    }

    fn project_stats(&self, filter: &QueryFilter) -> Result<Vec<ProjectStats>> {
        struct Acc {
            sessions: BTreeSet<usize>,
            events: u64,
            tokens: TokenSums,
            cost: Micros,
            last: Option<Timestamp>,
            branches: BTreeMap<Option<&'static str>, (BTreeSet<usize>, Micros)>,
        }
        let mut map: BTreeMap<Option<usize>, Acc> = BTreeMap::new();
        for e in self.in_filter(filter) {
            let sess = &self.sessions[e.session];
            let a = map.entry(sess.project).or_insert_with(|| Acc {
                sessions: BTreeSet::new(),
                events: 0,
                tokens: TokenSums::default(),
                cost: Micros::ZERO,
                last: None,
                branches: BTreeMap::new(),
            });
            a.sessions.insert(e.session);
            a.events += 1;
            add_tokens(&mut a.tokens, &e.tokens);
            a.cost += e.cost;
            a.last = a.last.max(Some(e.ts));
            let b = a
                .branches
                .entry(sess.branch)
                .or_insert_with(|| (BTreeSet::new(), Micros::ZERO));
            b.0.insert(e.session);
            b.1 += e.cost;
        }
        let mut out: Vec<ProjectStats> = map
            .into_iter()
            .map(|(p, a)| {
                let path = p.map(|p| PROJECTS[p].0);
                let mut branches: Vec<BranchStats> = a
                    .branches
                    .into_iter()
                    .map(|(b, (s, c))| BranchStats {
                        branch: b.map(str::to_owned),
                        sessions: s.len() as u64,
                        cost: c,
                    })
                    .collect();
                branches.sort_by_key(|a| std::cmp::Reverse(a.cost));
                ProjectStats {
                    id: p
                        .map(|p| ProjectId(p as i64 + 1))
                        .unwrap_or(ProjectId::NONE),
                    path: path.map(str::to_owned),
                    name: path.and_then(|p| p.rsplit('/').next()).map(str::to_owned),
                    sessions: a.sessions.len() as u64,
                    events: a.events,
                    tokens: a.tokens,
                    cost: a.cost,
                    last_active: a.last,
                    branches,
                }
            })
            .collect();
        out.sort_by_key(|a| std::cmp::Reverse(a.cost));
        Ok(out)
    }

    fn harness_status(&self) -> Result<Vec<HarnessStatus>> {
        let last_read = self.runs.first().and_then(|r| r.finished_at);
        Ok(HARNESSES
            .iter()
            .enumerate()
            .map(|(h, spec)| {
                let sessions: Vec<usize> = (0..self.sessions.len())
                    .filter(|s| self.sessions[*s].harness == h)
                    .collect();
                let events = sessions
                    .iter()
                    .map(|s| self.session_events[*s].len() as u64)
                    .sum();
                let last_warnings = if spec.id == HarnessId::GEMINI_CLI {
                    vec![ParseWarning {
                        harness: spec.id.clone(),
                        source: "~/.gemini/tmp/session-mock.json".to_owned(),
                        line: None,
                        byte_offset: None,
                        message: "unknown field `thoughtsTokenCount`; ignored".to_owned(),
                    }]
                } else {
                    Vec::new()
                };
                HarnessStatus {
                    config: HarnessConfig {
                        id: spec.id.clone(),
                        display_name: spec.name.to_owned(),
                        enabled: true,
                        roots: None,
                    },
                    files_tracked: sessions.len() as u64,
                    files_missing: (h as u64 * 3) % 4,
                    last_read,
                    events,
                    sessions: sessions.len() as u64,
                    last_warnings,
                }
            })
            .collect())
    }

    fn unpriced_models(&self) -> Result<Vec<UnpricedModel>> {
        let mut map: BTreeMap<usize, UnpricedModel> = BTreeMap::new();
        for e in self
            .events
            .iter()
            .filter(|e| MODELS[e.model].price.is_none())
        {
            let spec = &MODELS[e.model];
            let u = map.entry(e.model).or_insert_with(|| UnpricedModel {
                provider: spec.provider.to_owned(),
                model: spec.model.to_owned(),
                events: 0,
                tokens: TokenSums::default(),
                first_seen: e.ts,
                last_seen: e.ts,
            });
            u.events += 1;
            add_tokens(&mut u.tokens, &e.tokens);
            u.last_seen = u.last_seen.max(e.ts);
        }
        Ok(map.into_values().collect())
    }

    fn ingest_runs(&self, limit: usize) -> Result<Vec<IngestRunSummary>> {
        Ok(self.runs.iter().take(limit).cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tbt_core::TimeRange;
    use tbt_store::BucketSize;

    fn now() -> Timestamp {
        "2026-11-23T15:20:00Z".parse().unwrap()
    }

    fn month(bucket: BucketSize) -> QueryFilter {
        QueryFilter::new(
            TimeRange::new(add_secs(now(), -31 * 86_400), add_secs(now(), 3600)),
            bucket,
        )
    }

    #[test]
    fn deterministic_for_seed() {
        let a = MockData::new(7, now());
        let b = MockData::new(7, now());
        let f = month(BucketSize::Day);
        assert_eq!(a.sessions(&f).unwrap(), b.sessions(&f).unwrap());
        assert_eq!(a.kpi_summary(&f).unwrap(), b.kpi_summary(&f).unwrap());
        assert_eq!(
            a.cost_buckets(&f, BucketGroup::Harness).unwrap(),
            b.cost_buckets(&f, BucketGroup::Harness).unwrap()
        );
        let c = MockData::new(8, now());
        assert_ne!(a.kpi_summary(&f).unwrap(), c.kpi_summary(&f).unwrap());
    }

    #[test]
    fn plausible_spread() {
        let d = MockData::new(1, now());
        let f = month(BucketSize::Day);
        let k = d.kpi_summary(&f).unwrap();
        assert!(k.sessions > 50, "sessions {}", k.sessions);
        assert!(k.cost > Micros(1_000_000));
        assert!(k.unpriced_events > 0);
        assert!(k.tokens.cache_hit_ratio().unwrap() > 0.3);
        let harnesses: BTreeSet<String> = d
            .cost_buckets(&f, BucketGroup::Harness)
            .unwrap()
            .into_iter()
            .map(|b| b.key)
            .collect();
        assert_eq!(harnesses.len(), 4);
        assert!(d.model_stats(&f).unwrap().len() >= 6);
        assert!(d.project_stats(&f).unwrap().len() >= 5);
        let days: BTreeSet<Timestamp> = d
            .cost_buckets(&f, BucketGroup::Harness)
            .unwrap()
            .into_iter()
            .map(|b| b.start)
            .collect();
        assert!(days.len() >= 25);
        assert!(d.events.iter().all(|e| e.ts <= now()));
    }

    #[test]
    fn aggregates_agree() {
        let d = MockData::new(3, now());
        let f = month(BucketSize::Hour);
        let k = d.kpi_summary(&f).unwrap();
        for group in [
            BucketGroup::Harness,
            BucketGroup::Model,
            BucketGroup::Project,
        ] {
            let b = d.cost_buckets(&f, group).unwrap();
            assert_eq!(b.iter().map(|b| b.cost).sum::<Micros>(), k.cost);
            assert_eq!(b.iter().map(|b| b.events).sum::<u64>(), k.events);
        }
        let models = d.model_stats(&f).unwrap();
        assert_eq!(models.iter().map(|m| m.cost).sum::<Micros>(), k.cost);
        let projects = d.project_stats(&f).unwrap();
        assert_eq!(projects.iter().map(|p| p.events).sum::<u64>(), k.events);
    }

    #[test]
    fn harness_filter_and_detail() {
        let d = MockData::new(5, now());
        let mut f = month(BucketSize::Day);
        f.harnesses = vec![HarnessId::CODEX];
        let rows = d.sessions(&f).unwrap();
        assert!(!rows.is_empty());
        assert!(rows.iter().all(|r| r.harness == HarnessId::CODEX));
        assert!(rows.windows(2).all(|w| w[0].started_at >= w[1].started_at));
        let detail = d.session_detail(rows[0].id).unwrap().unwrap();
        assert_eq!(detail.events.len() as u64, rows[0].events);
        assert_eq!(
            detail.events.iter().map(|e| e.cost).sum::<Micros>(),
            rows[0].cost
        );
        assert!(d.session_detail(SessionId(0)).unwrap().is_none());
        assert_eq!(d.ingest_runs(5).unwrap().len(), 5);
        assert_eq!(d.unpriced_models().unwrap()[0].model, "qwen3-coder");
    }
}
