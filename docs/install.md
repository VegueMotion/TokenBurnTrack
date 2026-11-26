# Building, installing and development

## Build from source

The toolchain is pinned in `rust-toolchain.toml` and installed by rustup on first use.
`scripts/cargo` is a thin wrapper that puts `~/.cargo/bin` first in `PATH`.

System packages on Fedora:

```sh
sudo dnf install gcc gcc-c++ clang cmake pkgconf \
    libxkbcommon-devel wayland-devel libxcb-devel libX11-devel \
    fontconfig-devel freetype-devel \
    vulkan-loader mesa-vulkan-drivers
```

`vulkan-loader` and `mesa-vulkan-drivers` are runtime dependencies (GPUI renders with
Vulkan). Other distributions need the equivalent packages.

Only the Wayland backend is enabled, so the app needs a Wayland session. An X11 build
would also need `libxkbcommon-x11-devel` and the `x11` feature of `gpui-pre-platform`.

```sh
scripts/cargo build --release
# target/release/tbt-app     desktop app
# target/release/tbt-daemon  headless collector
```

## Linux (binaries + systemd user timer)

Copy the binaries somewhere stable first, because the schedule points at an absolute
path:

```sh
install -Dm755 target/release/tbt-daemon ~/.local/bin/tbt-daemon
install -Dm755 target/release/tbt-app ~/.local/bin/tbt-app
```

The app collects on its own while it is open. Install the daemon schedule if you also
want collection while it is closed:

```sh
~/.local/bin/tbt-daemon install --dry-run --app ~/.local/bin/tbt-app   # show what it would do
~/.local/bin/tbt-daemon install --app ~/.local/bin/tbt-app
```

`install` writes `tbt-daemon.service` and `tbt-daemon.timer` to
`$XDG_CONFIG_HOME/systemd/user/` with the binary path and data directory filled in,
creates the data directory, then runs `systemctl --user daemon-reload` and
`systemctl --user enable --now tbt-daemon.timer`. With `--app` it also installs
`io.VagueMotion.TokenBurnTrack.desktop` to `$XDG_DATA_HOME/applications/` and the icon
to `$XDG_DATA_HOME/icons/hicolor/scalable/apps/`, and removes files from older versions
(`tbt-app.desktop`, `tbt-app.svg`).

| Option | Effect |
|---|---|
| `--dry-run` | print every file (with contents) and command; change nothing |
| `--bin <path>` | daemon binary to schedule (default: the running `tbt-daemon`) |
| `--app <path>` | also install the desktop entry and icon for this `tbt-app` |
| `--no-enable` | write the files and reload systemd, but do not enable the timer |
| `--data-dir <dir>`, `--config <file>` | passed through to the scheduled daemon |

The timer fires hourly (`OnCalendar=hourly`) with a fixed per-machine delay of up to
2 minutes; `Persistent=true` runs a missed hour after suspend. The service is a
`Type=oneshot` run of `tbt-daemon --once` at `Nice=10` with idle I/O priority. It is
sandboxed: the file system is read-only except the data directory
(`ProtectSystem=strict` + `ReadWritePaths=`), with no network, a private `/tmp` and
`NoNewPrivileges`. If you later change `data_dir` in the config, run `install` again,
or the service cannot write to the new directory.

