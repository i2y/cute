# Cute Language for VS Code

Syntax highlighting and editor integration for the [Cute programming
language](https://github.com/i2y/cute) — a general-purpose language
dedicated to the Qt 6 / KDE Frameworks ecosystem.

## Features

- Syntax highlighting for `.cute` and `.qpi` files
- Bracket / paren / quote auto-closing
- 2-space indent default
- LSP integration via `cute-lsp` (install separately —
  `cargo install --path crates/cute-lsp` from the Cute repo)

## Setup

1. Install [`cute`](https://github.com/i2y/cute) and `cute-lsp` so the
   compiler and language server are on your `PATH`.
2. Install this extension by running:
   ```sh
   cute install-vscode
   ```
   This unpacks the extension into
   `~/.vscode/extensions/i2y.cute-language-<version>/` from files
   embedded in the `cute` binary at compile time — no clone of the
   Cute repo required. Reload VS Code (`Cmd/Ctrl+Shift+P` →
   "Reload Window") afterwards.
3. Run `cute init <project> --vscode` in a new project directory — it
   generates `.vscode/settings.json` wired to `cute-lsp` for diagnostics,
   hover, go-to-definition, completion, and format-on-save.

For an existing project, add to `.vscode/settings.json`:

```json
{
  "cute.lsp.path": "cute-lsp",
  "[cute]": {
    "editor.tabSize": 2,
    "editor.insertSpaces": true
  }
}
```

## Local development

Symlink this directory into VS Code's extension folder:

```sh
ln -s "$(pwd)/extensions/vscode-cute" \
      ~/.vscode/extensions/cute-language-dev
```

Reload VS Code (`Cmd+Shift+P` → "Reload Window"). Edits to the grammar
take effect after another reload.

## Packaging

```sh
npm install -g @vscode/vsce
cd extensions/vscode-cute
vsce package
```

Produces `cute-language-<version>.vsix`. Install with
`code --install-extension cute-language-<version>.vsix`.

## License

MIT
