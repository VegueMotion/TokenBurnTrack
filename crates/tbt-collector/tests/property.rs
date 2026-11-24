mod common;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

use common::*;
use proptest::prelude::*;
use tbt_collector::{Collector, CollectorConfig};
use tbt_core::HarnessAdapter;

struct FixtureSet {
    adapter: fn() -> Arc<dyn HarnessAdapter>,
    root: &'static str,
}

const FIXTURE_SETS: &[FixtureSet] = &[FixtureSet {
    adapter: claude,
    root: CLAUDE_FIXTURES,
}];

const APPENDABLE: &[&str] = &["jsonl"];

fn collector(env: &Env) -> Collector {
    env.collector_with(
        FIXTURE_SETS
            .iter()
            .map(|s| ((s.adapter)(), vec![env.root(s.root)]))
            .collect(),
        CollectorConfig::default(),
    )
}

fn reference() -> &'static Totals {
    static REFERENCE: OnceLock<Totals> = OnceLock::new();
    REFERENCE.get_or_init(|| {
        let roots: Vec<&str> = FIXTURE_SETS.iter().map(|s| s.root).collect();
        let env = Env::with_fixtures(&roots);
        let report = run(&collector(&env));
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        totals(&env.db())
    })
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

struct Pending {
    target: PathBuf,
    chunks: Vec<Vec<u8>>,
    next: usize,
}

fn plan(env: &Env, rng: &mut Rng) -> Vec<Pending> {
    let mut out = Vec::new();
    for set in FIXTURE_SETS {
        let src_root = fixtures_root().join(set.root);
        for file in files_under(&src_root) {
            let target = env
                .root(set.root)
                .join(file.strip_prefix(&src_root).unwrap());
            let bytes = std::fs::read(&file).unwrap();
            let appendable = file
                .extension()
                .is_some_and(|e| APPENDABLE.iter().any(|a| e == *a));
            let chunks = if appendable {
                let lines: Vec<&[u8]> = bytes.split_inclusive(|b| *b == b'\n').collect();
                let mut cuts: Vec<usize> =
                    (0..rng.below(3)).map(|_| rng.below(lines.len())).collect();
                cuts.push(lines.len());
                cuts.sort_unstable();
                cuts.dedup();
                let mut start = 0;
                cuts.into_iter()
                    .filter(|c| *c > 0)
                    .map(|end| {
                        let chunk = lines[start..end].concat();
                        start = end;
                        chunk
                    })
                    .collect()
            } else {
                vec![bytes]
            };
            out.push(Pending {
                target,
                chunks,
                next: 0,
            });
        }
    }
    out
}

fn shuffle_mtimes(pending: &[Pending], rng: &mut Rng) {
    let base = SystemTime::now() - Duration::from_secs(86_400);
    for p in pending.iter().filter(|p| p.next > 0) {
        let at = base + Duration::from_secs(rng.below(80_000) as u64);
        let f = std::fs::File::options()
            .write(true)
            .open(&p.target)
            .unwrap();
        f.set_modified(at).unwrap();
    }
}

fn check(seed: u64, extra_runs: usize) {
    let mut rng = Rng(seed);
    let env = Env::new();
    let mut pending = plan(&env, &mut rng);
    let collector = collector(&env);

    loop {
        let open: Vec<usize> = (0..pending.len())
            .filter(|i| pending[*i].next < pending[*i].chunks.len())
            .collect();
        if open.is_empty() {
            break;
        }
        let p = &mut pending[open[rng.below(open.len())]];
        std::fs::create_dir_all(p.target.parent().unwrap()).unwrap();
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&p.target)
            .unwrap();
        std::io::Write::write_all(&mut f, &p.chunks[p.next]).unwrap();
        drop(f);
        p.next += 1;
        if rng.below(2) == 0 {
            shuffle_mtimes(&pending, &mut rng);
            let report = run(&collector);
            assert!(report.errors.is_empty(), "{:?}", report.errors);
        }
    }
    for _ in 0..=extra_runs {
        shuffle_mtimes(&pending, &mut rng);
        run(&collector);
    }
    let last = run(&collector);
    assert_eq!(last.events_new, 0);

    let db = env.db();
    assert_session_totals_consistent(&db);
    let got = totals(&db);
    let want = reference();
    assert_eq!(got.events, want.events);
    assert_eq!(got.event_sums, want.event_sums);
    assert_eq!(got.hourly, want.hourly);
    assert_eq!(got.chains, want.chains);
    assert_eq!(got.parents, want.parents);
    assert_eq!(got.messages, want.messages);
}

#[test]
fn reference_matches_golden_counts() {
    assert_eq!(reference().events, claude_golden_total());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(12))]

    #[test]
    fn same_sources_in_any_order_and_interleaving_give_identical_totals(
        seed in any::<u64>(),
        extra_runs in 0usize..3,
    ) {
        check(seed, extra_runs);
    }
}
