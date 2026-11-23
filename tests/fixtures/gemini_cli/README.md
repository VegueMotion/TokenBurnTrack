# Gemini CLI fixtures

**confidence: low.** Gemini CLI is not installed on the harvest machine. These
files were produced by a one-off script that follows the upstream source; no real
Gemini CLI wrote them. All text is lorem, ids are made up, token numbers
are plausible but invented. Model ids used: `gemini-2.5-pro`, `gemini-2.5-flash`.
Revisit when real logs are available.

Sources (google-gemini/gemini-cli):

- Current format: `main` at commit `eecccfcd5aff25de5e78135ca20f7abe3f566955`
  (2026-11-20, package version `0.71.0-nightly.20261118.g615d6bc8c`):
  `packages/core/src/services/chatRecordingTypes.ts`,
  `packages/core/src/services/chatRecordingService.ts`,
  `packages/core/src/config/storage.ts`, `packages/core/src/config/projectRegistry.ts`.
- Legacy format: tag `v0.20.0`, `packages/core/src/services/chatRecordingService.ts`.

## Files

| File | Format | Scenario |
|---|---|---|
| `0.20.0/tmp/<sha256(project root)>/chats/session-2026-10-02T09-30-7a1b2c3d.json` | legacy JSON | 2 user + 3 gemini messages, 2 models, one tool call |
| `0.71.0-nightly/tmp/amber-acorn-00/chats/session-2026-11-20T10-15-5a1e2d3c.jsonl` | JSONL | main session: messages re-appended as they update, `$set`, `$rewindTo`, `$patch`, 2 models, a subagent tool call |
| `0.71.0-nightly/tmp/amber-acorn-00/chats/5a1e2d3c-…/f4c2b3a1-….jsonl` | JSONL | subagent session (`kind: "subagent"`), in a dir named after the parent session id |
| `0.71.0-nightly/tmp/bold-birch-00/chats/session-2026-10-02T09-30-7a1b2c3d.json` + `.jsonl` | legacy + JSONL | resume of a legacy session by the new version: the `.json` stays, the `.jsonl` copies all its messages (same ids) and appends new ones |

`.project_root` files hold the project path for the slug directory.

## Format (from source)

- Location: `~/.gemini/tmp/<project-id>/chats/`. Older versions use
  `sha256(project root)` hex as `<project-id>`; current versions use a slug of the
  project dir basename from a project registry (with a `.project_root` marker) and
  migrate old hash dirs.
- Legacy (`session-<YYYY-MM-DDTHH-MM>-<first 8 of sessionId>.json`): one
  pretty-printed `ConversationRecord {sessionId, projectHash, startTime,
  lastUpdated, messages[]}`, rewritten whole on every change.
- Current (`….jsonl`, append-only). Record kinds per line:
  - metadata: has `sessionId` + `projectHash` (`startTime`, `lastUpdated`, `kind`
    `main`|`subagent`, `directories`, `summary`);
  - message: has `id` (UUID), `timestamp`, `type` (`user`|`gemini`|`info`|`error`|
    `warning`), `content`; `gemini` messages add `model`, `tokens`, `thoughts`,
    `toolCalls[{id, name, args, result, status, timestamp, agentId?, …}]`;
  - `{"$set": {…}}` metadata updates (mostly `lastUpdated`);
  - `{"$rewindTo": <message id>}` drops that message and everything after it;
  - `{"$patch": {id, content?, toolCalls?, updates?, removeIds?, orderIds?}}`.
- The same message `id` is appended again whenever it changes (tokens arrive after
  the message, tool calls complete). The last record per `id` wins. Count tokens
  once per message id, from its last record that has `tokens`.
- `tokens = {input, output, cached, thoughts, tool, total}` mapped from the API
  `usageMetadata` (`promptTokenCount`, `candidatesTokenCount`,
  `cachedContentTokenCount`, `thoughtsTokenCount`, `toolUsePromptTokenCount`,
  `totalTokenCount`). `cached` is part of `input`. No cost is recorded.
- `$rewindTo` removes messages from the conversation, but their API calls were
  made; whether to keep their usage is an adapter decision (the main fixture
  rewinds past one gemini message that has tokens).
- Subagents: `chats/<parent sessionId>/<sessionId>.jsonl` with `kind: "subagent"`.
- Resuming a legacy `.json` writes `<same name>.jsonl` with a metadata line, every
  old message (same ids) and `{"$set": {"sessionId": …}}`, then new records.
  Dedup by message id across both files.
