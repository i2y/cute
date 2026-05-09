#!/usr/bin/env bash
# examples/gpu_chart/run.sh — animated bar / line chart demo for gpu_app.

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"

echo ">> cargo build cute"
cargo build -p cute-cli --manifest-path "$REPO_ROOT/Cargo.toml" --quiet

echo ">> cute install-cute-ui (idempotent)"
"$REPO_ROOT/target/debug/cute" install-cute-ui

echo ">> cute build gpu_chart.cute"
cd "$HERE"
"$REPO_ROOT/target/debug/cute" build gpu_chart.cute

echo ">> launching ./gpu_chart  (close the window to exit)"
"$HERE/gpu_chart"