User services do not see variables exported by your shell. If you set `TBT_DATA_DIR` or
one of the [harness variables](configuration.md#harness-locations), add them with
`systemctl --user edit tbt-daemon.service` (`[Service]` + `Environment=NAME=value`).

```sh
systemctl --user list-timers tbt-daemon.timer
journalctl --user -u tbt-daemon.service
systemctl --user start tbt-daemon.service   # run once now
tbt-daemon uninstall                        # disable, stop, remove the files
```

**Manual install.** The files in [`packaging/systemd/`](../packaging/systemd) work as they
are when the binary is `~/.local/bin/tbt-daemon` and the data directory is
`~/.local/share/tokenburntrack`:

```sh
mkdir -p ~/.config/systemd/user ~/.local/share/tokenburntrack
cp packaging/systemd/tbt-daemon.{service,timer} ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now tbt-daemon.timer
```

For the launcher entry, copy `packaging/linux/io.VagueMotion.TokenBurnTrack.desktop` to
`~/.local/share/applications/` and `packaging/icons/io.VagueMotion.TokenBurnTrack.svg`
to `~/.local/share/icons/hicolor/scalable/apps/` (`Exec=tbt-app` expects `tbt-app` in
`PATH`).

## Linux (Flatpak)

The manifest is `packaging/flatpak/io.VagueMotion.TokenBurnTrack.yml`. It builds
offline with the pinned Rust 1.100.0 toolchain (downloaded as a checksummed source) and
the crates listed in `packaging/flatpak/cargo-sources.json`. Regenerate that file
whenever `Cargo.lock` changes:

```sh
scripts/flatpak-cargo-sources.py
flatpak-builder --user --install --force-clean build-dir packaging/flatpak/io.VagueMotion.TokenBurnTrack.yml
flatpak run io.VagueMotion.TokenBurnTrack
```

The sandbox can read `~/.claude`, `~/.codex`, `~/.copilot/session-state`, `~/.gemini`, `~/.grok`, `~/.lmstudio/conversations`, `~/.pi/agent/sessions`, `~/.dsh/sessions` and Zed's `threads` folder (`~/.local/share/zed/threads`, or
`~/.var/app/dev.zed.Zed/data/zed/threads` for the Zed Flatpak), and can read and write
`~/.local/share/opencode` and `~/.local/share/goose/sessions` (SQLite needs to create the
`-shm` file of a database in WAL mode). Config and data stay in the usual host locations (`~/.config/tokenburntrack`,
`~/.local/share/tokenburntrack`), so the Flatpak and a host-installed `tbt-daemon` share
one database and one collector lock. If `~/.lmstudio-home-pointer` points LM Studio somewhere
else, grant that folder too: `flatpak override --user --filesystem=<path>/conversations:ro
io.VagueMotion.TokenBurnTrack`.

Roots outside those directories (custom roots, `[[generic_jsonl]]` paths, a
`CLAUDE_CONFIG_DIR` elsewhere) are not visible until you grant them. The Harnesses page
shows the command:

```sh
flatpak override --user --filesystem=/path/to/logs:ro io.VagueMotion.TokenBurnTrack
```

`tbt-daemon install` does not work inside the sandbox. To collect while the app is
closed, turn on *Settings → General → Collect while closed*: it asks the desktop's
background portal to start `tbt-daemon` at login, starts it right away in its own
sandbox (so it outlives the app window), and it then syncs every
`sync_interval_minutes`.

## Linux (AppImage)

The AppImage contains only the app. `scripts/build-appimage.sh` also writes a standalone
`tbt-daemon` binary next to it; install that one (as above) for collection while the
app is closed, because the AppImage's mount path changes on every run.

## macOS (launchd)

```sh
tbt-daemon install    # or --dry-run first
```

This writes `~/Library/LaunchAgents/com.tokenburntrack.daemon.plist` and loads it with
`launchctl bootstrap gui/$UID`. The agent runs `tbt-daemon --once` at load and then
every hour (`StartInterval` 3600) at low priority. Its stdout and stderr go to
`launchd-stdout.log` and `launchd-stderr.log` in the logs directory.
`tbt-daemon uninstall` runs `launchctl bootout` and removes the plist.

For a manual install, replace `@BIN@` and `@LOG_DIR@` in
[`packaging/launchd/com.tokenburntrack.daemon.plist`](../packaging/launchd) with absolute
paths.

## Windows (Task Scheduler)

```powershell
tbt-daemon.exe install    # or --dry-run first
```

This writes the task definition to `%APPDATA%\tokenburntrack\tbt-daemon-task.xml` and
registers it with `schtasks /Create /TN TokenBurnTrack\tbt-daemon /XML <file> /F`. The
task runs `tbt-daemon.exe --once` every hour, only while you are logged on, without
elevation, and catches up on a missed run. `tbt-daemon uninstall` deletes it.

