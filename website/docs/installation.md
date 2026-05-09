# Installation

The Cute compiler ships from source via Cargo. A native package
manager release (`brew install cute`, curl installer, AUR
PKGBUILD) is planned for a future release; until then, the path
below is the supported route on every platform.

!!! note "Prerequisites"
    - **Rust 1.85+** (Cargo)
    - **CMake ≥ 3.21**
    - **Qt 6.9 or newer** — Cute's moc replacement uses
      `QtCore/qtmochelpers.h`, which landed in Qt 6.9. Older Qt
      will fail at C++ compile time. (`gpu_app` / CuteUI mode
      pins **Qt 6.11+** for the QtCanvasPainter Tech Preview.)
    - **`just`** for the install recipe (`brew install just` /
      `cargo install just` / your distro's `just` package).
    - A C++20-capable compiler (g++ ≥ 10, clang ≥ 12, MSVC 2022).

## 1. Install the compiler

```sh
git clone https://github.com/i2y/cute && cd cute
just install        # → cargo install --path crates/cute-cli --force
```

`just install` builds release-mode and writes `~/.cargo/bin/cute`.
With `~/.cargo/bin` on `PATH`, the binary is immediately on
`cute build` / `cute check` / etc.

For tighter iteration on Cute itself, use `just install-dev`
instead — it symlinks the debug build, so `cargo build` updates
the on-disk `cute` without re-installing.

## 2. Install Qt 6

The compiler is platform-agnostic but the binaries it produces
link against system Qt 6. Pick the path for your OS:

### macOS (Homebrew)

```sh
brew install qt
```

Homebrew's `qt` formula is the umbrella: it covers
`Qt6::Core` / `Gui` / `Widgets` / `Qml` / `Quick` /
`QuickControls2` / `Network` / `HttpServer` / `Charts` / `Svg` /
`Multimedia` in one shot. This is enough for `qml_app`,
`widget_app`, `server_app`, `cli_app`, and `gpu_app` (Qt 6.11+).

### macOS (KDE Craft, for Kirigami / KF6)

Kirigami isn't on Homebrew. Use [KDE Craft](https://community.kde.org/Craft):

```sh
# (Once) bootstrap Craft into ~/CraftRoot
craft qtbase qtdeclarative qthttpserver qtcharts kirigami
```

`cute build` finds CraftRoot automatically when the project's
`cute.toml` declares `find_package = ["KF6Kirigami"]`. Kirigami
binaries get a `.app` bundle with adhoc-codesign + entitlements
so `open Foo.app` launches without DYLD env.

### Linux

| Distro | Install command |
|---|---|
| **openSUSE Tumbleweed** | `sudo zypper install qt6-base-devel qt6-declarative-devel qt6-httpserver-devel qt6-charts-devel qt6-svg-devel qt6-multimedia-devel` |
| **Fedora rawhide** | `sudo dnf install qt6-qtbase-devel qt6-qtdeclarative-devel qt6-qthttpserver-devel qt6-qtcharts-devel qt6-qtsvg-devel qt6-qtmultimedia-devel` |
| **Debian sid / Ubuntu 24.04+** | `sudo apt install qt6-base-dev qt6-declarative-dev qt6-httpserver-dev qt6-charts-dev qt6-svg-dev qt6-multimedia-dev` |
| **Arch / Manjaro** | `sudo pacman -S qt6-base qt6-declarative qt6-httpserver qt6-charts qt6-svg qt6-multimedia` |

!!! warning "Older Ubuntu / Debian"
    Ubuntu 24.04 LTS ships Qt 6.4 and Debian trixie ships
    Qt 6.7 — both are too old for Cute's `qtmochelpers.h`
    requirement. Use Debian sid, Fedora rawhide, Tumbleweed,
    or Arch.

A reproducible Tumbleweed Docker setup lives in the repo at
[`Dockerfile`](https://github.com/i2y/cute/blob/main/Dockerfile)
— the upstream Linux smoke test.

### Windows

The compiler builds on Windows; the `cute build` → cmake → MSVC
path is **unverified**. Reports welcome.

## 3. Verify with `cute doctor`

```sh
cute doctor
```

Run from inside any Cute project (or pass an entry file
explicitly: `cute doctor path/to/foo.cute`). `cute doctor`
enumerates the Qt 6 / KF 6 / CuteUI modules the build needs,
checks each against your local install, and prints the
platform-specific install command for anything missing.

```text
==> Cute doctor

Project: examples/charts/charts.cute (BuildMode::Widgets)

Qt 6 toolchain
  + Found at /opt/homebrew (6.11.0)

Required Qt 6 modules (4)
  + Qt6::Core
  + Qt6::Gui
  + Qt6::Widgets
  + Qt6::Charts

Detected distro: macOS (Homebrew)

All dependencies present. `cute build` should succeed.
```

If anything's missing, doctor surfaces a single copy-pasteable
install line — `brew install qt` on macOS (umbrella formula);
one `sudo zypper install …` / `sudo apt install …` /
`sudo pacman -S …` line on Linux. Re-run after installing.

## 4. Optional extras

### Editor LSP

```sh
cargo install --path crates/cute-lsp
```

`cute-lsp` is a stdio Language Server Protocol implementation.
Wire it into your editor's LSP client to get:

- live diagnostics (parse / type-check / unused / shadowing)
- hover with type + signature
- go-to-definition (cross-file via `cute.toml` + `use foo`)
- member completion (props / signals / slots / fns / methods)
- format-on-save (`cute fmt`)

`cute init --vscode` scaffolds a project-local
`.vscode/settings.json` that wires this up automatically. For
other editors, point your LSP client at `cute-lsp` (stdio,
JSON-RPC) per their docs.

### VS Code extension — `vscode-cute`

The bundled
[`extensions/vscode-cute`](https://github.com/i2y/cute/tree/main/extensions/vscode-cute)
extension provides:

- **Syntax highlighting** for `.cute` and `.qpi` files (TextMate
  grammar — `prop` / `state` / `signal` / `slot` / `view` /
  `widget` / `style` / `pub` / `arc` etc. classified)
- **Bracket / paren / quote auto-closing** + comment toggling
  via the language configuration
- **2-space indent default** for `.cute` files
- **LSP coordination** — picks up `cute-lsp` from a project
  `.vscode/settings.json` (`cute init --vscode` generates one)

Install:

```sh
cute install-vscode     # writes to ~/.vscode/extensions/i2y.cute-language-<version>/
# then in VS Code: Cmd/Ctrl+Shift+P → "Reload Window"
```

The extension files (TextMate grammar + language config +
metadata) are bundled into the `cute` binary at compile time,
so this works whether you built from source or installed from a
packaged release. Pass `--force` to overwrite an existing
install.

If you're hacking on the extension itself, `just install-vscode`
(from a repo clone) symlinks `extensions/vscode-cute/` directly
into `~/.vscode/extensions/cute-language-dev` instead, so edits
to the grammar / language config take effect after a Reload
Window without re-running install.

To produce a distributable `.vsix`, run `just package-vscode`
(requires [`vsce`](https://github.com/microsoft/vscode-vsce)).
The Marketplace-published path (`code --install-extension` from
the VS Code Marketplace) is planned for a future release.

### Kate syntax highlighting — `kate-cute`

The bundled
[`extensions/kate-cute`](https://github.com/i2y/cute/tree/main/extensions/kate-cute)
syntax definition lights up `.cute` and `.qpi` files in Kate,
KWrite, KDevelop, and any other editor that consumes the
[KSyntaxHighlighting](https://api.kde.org/frameworks/syntax-highlighting/html/)
framework. Covers declarations, control flow, modifiers,
constants, built-in types, `#{...}` interpolation with full
nested-expression highlighting, escape sequences, `:symbol`
literals, and decimal / hex / binary / float numerics.

Install:

```sh
cute install-kate
# then restart Kate, or:
#   Settings → Configure Kate → Open/Save → Modes & Filetypes
#   → re-select Cute
```

The `cute.xml` file is bundled into the `cute` binary at compile
time (same pattern as `cute install-vscode`) and unpacked into
the per-user KSyntaxHighlighting search path:

| Platform | Path |
|---|---|
| Linux | `$XDG_DATA_HOME/org.kde.syntax-highlighting/syntax/cute.xml` (default `~/.local/share/...`) |
| macOS | `~/Library/Application Support/org.kde.syntax-highlighting/syntax/cute.xml` |

Pass `--force` to overwrite an existing install. `.cute` and
`.qpi` files are auto-detected by extension.

### CuteUI runtime (for `gpu_app` / GPU canvas)

```sh
cute install-cute-ui
```

Builds and installs the C++ runtime under
`~/.cache/cute/cute-ui-runtime/<version>/<triple>/`. Required
once per machine before any `cute build` of a `gpu_app`
project. Pins Qt 6.11+ (QtCanvasPainter Tech Preview) — older
Qt installs are flagged by `cute doctor`.

### AI agent skills (Claude Code / Cursor / Codex / aider / cline / ...)

```sh
cute install-skills
```

Drops a Cute-aware skill into the agent tool's config dir so
the agent knows Cute's surface rules (`pub prop` auto-derives
the NOTIFY signal, `class` defaults to QObject, no `@x` sigil)
and common gotchas (`prop` not `property`, `use qml "..."` is
required, Qt 6.9+ for `qtmochelpers.h`, …) without per-project
setup.

| Target | File | Format |
|---|---|---|
| **Claude Code** | `~/.claude/skills/cute/SKILL.md` | YAML frontmatter (`name` / `description` / `type`) |
| **Cursor** | `~/.cursor/rules/cute.mdc` | `.mdc` (`description` / `globs` / `alwaysApply`) |
| **Codex / aider / cline / agents.md tools** | `~/.codex/AGENTS.md` | plain markdown |

Activation is per-tool: Claude Code matches the skill's
`description` against your prompt context; Cursor's `.mdc`
applies on a `globs` match (defaults to `**/*.cute`); Codex
and similar `AGENTS.md`-aware tools load it at session start.
Pass `--claude` / `--cursor` / `--codex` to write to a single
target only; `--force` overwrites an existing file. No flag =
install to all three.

## Next

```sh
cute init my_app --qml      # or --widget / --server / --cli / --kirigami / --gpu
cd my_app
cute build && ./my_app
```

[Browse examples →](https://github.com/i2y/cute/tree/main/examples){ .md-button .md-button--primary }
[Read the README →](https://github.com/i2y/cute){ .md-button }
