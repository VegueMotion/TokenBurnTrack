# TokenBurnTrack — Implementation Plan

Implements `ARCHITECTURE.md` with an orchestrator (main session) plus subagents.
Work is split into **waves**. Jobs inside a wave run in parallel; a wave starts only
after the previous wave is merged and green.

---

## 0. Orchestration rules

| Rule | Detail |
|---|---|
| Isolation | Every parallel job runs in its own git worktree (`isolation: "worktree"`), on branch `wN/<job-id>` |
| File ownership | Each job owns the files listed for it. It must not edit files owned by another job. Shared files (`Cargo.toml`, `lib.rs` module lists, registry) are edited only by the orchestrator at merge time |
| Frozen contracts | `tbt-core` types, the store API signatures and the pricing API signatures are frozen at the end of Wave 0. A job that needs a change stops and reports it; the orchestrator changes the contract and rebases the other jobs |
| Dependencies | All third-party crates are declared once in `[workspace.dependencies]` in Wave 0, so parallel jobs do not fight over `Cargo.toml` / `Cargo.lock` |
| Definition of done | `cargo fmt --check`, `cargo clippy -p <crate> -- -D warnings`, `cargo test -p <crate>` pass; job reports files changed, tests added, open issues |
| Merge | Orchestrator merges wave branches in the listed order, runs `cargo test --workspace`, fixes integration breaks, then starts the next wave |
| Docs | Any deviation from the architecture updates `ARCHITECTURE.md` **and** the diagrams in `tokenburntrack.openflow.json` in the same merge |
| Privacy | Fixtures copied from `~/.claude`, `~/.codex`, … are anonymized (paths, prompts, code, user names replaced) before they are committed |

Local facts that shape the plan:
- Rust 1.100 on Fedora 45 (Wayland) is available.
- Real logs exist for Claude Code (333 files), Codex (168), OpenCode (11). **Gemini CLI is not installed**, so its fixtures must come from upstream docs/source and get lower confidence.

---

## 1. Timeline overview

```
Wave 0  Foundation        J0.1 ─┬─ J0.2 contracts ───────────┐
        (gate)                  ├─ J0.3 GPUI spike ──────────┤
                                └─ J0.4 fixture harvest ─────┤
                                                             ▼ contracts frozen
Wave 1  Core libraries    A store │ B pricing │ C claude-code adapter │ D UI shell + charts
                                                             ▼ merge + green
Wave 2  Pipeline & more   E collector+daemon │ F recompute+price seed │ G codex │ H opencode
        adapters / data   I gemini │ J generic-jsonl │ K dashboard+sessions on real data
                                                             ▼ merge + green  (= M1–M3, M5)
Wave 3  Product           L app scheduler+sync │ M session detail+models │ N projects+harnesses
                          O settings+export+import │ P service units+packaging
                                                             ▼ merge + green  (= M4, M6)
Wave 4  Hardening         Q e2e on real data │ R perf 1M events │ S review + docs sync
```

Max parallel agents per wave: W0 3, W1 4, W2 7, W3 5, W4 3.

---

## 2. Wave 0 — Foundation (gate)

### J0.1 Workspace skeleton — orchestrator, sequential, first
- `Cargo.toml` workspace with all 8 crates from §2 of the architecture, empty `lib.rs`/`main.rs`.
- `[workspace.dependencies]`: gpui (version chosen by J0.3; placeholder until then), rusqlite (bundled), rusqlite_migration, serde, serde_json, toml, rust_decimal, jiff, directories, tracing, tracing-subscriber, tracing-appender, smol, notify, globset, fs4 (lock file), thiserror, anyhow; dev: insta, proptest, tempfile.
- `scripts/check.sh` (fmt + clippy + test), `rust-toolchain.toml`, `.gitignore` for `target/`.

### J0.2 Contracts — 1 agent
Owns `crates/tbt-core/**`, stub signatures in `crates/tbt-store/src/api.rs`, `crates/tbt-pricing/src/api.rs`.
- `tbt-core`: `HarnessId`, `SourceRef`, `Cursor` (serde, opaque JSON), `UsageEvent`, `SessionUpsert`, `MessageRecord`, `ReadBatch`, `ParseWarning`, `PrivacyLevel`, `Micros` newtype, error type, `HarnessAdapter` trait (exactly §3.1).
- Shared JSONL helper `tbt-core::jsonl` (or `tbt-adapters::util`): incremental line reader with byte-offset cursor, inode/size/mtime check, truncation → restart from 0. Every adapter reuses it.
- Store API stubs (`todo!()` bodies, real signatures): `open`, `begin_source_tx`, `upsert_session`, `insert_events`, `insert_messages`, `bump_hourly`, `save_cursor`, `cursor_for`, `start_run`/`finish_run`, plus read-side query types for the UI: `KpiSummary`, `CostBucket`, `SessionRow`, `SessionDetail`, `ModelStats`, `ProjectStats`, `HarnessStatus`.
- Pricing API stubs: `PriceCatalog::load`, `price_for(provider, model, ts) -> PriceMatch`, `cost(&UsageEvent, &Price) -> Micros`, `import_litellm(json, now) -> Vec<PriceChange>`.
- **Decide here:** how cumulative counters (Codex `token_count`) cross a resumed-session boundary. Proposal: adapter emits per-file deltas using state in its `Cursor`; `SessionUpsert` carries `resumed_from_ext_id` + `baseline_hint`, and the collector rebases the first delta against the linked session's last total.