For a manual install, register
[`packaging/windows/tbt-daemon-task.xml`](../packaging/windows/tbt-daemon-task.xml) with
PowerShell. Its `<Command>` expects
`%LOCALAPPDATA%\Programs\TokenBurnTrack\tbt-daemon.exe`; edit it if the binary is
elsewhere.

```powershell
Register-ScheduledTask -TaskName 'tbt-daemon' -TaskPath '\TokenBurnTrack\' `
    -Xml (Get-Content -Raw .\packaging\windows\tbt-daemon-task.xml)
Unregister-ScheduledTask -TaskName 'tbt-daemon' -TaskPath '\TokenBurnTrack\' -Confirm:$false
```

## Running the daemon by hand

```sh
tbt-daemon --once                    # one ingest run, then exit
tbt-daemon                           # stay running; ingest at startup and every sync_interval_minutes
tbt-daemon --once --trigger manual   # record the run as manual
```

A run that finds the collector lock taken (the app is ingesting) prints a message and
exits 0. A long-running `tbt-daemon` that finds another one already running for the
same data directory (`daemon.lock` taken) exits 0 too. The app and a long-running
`tbt-daemon` re-read `config.toml` before each run, so saved settings apply on the next
sync (a `data_dir` change needs a restart).

## Development

```sh
scripts/check.sh                  # fmt --check, clippy -D warnings, tests for the workspace
scripts/check.sh tbt-daemon       # the same for one or more crates
scripts/cargo test -p tbt-adapters
```

- Adapter fixtures are real, anonymized harness logs in
  `tests/fixtures/<harness>/<version>/`, with golden outputs; see
  [`tests/fixtures/README.md`](../tests/fixtures/README.md). A format change in a harness
  means adding a fixture for the new version and fixing the parser.
- `scripts/anonymize.py` turns real logs (JSONL, JSON, SQLite) into fixtures: IDs,
  paths, branches and all free text are replaced consistently, while structure, token
  counts, timestamps and model IDs are kept. Grep its output for your home path and user
  name before committing.
- Packaging templates live in [`packaging/`](../packaging) and are embedded into
  `tbt-daemon` at build time, so `tbt-daemon install` always writes what is in the repo.

### Performance harnesses

Release builds; `<D>` is a scratch directory.

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

### Release packages

Each script writes to `dist/`. GPUI compiles its shaders with platform tools (`fxc.exe`
on Windows, the Metal compiler on macOS), so Windows and macOS packages must be built on
those systems.

| Package | Command | Needs |
|---|---|---|
| AppImage | `scripts/build-appimage.sh` | podman; builds in `ubuntu:22.04` (glibc 2.35) so the result runs on older distributions. `--host` builds with the local toolchain instead. Wayland only |
| Windows exe | `powershell scripts\build-windows.ps1`; writes `TokenBurnTrack-<version>-windows-x64.exe` and `tbt-daemon-<version>-windows-x64.exe` | rustup, Visual Studio Build Tools (C++ workload) and the Windows SDK |
| macOS dmg | `scripts/build-macos.sh` | rustup and Xcode; produces a universal `TokenBurnTrack.app`, ad-hoc signed unless `CODESIGN_IDENTITY` is set. Set `NOTARY_PROFILE` to an `xcrun notarytool store-credentials` profile to also notarize and staple the dmg; without it, Gatekeeper blocks the dmg on other Macs |

Pushing a `v*` tag runs `.forgejo/workflows/release.yml`, which builds the Flatpak
bundle and the AppImage on a Linux runner with the `docker` label and attaches them to
the tag's release (a prerelease if the tag contains `-`). `flatpak-builder` needs
privileged containers, so set `container.privileged: true` in that runner's config.
Windows and macOS packages are not built there; build them with the scripts above and
attach them by hand.

On GitHub, pushing a `v*` tag runs `.github/workflows/release.yml` instead, which builds
all four packages on GitHub-hosted runners and attaches them to the tag's GitHub release
(a prerelease if the tag contains `-`). The macOS dmg is ad-hoc signed there. Forgejo
ignores `.github/workflows` while `.forgejo/workflows` exists.
