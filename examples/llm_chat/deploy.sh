#!/usr/bin/env bash
# Self-contained .app builder for the Cute LLM chat showcase.
#
# `cute build examples/llm_chat/llm_chat.cute` produces a thin .app
# (180 KB binary) that links against Homebrew's Qt6 in-place. To
# distribute it as a single bundle, the Qt frameworks need to be
# copied in via Qt's `macdeployqt`. This script wraps that step,
# threading the right `-libpath` flags through so transient
# dependencies in qtdeclarative (QtSvg, QtPdf, QtVirtualKeyboard,
# QtStateMachine — Qt 6.11 fanout) get located on Homebrew's keg-
# only layout.
#
# Even with every keg path passed in, Qt 6.11's qtdeclarative drags
# in modules the chat doesn't actually use (Qt3D, PdfQuick, ...).
# `macdeployqt` prints `ERROR: Cannot resolve rpath …` for those —
# the warnings are non-fatal: the bundle still launches, those
# modules just aren't copied because nothing in the chat references
# them at runtime. We grep them out below to keep the output
# readable; the genuine deploy failures still surface.
#
# Usage:
#   cd <cute-repo-root>
#   ./examples/llm_chat/deploy.sh
#   open llm_chat-deployed.app
#   tar --zstd -cf llm_chat-deployed.app.tar.zst llm_chat-deployed.app

set -euo pipefail

cd "$(dirname "$0")/../.."

echo "==> cute build llm_chat"
cargo run -p cute-cli --quiet -- build examples/llm_chat/llm_chat.cute

# Stage the freshly-built app to a deploy-prefixed name so successive
# runs don't `macdeployqt` over an already-deployed bundle (it
# refuses, and in any case we want a clean copy of just the binary).
SRC=llm_chat.app
DST=llm_chat-deployed.app
echo "==> staging ${DST}"
rm -rf "${DST}"
cp -R "${SRC}" "${DST}"

# QML import scan dir = the cute build cache for this source. The
# cache hash is stable across rebuilds of the same .cute file.
QML_DIR="$(ls -td ~/.cache/cute/build/*/Main.qml 2>/dev/null | head -1 | xargs dirname)"

# Every Homebrew keg dir whose .framework an installed Qt module
# could live in. Listed individually because Homebrew's qt is a
# meta-formula whose dependencies install keg-only — the framework
# isn't on /opt/homebrew/lib's flat layout, only under the keg's
# own lib/.
LIB_PATHS=(
  /opt/homebrew/opt/qtbase/lib
  /opt/homebrew/opt/qtdeclarative/lib
  /opt/homebrew/opt/qtsvg/lib
  /opt/homebrew/opt/qttools/lib
  /opt/homebrew/opt/qtmultimedia/lib
  /opt/homebrew/opt/qtvirtualkeyboard/lib
  /opt/homebrew/opt/qtwebsockets/lib
  /opt/homebrew/lib
)

ARGS=(-qmldir="${QML_DIR}" -appstore-compliant)
for p in "${LIB_PATHS[@]}"; do
  if [[ -d "$p" ]]; then
    ARGS+=(-libpath="$p")
  fi
done

echo "==> macdeployqt ${DST}"
# Filter the noisy transient-rpath warnings (see header). They all
# follow the same `Cannot resolve rpath "@rpath/Qt*.framework/...` /
# `using QList(...)` pair. Anything else still surfaces.
LOG=$(mktemp)
set +e
macdeployqt "${DST}" "${ARGS[@]}" >"${LOG}" 2>&1
SKIPPED=$(grep -cE 'Cannot resolve rpath "@rpath/Qt' "${LOG}")
grep -vE 'Cannot resolve rpath "@rpath/Qt|^ERROR:  using QList' "${LOG}"
set -e
echo "(suppressed ${SKIPPED} 'Cannot resolve rpath' lines for Qt6 transient deps)"
rm -f "${LOG}"

echo "==> bundle size"
du -sh "${DST}"

echo "==> compressed (.tar.zst)"
tar --zstd -cf "${DST}.tar.zst" "${DST}"
du -sh "${DST}.tar.zst"

echo
echo "Done. Launch with:  open ${DST}"
