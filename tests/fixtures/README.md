# Adapter fixtures

Real harness logs from one machine (harvested 2026-11-23), anonymized with
`scripts/anonymize.py`. Gemini CLI fixtures are synthetic (see `gemini_cli/README.md`,
confidence: low), and so are the GitHub Copilot CLI and Antigravity ones (see below). The Cursor fixture is synthetic too:
`cursor/3.31.15/User/globalStorage/state.vscdb` is built from `cursor/fixture.sql`
(`sqlite3 state.vscdb < fixture.sql`) with the table layout of a real Cursor 3.31.15
database, and holds 2 chats and 3 calls with token counts.

Layout: `<harness>/<harness-version>/<path as the harness writes it, below its data dir>`.

## What the anonymizer keeps and changes

Kept as-is: JSON structure and field names, line order, token counts and all other
numbers, timestamps, model ids, provider ids, tool names, event/line types, enum-like
fields (`stop_reason`, `service_tier`, `status`, …), reported cost fields.

Replaced consistently (same input → same output across all files and the DB, keyed
HMAC with a fixed salt):

- UUIDs → fake UUIDs (same version/variant; UUIDv7 keeps its 48-bit time prefix, so
  Codex ids still sort by time).
- Prefixed ids (`msg_…`, `req_…`, `toolu_…`, `call_…`, `resp_…`) → same prefix,
  same length and character classes. OpenCode ids (`ses_`/`msg_`/`prt_`/`evt_`) keep
  their 12-hex time prefix.
- Hex ids/hashes (git SHAs, OpenCode project ids, `shapeHash`, …), Claude Code agent
  ids (`a` + 16 hex), dict keys that are ids.
- Paths: the home dir prefix → `/u/user`, other segments → fake names like `pale-fjord-8a`
  (extension kept). Claude Code project dir slugs are recomputed from the fake cwd.
- Git branches other than `main`/`master`/… → `branch-xxxxxx`; URLs →
  `https://example.invalid/…`; timezone → `UTC`; emails, user and host names scrubbed.
- All other text (prompts, responses, thinking, code, tool inputs/outputs, titles) →
  lorem text of the same length, capped at 160 chars. JSON embedded in strings (e.g.
  Codex `arguments`, OpenCode `data` columns) is parsed and anonymized recursively.

Consequences: dedup keys still collide where they did in the real data (copied
lines in resumed Claude Code sessions), `parentUuid` chains still link, subagent file
names still match `agentId`, the Codex `history_base` thread id still names the base
file.

