# TokenBurnTrack

TokenBurnTrack is a local desktop app (Rust + GPUI) that tracks
the token usage and cost of AI coding agents. Every hour it copies new usage records from
the agents' own log files into a SQLite database on your machine, prices each API call
from a versioned price table, and shows dashboards for cost, tokens, sessions, models and
projects. Because the agents delete or rotate their logs, the hourly snapshot keeps your
history after the source files are gone. A small headless collector, `tbt-daemon`, keeps
the snapshots going while the app is closed.

## Features

- Hourly, incremental, idempotent collection: each run reads only what is new since the
  last one, a re-run never counts anything twice, and a laptop that slept catches up on
  the next run.
- Resumed and forked sessions are linked instead of double counted; subagent runs are
  linked to the session that started them.
- Sessions run in a git worktree count toward the main checkout's project, also after the
  worktree is deleted if it was seen before, is a Claude Code `.claude/worktrees` worktree,
  or git still lists it.
- Cost per API call, from bundled prices (`assets/prices.toml`), an optional LiteLLM
  price import and your own overrides. Prices are versioned by date, so a price change
  does not rewrite old costs. Calls to models without a price count as $0;
  those models are listed under Settings → Prices so you can add a price. When a harness logs its own cost, both values are kept.
- Desktop app: dashboard (KPIs, hourly/daily cost bars, tokens by type, top models and
  projects, weekday × hour heatmap), sessions table and session detail, models,
  projects, harnesses and settings pages, CSV/JSON export. While it is open, the app
  syncs at startup, every `sync_interval_minutes` on the hour grid, and on *Sync now*;
  set `TBT_WATCH=1` to also sync shortly after log files change.
- Headless collector `tbt-daemon` for a systemd user timer, a launchd agent or a Windows
  scheduled task. The app and the daemon share a lock, so only one of them ingests at a
  time.

## Supported harnesses

| Harness | Reads | Override |
|---|---|---|
| Claude Code | `~/.claude/projects/**/*.jsonl` (sessions and subagent runs) | `CLAUDE_CONFIG_DIR` |
| Codex CLI | `~/.codex/sessions/YYYY/MM/DD/*.jsonl` | `CODEX_HOME` |
| Gemini CLI | `~/.gemini/tmp/<project>/chats/*.jsonl` and legacy `session-*.json` (macOS also `~/.cache/.gemini/tmp`) | `GEMINI_CLI_HOME` |
| OpenCode | `~/.local/share/opencode/opencode.db` (opened read-only) | `XDG_DATA_HOME` |
| Generic JSONL | any JSONL log you describe in `config.toml` | `[[generic_jsonl]]` |

Roots can also be changed per harness on the Harnesses page. Source files are only ever
read. The Gemini CLI parser was built from upstream source rather than real logs, so
treat its numbers with more caution.

## Build

The toolchain is pinned in `rust-toolchain.toml` (installed by rustup on first use).
`scripts/cargo` is a thin wrapper that puts `~/.cargo/bin` first in `PATH`.

System packages on Fedora (Wayland):

```sh
sudo dnf install gcc gcc-c++ clang cmake pkgconf \
    libxkbcommon-devel wayland-devel libxcb-devel libX11-devel \
    fontconfig-devel freetype-devel \
    vulkan-loader mesa-vulkan-drivers
```

The last two are runtime dependencies (GPUI renders with Vulkan). Only the Wayland
backend is enabled; an X11 build would also need `libxkbcommon-x11-devel` and the `x11`
feature of `gpui-pre-platform`. Other distributions need the equivalent packages.

```sh
scripts/cargo build --release
# target/release/tbt-app     desktop app
# target/release/tbt-daemon  headless collector
```

### Release packages

Each script writes to `dist/`. GPUI compiles its shaders with platform tools (`fxc.exe`
on Windows, the Metal compiler on macOS), so Windows and macOS packages must be built on
those systems.

