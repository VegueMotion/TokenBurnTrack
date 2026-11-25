# pi fixtures

**confidence: low.** pi is not installed on the harvest machine. These files were written
by a one-off script that follows the upstream docs and source; no real pi wrote them.
Titles are lorem, ids are made up, token numbers and costs are plausible but invented.

Source: badlogic/pi-mono `main` (2026-11-25):
`packages/coding-agent/docs/session-format.md`, `packages/coding-agent/docs/sessions.md`,
`packages/coding-agent/src/core/session-manager.ts`, `packages/coding-agent/src/config.ts`,
`packages/ai/src/api/anthropic-messages.ts`.

## Files

| File under `v3/sessions/` | Scenario |
|---|---|
| `--home-user-projects-lorem--/2026-11-20T09-00-00-000Z_3f1c…01.jsonl` | main session: two assistant turns (cache writes, 1h cache, reasoning), a `session_info` name, an aborted turn with zero usage, a truncated line, a compaction with usage |
| `--home-user-projects-lorem--/2026-11-21T14-30-00-000Z_7a8b…02.jsonl` | fork of the main session (`parentSession`): the first entries copied with their ids and timestamps, then one new `gpt-5.5` turn |
| `--home-user-projects-ipsum--/2026-11-22T18-00-00-000Z_c4d5…03.jsonl` | an OpenRouter model with no known family, and a `usage` entry (`kind: "cache_warm"`) for another model |

## Format (from source)

- Location: `$PI_CODING_AGENT_DIR/sessions` (default `~/.pi/agent/sessions`), or
  `PI_CODING_AGENT_SESSION_DIR` per the docs. One directory per cwd:
  `--<cwd without leading separator, with / \ : replaced by ->--`, files
  `<ISO timestamp with : and . replaced by ->_<session id>.jsonl`. Append-only.
- Line 1 is the header: `{"type":"session","version":3,"id","timestamp","cwd","parentSession"?}`.
  `parentSession` is the absolute path of the parent's file.
- Every other entry has `type`, `id` (8 hex), `parentId`, `timestamp` (ISO 8601). Usage
  sits on `message` entries with `message.role == "assistant"` (`provider`, `model`,
  `usage`, `timestamp` in Unix ms), on `usage` entries (`provider`, `model`, `usage`), and
  optionally on `compaction` and `branch_summary` entries (no model; the adapter uses the
  session's current one). `session_info.name` is the title, `model_change` has
  `provider` and `modelId`.
- `usage`: `input` (excludes cache), `output` (includes `reasoning`), `cacheRead`,
  `cacheWrite` (includes `cacheWrite1h`), `totalTokens`, `cost.total` (USD from pi's
  per-million price table). Each message holds one call's usage, not a running total.
- `/fork`, `/clone` and `--fork` write a new file whose entries are copied from the parent
  with their `id` and `timestamp` kept. The adapter skips entries of a session with
  `parentSession` dated at or before its header, so the parent keeps them.
