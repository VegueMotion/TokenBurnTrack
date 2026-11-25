# Development

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

## Performance harnesses

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

## Release packages

Each script writes to `dist/`. GPUI compiles its shaders with platform tools (`fxc.exe`
on Windows, the Metal compiler on macOS), so Windows and macOS packages must be built on
those systems.

| Package | Command | Needs |
|---|---|---|
| AppImage | `scripts/build-appimage.sh` | podman; builds in `ubuntu:22.04` (glibc 2.35) so the result runs on older distributions. `--host` builds with the local toolchain instead. Wayland only |
| Windows zip | `powershell scripts\build-windows.ps1` | rustup, Visual Studio Build Tools (C++ workload) and the Windows SDK |
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