| Package | Command | Needs |
|---|---|---|
| AppImage | `scripts/build-appimage.sh` | podman; builds in `ubuntu:22.04` (glibc 2.35) so the result runs on older distributions. `--host` builds with the local toolchain instead. Runs on Wayland only; X11 sessions are not supported |
| Windows zip | `powershell scripts\build-windows.ps1` | rustup, Visual Studio Build Tools (C++ workload) and the Windows SDK |
| macOS dmg | `scripts/build-macos.sh` | rustup and Xcode; produces a universal `TokenBurnTrack.app`, ad-hoc signed unless `CODESIGN_IDENTITY` is set. Set `NOTARY_PROFILE` to a `xcrun notarytool store-credentials` profile to also notarize and staple the dmg; without it, Gatekeeper blocks the dmg on other Macs |

The AppImage contains only the app. The script also writes a standalone `tbt-daemon`
binary next to it; install that one for collection while the app is closed, because
the AppImage's mount path changes on every run.

Pushing a `v*` tag runs `.forgejo/workflows/release.yml`, which builds the Flatpak
bundle and the AppImage on a Linux runner with the `docker` label and attaches them to
the tag's release (a prerelease if the tag contains `-`). `flatpak-builder` needs
privileged containers, so set `container.privileged: true` in that runner's config.
Windows and macOS packages are not built there; build them with the scripts above and
attach them by hand.

## Install

Copy the binaries somewhere stable first, because the schedule points at an absolute
path:

```sh
install -Dm755 target/release/tbt-daemon ~/.local/bin/tbt-daemon
install -Dm755 target/release/tbt-app ~/.local/bin/tbt-app
```

The app collects on its own while it is open (at startup, every hour and on *Sync now*).
Install the daemon schedule if you also want collection while the app is closed.

### Linux (systemd user timer)

```sh
~/.local/bin/tbt-daemon install --dry-run --app ~/.local/bin/tbt-app   # show what it would do
~/.local/bin/tbt-daemon install --app ~/.local/bin/tbt-app
```

`install` writes `tbt-daemon.service` and `tbt-daemon.timer` to
`$XDG_CONFIG_HOME/systemd/user/` with the binary path and data directory filled in,
creates the data directory, then runs `systemctl --user daemon-reload` and
`systemctl --user enable --now tbt-daemon.timer`. With `--app` it also installs
`io.VagueMotion.TokenBurnTrack.desktop` to `$XDG_DATA_HOME/applications/` and the icon to
`$XDG_DATA_HOME/icons/hicolor/scalable/apps/`, so launchers show the app with its name
and icon. Files from older versions (`tbt-app.desktop`, `tbt-app.svg`) are removed.

| Option | Effect |
|---|---|
| `--dry-run` | print every file (with contents) and command; change nothing |
| `--bin <path>` | daemon binary to schedule (default: the running `tbt-daemon`) |
| `--app <path>` | also install the desktop entry and icon for this `tbt-app` |
| `--no-enable` | write the files and reload systemd, but do not enable the timer |
| `--data-dir <dir>`, `--config <file>` | passed through to the scheduled daemon |

The timer fires every hour (`OnCalendar=hourly`) with a fixed per-machine delay of up
to 2 minutes, and `Persistent=true` runs a missed hour after suspend. The service is a
`Type=oneshot` run of `tbt-daemon --once` at `Nice=10` with idle I/O priority. It is
sandboxed: the file system is read-only except the data directory
(`ProtectSystem=strict` + `ReadWritePaths=`), and it has no network, a private `/tmp`
and `NoNewPrivileges`. If you later change `data_dir` in the config, run `install`
again, or the service cannot write to the new directory.

