# Configuration

Settings live in `~/.config/tokenburntrack/config.toml` (the platform config directory
on macOS and Windows). Every field is optional.

```toml
privacy = "metadata_only"        # off | metadata_only | truncated_preview
sync_interval_minutes = 60       # app scheduler and long-running daemon
# data_dir = "/path/to/data"     # default: $XDG_DATA_HOME/tokenburntrack
# raw_json_retention_days = 30   # drop the stored raw usage JSON of events older than N days

[[generic_jsonl]]
id = "llm_proxy"
display_name = "LLM proxy"
roots = ["~/.local/share/llm-proxy/logs"]
glob = "**/*.jsonl"
filter = "$.usage"
timestamp = "$.created_ms"
timestamp_format = "unix_millis" # rfc3339 (default) | unix_seconds | unix_millis
session_id = "$.session"
provider = "$.provider"
model = "$.model"
input_tokens = "$.usage.prompt_tokens"
input_includes_cache_read = true
cache_read_tokens = "$.usage.prompt_tokens_details.cached_tokens"
output_tokens = "$.usage.completion_tokens"
reported_cost = "$.cost_usd"
project_path = "$.metadata.cwd"
dedup_key = "{$.id}"
```

In `[[generic_jsonl]]` tables, values starting with `$` are paths into each JSON line;
`provider`, `model`, `session_id`, `project_path` and `git_branch` may also be
constants. `dedup_key` must be built from IDs that are stable per API call. More
examples are in
[`tests/fixtures/generic_jsonl/config.toml`](../tests/fixtures/generic_jsonl/config.toml).

The data directory is chosen in this order: `--data-dir`, `TBT_DATA_DIR`, `data_dir` in
the config, then the platform default (`~/.local/share/tokenburntrack` on Linux).

## Harness locations

Each harness is read from the default location listed in [harnesses.md](harnesses.md).
To read it from somewhere else, change its roots on the Harnesses page, or set the same
environment variable the harness itself uses:

| Harness | Variable or file |
|---|---|
| Claude Code | `CLAUDE_CONFIG_DIR` |
| Codex CLI | `CODEX_HOME` |
| Gemini CLI | `GEMINI_CLI_HOME` |
| Grok Build | `GROK_HOME` |
| pi | `PI_CODING_AGENT_DIR`, `PI_CODING_AGENT_SESSION_DIR` |
| OpenCode | `XDG_DATA_HOME` |
| Cursor | `XDG_CONFIG_HOME` (Linux) |
| GitHub Copilot CLI | `COPILOT_HOME` |
| Antigravity | `GEMINI_CLI_HOME` |
| LM Studio | `~/.lmstudio-home-pointer` (a file holding LM Studio's home folder) |
| Zed Agent | `XDG_DATA_HOME` (Linux) |
| Goose | `GOOSE_PATH_ROOT`, `XDG_DATA_HOME` (not Windows) |
| DeepSeek Harness | `DSH_HOME` |
| Generic JSONL | `roots` in its `[[generic_jsonl]]` table |

The scheduled `tbt-daemon` does not see variables exported by your shell; see
[install.md](install.md) for passing them to it.

## Sync and prices

While the app is open it syncs at startup, every `sync_interval_minutes` on the hour
grid, and on *Sync now*. Set `TBT_WATCH=1` to also sync shortly after log files change.

Prices come from the bundled table (`assets/prices.toml`), an optional LiteLLM import
and your own overrides. Calls to models without a price count as $0; those models are
listed under *Settings → Prices* so you can add a price.
