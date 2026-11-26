# TokenBurnTrack

TokenBurnTrack reads the local log files of AI coding agents ("harnesses"), turns them
into token usage and cost, stores them in a local SQLite database and shows them in a
desktop app (GPUI). A headless daemon collects on a schedule while the app is closed.
Nothing is ever sent off the machine, and harness source files are only ever read.

## Workspace

Rust workspace, edition 2024, toolchain pinned in `rust-toolchain.toml`.

| Crate | Role |
|---|---|
| `tbt-core` | Shared types and the `HarnessAdapter` trait. Contracts in [`crates/tbt-core/CONTRACTS.md`](crates/tbt-core/CONTRACTS.md) |
| `tbt-adapters` | One parser per harness (`claude_code.rs`, `codex.rs`, …) plus `generic_jsonl` |
| `tbt-pricing` | Model prices (LiteLLM data) |
| `tbt-store` | SQLite schema, migrations, ingest and queries |
| `tbt-collector` | Adapter registry, ingest pipeline, collector lock, scheduler, config |
| `tbt-daemon` | `tbt-daemon` binary and `install`/`uninstall` for systemd, launchd and Task Scheduler |
| `tbt-app` | `tbt-app` desktop app |

## Commands

Use `scripts/cargo` instead of `cargo`. It puts `~/.cargo/bin` first in `PATH`, so the
pinned rustup toolchain is used rather than a system Rust.

```sh
scripts/check.sh                   # fmt --check, clippy -D warnings, tests (whole workspace)
scripts/check.sh tbt-adapters      # the same for one or more crates
scripts/cargo test -p tbt-store
scripts/cargo build --release
```

Run `scripts/check.sh` for the crates you touched before you call a change done. Clippy
runs with `-D warnings`, so any warning fails.

Full build, install and release details: [docs/install.md](docs/install.md).

## Ways to break the user's machine

The tool runs against the developer's real data and desktop. Never:

- **Write to harness logs** (`~/.claude`, `~/.codex`, `~/.gemini`, …). Read or copy them only.
- **Write to the live data or config dir** (`~/.local/share/tokenburntrack`,
  `~/.config/tokenburntrack`, or the macOS and Windows equivalents). Running `tbt-app` or
  `tbt-daemon` without a scratch data dir writes there.
- **Run `tbt-daemon install` or `uninstall` for real.** They change the user's systemd,
  launchd or Task Scheduler setup and desktop entries. `--dry-run` is fine.
- **Kill `tbt-app` or `tbt-daemon` by name or pattern.** Kill only a PID you started.
- **Run the app under `dbus-run-session`** (for example with a nested `kwin_wayland
  --virtual` for screenshots). It leaves the Flatpak document portal unmounted, and every
  Flatpak on the machine stops starting. Ask the user for screenshots instead.

## Test data

- Point `TBT_DATA_DIR` (or `tbt-daemon --data-dir`) at a scratch directory, and pass
  `tbt-daemon --config <file>` if you need settings.
- For a database built from the fixtures:
  `TBT_DATA_DIR=<D> scripts/cargo run -p tbt-app --example fixture_db`.
- For a large synthetic database, use the `perf` example in `tbt-store` (see
  [docs/install.md](docs/install.md#performance-harnesses)).
- To test against a real database, copy `tbt.sqlite` into the scratch directory. Never
  symlink it.

## Conventions

Read `crates/tbt-core/CONTRACTS.md` before changing anything that touches events,
prices, cursors or sessions. The most important rules:

- **Money** is never `f64`: rates are `Decimal`, stored amounts are `Micros` (i64, USD × 1e6).
- **Time** is UTC `jiff::Timestamp`. Local time only for display and day buckets.
- **`input_tokens`** is uncached input only. `reasoning_tokens` is a subset of
  `output_tokens` and is never billed again.
- **Dedup keys** come only from provider or harness IDs, never from session IDs or file paths.
- **Cursors** never contain message text.
- Ingest is incremental and idempotent: re-running must never count anything twice.

## Adapters and fixtures

- Fixtures are real, anonymized harness logs in `tests/fixtures/<harness>/<version>/`,
  with insta snapshots in `crates/tbt-adapters/tests/snapshots/`. See
  [`tests/fixtures/README.md`](tests/fixtures/README.md).
- A harness format change means adding a fixture for the new version and fixing the
  parser. Don't edit existing fixtures to make a test pass.
- Make new fixtures with `scripts/anonymize.py`. Never commit raw logs, and grep the
  output for home paths and user names before committing.
- A new adapter is registered in `crates/tbt-collector/src/registry.rs` and documented
  in [docs/harnesses.md](docs/harnesses.md). Under Flatpak it also needs read access
  in `packaging/flatpak/io.VagueMotion.TokenBurnTrack.yml`.

## Packaging

- Templates in `packaging/` are embedded into `tbt-daemon` at build time. Edit them
  there, not in generated output.
- After changing `Cargo.lock`, regenerate `packaging/flatpak/cargo-sources.json` with
  `scripts/flatpak-cargo-sources.py`.

## Git

- Branch from `master` and target `master`. `stable` is for releases only.
- To release, bump the version in `Cargo.toml` on `master`, then open a PR from `master`
  into `stable` and merge it with a merge commit (the only method `stable` allows; it
  can't be pushed to directly). The merge runs the release workflow, which fails if `vX.Y.Z` is already tagged and
  otherwise tags the merge commit and creates the release. Don't tag by hand.
- After the release workflow succeeds, point the README download buttons at the new
  version's files (`releases/download/vX.Y.Z/…-X.Y.Z-…`) in a PR to `master`.
- One change per PR. No drive-by refactors, mass reformatting or renaming.
- Commit messages in the imperative mood: `Fix cost rounding for cached tokens`.
- PR titles start with a type prefix: `fix:`, `feat:`, `perf:`, `docs:`, `refactor:`,
  `test:`, `ci:` or `chore:`. See [CONTRIBUTING.md](CONTRIBUTING.md#rules-for-a-pr).
- Bug report issue titles start with `[Bug]:`.
- Don't add dependencies without a clear reason.
- Don't commit plans, research notes or scratch files. Track work in issues.

## Documentation

- `docs/` is for users: installing, configuring, which harnesses are read. Update it
  when behavior, configuration or installation changes.
- Don't document every feature or narrate control flow.
- When a decision changes, update or remove the docs that describe it.