User services do not see variables exported by your shell. If you set
`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `GEMINI_CLI_HOME` or `TBT_DATA_DIR`, add them with
`systemctl --user edit tbt-daemon.service` (`[Service]` + `Environment=NAME=value`).

Check and remove:

```sh
systemctl --user list-timers tbt-daemon.timer
journalctl --user -u tbt-daemon.service
systemctl --user start tbt-daemon.service   # run once now
tbt-daemon uninstall                        # disable, stop, remove the files
```

Manual install: the files in [`packaging/systemd/`](packaging/systemd) work as they
are when the binary is `~/.local/bin/tbt-daemon` and the data directory is
`~/.local/share/tokenburntrack`:

```sh
mkdir -p ~/.config/systemd/user ~/.local/share/tokenburntrack
cp packaging/systemd/tbt-daemon.{service,timer} ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now tbt-daemon.timer
```

For the launcher entry, copy `packaging/linux/io.VagueMotion.TokenBurnTrack.desktop` to
`~/.local/share/applications/` and `packaging/icons/io.VagueMotion.TokenBurnTrack.svg` to
`~/.local/share/icons/hicolor/scalable/apps/` (`Exec=tbt-app` expects `tbt-app` in
`PATH`).

### Linux (Flatpak)

The manifest is `packaging/flatpak/io.VagueMotion.TokenBurnTrack.yml`. It builds offline with the pinned
Rust 1.100.0 toolchain (downloaded as a checksummed source) and the crates listed in
`packaging/flatpak/cargo-sources.json`. Regenerate that file whenever `Cargo.lock`
changes:

```sh
scripts/flatpak-cargo-sources.py
flatpak-builder --user --install --force-clean build-dir packaging/flatpak/io.VagueMotion.TokenBurnTrack.yml
flatpak run io.VagueMotion.TokenBurnTrack
```

The sandbox can read `~/.claude`, `~/.codex` and `~/.gemini`, and can read and write
`~/.local/share/opencode` (SQLite needs to create the `-shm` file of a database in
WAL mode). Config and data stay in the usual host locations
(`~/.config/tokenburntrack`, `~/.local/share/tokenburntrack`), so the Flatpak and a
`tbt-daemon` installed on the host share one database and one collector lock.

Roots outside those directories (custom roots, `[[generic_jsonl]]` paths, a
`CLAUDE_CONFIG_DIR` elsewhere) are not visible until you grant them; the Harnesses
page shows the command:

```sh
flatpak override --user --filesystem=/path/to/logs:ro io.VagueMotion.TokenBurnTrack
```

`tbt-daemon install` does not work inside the sandbox. To collect while the app is
closed, use *Settings → General → Collect while closed*. It asks the desktop's
background portal to start `tbt-daemon` at login, which then syncs every
`sync_interval_minutes`. Only the Wayland backend is built, so the app needs a Wayland
session.

### macOS (launchd)

```sh
tbt-daemon install    # or --dry-run first
```

This writes `~/Library/LaunchAgents/com.tokenburntrack.daemon.plist` and loads it with
`launchctl bootstrap gui/$UID`. The agent runs `tbt-daemon --once` at load and then
every hour (`StartInterval` 3600), at low priority. Its stdout and stderr go to
`launchd-stdout.log` and `launchd-stderr.log` in the logs directory. For a manual
install, replace `@BIN@` and `@LOG_DIR@` in
[`packaging/launchd/com.tokenburntrack.daemon.plist`](packaging/launchd) with absolute
paths. `tbt-daemon uninstall` runs `launchctl bootout` and removes the plist.

### Windows (Task Scheduler)

```powershell
tbt-daemon.exe install    # or --dry-run first
```

This writes the task definition to `%APPDATA%\tokenburntrack\tbt-daemon-task.xml` and
registers it with `schtasks /Create /TN TokenBurnTrack\tbt-daemon /XML <file> /F`. The
task runs `tbt-daemon.exe --once` every hour, only while you are logged on, without
elevation, and catches up on a missed run. `tbt-daemon uninstall` deletes it.

Manual registration with PowerShell from
[`packaging/windows/tbt-daemon-task.xml`](packaging/windows/tbt-daemon-task.xml) (its
`<Command>` expects `%LOCALAPPDATA%\Programs\TokenBurnTrack\tbt-daemon.exe`; edit it if
the binary is elsewhere):

```powershell
Register-ScheduledTask -TaskName 'tbt-daemon' -TaskPath '\TokenBurnTrack\' `
    -Xml (Get-Content -Raw .\packaging\windows\tbt-daemon-task.xml)
