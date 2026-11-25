# Harnesses

Source files are only ever read. Roots can be changed per harness on the Harnesses page.

| Harness | Reads | Override |
|---|---|---|
| Claude Code | `~/.claude/projects/**/*.jsonl` (sessions and subagent runs) | `CLAUDE_CONFIG_DIR` |
| Codex CLI | `~/.codex/sessions/YYYY/MM/DD/*.jsonl` | `CODEX_HOME` |
| Gemini CLI | `~/.gemini/tmp/<project>/chats/*.jsonl` and legacy `session-*.json` (macOS also `~/.cache/.gemini/tmp`) | `GEMINI_CLI_HOME` |
| Grok Build | `~/.grok/sessions/<cwd>/<session>/usage.json` (subagent sessions are skipped; their usage is already in the parent) | `GROK_HOME` |
| pi | `~/.pi/agent/sessions/--<cwd>--/*.jsonl` (turns a fork copied from its parent are skipped) | `PI_CODING_AGENT_DIR`, `PI_CODING_AGENT_SESSION_DIR` |
| OpenCode | `~/.local/share/opencode/opencode.db` (opened read-only) | `XDG_DATA_HOME` |
| Cursor | `~/.config/Cursor/User/globalStorage/state.vscdb` (opened read-only; macOS `~/Library/Application Support/Cursor`, Windows `%APPDATA%\Cursor`) | `XDG_CONFIG_HOME` |
| GitHub Copilot CLI | `~/.copilot/session-state/<session-id>/events.jsonl` | `COPILOT_HOME` |
| Antigravity | `~/.gemini/{antigravity,antigravity-insiders,antigravity-cli}/conversations/*.db` (app, Insiders and CLI; opened read-only) | `GEMINI_CLI_HOME` |
| LM Studio | `~/.lmstudio/conversations/**/*.conversation.json` (the chat window; not requests to its local server) | `~/.lmstudio-home-pointer` |
| Zed Agent | `~/.local/share/zed/threads/threads.db` (Zed's own agent; opened read-only; Zed Flatpak `~/.var/app/dev.zed.Zed/data/zed/threads`, macOS `~/Library/Application Support/Zed/threads`, Windows `%LOCALAPPDATA%\Zed\threads`) | `XDG_DATA_HOME` |
| Goose | `~/.local/share/goose/sessions/sessions.db` (opened read-only; Windows `%APPDATA%\Block\goose\data\sessions`) | `GOOSE_PATH_ROOT`, `XDG_DATA_HOME` |
| DeepSeek Harness | `~/.dsh/sessions/<project>/<session>/session.v4.jsonl.zstd` (the newest log generation per session; plain `.jsonl` logs too) | `DSH_HOME` |
| Generic JSONL | any JSONL log you describe in `config.toml` (see [configuration.md](configuration.md)) | `[[generic_jsonl]]` |

## Notes per harness

**Gemini CLI, Grok Build.** Built from upstream source rather than real logs, so treat
their numbers with more caution.

**Cursor.** Treat its numbers with more caution too: the chat database format is not
documented. It reads the agent and chat
panel's per-message `tokenCount`, which Cursor does not fill in for every model, and it
does not read the `cursor-agent` CLI.

**GitHub Copilot CLI.** Built from the published event schema, not real logs. Copilot
does not write per-call usage to disk, only per-model running totals at checkpoints and at
shutdown, so each usage row covers the calls since the previous total, and only the CLI is
read, not Copilot in VS Code or other editors.

**Antigravity.** The least certain parser: Antigravity keeps token counts in undocumented
protobuf blobs, and the field layout comes from other open-source usage trackers, not from
Google or from real logs. Model names come from the model picker label
(`Gemini 3.6 Flash (High)` → `gemini-3.6-flash`), or from the response model when it
extends that name (`gemini-3.1-pro-preview`), so the thinking level is not priced
separately. Messages are not read.

**LM Studio.** Models run locally, so its usage is counted but has no price unless you add
one for the `lmstudio` provider.

**Zed Agent.** Built from Zed's source, not real threads. Zed keeps only a running token
total per thread, so each usage row covers the calls since the previous read, is timed at
the thread's last update and is charged to the model the thread had selected then. Threads
run through external agents (Claude Code, Codex, Gemini CLI) are not in Zed's database;
they are counted by those harnesses. Models served by LM Studio, Ollama or llama.cpp keep
that provider, so they have no price.

**Goose.** Built from Goose's source, not real sessions. It reads Goose's per-call usage
ledger, keeps Goose's own cost only when the provider reported it, and skips sessions run
through the Claude Code, Codex, Gemini CLI, Copilot or pi providers, which those harnesses
already count. Sessions from before the ledger (Goose before schema version 15) only have
a running total, so each of their usage rows covers the calls since the previous read and
is timed at the session's last update. Models served by Goose's local providers (Ollama,
LM Studio, llama-swap, built-in local inference and the like) keep that provider, so they
have no price.

**DeepSeek Harness.** Built from DeepSeek Harness's source, not real sessions. It reads the
usage on each model reply, on failed attempts that were retried, and on compaction
summaries; session titles written by a model are not counted, since their usage is not
saved. A fork skips the rows it copied from its parent, and subagents are their own
sessions linked to their parent. The `deepseek-official`, `deepseek-account` and
`deepseek-messages` providers are priced as `deepseek`; other providers keep their name.
