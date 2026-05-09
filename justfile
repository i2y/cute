# Cute task runner. Run `just` (no args) for the recipe list.
#
# Most common flows:
#   just install        # install `cute` to ~/.cargo/bin (release build)
#   just test           # run the workspace test suite
#   just demo NAME      # build + run an example by directory name
#                       # (e.g. `just demo charts`)
#   just check          # cargo check across the workspace

# Default recipe: print the list when `just` is run with no args.
default:
    @just --list

# ---- build / install ------------------------------------------------------

# Build everything in dev mode.
build:
    cargo build --workspace

# Build everything in release mode.
release:
    cargo build --workspace --release

# Install `cute` into ~/.cargo/bin (release build).
install:
    cargo install --path crates/cute-cli --force

# Symlink the dev binary into ~/.cargo/bin (faster iteration).
install-dev: build
    mkdir -p ~/.cargo/bin
    ln -sf "$(pwd)/target/debug/cute" ~/.cargo/bin/cute

# ---- quality gates --------------------------------------------------------

# Run the workspace test suite.
test:
    cargo test --workspace

# Compile-only check across the workspace (no codegen).
check:
    cargo check --workspace

# Format every crate.
fmt:
    cargo fmt --all

# Lint with clippy (warnings -> errors).
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Clean cargo + incremental cute build cache.
clean:
    cargo clean
    rm -rf ~/.cache/cute/build

# ---- demos ----------------------------------------------------------------

# Build examples/NAME/NAME.cute via `cargo run` (no install needed).
build-demo NAME:
    cargo run -p cute-cli --quiet -- build "examples/{{NAME}}/{{NAME}}.cute"

# Build + run examples/NAME/NAME.cute (foreground, Cmd+Q to close).
demo NAME: (build-demo NAME)
    "./{{NAME}}"

