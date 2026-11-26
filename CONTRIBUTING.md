# Contributing to TokenBurnTrack

Thanks for your interest. Please read this before opening a pull request. PRs that ignore it may be closed without review.

To build from source, set up the daemon and run the checks, see [docs/install.md](docs/install.md).

## Branches

| Branch    | Purpose                    |
|-----------|----------------------------|
| `master`  | Release code only.         |
| `develop` | Active development.        |

- All PRs target `develop`.
- `develop` is merged into `master` for a release.

## Issues

Start the title of a bug report with `[Bug]:`, for example `[Bug]: Daemon crashes on an empty Codex log`.

## The best way to help

These are the changes most likely to be merged:

- Small, focused bug fixes
- Reliability fixes, such as parsing errors, missed usage data or daemon crashes
- Small performance improvements
- Support for a new agent or platform, discussed in an issue first
- Clear bug reports with steps to reproduce

Reporting a bug with good details is often more useful than a PR.

## Rules for a PR

1. **Discuss first.** For anything beyond a small fix, open an issue before writing code.
2. **Keep it small.** One PR does one thing. Don't mix unrelated fixes or refactors.
3. **Explain it.** Say what changed and why it should exist. Link the issue.
4. **Target `develop`.** Branch from `develop`, rebase on it before opening the PR.
5. **It must build and pass.** Run `cargo fmt`, `cargo clippy` with no new warnings, and `cargo test`.
6. **Show UI changes.** Include before/after screenshots. For anything animated or interactive, include a short video.
7. **Update docs** if behavior, configuration or installation changes.
8. **Write clear commits** in the imperative mood: `Fix cost rounding for cached tokens`, not `fixes`.
9. **Prefix the PR title** with its type: `<type>: <summary>`, for example `fix: Round cached token costs correctly`.

| Prefix     | Use for                                   |
|------------|-------------------------------------------|
| `fix`      | Bug fixes                                 |
| `feat`     | New features or harness support           |
| `perf`     | Performance improvements                  |
| `docs`     | Documentation only                        |
| `refactor` | Code changes that don't change behavior   |
| `test`     | Tests and fixtures only                   |
| `ci`       | CI, release workflows and packaging       |
| `chore`    | Version bumps, dependencies, other upkeep |

At least one maintainer approval is required before merging.

## Use of AI tools

AI tools are allowed. **You are fully responsible for everything you submit**, no matter how it was written:

- You understand every line and can explain it in review.
- You built and tested it yourself.
- You checked that it does not copy third-party code under an incompatible license.

"The AI wrote it" is not an answer to a review question.

## Bans

These will be closed, usually without discussion:

- Large PRs, especially feature PRs over a few hundred lines that weren't agreed on in an issue
- Drive-by features or scope changes nobody asked for
- Opinionated rewrites, mass reformatting or renaming
- Unreviewed AI-generated PRs, issues or descriptions
- PRs targeting `master`
- Dependency additions without a clear reason

Repeated low-effort or spam submissions will get you blocked from the repository.

## License

By contributing, you agree that your contributions are licensed under the same license as this project (see [LICENSE](LICENSE)).