### J0.3 GPUI spike — 1 agent, parallel with J0.2
Owns `crates/tbt-app/**` (throwaway content).
- Choose gpui source (crates.io release vs pinned Zed git rev); check `gpui-component` compatibility with that version.
- Build and open a window on Fedora/Wayland; list required system packages.
- Prove: `canvas()` painting a bar + path, `uniform_list` with 100k rows, a `BackgroundExecutor` task posting back to an `Entity`.
- Output: chosen versions written into `[workspace.dependencies]`, short notes in the job report.

### J0.4 Fixture harvest — 1 agent, parallel with J0.2
Owns `tests/fixtures/**`, `scripts/anonymize.*`.
- Copy 3–5 representative files per harness (short, long, resumed session, multi-model, tool-heavy), record harness version.
- Anonymizer script: rewrites paths, text content, IDs consistently (same input → same fake value, so dedup keys still collide where they should).
- Gemini CLI: build fixtures from upstream source/docs; mark `confidence: low` in a `README` next to them.

**Gate:** orchestrator reviews contracts, merges, tags `contracts-v1`.

---

## 3. Wave 1 — Core libraries (4 parallel agents)

| Job | Owns | Delivers | Tests |
|---|---|---|---|
| **A store** | `crates/tbt-store/**` | Migrations for the full §5 schema; WAL + FK pragmas; all write API from J0.2; hourly rollup upsert in the same tx; session totals + `active_seconds` (30-min gap rule); read queries for UI types; read-only connection constructor | Migration test empty→latest; ON CONFLICT dedup; rollup = sum of events; tx rollback leaves cursor unchanged |
| **B pricing** | `crates/tbt-pricing/**`, `assets/prices.toml` | TOML format + bundled prices for current Anthropic/OpenAI/Google models; exact → glob → unpriced matching; `effective_from/to` selection; long-context multiplier; `rust_decimal` math → micros; LiteLLM import diff (close row, open row, skip unchanged); priority user > imported > bundled | Tier boundaries; price switch mid-session; glob precedence; import idempotency |
| **C claude-code adapter** | `crates/tbt-adapters/src/claude_code.rs`, `tests/fixtures/claude_code/**`, golden snapshots | `discover` over `~/.claude/projects/*/*.jsonl`; parse `assistant` lines; dedup key `message.id + requestId`; cache 5m/1h split; project path / branch / version; messages per privacy level; resumed sessions (copied IDs) | Golden (`insta`) per fixture; re-read from cursor yields no dupes; truncated file restarts |
| **D UI shell + charts** | `crates/tbt-app/**` | Window, sidebar, top bar (date range, harness filter, Sync button stub), page router, dark theme, harness colour map; `charts` module: bar, stacked bar, line/area, heatmap, sparkline with hover tooltips; pages render from a `MockData` impl of the read query types | Snapshot/unit tests on chart scale math; manual screenshot in report |

C establishes the adapter pattern (file layout, golden test harness) that Wave 2 adapters copy.

Merge order: A → B → C → D.

---

## 4. Wave 2 — Pipeline, adapters, real data (7 parallel agents)

