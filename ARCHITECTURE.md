# TokenBurnTrack — Architecture

A local desktop app (Rust + GPUI) that collects AI usage history from coding-agent
harnesses (Claude Code, Codex CLI, Gemini CLI, OpenCode, …), stores it in SQLite,
prices it, and shows usage and cost dashboards.

The diagrams (System, Ingest sequence, Data model) are in `tokenburntrack.openflow.json`, with an SVG export in `tokenburntrack.svg`.

---

## 1. Goals

| # | Requirement | How it is met |
|---|-------------|---------------|
| G1 | Pull usage from harnesses **every hour** | `Scheduler` runs a full sweep every hour, plus once at startup and when the user clicks *Sync now* |
| G2 | Persist usage **and session data** | SQLite (WAL) with `session`, `usage_event` and `message` tables, plus hourly rollups |
| G3 | Support **many harnesses** | One `HarnessAdapter` trait, one adapter per harness, all listed in a registry |
| G4 | Calculate **cost from tokens** | Versioned price table per model, with input, output, cache-write and cache-read rates |
| G5 | **Good UI with stats** | A GPUI app: dashboard, sessions, models, projects, settings |

Non-goals for v1: cloud sync, multiple users, and pulling billing data from provider APIs.
The adapter trait still leaves room for an "API source" adapter later.

### Why snapshot hourly instead of reading logs on demand?
Harness logs are not durable. Claude Code deletes old transcripts (`cleanupPeriodDays`,
30 days by default), users clear caches, and the formats change between versions. An
hourly sweep copies each new event into our own DB before the source can disappear.
Once an event is in the DB, it stays there.

---

## 2. Workspace layout

```
tokenburntrack/
├─ Cargo.toml                  # [workspace]
├─ crates/
│  ├─ tbt-core/                # domain types, HarnessAdapter trait, errors
│  ├─ tbt-adapters/            # one module per harness (feature-gated)
│  │   ├─ discover.rs          # shared recursive source discovery
│  │   ├─ claude_code.rs
│  │   ├─ codex.rs
│  │   ├─ gemini_cli.rs
│  │   ├─ opencode.rs
│  │   └─ generic_jsonl/       # config-driven adapter: mod, config, path (JSONPath-lite), glob
│  ├─ tbt-pricing/             # price catalog, cost engine, price import
│  ├─ tbt-store/               # SQLite schema, migrations, repositories, rollups
│  ├─ tbt-collector/           # scheduler, ingest pipeline, cursors, dedup
│  ├─ tbt-app/                 # GPUI desktop app (bin)
│  └─ tbt-daemon/              # headless collector (bin) for systemd/launchd
├─ assets/
│  └─ prices.toml              # bundled default prices
├─ packaging/                  # systemd, launchd, Task Scheduler, .desktop + icon (embedded in tbt-daemon)
├─ scripts/                    # check.sh, cargo wrapper, anonymize.py
└─ tests/fixtures/             # anonymized harness logs per harness/version, DB snapshots
```

`tbt-collector` is a library. **Both** the GUI and the headless daemon use it, so
hourly tracking keeps running when the window is closed. A lock file
(`$DATA_DIR/collector.lock`) ensures only one process ingests at a time.

