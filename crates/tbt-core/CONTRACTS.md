# tbt-core contracts

Frozen at the end of Wave 0. Changes go through the orchestrator.

## Types

| Type | Role |
|---|---|
| `HarnessAdapter` | §3.1 trait: `id`, `display_name`, `default_roots`, `discover`, `read` (`Send + Sync`) |
| `SourceRef` / `SourceKind` | One readable unit (JSONL file, JSON file, SQLite DB) |
| `Cursor` | Opaque JSON per source; `null` = never read. Adapters store their own state via `from_state` / `state_or_default` |
| `ReadBatch` | `sessions`, `events`, `messages`, `next_cursor`, `warnings` |
| `UsageEvent` | §3.2, one API call |
| `PricedEvent` | `UsageEvent` + `cost: Micros` + `price_id` (`None` = unpriced) |
| `SessionUpsert` | Session metadata, `parent_ext_id` (subagent), `resumed_from_ext_id`, `baseline_hint` |
| `MessageRecord` | Role/kind/tool/preview, `key` (stable per message, for dedup), optional `event_dedup_key` |
| `ParseWarning` | Harness, source path, line, byte offset, message |
| `PrivacyLevel` | `Off` / `MetadataOnly` (default) / `TruncatedPreview`; `apply()` is run by the collector |
| `ModelPrice`, `LongContextTier`, `PriceSource` | Price rows shared by store and pricing |
| `TokenCounts` | The six token fields; `checked_sub` / `delta_since` for running counters |
| `Micros`, `Hour`, `TimeRange`, row-id newtypes | See conventions |
| `jsonl::{FileCursor, JsonlReader, read_new_lines}` | Incremental line reader for JSONL adapters |

## Conventions

- **Money** is never `f64`. Rates and harness-reported costs are `Decimal` USD; stored and summed
  amounts are `Micros` (i64, USD × 1e6). Cost per event is `Σ tokens × usd_per_mtok`, which is
  already in micros, rounded once (midpoint away from zero).
- **Time** is UTC `jiff::Timestamp` everywhere. Local time only for display and day/heatmap
  buckets (`QueryFilter::time_zone`). `Hour` is a UTC hour start; ranges are half-open.
- **Tokens:** `input_tokens` is uncached input only (adapters subtract cached tokens if the
  harness includes them). `output_tokens` includes reasoning; `reasoning_tokens` is a subset,
  informational, never billed again. Prompt size for long-context tiers = input + cache writes
  + cache reads.
- **Missing cache rates** are billed at the input rate, including the input long-context
  multiplier. OpenAI's 30-minute and 12-hour cache-write prices use the `cache_write_5m` and
  `cache_write_1h` slots.
- **Dedup keys** come only from provider/harness IDs, never from session ID or file path.

## JSONL cursor

`FileCursor { byte_offset, inode, size, mtime, last_line_hash, last_line_start, line_no }`.
On open: empty → `Fresh`; inode changed → `Restarted(Rotated)`; size < offset →
`Restarted(Truncated)`; size and mtime equal → `Unchanged` (no read); last consumed line's
hash differs → `Restarted(Rewritten)`; else `Continue`. Only `\n`-terminated lines are
consumed, and reading stops at the size seen at open. If the caller stops early, `mtime` is left
unset so the next run does not skip the rest. Adapters with extra state embed `FileCursor` with
`#[serde(flatten)]`.

## Cumulative counters across resumed sessions (decision)

Adopted the plan's proposal, made concrete:

1. **Adapter, per file.** The cursor holds the last raw running total of that file. Each
   counter event becomes a delta: `total.delta_since(prev)` (a component going down is a
   reset, and then the full value counts). The first counter event of a file has no `prev`, so
   its tokens are the raw total. The adapter reports this in
   `SessionUpsert::baseline_hint = BaselineHint { first_event_key, last_total }`:
   `first_event_key` is that first event's `dedup_key` (`None` when the batch continued from a
   cursor), and `last_total` is the raw total at the end of the batch.
2. **Collector, per batch.** It finds the original session: first `resumed_from_ext_id`, and
   if that is missing, `session_holding_dedup_key` of the first event. If an original exists
   and `first_event_key` is `Some`, it reads `base = last_cumulative_total(original)` and
   replaces the first event's tokens with `tokens.delta_since(base)` **before pricing**. With
   no saved base, nothing changes.
3. **Store.** After inserting, the collector calls `save_cumulative_total(session,
   last_total)` for every session with a hint, in the same `SourceTx`.
4. Sources of one adapter are processed in ascending mtime order, so an original is normally
   ingested before the file that resumes it.

Known limits: if a resumed file resets its counter **and** its first total already exceeds the
old total in every component, the delta is undercounted (the same ambiguity the architecture
accepts). If the original was never ingested, the first event counts in full.
