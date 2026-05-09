#!/usr/bin/env bash
# examples/gpu_notes/run.sh — TextField + Image + ListView showcase.
#
# `cute build gpu_notes.cute` lowers the widget tree to cute::ui calls
# linked against libcute_ui.a, which renders via QRhi + QCanvasPainter.
# Cmd/Ctrl+T toggles dark/light theme at runtime.

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"

echo ">> cargo build cute"
cargo build -p cute-cli --manifest-path "$REPO_ROOT/Cargo.toml" --quiet

echo ">> cute install-cute-ui (idempotent)"
"$REPO_ROOT/target/debug/cute" install-cute-ui

echo ">> cute build gpu_notes.cute"
cd "$HERE"
"$REPO_ROOT/target/debug/cute" build gpu_notes.cute

echo ">> launching ./gpu_notes  (close the window to exit)"
"$HERE/gpu_notes"