### Key crates
| Concern | Crate |
|---|---|
| UI | `gpui-pre` + `gpui-pre-platform` (`wayland` feature; weekly snapshots of Zed's gpui, imported as `gpui` / `gpui_platform`) + `gpui-component` for tables, inputs and theme. All three are pinned with `=` and must be bumped together. The crates.io `gpui` 0.2.x is too old for current `gpui-component` |
| DB | `rusqlite` (bundled, WAL) + `rusqlite_migration` — sync, simple, fast |
| Concurrency | collector: std threads + `crossbeam-channel`; app: GPUI `BackgroundExecutor`; no async runtime |
| File watching (optional) | `notify` — triggers an early sync, never replaces the hourly sweep |
| Parsing | `serde`, `serde_json` (streaming line by line), `toml` |
| Money | `rust_decimal` — never `f64` for cost |
| Time | `jiff` (store UTC, display local) |
| Paths | `directories` (XDG / macOS / Windows data dirs) |
| Logging | `tracing` + `tracing-appender` |
| Other | `fs4` (lock file), `blake3` (line hashes in cursors), `globset` (price patterns), `clap` (daemon CLI), `ureq` (LiteLLM fetch, only on click) |

---

## 3. Harness adapters

### 3.1 Trait

```rust
pub trait HarnessAdapter: Send + Sync {
    fn id(&self) -> HarnessId;                    // "claude_code", "codex", …
    fn display_name(&self) -> &str;
    fn default_roots(&self) -> Vec<PathBuf>;      // user-overridable in Settings
    fn discover(&self, roots: &[PathBuf]) -> Result<Vec<SourceRef>>;
    fn read(&self, src: &SourceRef, cursor: &Cursor) -> Result<ReadBatch>;
}

pub struct ReadBatch {
    pub sessions: Vec<SessionUpsert>,
    pub events:   Vec<UsageEvent>,
    pub messages: Vec<MessageRecord>,   // optional, see §5 privacy setting
    pub next_cursor: Cursor,
    pub warnings: Vec<ParseWarning>,
}
```

A `SourceRef` is a single readable unit, such as a JSONL file, a JSON file or a SQLite DB.
A `Cursor` is opaque to the collector. For log files it is usually
`{ byte_offset, inode, size, mtime, last_line_hash }`; for a harness's own SQLite DB it is a
row watermark per table, e.g. OpenCode's `{ time_updated, id, done[] }` for `session`, `message`
and `part`. Cursors are stored at every privacy level, so they must never contain message text.

### 3.2 Normalized event

```rust
pub struct UsageEvent {
    pub harness: HarnessId,
    pub dedup_key: String,          // stable per real API call
    pub session_ext_id: String,
    pub ts: Timestamp,              // UTC
    pub provider: String,           // anthropic, openai, google, …
    pub model: String,              // raw id as logged
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_write_5m_tokens: u64,
    pub cache_write_1h_tokens: u64,
    pub cache_read_tokens: u64,
    pub reasoning_tokens: u64,      // when reported separately
    pub reported_cost: Option<Decimal>, // if the harness logs its own cost
    pub project_path: Option<String>,
    pub git_branch: Option<String>,
    pub raw: Option<Box<RawValue>>, // kept for re-parsing after format changes
}
```

### 3.3 Initial adapters (typical locations — verify per version)

| Harness | Typical source | Notes |
|---|---|---|
| Claude Code | `~/.claude/projects/<slug>/*.jsonl` and `~/.claude/projects/<slug>/<session-id>/subagents/agent-*.jsonl` | One file per session, plus one per subagent run. Discovery is recursive. `assistant` lines carry `message.usage`. The same `message.id` can repeat across content blocks, so dedup on `message.id + requestId`. Subagent runs become their own `session` rows with `parent_session_id` set to the session in the `<session-id>` directory |
| Codex CLI | `~/.codex/sessions/YYYY/MM/DD/*.jsonl` | One event per response: `token_usage_record` lines (0.155+, dedup key = `response_id`; also the only place compaction calls appear), else `token_count.last_token_usage` (key = thread id + running total; repeated totals are skipped). `cached_input_tokens` is part of `input_tokens` and is subtracted. A resume keeps the thread id, so both files are one session; a fork becomes a new session linked with `resumed_from` |
| Gemini CLI | `~/.gemini/tmp/<project>/chats/*.jsonl` (current, append-only) and `session-*.json` (legacy) | A message is re-appended when it changes, so the adapter holds the newest message back until a later message arrives or the file has been idle for 10 minutes. The held-back message is kept in the cursor without its text, so if it is finalized on a later run it is stored without a preview. Project path from the `.project_root` marker. Fixtures were built from upstream source, not real logs |
| OpenCode | `~/.local/share/opencode/opencode.db` | SQLite (`session`, `message`, `part` tables), opened read-only. Cursor is a row watermark per table, not a byte offset. An assistant message is counted only once it is completed (or errored, or idle for 24 h), because a stored event is never updated. Messages sometimes include cost. If OpenCode is not running (no `-wal` file) and the directory is read-only (as in the hardened systemd unit), the DB is opened with `immutable=1`. Older versions used per-message JSON under `storage/` (not supported) |
| Generic JSONL | configured in settings | Field-path mapping (`$.usage.input_tokens` → `input_tokens`) for new or in-house tools |

Each adapter has fixture tests: real sample logs (anonymized) go in `tests/fixtures/<harness>/<version>/`,
with golden outputs. When a harness changes its format, the work is to add a fixture and fix the parser.

---

## 4. Collection pipeline

```
Scheduler tick (hourly / startup / manual / fs-notify debounce)
  └─ try collector.lock                     # busy → skip this run
     └─ sync bundled prices, recompute rows that were added or corrected
        ingest_run = start
        for adapter in registry (parallel per adapter, sequential per source):
           sources = adapter.discover(roots)
           mark known sources that are gone as missing
           sort sources by mtime, oldest first
           for src in sources:                   # every source, every run
              cursor = store.cursor(src)
              batch  = adapter.read(src, cursor) # cheap when nothing changed
              if batch is empty and cursor unchanged: continue
              link parents / resumed sessions, rebase running totals, apply privacy level
              priced = pricing.apply(batch.events)
              store.tx {                          # one transaction per source
                 upsert sessions
                 insert events ON CONFLICT(harness, dedup_key) DO NOTHING
                 insert messages (if enabled)
                 upsert hourly_usage buckets touched
                 save cursor
              }
        if raw_json_retention_days: set raw_json = NULL on events older than N days
        ingest_run = finish(stats, warnings, per-source errors)
     └─ release lock, notify UI (channel → GPUI model refresh)
```

Properties:
- **Idempotent.** Unique `(harness, dedup_key)` means a re-run, a crash mid-run or a reset cursor never double counts.
- **Incremental.** The byte-offset cursor means each hourly run reads only new lines. If a file is truncated or rotated (inode changed or size < offset), the source is read again from 0, and dedup absorbs the repeats. DB sources (OpenCode) use a row watermark and re-read rows updated since it.
- **Atomic per source.** The cursor and data commit in the same transaction.
- **Always ask the adapter.** The collector never skips a source because its size or mtime is unchanged. OpenCode writes new rows to its `-wal` file without touching the DB file, and the Gemini adapter releases a held-back message on a later read of an unchanged file. Adapters detect "nothing new" themselves.
- **Fault isolation.** Adapters run on their own threads and send batches to a single writer. An error or panic in one source is recorded in `ingest_run` and never aborts the run.
- **Missed hours.** The tick only asks "what is new since the cursor", so a laptop that sleeps for 10 hours catches up on the next tick without losing anything.
- **Backfill.** On first run, the full history is ingested the same way.

### Resumed sessions
Harnesses handle resume in two ways, and the pipeline supports both:

| Harness behaviour | What happens |
|---|---|
| Appends to the **same file** | The cursor reads only the new lines. Events attach to the existing `session` row and `ended_at` moves forward |
| Starts a **new file and copies the old conversation** (Claude Code) | Copied lines keep their original IDs and are dropped by dedup. Only new calls count. The new file gets a new `session` row, linked to the original with `resumed_from_session_id`. Its `started_at` is its first new event, not the copied history's |
| Starts a **new file that points into the old one** without copying (Codex 0.155+ `history_base`) | The adapter reads per-response usage, so nothing needs rebasing. A resume keeps the thread id and maps to the same session; a fork gets its own session linked to the original |

Rules:
- `dedup_key` is built only from IDs that come from the provider or the harness, never from the session ID or file path. Otherwise copied history would be counted again.
- A resumed session is linked to the original named in the log (`resumed_from_ext_id`), or else to the session that already holds the first copied message ID. Linking by copied ID only happens when the new session is first stored. The UI shows the chain and a combined total.
- A subagent run or explicit original that has not been ingested yet gets a placeholder `session` row, filled in when its own file is read.
- Cost goes into the hours when each event happened (`usage_event.ts`), not into the session's start date. Resuming a March session today adds cost to today.
- Adapters prefer per-call usage when the log has it (all current adapters do). For a harness that only logs running totals, the adapter emits deltas within each file and a `baseline_hint`; the collector rebases the first delta of a resumed file against the original session's saved total (`session.cumulative_json`). If the counter goes down, it is treated as a reset, so the full old total is never counted as new usage. See `crates/tbt-core/CONTRACTS.md`.
- Session duration: `active_seconds` adds up the time between events and leaves out gaps longer than 30 minutes. A session resumed after weeks doesn't look weeks long. The wall-clock span (`started_at` → `ended_at`) is kept as well.
- If the original log file was already deleted by the harness, its data is still in the DB. The resumed part becomes a new session, linked to it if a copied message ID matches.

### Scheduler
- Ticks are on a UTC grid (`next_tick(now, interval)`): hourly means UTC `:00`. Sleeping happens in chunks of at most 30 s against the wall clock, so a suspended laptop notices the missed tick when it wakes.
- In the app (`tbt-app::collector_bridge`): a startup run, then a GPUI background task that waits for the next tick; *Sync now* runs a manual one. Runs execute on the background executor and never overlap. When the daemon holds the lock, the app shows "Daemon is syncing…" and polls the newest `ingest_run` row (read-only) until the daemon's run lands, then refreshes. With `TBT_WATCH=1`, a debounced `notify` watcher on the enabled roots also triggers runs (30 s quiet period, at most one run per 2 min); it never replaces the hourly tick.
- Config changes apply on the next run: the app and the long-running daemon re-read `config.toml` before each run and rebuild the collector when it changed; the app also re-reads the interval on every tick. A `data_dir` change needs a restart.
- Headless: `tbt-daemon --once` launched by a systemd user timer (`OnCalendar=hourly`, `Persistent=true`) / launchd `StartInterval=3600` / Windows Task Scheduler. `tbt-daemon` can also run as a long-lived loop. It logs to `$DATA_DIR/logs/` (daily rotation) and exits 0 when the lock is busy.
- `tbt-daemon install [--dry-run] [--no-enable] [--bin <path>] [--app <tbt-app>] [--data-dir <dir>] [--config <file>]` writes and enables the schedule for the current platform, and `uninstall [--dry-run]` removes it. The unit, timer, plist, task XML, `.desktop` file and icon live in `packaging/` and are embedded into the binary, so the repo files are the single source.
- Settings live in `~/.config/tokenburntrack/config.toml` (`privacy`, `sync_interval_minutes`, `data_dir`, `raw_json_retention_days`, `[[generic_jsonl]]` adapters). The data dir is `--data-dir`, else `TBT_DATA_DIR`, else the config, else the platform default.

---

## 5. Storage (SQLite)

File: `$XDG_DATA_HOME/tokenburntrack/tbt.sqlite`, with `journal_mode=WAL`, `foreign_keys=ON`, `synchronous=NORMAL` and a 5 s busy timeout.

```sql
CREATE TABLE harness (
  id TEXT PRIMARY KEY, display_name TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1, roots_json TEXT
);

CREATE TABLE source_file (
  id INTEGER PRIMARY KEY, harness_id TEXT NOT NULL REFERENCES harness(id),
  path TEXT NOT NULL, cursor_json TEXT NOT NULL,
  first_seen TEXT NOT NULL, last_read TEXT, missing INTEGER NOT NULL DEFAULT 0,
  UNIQUE(harness_id, path)
);

CREATE TABLE project (
  id INTEGER PRIMARY KEY, path TEXT UNIQUE NOT NULL, name TEXT
);

CREATE TABLE session (
  id INTEGER PRIMARY KEY, harness_id TEXT NOT NULL REFERENCES harness(id),
  ext_id TEXT NOT NULL, project_id INTEGER NOT NULL DEFAULT 0 REFERENCES project(id),
  title TEXT, git_branch TEXT, harness_version TEXT,
  resumed_from_session_id INTEGER REFERENCES session(id),
  parent_session_id INTEGER REFERENCES session(id),   -- subagent run → session that spawned it
  started_at TEXT NOT NULL, ended_at TEXT, active_seconds INTEGER NOT NULL DEFAULT 0,
  -- denormalized totals, maintained on ingest
  events INTEGER NOT NULL DEFAULT 0, unpriced_events INTEGER NOT NULL DEFAULT 0,
  input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
  cache_write_tokens INTEGER NOT NULL DEFAULT 0, cache_read_tokens INTEGER NOT NULL DEFAULT 0,
  cost_micros INTEGER NOT NULL DEFAULT 0,
  cumulative_json TEXT,          -- last running total (Codex token_count), for resume rebasing
  UNIQUE(harness_id, ext_id)
);

CREATE TABLE usage_event (
  id INTEGER PRIMARY KEY,
  harness_id TEXT NOT NULL, dedup_key TEXT NOT NULL,
  session_id INTEGER NOT NULL REFERENCES session(id),
  project_id INTEGER NOT NULL DEFAULT 0 REFERENCES project(id),  -- rollup bucket chosen at ingest
  ts TEXT NOT NULL, provider TEXT NOT NULL, model TEXT NOT NULL,
  input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,
  cache_write_5m_tokens INTEGER NOT NULL, cache_write_1h_tokens INTEGER NOT NULL,
  cache_read_tokens INTEGER NOT NULL, reasoning_tokens INTEGER NOT NULL,
  cost_micros INTEGER NOT NULL,            -- computed by us (USD * 1e6)
  reported_cost_micros INTEGER,            -- what the harness claimed, if any
  price_id INTEGER REFERENCES model_price(id),
  source_file_id INTEGER REFERENCES source_file(id),
  raw_json TEXT,
  UNIQUE(harness_id, dedup_key)
);
CREATE INDEX ix_event_ts ON usage_event(ts);
CREATE INDEX ix_event_session ON usage_event(session_id, ts);
CREATE INDEX ix_event_unpriced ON usage_event(provider, model) WHERE price_id IS NULL;

CREATE TABLE session_model (      -- models used per session, for the sessions table
  session_id INTEGER NOT NULL REFERENCES session(id), model TEXT NOT NULL,
  PRIMARY KEY(session_id, model)
) WITHOUT ROWID;

CREATE TABLE message (            -- optional session content
  id INTEGER PRIMARY KEY, session_id INTEGER NOT NULL REFERENCES session(id),
  key TEXT NOT NULL,                                 -- stable per message, from harness IDs
  ts TEXT NOT NULL, role TEXT NOT NULL, kind TEXT,   -- text / tool_use / tool_result / thinking
  tool_name TEXT, preview TEXT,                      -- truncated
  usage_event_id INTEGER REFERENCES usage_event(id),
  UNIQUE(session_id, key)
);

CREATE TABLE hourly_usage (       -- rollup for fast charts
  hour TEXT NOT NULL, harness_id TEXT NOT NULL, model TEXT NOT NULL,
  project_id INTEGER NOT NULL REFERENCES project(id),
  events INTEGER NOT NULL, unpriced_events INTEGER NOT NULL,
  input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,
  cache_write_tokens INTEGER NOT NULL, cache_read_tokens INTEGER NOT NULL,
  cost_micros INTEGER NOT NULL,
  PRIMARY KEY(hour, harness_id, model, project_id)
);

CREATE TABLE model_price (
  id INTEGER PRIMARY KEY, provider TEXT NOT NULL, model_pattern TEXT NOT NULL,
  effective_from TEXT NOT NULL, effective_to TEXT,
  input_per_mtok TEXT NOT NULL, output_per_mtok TEXT NOT NULL,
  cache_write_5m_per_mtok TEXT, cache_write_1h_per_mtok TEXT,
  cache_read_per_mtok TEXT, long_context_threshold INTEGER,
  long_context_multiplier_json TEXT, source TEXT NOT NULL   -- bundled / imported / user
);

CREATE TABLE ingest_run (
  id INTEGER PRIMARY KEY, trigger TEXT NOT NULL,   -- hourly/startup/manual/watch
  started_at TEXT NOT NULL, finished_at TEXT,
  sources_scanned INTEGER, events_new INTEGER, warnings_json TEXT, error TEXT
);
```

Notes:
- Money is stored as integer **micro-USD**, so sums never drift.
- Timestamps are stored as fixed-width UTC text (`YYYY-MM-DDTHH:MM:SS.nnnnnnnnnZ`), so text order equals time order.
- `usage_event.project_id` records which project bucket the event was rolled up into (the event's own project, else its session's), so `hourly_usage` can be rebuilt exactly after a recompute.
- `hourly_usage` is updated in the same transaction as the events. The dashboard reads rollups and never scans the event table for ranges.
- `hourly_usage.project_id` is `NOT NULL`. Events without a project go to a seeded `project` row with `id = 0` ("no project"). SQLite treats NULLs in a primary key as distinct, so a nullable column would make `ON CONFLICT` upserts insert duplicate rows instead of adding to the bucket.
- **Privacy setting:** `message` storage is off / metadata-only / truncated preview (default: metadata only, which means role, tool name and timestamps but no text). Adapters always emit previews; the collector applies the privacy level before storing.
- `message.key` makes message inserts idempotent the same way `dedup_key` does for events, so a re-read file does not duplicate messages. Uniqueness is per session: history that Claude Code copies into a resumed session is stored again under the new session (its events are not, so costs are unaffected; tool counts in that session's detail include the copied history).
- `session.cumulative_json` holds the last running token total of sessions from harnesses that log cumulative counters. The collector uses it to rebase the first delta of a resumed session (see §4 *Resumed sessions* and `crates/tbt-core/CONTRACTS.md`).
- Retention: events are kept forever. They are small (≈200 B per row without raw), so 1M events ≈ 200 MB. `raw_json_retention_days` (config, Settings → General) sets `raw_json` to NULL on events older than N days at the end of every ingest run.

---

## 6. Pricing and cost

```
cost = in   × p_in
     + out  × p_out           (reasoning tokens billed as output unless the model says otherwise)
     + cw5m × p_cache_write_5m
     + cw1h × p_cache_write_1h
     + cr   × p_cache_read
   (each p in USD per 1M tokens; long-context tier multipliers applied when the prompt
    size, input + cache writes + cache reads, is above the threshold; a missing cache
    rate is billed at the input rate, including its multiplier)
```

- **Model matching:** among rows of the event's provider that are effective at its timestamp, higher source priority wins (user > imported > bundled), then an exact id over a glob (`claude-sonnet-4-5*`), then the glob with the longest literal prefix, then the latest `effective_from`. No match → *unpriced*. Unpriced events show a warning badge in the UI and are not counted as $0.
- **Where cost is stored:** prices are not stored per session or per day. Cost is calculated **per event** (one API call) at ingest and stored in `usage_event.cost_micros`, together with the `price_id` that was used. Session totals and `hourly_usage` (and the day and month views built from it) are sums of events.
- **Versioned:** the price row is chosen by `effective_from <= event.ts < effective_to`. A session that spans a price change is costed correctly, call by call.
- **Sources:** bundled `assets/prices.toml`, an optional import from the LiteLLM price list (`model_prices_and_context_window.json`, fetched on demand), and user overrides in Settings (for negotiated or enterprise rates). Priority: user > imported > bundled.
- **Bundled rows** are re-synced at the start of every run (same provider + pattern + `effective_from` = same row); a changed bundled row is overwritten and its events recomputed. Settings edits only user rows; bundled and imported rows are overridden, not edited.
- **Import never rewrites history:** the LiteLLM list has no dates. On import, a price that differs from the current row closes that row (`effective_to = now`) and adds a new row with `effective_from = now`. Unchanged prices are skipped.

### When a price changes
| Kind of change | Effect on old usage |
|---|---|
| Provider changes its price (new row, `effective_from` = change date) | None. Old events keep their stored cost |
| A price was wrong (edit the row), or an override is set back in time | A **recompute job** finds affected events by `price_id` / time range, updates `cost_micros`, and rebuilds the session totals and `hourly_usage` for those hours |
| Price added for an *unpriced* model | The same recompute job, run on that model's unpriced events |

`reported_cost_micros` (the cost a harness logged itself) never changes.
- **Reported vs computed:** when a harness logs its own cost, store both and show the difference in Session detail. Subscription plans (Max/Pro) are shown as "API-equivalent cost".

---

## 7. UI (GPUI)

### Structure
```
TbtApp (Application)
└─ Workspace window
   ├─ Sidebar: Dashboard · Sessions · Models · Projects · Harnesses · Settings
   ├─ TopBar: date range (Today / 7d / 30d; Custom is shown disabled), harness filter, "Sync now", last-sync status
   └─ Main view (one Entity per page)
```

State: one `Entity<AppState>` holds the filters and the last-sync info, and emits
`FiltersChanged`, `DataChanged`, `OpenSession(id)` and `CloseSession`. Each page loads its
queries on the `BackgroundExecutor` through a `DataSource` (`StoreData`: a small pool of
read-only SQLite connections; `MockData` with `TBT_MOCK=1`) and calls `cx.notify()` when the
results arrive. After a finished sync, or after a page writes (harness toggles, prices,
config), `DataChanged` makes visible pages re-query and refreshes the harness filter.
Pages that write open a short-lived writable connection; harness enable and roots edits
apply directly, while price changes also hold
`collector.lock` so they never interleave with an ingest run.

### Pages
| Page | Contents |
|---|---|
| **Dashboard** | KPI tiles (cost in range, total tokens, sessions, cache-hit %, burn rate $/h); stacked **hourly/daily cost bars** by harness; tokens-by-type area chart; top models; top projects; heatmap (weekday × hour) |
| **Sessions** | Virtualized table (`uniform_list`): start, harness, project, model(s), duration, tokens, cost; sort/filter/search |
| **Session detail** | Opened from a Sessions row. Timeline of calls (tokens per call bar strip), cumulative cost line, tool-use counts, reported vs computed cost ("API-equivalent" when the harness logs none), resume chain with combined total, parent and subagents, message list (if stored) |
| **Models** | Cost and tokens per model over time, effective $/Mtok, cache savings |
| **Projects** | Cost per project (by the directory each call ran in) with a branch breakdown; "no project" as its own row |
| **Harnesses** | Enable/disable, roots (custom or default, editable), last ingest, files tracked/missing, warnings and errors, recent runs |
| **Settings** | Tabs: Prices (user overrides with "price changed from" or "fix this row", unpriced models with "add price", each followed by a recompute), LiteLLM import (preview, then apply), General (privacy level, sync interval, raw JSON retention, data dir), Generic JSONL (TOML editor validated by the adapter), Export (sessions or hourly usage as CSV/JSON for the current filter) |

### Charts
GPUI has no built-in chart widget. Charts are an in-house `tbt-app::charts` module built on
`canvas()` + `window.paint_quad` / `PathBuilder`: bar, stacked bar, line/area, heatmap,
sparkline and the call timeline, with hover tooltips. Scales, ticks, stacking, hit testing
and formatting are plain functions with unit tests. `gpui-component` provides buttons,
inputs and theming.

Theme: dark by default. Each harness gets a consistent categorical colour, and the same colour is used in every chart and legend.

---

## 8. Error handling and observability
- Parse errors are recorded per line and never abort a source. Warnings are stored in `ingest_run.warnings_json` and shown on the Harnesses page.
- Unknown model ids → a list of *unpriced* models with a one-click "add price".
- The daemon logs to stderr and `$DATA_DIR/logs/tbt-daemon.<date>.log` (daily rotation, 30 files kept); the app writes no log files. Per-source errors are stored in `warnings_json` with an `error:` prefix.

## 9. Testing
- Adapter golden tests against fixtures (per harness, per format version).
- Collector property test: ingest the same sources N times in random order → identical DB totals.
- Pricing unit tests, including tier boundaries and `effective_from` switching.
- Store migration tests (empty DB → latest; a Wave 2 database snapshot in `tests/fixtures/db/` → latest).
- End-to-end tests: backfill of all harness fixtures, hourly re-runs, resumes in every harness, deleted sources, back-dated price overrides, and the daemon binary in a sandbox.

## 10. Milestones
1. `tbt-core` + `tbt-store` + Claude Code adapter + `tbt-daemon --once` → data in SQLite.
2. Pricing engine + bundled prices + recompute.
3. GPUI shell: Dashboard KPIs + daily bar chart + Sessions table.
4. Hourly scheduler in app + systemd/launchd units for the daemon.
5. Codex, Gemini CLI and OpenCode adapters; generic JSONL adapter.
6. Session detail, heatmap, Models/Projects pages, CSV export.
