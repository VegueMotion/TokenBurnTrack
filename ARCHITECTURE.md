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
│  │   ├─ claude_code.rs
│  │   ├─ codex.rs
│  │   ├─ gemini_cli.rs
│  │   ├─ opencode.rs
│  │   └─ generic_jsonl.rs     # config-driven adapter for simple JSONL formats
│  ├─ tbt-pricing/             # price catalog, cost engine, price import
│  ├─ tbt-store/               # SQLite schema, migrations, repositories, rollups
│  ├─ tbt-collector/           # scheduler, ingest pipeline, cursors, dedup
│  ├─ tbt-app/                 # GPUI desktop app (bin)
│  └─ tbt-daemon/              # headless collector (bin) for systemd/launchd
└─ assets/
   └─ prices.toml              # bundled default prices
```

`tbt-collector` is a library. **Both** the GUI and the headless daemon use it, so
hourly tracking keeps running when the window is closed. A lock file
(`$DATA_DIR/collector.lock`) ensures only one process ingests at a time.

### Key crates
| Concern | Crate |
|---|---|
| UI | `gpui` (+ optionally `gpui-component` for tables, inputs and charts) |
| DB | `rusqlite` (bundled, WAL) + `rusqlite_migration` — sync, simple, fast |
| Async / timers | `smol` or GPUI's `BackgroundExecutor` in the app; `tokio` is not needed |
| File watching (optional) | `notify` — triggers an early sync, never replaces the hourly sweep |
| Parsing | `serde`, `serde_json` (streaming line by line), `toml` |
| Money | `rust_decimal` — never `f64` for cost |
| Time | `jiff` or `chrono` (store UTC, display local) |
| Paths | `directories` (XDG / macOS / Windows data dirs) |
| Logging | `tracing` + `tracing-appender` |

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
A `Cursor` is opaque to the collector but is usually
`{ byte_offset, inode, size, mtime, last_line_hash }`.

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
| Claude Code | `~/.claude/projects/<slug>/*.jsonl` | One file per session. `assistant` lines carry `message.usage`. The same `message.id` can repeat across content blocks, so dedup on `message.id + requestId` |
| Codex CLI | `~/.codex/sessions/YYYY/MM/DD/*.jsonl` | `token_count` events can be **cumulative**, so store deltas |
| Gemini CLI | `~/.gemini/tmp/<hash>/…` | Session/chat JSON with usage metadata |
| OpenCode | `~/.local/share/opencode/storage/…` | Per-message JSON; sometimes includes cost |
| Generic JSONL | configured in settings | Field-path mapping (`$.usage.input_tokens` → `input_tokens`) for new or in-house tools |

Each adapter has fixture tests: real sample logs (anonymized) go in `tests/fixtures/<harness>/<version>/`,
with golden outputs. When a harness changes its format, the work is to add a fixture and fix the parser.

---

## 4. Collection pipeline

```
Scheduler tick (hourly / startup / manual / fs-notify debounce)
  └─ acquire collector.lock
     └─ ingest_run = start
        for adapter in registry (parallel per adapter, sequential per source):
           sources = adapter.discover(roots)
           for src in sources:
              cursor = store.cursor(src)          # skip if size/mtime unchanged
              batch  = adapter.read(src, cursor)
              priced = pricing.apply(batch.events)
              store.tx {                          # one transaction per source
                 upsert sessions
                 insert events ON CONFLICT(harness, dedup_key) DO NOTHING
                 insert messages (if enabled)
                 upsert hourly_usage buckets touched
                 save cursor
              }
        ingest_run = finish(stats, warnings)
     └─ release lock, notify UI (channel → GPUI model refresh)
```

Properties:
- **Idempotent.** Unique `(harness, dedup_key)` means a re-run, a crash mid-run or a reset cursor never double counts.
- **Incremental.** The byte-offset cursor means each hourly run reads only new lines. If a file is truncated or rotated (inode changed or size < offset), the source is read again from 0, and dedup absorbs the repeats.
- **Atomic per source.** The cursor and data commit in the same transaction.
- **Missed hours.** The tick only asks "what is new since the cursor", so a laptop that sleeps for 10 hours catches up on the next tick without losing anything.
- **Backfill.** On first run, the full history is ingested the same way.

### Resumed sessions
Harnesses handle resume in two ways, and the pipeline supports both:

| Harness behaviour | What happens |
|---|---|
| Appends to the **same file** | The cursor reads only the new lines. Events attach to the existing `session` row and `ended_at` moves forward |
| Starts a **new file and copies the old conversation** | Copied lines keep their original IDs and are dropped by dedup. Only new calls count. The new file gets a new `session` row, linked to the original with `resumed_from_session_id` |

Rules:
- `dedup_key` is built only from IDs that come from the provider or the harness, never from the session ID or file path. Otherwise copied history would be counted again.
- A resumed session is linked to the session that already holds the first copied message ID. The UI shows the chain and a combined total.
- Cost goes into the hours when each event happened (`usage_event.ts`), not into the session's start date. Resuming a March session today adds cost to today.
- Harnesses that log running totals (Codex `token_count`): deltas start from the last known total of the linked original session. If the counter goes down, it is treated as a reset, so the full old total is never counted as new usage.
- Session duration: `active_seconds` adds up the time between events and leaves out gaps longer than 30 minutes. A session resumed after weeks doesn't look weeks long. The wall-clock span (`started_at` → `ended_at`) is kept as well.
- If the original log file was already deleted by the harness, its data is still in the DB. The resumed part becomes a new session, linked to it if a copied message ID matches.

### Scheduler
- In the app: a GPUI background task loops `Timer::after(until_next_hour)` and aligns to `:00`, plus a startup run.
- Headless: `tbt-daemon --once` launched by a systemd user timer (`OnCalendar=hourly`) / launchd `StartInterval=3600` / Windows Task Scheduler. `tbt-daemon` can also run as a long-lived loop.

---

## 5. Storage (SQLite)

File: `$XDG_DATA_HOME/tokenburntrack/tbt.sqlite`, with `journal_mode=WAL` and `foreign_keys=ON`.

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
  ext_id TEXT NOT NULL, project_id INTEGER REFERENCES project(id),
  title TEXT, git_branch TEXT, harness_version TEXT,
  resumed_from_session_id INTEGER REFERENCES session(id),
  started_at TEXT NOT NULL, ended_at TEXT, active_seconds INTEGER NOT NULL DEFAULT 0,
  -- denormalized totals, maintained on ingest
  events INTEGER NOT NULL DEFAULT 0,
  input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
  cache_write_tokens INTEGER NOT NULL DEFAULT 0, cache_read_tokens INTEGER NOT NULL DEFAULT 0,
  cost_micros INTEGER NOT NULL DEFAULT 0,
  UNIQUE(harness_id, ext_id)
);

CREATE TABLE usage_event (
  id INTEGER PRIMARY KEY,
  harness_id TEXT NOT NULL, dedup_key TEXT NOT NULL,
  session_id INTEGER NOT NULL REFERENCES session(id),
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
CREATE INDEX ix_event_session ON usage_event(session_id);

CREATE TABLE message (            -- optional session content
  id INTEGER PRIMARY KEY, session_id INTEGER NOT NULL REFERENCES session(id),
  ts TEXT NOT NULL, role TEXT NOT NULL, kind TEXT,   -- text / tool_use / tool_result
  tool_name TEXT, preview TEXT,                      -- truncated
  usage_event_id INTEGER REFERENCES usage_event(id)
);

CREATE TABLE hourly_usage (       -- rollup for fast charts
  hour TEXT NOT NULL, harness_id TEXT NOT NULL, model TEXT NOT NULL,
  project_id INTEGER, events INTEGER NOT NULL,
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
- `hourly_usage` is updated in the same transaction as the events. The dashboard reads rollups and never scans the event table for ranges.
- **Privacy setting:** `message` storage is off / metadata-only / truncated preview (default: metadata only, which means role, tool name and timestamps but no text).
- Retention: events are kept forever. They are small (≈200 B per row without raw), so 1M events ≈ 200 MB. `raw_json` can be dropped after N days.

---

## 6. Pricing and cost

```
cost = in   × p_in
     + out  × p_out           (reasoning tokens billed as output unless the model says otherwise)
     + cw5m × p_cache_write_5m
     + cw1h × p_cache_write_1h
     + cr   × p_cache_read
   (each p in USD per 1M tokens; long-context tier multiplier applied when input > threshold)
```

- **Model matching:** exact id first, then glob pattern (`claude-sonnet-4-5*`), then a provider-level fallback marked *unpriced*. Unpriced events show a warning badge in the UI and are not counted as $0.
- **Where cost is stored:** prices are not stored per session or per day. Cost is calculated **per event** (one API call) at ingest and stored in `usage_event.cost_micros`, together with the `price_id` that was used. Session totals and `hourly_usage` (and the day and month views built from it) are sums of events.
- **Versioned:** the price row is chosen by `effective_from <= event.ts < effective_to`. A session that spans a price change is costed correctly, call by call.
- **Sources:** bundled `assets/prices.toml`, an optional import from the LiteLLM price list (`model_prices_and_context_window.json`, fetched on demand), and user overrides in Settings (for negotiated or enterprise rates). Priority: user > imported > bundled.
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
   ├─ TopBar: date-range picker (Today / 7d / 30d / Custom), harness filter, "Sync now", last-sync status
   └─ Main view (one Entity per page)
```

State: one `Entity<AppState>` holds the filters and the last-sync info. Each page owns an
`Entity<PageModel>` that loads its query on the `BackgroundExecutor`, using its own read-only
SQLite connection, and calls `cx.notify()` when the results arrive. The collector sends
`IngestFinished` through a channel, which makes visible pages re-query.

### Pages
| Page | Contents |
|---|---|
| **Dashboard** | KPI tiles (cost in range, total tokens, sessions, cache-hit %, burn rate $/h); stacked **hourly/daily cost bars** by harness; tokens-by-type area chart; top models; top projects; heatmap (weekday × hour) |
| **Sessions** | Virtualized table (`uniform_list`): start, harness, project, model(s), duration, tokens, cost; sort/filter/search |
| **Session detail** | Timeline of calls (tokens per call bar strip), cumulative cost line, tool-use counts, reported vs computed cost, message list (if stored) |
| **Models** | Cost and tokens per model over time, effective $/Mtok, cache savings |
| **Projects** | Cost per repo/branch |
| **Harnesses** | Enable/disable, roots, last ingest, warnings, files tracked |
| **Settings** | Prices editor, privacy level, sync interval, data dir, export CSV/JSON |

### Charts
GPUI has no built-in chart widget. Options:
1. `gpui-component` charts (bar/line/area/pie), quickest if they cover what we need.
2. A small in-house `tbt-app::charts` module built on `canvas()` + `window.paint_quad` / `PathBuilder`: bar, stacked bar, line/area, heatmap, sparkline. This takes about 500 lines and gives full control over hover tooltips and theming.

Recommendation: start with (2) for the bar, line and heatmap charts the dashboard needs, and use `gpui-component` for the table and inputs.

Theme: dark by default. Each harness gets a consistent categorical colour, and the same colour is used in every chart and legend.

---

## 8. Error handling and observability
- Parse errors are recorded per line and never abort a source. Warnings are stored in `ingest_run.warnings_json` and shown on the Harnesses page.
- Unknown model ids → a list of *unpriced* models with a one-click "add price".
- `tracing` logs go to `$DATA_DIR/logs/` with daily rotation.

## 9. Testing
- Adapter golden tests against fixtures (per harness, per format version).
- Collector property test: ingest the same sources N times in random order → identical DB totals.
- Pricing unit tests, including tier boundaries and `effective_from` switching.
- Store migration tests (empty DB → latest; previous release DB → latest).

## 10. Milestones
1. `tbt-core` + `tbt-store` + Claude Code adapter + `tbt-daemon --once` → data in SQLite.
2. Pricing engine + bundled prices + recompute.
3. GPUI shell: Dashboard KPIs + daily bar chart + Sessions table.
4. Hourly scheduler in app + systemd/launchd units for the daemon.
5. Codex, Gemini CLI and OpenCode adapters; generic JSONL adapter.
6. Session detail, heatmap, Models/Projects pages, CSV export.
