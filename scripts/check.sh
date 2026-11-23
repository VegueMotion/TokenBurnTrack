#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

if [ $# -eq 0 ]; then
  cargo fmt --all --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
else
  cargo fmt --all --check
  for p in "$@"; do
    cargo clippy -p "$p" --all-targets -- -D warnings
    cargo test -p "$p"
  done
fi
