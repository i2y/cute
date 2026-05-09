#!/usr/bin/env bash
# examples/gpu_table/run.sh — DataTable demo for gpu_app.

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"

echo ">> cargo build cute"
cargo build -p cute-cli --manifest-path "$REPO_ROOT/Cargo.toml" --quiet

echo ">> cute install-cute-ui (idempotent)"
"$REPO_ROOT/target/debug/cute" install-cute-ui

echo ">> cute build gpu_table.cute"
cd "$HERE"
"$REPO_ROOT/target/debug/cute" build gpu_table.cute

echo ">> launching ./gpu_table  (close the window to exit)"
"$HERE/gpu_table"