Files over ~290 KB were trimmed (`--max-bytes 290000`): non-usage lines are dropped
largest first (children re-linked to the dropped line's parent), then, if still too
big, lines are cut from the end. Usage lines are never dropped before the tail cut.
Trimmed files are noted below. Codex `history_base.end_byte_offset` is recomputed
for the trimmed base file.

Regenerate: copy the raw sources into a staging dir with this same layout, then
`python3 scripts/anonymize.py <staging> <out> --max-bytes 290000 --scrub <name>`, where
`--scrub` was given the desktop client name that Codex writes into `originator`.
Output is byte-identical between runs.

## Claude Code (`claude_code/<version>/projects/<slug>/`)

| File | Version | Scenario | Size |
|---|---|---|---|
| `2.1.330/…-pale-fjord-8a/35395b06-….jsonl` | 2.1.330 | short: 1 API call (sonnet), 44 lines | 96 KB |
| `2.1.330/…-pale-quartz-b9/981f8c38-….jsonl` | 2.1.330 | long: prefix of a 791-line session, 178 lines, 64 of 88 calls (trimmed) | 284 KB |
| `2.1.330/…-pale-fjord-8a/64ed9da4-….jsonl` + `64ed9da4-…/subagents/agent-a919fc890880d1075.jsonl` | 2.1.330 | subagents: main (opus, 19 calls) + one subagent run (haiku, 6 calls) | 244 + 96 KB |
| `2.1.329/…-pale-fjord-8a/3fc87bac-….jsonl` + `d1774c7e-….jsonl` | 2.1.329 (+2.1.330 lines in the second) | resumed: original (50 calls) and the session that continues it (55 calls, 50 shared with the original) | 284 + 284 KB |
| `2.1.325/…-keen-lagoon-45/f001a22b-….jsonl` | 2.1.325 | tool-heavy: 69 tool_use blocks, 53 of 69 calls (trimmed) | 284 KB |
| `2.1.319/…-noble-orchid-25/ee7bdb9e-….jsonl` | 2.1.319 | multi-model: `claude-fable-5-2` + `claude-opus-5-5` + a `<synthetic>` API-error line; spans 2 days | 184 KB |

Format notes:

- Every line has `type`. Usage lines are `type: "assistant"` with
  `message.usage` = `{input_tokens, output_tokens, cache_read_input_tokens,
  cache_creation_input_tokens, cache_creation: {ephemeral_5m_input_tokens,
  ephemeral_1h_input_tokens}, output_tokens_details.thinking_tokens, service_tier,
  speed, inference_geo, iterations[…], server_tool_use{web_search_requests,
  web_fetch_requests}}`. `input_tokens` excludes cache reads/writes.
- One API response is split into one line per content block (`thinking`, `text`,
  `tool_use`), each with the same `message.id` + `requestId` and an identical,
  final `usage`, distinguished by `apiBlockIndex`. Count each `(message.id,
  requestId)` once.
- Model: `message.model`; also `requestedModel`/`advisorModel` on the line.
  `<synthetic>` lines (`isApiErrorMessage: true`) have all-zero usage and a UUID as
  `message.id`; skip them.
- Per line: `sessionId`, `cwd`, `gitBranch`, `version`, `timestamp` (UTC ISO),
  `uuid`/`parentUuid`. A file can contain two `version` values when the CLI was
  updated mid-session.
- Many non-message line types exist (`api-request`, `api-request-blob`,
  `attachment`, `system`, `file-history-snapshot`, `data-flow-records`, `mode`,
  `ai-title`, `last-prompt`, `session-owner`, …); ignore unknown types.
  `api-request` lines carry `params.model` and an `id` that assistant lines reference
  as `requestRef`.
- `cost-state` lines: running per-session `totalCostUSD` and `modelUsage[model]`
  (`inputTokens`, `outputTokens`, `cacheRead…`, `cacheCreationEphemeral5m/1h…`,
  `costUSD`). They include background calls that never appear as assistant lines
  (e.g. haiku title generation in the short fixture), so they don't sum to the
  transcript. Useful only as a reported-cost cross-check.
- Subagents: `<session-id>/subagents/agent-<agentId>.jsonl`. Lines have
  `isSidechain: true`, `agentId` = file suffix and `sessionId` = the parent session
  id (not their own). The parent's `Agent` tool result references the same `agentId`.
- Resume/fork: the new file copies the earlier conversation line by line, keeping
  `uuid`, `parentUuid`, `message.id`, `requestId` and `timestamp`, but rewriting
  `sessionId` to the new session. The new file has a `derived-from` line
  (`sessions: [{type: "claude_code_session", id: <original>, via: "fork"}]`) and
  the original gets a `continued-in` line (`continuedInSessionId`).

## Codex CLI (`codex/<version>/sessions/YYYY/MM/DD/rollout-<ts>-<thread-id>.jsonl`)

| File | Version | Scenario | Size |
|---|---|---|---|
| `0.135.0/…/rollout-2026-05-31T19-42-32-019e7f21-….jsonl` | 0.135.0 | short: 2 `token_count`, gpt-5.5 | 8 KB |
| `0.139.0/…/rollout-2026-06-11T23-38-02-019eb89e-….jsonl` | 0.139.0 | tool-heavy: 560 lines, `function_call` + `custom_tool_call` (apply_patch), 78 `token_count` incl. one repeated total | 212 KB |
| `0.155.0/…/rollout-2026-09-18T13-35-05-01a0b44c-….jsonl` + `…T15-31-24-01a0b44c-…_01a0b4b6-….jsonl` | 0.155.0 | resumed: base thread + resumed file branching at ordinal 52 | 116 + 140 KB |
| `0.163.0/…/rollout-2026-10-20T10-49-13-01a15880-….jsonl` | 0.163.0 | long + compaction: 961 → 326 lines (trimmed; all 131 `token_count` and 126 `token_usage_record` kept), `compacted` line, 6 repeated totals | 284 KB |
| `0.163.0/…/rollout-2026-10-20T13-05-40-01a158fd-….jsonl` | 0.163.0 | model switch (gpt-6.1-sol → gpt-6-luna in `turn_context`), both `token_count.info` are `null`, no usage | 32 KB |
| `0.165.0/…/rollout-2026-11-03T13-25-20-01a1a15f-….jsonl` + `…T20-23-44-01a1a2de-….jsonl` | 0.165.0 | fork: parent (963 → 384 lines, trimmed, all usage kept) + fork with `forked_from_id` | 284 + 80 KB |
| `0.171.0/…/rollout-2026-11-22T01-31-59-01a200aa-….jsonl` | 0.171.0 | no usage: turn ends in `task_complete.error` (`model_not_found`) | 16 KB |

Format notes:

- Line = `{timestamp, type, payload}`; from 0.155 also `ordinal` (per-thread line
  counter). First line is `session_meta` with `payload.id` (thread id),
  `cli_version`, `cwd`, `originator`, `source`, `model_provider`, `git{commit_hash,
  branch, repository_url}`. Model per turn: `turn_context.payload.model` (also
  `event_msg/thread_settings_applied.thread_settings.model`).
- `event_msg` with `payload.type: "token_count"`: `info.total_token_usage`
  (cumulative for the thread) and `info.last_token_usage` (last response), each
  `{input_tokens, cached_input_tokens, output_tokens, reasoning_output_tokens,
  total_tokens}`; `cache_write_input_tokens` from 0.145. `cached_input_tokens` is a
  subset of `input_tokens`, `reasoning_output_tokens` a subset of `output_tokens`,
  `total = input + output`. `info` can be `null`. The same total is sometimes
  emitted twice in a row (rate-limit refresh); count a delta only when the total
  changes. `rate_limits` rides along.
- From 0.155: `type: "token_usage_record"` lines, one per model response:
  `payload.{thread_id, turn_id, response_id, usage, turn_token_usage,
  thread_token_usage}`. `response_id` (`resp_…`) is unique per response and is a
  natural dedup key; `usage` is the per-response count. In 0.163 there are a few
  more `token_count` than `token_usage_record` (the repeats).
- No totals decrease in any real file, including across `compacted`.
- 0.155+ use `history_mode: "paginated"`. A resumed or forked session starts a new
  file whose `session_meta.history_base = {thread_id, end_ordinal_exclusive,
  end_byte_offset}` points into the base file instead of copying it. The
  anonymizer recomputed `end_byte_offset` for the fixtures.
  - Resume (0.155 pair): new file name `rollout-<ts>-<thread>_<new-id>.jsonl`,
    `session_meta.id` is the **same** thread id. It branches at ordinal 52 of the
    base, and its cumulative totals continue from the base's total at that point
    (`146644 - 21541 = 125103`), not from the base's final total (535496), because
    the base thread went on after ordinal 52. Baseline = last total before
    `end_ordinal_exclusive` in the base file.
  - Fork (0.165 pair): new thread id, `forked_from_id` = parent, `history_base`
    covers the whole parent; ordinals and cumulative totals continue from the
    parent's last values (15584244 → 15788100).
- Recent short sessions (0.170, 0.171) contain no `token_count` at all.

## OpenCode (`opencode/1.19.5/opencode.db`)

SQLite copy of `~/.local/share/opencode/opencode.db`, trimmed to 6 sessions and
VACUUMed (page size 1024), 324 KB. Installed version 1.19.5 (`opencode --version`);
latest migration `20260622202450_simplify_session_input` in table `migration`
(`__drizzle_migrations` stops at `20260511173437_session-metadata`).

| Session (`agent`, `version`) | Scenario |
|---|---|
| `ses_1859d49beffe…` (build, `local`) | 22 messages, parent of a subagent |
| `ses_1859d3a5bffe…` (explore, `local`) | subagent: `parent_id` = the session above |
| `ses_181201c62ffe…` (build, `1.15.13`) | short, ends with `MessageAbortedError` |
| `ses_181a135d8ffe…` (build, `local`) | gpt-5.5, API error (quota), zero tokens |
| `ses_e20abeb75ffe…` (build, `1.19.5`) | ollama `qwen3.5:9b`, has `event` rows |
| `ses_dff56f2c1ffe…` (build, `1.19.5`) | `session_message` `model-switched`, API errors, zero tokens |

Format notes:

- `message.data` (JSON) for `role: "assistant"`: `modelID`, `providerID`, `mode`,
  `agent`, `path{cwd, root}`, `cost`, `tokens{total, input, output, reasoning,
  cache{read, write}}`, `time{created, completed}` (epoch ms), `finish`, optional
  `error{name, data}`. `total = input + output + reasoning + cache.read`.
  User messages carry `model{providerID, modelID}` and `summary.diffs`.
- `part.data` (JSON) types: `text`, `reasoning`, `tool` (`tool`, `callID`,
  `state{status, input, output, …}`), `step-start`, `step-finish` (`tokens`,
  `cost`, `reason`), `patch`. Each assistant message has at most one
  `step-finish`, with the same tokens as the message; use the message, not both.
- `session` has denormalized totals (`cost`, `tokens_input`, `tokens_output`,
  `tokens_reasoning`, `tokens_cache_read`, `tokens_cache_write`; added by migration
  `20260510033149_session_usage`), equal to the sum over its messages. `model` is
  JSON `{id, providerID, variant}`; `version` is the OpenCode version or `local`;
  `parent_id` links subagent sessions; `directory` is the cwd.
- `cost` is 0 everywhere in this data (free and local models), so the fixture
  can't test reported cost.
- Timestamps are epoch ms integers (`time_created`, `time_updated`). Ids are
  `<prefix>_<12 hex time><14 base62>`; session ids sort descending by time,
  message/part ids ascending but the hex prefix wraps (`msg_e7a6…` in May,
  `msg_200a…` in November), so order by `time_created`, not by id.
- `event` (`aggregate_id` = session id, `seq`, `type` like `message.updated.1`,
  `data` JSON snapshot) exists only for 1.19.x sessions; `event_sequence` holds
  the last `seq` per session.
- No legacy per-message JSON on this machine: `storage/` holds only
  `session_diff/*.json` and a `migration` marker.

## Gemini CLI (`gemini_cli/`)

Synthetic, built from upstream source. See `gemini_cli/README.md`.

## GitHub Copilot CLI (`copilot_cli/1.0.4/session-state/<session-id>/events.jsonl`)

**confidence: low.** Synthetic: Copilot CLI is not installed on the harvest machine. Written by
hand from the event types generated from `session-events.schema.json` in `github/copilot-sdk`
(`nodejs/src/generated/session-events.ts`, `main`, read 2026-11-25) and the Copilot CLI
config-dir docs. Text is lorem, ids and token numbers are invented, and the version number is
made up.

| Session | Scenario |
|---|---|
| `3b0f6c2e-…` | model switch (`claude-sonnet-4.5` → `gpt-5`), 2 `session.usage_checkpoint` + `session.shutdown`, tool calls incl. a failed one |
| `7d9e1a2b-…` | shutdown, `session.resume` on another day with a branch change, then a second shutdown whose totals continue from the first; a subagent message (`agentId`), an ephemeral `session.idle` and an unknown event type |

Format notes:

- Location: `~/.copilot/session-state/<session-id>/events.jsonl` (`COPILOT_HOME` replaces
  `~/.copilot`). A session dir also holds workspace artifacts; `session.journal_link` names a
  numbered predecessor log in the same dir.
- Line = `{type, id, parentId, timestamp, data}` plus optional `ephemeral`, `agentId`
  (subagent events). `session.start.data`: `sessionId`, `copilotVersion`, `startTime`,
  `selectedModel`, `context{cwd, gitRoot, branch, repository}`; `session.resume.data` has
  `context` too. `session.model_change.data.newModel`.
- `assistant.usage` (per call: `model`, `inputTokens`, `outputTokens`, `cacheReadTokens`,
  `cacheWriteTokens`, `reasoningTokens`, `apiCallId`) is marked `ephemeral: true` in the
  schema, which means it is not written to `events.jsonl`. What is written: per-model running
  totals in `session.usage_checkpoint.data.accountingSnapshot.modelMetrics` and
  `session.shutdown.data.modelMetrics`, keyed by model id, each `{requests{count, cost},
  usage{inputTokens, outputTokens, cacheReadTokens, cacheWriteTokens, reasoningTokens?}}`.
  The adapter emits one event per model whenever its total changes, with the difference from
  the previous total (the full total if any count went down). The dedup key is
  `<session>:<model>:<running total>`.
- The schema doesn't say whether `inputTokens` includes cache reads/writes. The adapter
  subtracts them from each difference when its `inputTokens` is at least their sum, and
  otherwise takes it as uncached input. `requests.cost` is a premium-request multiplier, not money, so no
  reported cost is set. Claude model ids use dots (`claude-sonnet-4.5`) and are mapped to
  the API names (`claude-sonnet-4-5`).
- Messages: `user.message.data.content`, `assistant.message.data{content, toolRequests[{toolCallId,
  name, arguments}]}`, `assistant.reasoning.data.content`,
  `tool.execution_complete.data{toolCallId, success, result{content}, error{message}}`.

## Grok Build (`grok_build/`)

Synthetic, built from upstream source. See `grok_build/README.md`.

## Antigravity (`antigravity/1.1.17/{antigravity,antigravity-cli}/conversations/<id>.db`)

**confidence: low.** Synthetic: Antigravity is not installed on the harvest machine. The
layout follows the Antigravity parsers in `junhoyeo/tokscale`
(`crates/tokscale-core/src/sessions/antigravity_cli.rs`, `main`, read 2026-11-25) and the
`miiiiiiich/agent-walker` notes (`docs/agy.md`). Ids, token numbers and the version number
are invented. The databases are built from `app.sql` and `cli.sql`
(`sqlite3 <id>.db < app.sql`), which `gen_sql.py` writes (`python3 gen_sql.py <dir>`).

| Conversation | Scenario |
|---|---|
| `5c1e9a7b-…` (app) | 2 Gemini 3.6 Flash calls with their own timestamps (one with cache reads), a Claude call timed from its `steps` row by response id (cache write), and an all-zero failed call |
| `8e2d4c6b-…` (CLI) | 2 calls with only a response model, no label: one timed from its `steps` row by generation index, one from the conversation's created-at |

Format notes:

- Location: `~/.gemini/antigravity/conversations/` (app), `~/.gemini/antigravity-insiders/…`
  (Insiders) and `~/.gemini/antigravity-cli/…` (CLI); one SQLite database per conversation,
  named `<conversation-id>.db`. `.pb` files next to them are not read.
- `gen_metadata(idx, data, size)`: one row per model call, `idx` from 0. `data` is protobuf:
  `#1` chat model message with `#19` response model (an internal name like
  `gemini-3-flash-a`), `#21` picker label (`Gemini 3.6 Flash (High)`) and `#4` usage:
  `#1` model enum, `#2` uncached input, `#3` output (`#9 + #10`), `#4` cache write, `#5`
  cache read, `#9` thinking output, `#10` text output, `#11` response id. `#9.#4` is a
  `{#1 seconds, #2 nanos}` timestamp in older builds.
- `steps(idx, step_type, metadata)`: `step_type` 15 is a model response; `metadata.#1` is
  its timestamp, `#9.#11` the response id and `#20.#3` the generation index.
- `trajectory_metadata_blob(id, data)`: `data.#1.#1` the workspace `file://` URI, `data.#2`
  the created-at timestamp.
- The adapter uses the response id as the dedup key, so a call stored by both the app and
  the CLI counts once. A row with zero usage that is less than a day old is held back in
  case it is still being written.

## LM Studio (`lmstudio/0.4.20/conversations/`)

Two real chats from the chat window, anonymized (the files were written by an earlier 0.3.x
build; 0.4.20 is the version installed at harvest, 2026-11-25), and one synthetic chat in a
folder, hand-written in the same shape.

| File | Scenario |
|---|---|
| `1766924503826.conversation.json`, `1766925535102.conversation.json` | real: one question, one `openai/gpt-oss-20b` answer with a thinking block |
| `lorem-ipsum/1766930000000.conversation.json` | synthetic: a regenerated answer (2 versions, different models, the second selected), a second question, and an answer still being generated (no `stats`) |

Format notes:

- Location: `~/.lmstudio/conversations/**/<created-ms>.conversation.json`; the home moves
  where `~/.lmstudio-home-pointer` says. One JSON document per chat, rewritten on every change.
- Top level: `name`, `createdAt`, `userLastMessagedAt`, `assistantLastMessagedAt` (epoch ms),
  `lastUsedModel.identifier`, `messages[]`. Each message has `versions[]` (regenerations) and
  `currentlySelected`.
- User versions: `type: "singleStep"`, `content[{type: "text", text}]`, no timestamp.
  Assistant versions: `type: "multiStep"`, `steps[]`, `senderInfo.senderName` (the model).
- Steps: `contentBlock` (`content[{text, tokensCount, isStructural}]`, `style.type: "thinking"`
  for reasoning), `debugInfoBlock`, …. `stepIdentifier` is `<epoch ms>-<random>`. The step
  that finished a prediction carries `genInfo{identifier, indexedModelIdentifier, stats}`,
  with `stats{promptTokensCount, predictedTokensCount, totalTokensCount, stopReason, …}`.
  `predictedTokensCount` includes the thinking tokens.
- The adapter counts every version, not only the selected one, since each was generated;
  messages are kept for the selected version only. The anonymizer keeps `identifier`,
  `indexedModelIdentifier`, `senderName`, `stepIdentifier` and `stopReason`.

## Database snapshots (`db/`)

- `db/wave2/tbt.db`: a TokenBurnTrack database written by the Wave 2 code (`wave2-done` tag,
  schema version 1) from these fixtures and `generic_jsonl/config.toml`: 640 events,
  26 sources, 2 ingest runs. Source paths were rewritten to `/sandbox/...`. Used by
  `crates/tbt-store/tests/migration.rs` and the collector's end-to-end test to check that
  older databases open and continue with the current code.

## pi (`pi/`)

Synthetic, built from upstream docs and source. See `pi/README.md`.
