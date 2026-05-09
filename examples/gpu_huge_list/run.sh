#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
cargo build -p cute-cli --manifest-path "$REPO_ROOT/Cargo.toml" --quiet
"$REPO_ROOT/target/debug/cute" install-cute-ui
cd "$HERE"
"$REPO_ROOT/target/debug/cute" build gpu_huge_list.cute
"$HERE/gpu_huge_list"
