# TokenBurnTrack

A local desktop app that tracks the token usage and cost of AI coding agents. It copies
usage records from the agents' own log files into a SQLite database on your machine,
prices every API call, and shows dashboards for cost, tokens, sessions, models and
projects. Your history survives after the agents delete or rotate their logs.

![Dashboard](docs/images/dashboard.png)
![Sessions](docs/images/sessions.png)

## Quick start

Build from source (Rust toolchain is installed by rustup; Linux needs a Wayland session):

```sh
scripts/cargo build --release
install -Dm755 target/release/tbt-app ~/.local/bin/tbt-app
install -Dm755 target/release/tbt-daemon ~/.local/bin/tbt-daemon
~/.local/bin/tbt-daemon install --app ~/.local/bin/tbt-app   # hourly collection while the app is closed
```

Then start `tbt-app`. It syncs on startup, every hour and on *Sync now*.

Flatpak, AppImage, macOS and Windows packages, system dependencies and the daemon
options are in [docs/install.md](docs/install.md). Settings are in
[docs/configuration.md](docs/configuration.md), and tests and release builds in
[docs/development.md](docs/development.md).

## Features

- **Incremental, idempotent collection.** Each run reads only what is new, a re-run
  never counts anything twice, and a machine that slept catches up on the next run.
- **Session linking.** Resumed and forked sessions are linked instead of double counted;
  subagent runs are linked to the session that started them.
- **Worktree projects.** Sessions run in a git worktree count toward the main checkout's
  project, also after the worktree is deleted if it was seen before, is a Claude Code
  `.claude/worktrees` worktree, or git still lists it.
- **Per-call cost.** Bundled prices, an optional LiteLLM import and your own overrides,
  versioned by date so a price change does not rewrite old costs. When a harness logs
  its own cost, both values are kept.
- **Dashboards.** KPIs, hourly/daily cost, tokens by type, top models and projects, a
  weekday × hour heatmap, a sessions table with session detail, and CSV/JSON export.
- **Background collector.** `tbt-daemon` runs from a systemd user timer, a launchd agent
  or a Windows scheduled task, and shares a lock with the app so only one ingests at a
  time.

## 🔌 Supported harnesses

| Harness | State | Info |
|---|---|---|
| Claude Code | ✅ | Passive reader `~/.claude/projects/**/*.jsonl` |
| Codex CLI | ✅ | Passive reader `~/.codex/sessions/**/*.jsonl` |
| OpenCode | ✅ | Passive reader `~/.local/share/opencode/opencode.db` |
| LM Studio | ✅ | Passive reader `~/.lmstudio/conversations/**/*.conversation.json` |
| Generic JSONL | ✅ | Passive reader for any JSONL log described in `config.toml` |
| Gemini CLI | 🧪 | Passive reader `~/.gemini/tmp/<project>/chats/*.jsonl` |
| Grok Build | 🧪 | Passive reader `~/.grok/sessions/**/usage.json` |
| pi | 🧪 | Passive reader `~/.pi/agent/sessions/**/*.jsonl` |
| Cursor | 🧪 | Passive reader `~/.config/Cursor/User/globalStorage/state.vscdb` |
| GitHub Copilot CLI | 🧪 | Passive reader `~/.copilot/session-state/*/events.jsonl` |
| Antigravity | 🧪 | Passive reader `~/.gemini/antigravity*/conversations/*.db` |
| Zed Agent | 🧪 | Passive reader `~/.local/share/zed/threads/threads.db` |
| Goose | 🧪 | Passive reader `~/.local/share/goose/sessions/sessions.db` |

✅ tested on real logs · 🧪 built from upstream source or schema, no real logs yet.
Paths per platform, overrides and per-harness caveats are in
[docs/harnesses.md](docs/harnesses.md).

## Privacy

Everything stays on your machine. The data directory
(`~/.local/share/tokenburntrack` on Linux) holds:

- `tbt.sqlite`: sessions, one row per API call (time, harness, provider, model, token
  counts by type, computed and reported cost, project path, git branch, and the raw
  usage JSON unless `raw_json_retention_days` drops it), hourly rollups, prices and a
  log of ingest runs;
- `collector.lock`: the lock shared by the app and the daemon;
- `daemon.lock`: held by a long-running `tbt-daemon`, so only one runs per data
  directory;
- `logs/`: daily-rotated daemon logs (30 days kept).

Message content depends on the `privacy` setting:

| Level | Stored per message |
|---|---|
| `off` | nothing |
| `metadata_only` (default) | role, kind, tool name, timestamps; no text |
| `truncated_preview` | the above plus the first 280 characters of the text |

Nothing is sent anywhere. The only network access is the optional LiteLLM price import,
which downloads the public price list when you click it in Settings. The systemd unit
runs the daemon without network access.

## License

Copyright (C) 2026 vaguemotion

TokenBurnTrack is free software: you can redistribute it and/or modify it under the
terms of the GNU General Public License as published by the Free Software Foundation,
either version 3 of the License, or (at your option) any later version. See
[`LICENSE`](LICENSE).