Unregister-ScheduledTask -TaskName 'tbt-daemon' -TaskPath '\TokenBurnTrack\' -Confirm:$false
```

### Running the daemon by hand

```sh
tbt-daemon --once                    # one ingest run, then exit
tbt-daemon                           # stay running, ingest at startup and every sync_interval_minutes
tbt-daemon --once --trigger manual   # record the run as manual
```

A run that finds the collector lock taken (the app is ingesting) prints a message and
exits 0. Both the app and a long-running `tbt-daemon` re-read `config.toml` before each
run, so saved settings apply on the next sync (a `data_dir` change needs a restart).

## Configuration

Settings live in `~/.config/tokenburntrack/config.toml` (the platform config dir on
macOS and Windows). Every field is optional.

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
`provider`, `model`, `session_id`, `project_path` and `git_branch` may also be constants.
`dedup_key` must be built from IDs that are stable per API call. More examples are in
[`tests/fixtures/generic_jsonl/config.toml`](tests/fixtures/generic_jsonl/config.toml).

The data directory is chosen in this order: `--data-dir`, `TBT_DATA_DIR`, `data_dir` in
the config, then the platform default (`~/.local/share/tokenburntrack` on Linux).

## Data and privacy

Everything stays on your machine. The data directory holds:

- `tbt.sqlite`: sessions, one row per API call (time, harness, provider, model, token
  counts by type, computed and reported cost, project path, git branch, and the raw
  usage JSON unless `raw_json_retention_days` drops it), hourly rollups, prices and a
  log of ingest runs;
- `collector.lock`: the lock shared by the app and the daemon;
- `logs/`: daily-rotated daemon logs (30 days kept).

Message content depends on `privacy`:

| Level | Stored per message |
|---|---|
| `off` | nothing |
| `metadata_only` (default) | role, kind, tool name, timestamps; no text |
| `truncated_preview` | the above plus the first 280 characters of the text |

Nothing is sent anywhere. The only network access is the optional LiteLLM price import,
which downloads the public price list when you click it in Settings. The systemd unit
runs the daemon without network access.

## Development

```sh
scripts/check.sh                  # fmt --check, clippy -D warnings, tests for the workspace
scripts/check.sh tbt-daemon       # the same for one or more crates
scripts/cargo test -p tbt-adapters
```

- Adapter fixtures are real, anonymized harness logs in
  `tests/fixtures/<harness>/<version>/`, with golden outputs; see
  [`tests/fixtures/README.md`](tests/fixtures/README.md). A format change in a harness
  means adding a fixture for the new version and fixing the parser.
- `scripts/anonymize.py` turns real logs (JSONL, JSON, SQLite) into fixtures: IDs, paths,
  branches and all free text are replaced consistently, while structure, token counts,
  timestamps and model ids are kept. Grep its output for your home path and user name
  before committing.
- Performance harnesses (release builds; `<D>` is a scratch directory):

  ```sh
  # build a synthetic 1M-event DB through the real write path and time the read queries
  scripts/cargo run --release -p tbt-store --example perf -- --events 1000000 --dir <D>
  #   --regen (rebuild), --runs N (warm runs per query), --seed N, --gen-only, --no-open
  # time collector runs on a copy of it: first run, nothing changed, append, raw JSON retention
  scripts/cargo run --release -p tbt-collector --example perf_ingest -- --dir <D>
  #   --files N (synthetic Claude Code files, default 300), --append N (default 20)
  # time the sessions page's background load, sort and search
  scripts/cargo run --release -p tbt-app --example perf_sessions -- <D>/tbt.sqlite
  ```

- Packaging templates live in [`packaging/`](packaging) and are embedded into
  `tbt-daemon` at build time, so `tbt-daemon install` always writes what is in the repo.

## License

Copyright (C) 2026 vaguemotion

TokenBurnTrack is free software: you can redistribute it and/or modify it under the terms
of the GNU General Public License as published by the Free Software Foundation, either
version 3 of the License, or (at your option) any later version. See [`LICENSE`](LICENSE).