| Job | Owns | Delivers | Depends on |
|---|---|---|---|
| **E collector + daemon** | `crates/tbt-collector/src/{lib,pipeline,lock,scheduler,registry}.rs`, `crates/tbt-daemon/**` | §4 pipeline: lock file, `ingest_run`, adapters in parallel / sources sequential, skip unchanged sources, pricing applied, one tx per source, resumed-session linking, cumulative baseline rebasing, `IngestFinished` channel; `tbt-daemon --once` and loop mode; `tracing` to `$DATA_DIR/logs` | A, B, C |
| **F recompute + seed** | `crates/tbt-collector/src/recompute.rs`, `crates/tbt-collector/src/price_seed.rs` | Seed `model_price` from bundled TOML on startup; recompute job by `price_id` / time range / unpriced model; rebuild session totals and affected `hourly_usage` hours | A, B |
| **G codex adapter** | `adapters/src/codex.rs`, `fixtures/codex/**` | Cumulative `token_count` → deltas, reset handling, cursor state | C pattern |
| **H opencode adapter** | `adapters/src/opencode.rs`, `fixtures/opencode/**` | Per-message JSON, `reported_cost` | C pattern |
| **I gemini adapter** | `adapters/src/gemini_cli.rs`, `fixtures/gemini_cli/**` | Session JSON usage metadata; feature-gated; low-confidence note | C pattern |
| **J generic JSONL** | `adapters/src/generic_jsonl.rs` | Field-path mapping config (serde), JSONPath-lite evaluator, dedup-key template | C pattern |
| **K dashboard + sessions** | `crates/tbt-app/src/pages/{dashboard,sessions}.rs`, `crates/tbt-app/src/data.rs` | Swap `MockData` for real read-only store queries on `BackgroundExecutor`; KPI tiles, stacked hourly/daily cost bars, tokens-by-type area, top models/projects; virtualized sessions table with sort/filter/search | A, D |

Orchestrator at merge: register G/H/I/J in `registry.rs` and adapter features; run E's property test (ingest the same sources N times in random order → identical totals) against all fixtures.

Merge order: E → F → C-family adapters (G, H, I, J) → K.
After merge: first real run `tbt-daemon --once` on this machine; compare Claude Code totals with a manual spot check of a few sessions.

---

## 5. Wave 3 — Product completion (5 parallel agents)

| Job | Owns | Delivers |
|---|---|---|
| **L app scheduler + sync** | `tbt-app/src/{collector_bridge,app_state}.rs` | Startup run, `:00`-aligned hourly timer, Sync now, last-sync status, `IngestFinished` → visible pages re-query; lock contention with daemon handled gracefully; optional `notify` watcher with debounce |
| **M session detail + models** | `pages/{session_detail,models}.rs` | Call timeline bars, cumulative cost line, tool-use counts, reported vs computed, message list, resume chain + combined total; per-model cost/tokens over time, effective $/Mtok, cache savings |
| **N projects + harnesses + heatmap** | `pages/{projects,harnesses}.rs`, dashboard heatmap widget | Cost per repo/branch; enable/disable, roots, last ingest, warnings, files tracked; weekday × hour heatmap |
| **O settings** | `pages/settings.rs`, `tbt-app/src/export.rs` | Prices editor (user overrides → triggers F's recompute), unpriced-models list with "add price", LiteLLM import button, privacy level, sync interval, data dir, CSV/JSON export |
| **P service units** | `packaging/**`, `README.md` install section | systemd user timer + service, launchd plist, Windows Task Scheduler XML, `tbt-daemon install` helper |

Merge order: L → M → N → O → P.

---

## 6. Wave 4 — Hardening (3 parallel agents)

| Job | Delivers |
|---|---|
| **Q end-to-end** | Fresh data dir → backfill all local harnesses → hourly re-run → resume a session → verify no double count; migration test from the Wave 2 DB to latest |
| **R performance** | Synthetic 1M-event DB: dashboard queries < 50 ms from rollups, sessions table scroll smooth, incremental hourly run < 1 s when nothing changed |
| **S review + docs** | `/code-review` on the full branch, fix findings; sync `ARCHITECTURE.md` + diagrams with what was actually built; `raw_json` retention setting |

---

## 7. Mapping to architecture milestones

| Milestone | Done after |
|---|---|
| M1 core + store + Claude adapter + daemon | Wave 2 (E) |
| M2 pricing + bundled prices + recompute | Wave 2 (F) |
| M3 GPUI shell + KPIs + daily bars + sessions | Wave 2 (K) |
| M4 hourly scheduler + service units | Wave 3 (L, P) |
| M5 Codex, Gemini, OpenCode, generic JSONL | Wave 2 (G–J) |
| M6 session detail, heatmap, models/projects, export | Wave 3 (M, N, O) |

---

## 8. Risks

| Risk | Mitigation |
|---|---|
| GPUI API churn / build issues on Wayland | J0.3 spike pins one version before any UI work |
| Contract change mid-wave | Only the orchestrator edits frozen contracts; affected jobs rebase |
| Gemini format guessed without real logs | Feature-gated, low-confidence fixtures, revisit when installed |
| Cumulative-counter rebasing across resumes | Decided in J0.2, property-tested in E |
| Fixtures leaking private content | Anonymizer in J0.4; orchestrator greps fixtures for home paths before merge |
