# Grok Build fixtures

**confidence: low.** Grok Build is not installed on the harvest machine. These files
were written by a one-off script that follows the upstream source; no real Grok Build
wrote them. Titles are lorem, ids are made up, token numbers and costs are plausible
but invented. Model ids used: `grok-4.5`, `grok-3-fast`, `grok-4`.

Source: xai-org/grok-build `main` at commit `2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8`
(2026-09-29, `xai-grok-shell` version `1.0.45`):
`crates/codegen/xai-grok-shell/src/session/usage_file.rs`,
`crates/codegen/xai-chat-state/src/usage.rs`,
`crates/codegen/xai-grok-sampling-types/src/conversation.rs` (`TokenUsage`),
`crates/codegen/xai-grok-shell/src/session/persistence.rs` (`Summary`, `persist_usage_turn`),
`crates/codegen/xai-grok-shell/src/session/storage/jsonl/copy.rs` (fork copy).

## Files

| Session dir under `1.0.45/sessions/` | Scenario |
|---|---|
| `%2Fhome%2Fuser%2Fprojects%2Florem/019a6b21-…01/` | main session: 3 turns, turn 2 uses two models, costs |
| `%2Fhome%2Fuser%2Fprojects%2Florem/019a6b4e-…02/` | fork of the main session (`session_kind: "fork"`): turns 1–2 copied verbatim, a new turn 3 without cost |
| `%2Fhome%2Fuser%2Fprojects%2Florem/019a6b25-…03/` | subagent session (`session_kind: "subagent"`), skipped |
| `consectetur-adipiscing-5f0c9a7e/019a6c10-…04/` | hashed cwd group with a `.cwd` file and no `summary.json`; turn 1 has no `modelUsage`, turn 2 has no `endedAt` |

## Format (from source)

- Location: `$GROK_HOME/sessions` (default `~/.grok/sessions`), then
  `<urlencoding::encode(cwd)>/<session id>/`. A cwd group name longer than 255 bytes
  becomes a slug plus hash, and the real path is written to `<group>/.cwd`.
- `usage.json` (`SessionUsageFile`, camelCase): `sessionId`, `updatedAt`, `session`
  (totals), `turns[]`. Each turn: `turnNumber`, `endedAt` (RFC 3339, nanoseconds),
  the flattened `UsageSummary` (`inputTokens`, `outputTokens`, `cachedReadTokens`,
  `cacheCreationTokens`, `reasoningTokens`, `totalTokens`, `modelCalls`,
  `costUsdTicks` (1e10 per USD), `costIsPartial`, `usageIsIncomplete`, `turnCount`,
  `primaryModelId`) and `modelUsage` (the same summary per model id). Rewritten whole
  at the end of every turn.
- `inputTokens` is the full prompt (uncached + cache reads + cache writes);
  `reasoningTokens` is part of `outputTokens`.
- Turns are per-turn deltas, not running totals. Only the last turn can grow later
  (a late interjection folds into it and moves its `endedAt`).
- A fork copies the parent's `usage.json` verbatim (turn rows keep their `endedAt`)
  and restamps `sessionId`. The adapter's dedup key is `endedAt:turnNumber:model`, so
  copied turns collide with the parent's and are stored once.
- A subagent's usage is folded into the parent session's ledger, so subagent sessions
  (`summary.json` `session_kind` starting with `subagent`) are skipped.
- `summary.json` (snake_case): `info {id, cwd}`, `generated_title`, `created_at`,
  `updated_at`, `parent_session_id`, `session_kind`, `head_branch`, …