# Smoke-build every example in examples/. Demos that require
# extra system dependencies (KDE Frameworks for kirigami_hello,
# QtCharts for charts) are still listed but failures from the
# CMake / link step are tolerated — Cute's frontend ran fine if
# we got that far. The frontend errors (parse / type-check) DO
# fail the loop. Test-only demos (`examples/test_*/`) are routed
# to `cute test` instead of `cute build`, since they have no
# main / view / widget and would otherwise fail to link.
demos-all:
    #!/usr/bin/env bash
    set -uo pipefail
    failed=()
    for d in examples/*/; do
      name="$(basename "$d")"
      cute_file="$d$name.cute"
      if [[ -f "$cute_file" ]]; then
        if [[ "$name" == test_* ]]; then
          echo "==> testing $name"
          if ! cargo run -p cute-cli --quiet -- test "$cute_file"; then
            failed+=("$name")
          fi
          continue
        fi
        echo "==> building $name"
        if ! cargo run -p cute-cli --quiet -- build "$cute_file"; then
          # Tolerate environment-dependent demos: smoke check that
          # the frontend stays clean, not that every CI runner has
          # KF6 / Qt 6.11 Canvas Painter / cute_ui runtime /
          # installed Cute libraries.
          case "$name" in
            kirigami_hello|calculator_kirigami|reading_list)
              echo "    (skipping $name — KF6Kirigami not installed)" ;;
            gpu_*)
              echo "    (skipping $name — Qt 6.11 Canvas Painter / cute_ui runtime not installed)" ;;
            lib_counter_app)
              echo "    (skipping $name — LibCounter not installed)" ;;
            *)
              failed+=("$name") ;;
          esac
        fi
      fi
    done
    if [[ ${#failed[@]} -gt 0 ]]; then
      echo "FAILED: ${failed[*]}"
      exit 1
    fi

# Verify every `.cute` source under examples/ is fmt-clean.
# Iterates each .cute file individually (including sub-files like
# examples/two_counters/model.cute) so the failure list names every
# offender. `cute fmt --check` takes one path at a time.
demos-fmt-check:
    #!/usr/bin/env bash
    set -uo pipefail
    failed=()
    while IFS= read -r f; do
      if ! cargo run -p cute-cli --quiet -- fmt --check "$f"; then
        failed+=("$f")
      fi
    done < <(find examples -name '*.cute' | sort)
    if [[ ${#failed[@]} -gt 0 ]]; then
      echo "FAILED fmt --check:"
      printf '  %s\n' "${failed[@]}"
      exit 1
    fi

# ---- bindings (cute-qpi-gen) ----------------------------------------------

# `cute-qpi-gen` needs libclang.dylib at runtime. macOS Homebrew Qt 6
# headers and the Xcode toolchain libclang are the assumed defaults;
# override via `CUTE_QPI_DYLD=...` if you're on a non-standard layout.

# Regenerate every typesystem-driven `.qpi` file under stdlib/qt/.
# Walks `stdlib/qt/typesystem/*.toml`, runs cute-qpi-gen, and writes
# the result to the matching `stdlib/qt/<name>.qpi`. Re-run this
# after editing a typesystem or after pulling a new Qt headers
# layout.
qpi-regen: build
    #!/usr/bin/env bash
    set -euo pipefail
    : "${CUTE_QPI_DYLD:=/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib}"
    export DYLD_FALLBACK_LIBRARY_PATH="$CUTE_QPI_DYLD"
    GEN=./target/debug/cute-qpi-gen
    for ts_file in stdlib/qt/typesystem/*.toml; do
      name=$(basename "$ts_file" .toml)
      out_file="stdlib/qt/${name}.qpi"
      $GEN \
        --typesystem "$ts_file" \
        --header-comment "Auto-generated by cute-qpi-gen from stdlib/qt/typesystem/${name}.toml.

    Do not edit by hand — re-run with:
      just qpi-regen" \
        > "$out_file"
      echo "wrote $out_file"
    done

# Check that every typesystem-driven `.qpi` matches what the
# generator produces from its `.toml`. Fails (with a unified diff)
# if the committed `.qpi` is stale or hand-edited. Pre-commit / CI
# gate; safe to skip locally if you don't have Qt 6 + libclang
# installed.
qpi-check: build
    #!/usr/bin/env bash
    set -uo pipefail
    : "${CUTE_QPI_DYLD:=/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib}"
    export DYLD_FALLBACK_LIBRARY_PATH="$CUTE_QPI_DYLD"
    GEN=./target/debug/cute-qpi-gen
    failed=()
    for ts_file in stdlib/qt/typesystem/*.toml; do
      name=$(basename "$ts_file" .toml)
      out_file="stdlib/qt/${name}.qpi"
      tmp=$(mktemp)
      if ! $GEN \
        --typesystem "$ts_file" \
        --header-comment "Auto-generated by cute-qpi-gen from stdlib/qt/typesystem/${name}.toml.

    Do not edit by hand — re-run with:
      just qpi-regen" \
        > "$tmp" 2>&1; then
        echo "GEN FAILED: $name"
        cat "$tmp"
        failed+=("$name")
        rm -f "$tmp"
        continue
      fi
      if ! diff -u "$out_file" "$tmp" > /dev/null 2>&1; then
        echo "STALE: $out_file (re-run \`just qpi-regen\`)"
        diff -u "$out_file" "$tmp" | head -30
        failed+=("$name")
      fi
      rm -f "$tmp"
    done
    if [[ ${#failed[@]} -gt 0 ]]; then
      echo
      echo "FAILED qpi-check: ${failed[*]}"
      exit 1
    fi
    count=$(ls stdlib/qt/typesystem/*.toml | wc -l | tr -d ' ')
    echo "qpi-check: all $count typesystems match"

# ---- documentation site (Zensical) ---------------------------------------

# `website/` is a Zensical (Material for MkDocs successor) site.
# `docs-serve` / `docs-build` set up a Python 3.11 venv at
# `website/.venv/` on first use with zensical + the local
# cute-pygments lexer editable-installed, then drive the standard
# zensical lifecycle. `uv` (https://docs.astral.sh/uv/) is the
# assumed venv manager — fast, picks the right Python, no manual
# `python3 -m venv` dance.

# Set up website/.venv/ with zensical + cute-pygments. Idempotent —
# re-runs are no-ops once the venv exists. Force a re-install with
# `just docs-clean` first.
docs-setup:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -d website/.venv ]]; then
      exit 0
    fi
    if ! command -v uv >/dev/null 2>&1; then
      echo "error: \`uv\` is required (https://docs.astral.sh/uv/). Install with \`curl -LsSf https://astral.sh/uv/install.sh | sh\` and re-run." >&2
      exit 1
    fi
    cd website
    uv venv --python 3.14 .venv
    # `uv venv` doesn't seed pip; use `uv pip install --python` to
    # install into the bare venv.
    uv pip install --python .venv/bin/python --quiet zensical
    uv pip install --python .venv/bin/python --quiet -e ../extensions/cute-pygments/
    echo "docs-setup: website/.venv ready (zensical + cute-pygments installed)"

# Live preview at http://localhost:8000. Watches zensical.toml +
# every file under website/docs/ and re-renders on save.
docs-serve: docs-setup
    cd website && .venv/bin/zensical serve

# Build the static site into website/build/. Deploy that
# directory to any static host (GitHub Pages / Cloudflare /
# Netlify / S3 / …).
docs-build: docs-setup
    cd website && .venv/bin/zensical build

# Remove generated site + the docs venv. Next `docs-serve` /
# `docs-build` will re-run `docs-setup`.
docs-clean:
    rm -rf website/build website/.venv

# ---- editor integration ---------------------------------------------------

# Symlink the VS Code extension into ~/.vscode/extensions for local dev.
# After running this, reload VS Code (Cmd+Shift+P → "Reload Window") to
# pick up the Cute syntax highlighting and language configuration.
install-vscode:
    mkdir -p ~/.vscode/extensions
    ln -sfn "$(pwd)/extensions/vscode-cute" ~/.vscode/extensions/cute-language-dev
    @echo "Linked extensions/vscode-cute → ~/.vscode/extensions/cute-language-dev"
    @echo "Reload VS Code to activate."

# Package the VS Code extension as a .vsix (requires `vsce`).
package-vscode:
    cd extensions/vscode-cute && vsce package

# ---- repo housekeeping ----------------------------------------------------

# Pre-commit sweep: fmt + check + test (Rust side only, fast).
ci: fmt check test

# Full CI gate: cargo fmt + cargo check + cargo test + Cute fmt-check + demos build.
# This is the gate the GitHub Actions workflow runs; `ci` stays fast for
# pre-commit, `ci-full` is the v1.0 contract.
ci-full: fmt check test demos-fmt-check demos-all
