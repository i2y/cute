# Toolchain

Once `cute` is on your PATH (see [Installation](installation.md)):

| Command | Does | When to use |
|---|---|---|
| `cute init my_app [--qml\|--widget\|--server\|--cli\|--kirigami\|--gpu] [--vscode]` | scaffolds a new project under `my_app/`: `cute.toml`, `my_app.cute` (compile-ready hello-world for the chosen intrinsic), `.gitignore`, optional `.vscode/settings.json` wired to `cute-lsp`. `--gpu` sets up a `gpu_app` project (Qt 6.11 Canvas Painter, requires `cute install-cute-ui` once); `--vscode` only configures the LSP — for syntax highlighting also install the [`vscode-cute`](https://github.com/i2y/cute/tree/main/extensions/vscode-cute) extension (`just install-vscode`) | starting a new project |
| `cute build [foo.cute]` | parse → check → codegen → cmake configure + build → native binary in cwd. With no arg, picks `<dir>.cute` / `main.cute` / the single `.cute` in cwd | shipping, demos, day-to-day |
| `cute build [foo.cute] --out-dir gen/` | writes the generated `.h` + `.cpp` pair into `gen/`, skips cmake | embedding Cute output into an existing C++ project |
| `cute check [foo.cute]` | parse + resolve + type-check only; renders diagnostics, exits non-zero on error. No-arg form picks the same entry as `cute build` | pre-commit, CI fast lane, editor-on-save |
| `cute test [foo.cute]` | builds a TAP-lite runner from every `test fn` / `test "..."` / `suite "X" { ... }` in the file (or every `.cute` under cwd), runs it, exits non-zero on any failed assertion | unit / integration tests in CI |
| `cute fmt [foo.cute]` | rewrites the file in canonical form (preserves comments at item / member / stmt boundaries). No-arg form recursively formats every `.cute` under cwd, skipping `target/` / `.git/` / `.vscode/` etc. | format-on-save, project-wide cleanup |
| `cute fmt --check [foo.cute]` | exit 0 if already canonical, 1 otherwise — list every offender with no arg | CI |
| `cute install-skills [--claude] [--cursor] [--codex]` | drops a Cute-aware skill into `~/.claude/skills/cute/SKILL.md`, `~/.cursor/rules/cute.mdc`, and/or `~/.codex/AGENTS.md` so AI coding agents know Cute syntax + gotchas. No flag = install to all three | one-time per machine setup |
| `cute install-vscode` | unpacks the bundled VS Code extension into `~/.vscode/extensions/i2y.cute-language-<version>/` (TextMate grammar + language config + metadata, embedded in the binary at compile time). Reload VS Code after install. Pass `--force` to overwrite | one-time per machine setup |
| `cute install-kate` | unpacks the bundled Kate / KSyntaxHighlighting syntax definition (`cute.xml`) into `$XDG_DATA_HOME/org.kde.syntax-highlighting/syntax/` (Linux) or `~/Library/Application Support/org.kde.syntax-highlighting/syntax/` (macOS). Restart Kate / KWrite / KDevelop after install. Pass `--force` to overwrite | one-time per machine setup |
| `cute install-cute-ui [--source <dir>] [--prefix <dir>]` | builds and installs the cute_ui runtime (Qt Canvas Painter) under `~/.cache/cute/cute-ui-runtime/<version>/<triple>/`; required before `cute build` of any `gpu_app` project. `--source` / `--prefix` override the bundled source tree and install location for unusual layouts | one-time per machine setup |
| `cute doctor [foo.cute]` | enumerates every Qt 6 / KF 6 / CuteUI module the build needs, checks each against the local install, prints the platform-specific install command for anything missing (single-line `brew install qt` on macOS; `sudo zypper`/`dnf`/`apt`/`pacman ...` on Linux). No-arg form picks the same entry as `cute build` | first-time setup on a new machine, post-`find_package` failure |
| `cute watch [foo.cute]` | builds + spawns the binary, then watches the source's directory for `.cute` / `cute.toml` edits and auto-rebuilds + relaunches. Not in-process hot reload (state isn't preserved), but tightens the edit/run loop to 2–3 s | active dev loop |
| `cute parse foo.cute --emit ast` | dump the AST, useful for compiler debugging | hacking on Cute itself |
| `cute lex foo.cute` | dump the token stream | same |
| `cute-lsp` | stdio Language Server Protocol implementation | wire into your editor for diagnostics + hover + go-to-definition + completion + format |

## Editor LSP

The LSP supports both single-file and multi-file projects (it
walks `cute.toml` + transitive `use foo` to build the dependency
graph), with cross-file go-to-definition and member completion.
Editor wiring: launch `cute-lsp` as the LSP for `.cute` files;
it speaks JSON-RPC over stdin/stdout. `cute init --vscode`
generates a project-local `.vscode/settings.json` that does this
for VS Code; for other editors see [Installation](installation.md#editor-lsp)
and your editor's LSP-client docs.

**VS Code extension**: `cute install-vscode` writes the bundled
[`vscode-cute`](https://github.com/i2y/cute/tree/main/extensions/vscode-cute)
extension (TextMate grammar + language configuration + metadata)
into `~/.vscode/extensions/i2y.cute-language-<version>/`. The
files are embedded in the `cute` binary at compile time so the
command is self-sufficient. Reload VS Code afterwards
(Cmd/Ctrl+Shift+P → "Reload Window").

If you're hacking on the extension itself, `just install-vscode`
(from a repo clone) symlinks `extensions/vscode-cute/` straight
into `~/.vscode/extensions/cute-language-dev` so edits take
effect after a Reload Window without re-running install. To
produce a distributable `.vsix`, run `just package-vscode`
(requires [`vsce`](https://github.com/microsoft/vscode-vsce)).

**Kate / KSyntaxHighlighting**: `cute install-kate` writes the
bundled
[`kate-cute/cute.xml`](https://github.com/i2y/cute/tree/main/extensions/kate-cute)
syntax definition into the per-user KSyntaxHighlighting search
path. Picks up across Kate, KWrite, KDevelop, and any other
KDE editor that links the framework. See
[Installation › Kate syntax highlighting](installation.md#kate-syntax-highlighting--kate-cute)
for path details.

## AI agent integration

```sh
cute install-skills          # writes to all three targets at once
```

Each target's file is plain Markdown wrapped in the appropriate
frontmatter:

| Target | File | Format |
|---|---|---|
| Claude Code | `~/.claude/skills/cute/SKILL.md` | YAML frontmatter (`name` / `description` / `type`) |
| Cursor | `~/.cursor/rules/cute.mdc` | `.mdc` (`description` / `globs` / `alwaysApply`) |
| Codex / aider / cline / agents.md tools | `~/.codex/AGENTS.md` | plain markdown |

The body covers the common AI mistakes when writing Cute
(`prop` not `property`, `use qml "..."` is required, **`pub`
keyword** for cross-module / cross-class visibility, plain
`pub prop X : T, default: V` auto-derives the `<X>Changed` notify
+ signal — no manual `notify:` / `pub signal` / `emit`, no `@x`
sigil — bare member names auto-resolve in class methods), plus
the Qt 6.9+ requirement, memory model, app intrinsics, and
toolchain. Pass `--force` to overwrite existing files.
