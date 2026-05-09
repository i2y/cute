# Cute Language for Kate

Syntax highlighting for the [Cute programming
language](https://github.com/i2y/cute) in Kate, KWrite, KDevelop, and
any other editor that consumes
[KSyntaxHighlighting](https://api.kde.org/frameworks/syntax-highlighting/html/).

## Install

```sh
cute install-kate
```

This unpacks `cute.xml` (embedded in the `cute` binary at compile
time) into the per-user KSyntaxHighlighting search path:

| Platform | Path |
|---|---|
| Linux | `$XDG_DATA_HOME/org.kde.syntax-highlighting/syntax/cute.xml` (default `~/.local/share/...`) |
| macOS | `~/Library/Application Support/org.kde.syntax-highlighting/syntax/cute.xml` |

Restart Kate (or *Settings → Configure Kate → Open / Save → Modes
& Filetypes* and re-select Cute) to pick up the syntax. `.cute` and
`.qpi` files are auto-detected by extension.

Pass `--force` to overwrite an existing `cute.xml`.

## Manual install

If you'd rather drop the XML in by hand, copy
[`cute.xml`](cute.xml) into the same per-user path above:

```sh
mkdir -p ~/.local/share/org.kde.syntax-highlighting/syntax
cp extensions/kate-cute/cute.xml \
   ~/.local/share/org.kde.syntax-highlighting/syntax/cute.xml
```

## What it covers

- **Declarations** — `class` / `struct` / `arc` / `enum` / `flags` /
  `trait` / `impl` / `view` / `widget` / `style` / `fn` / `prop` /
  `signal` / `slot` / `init` / `deinit` / `let` / `var` / `test` /
  `error` / `state` / `store` / `suite` / `type`
- **Control flow** — `if` / `else` / `case` / `when` / `for` /
  `while` / `break` / `continue` / `return` / `try` / `emit` /
  `batch` / `async` / `await` / `in` / `of`
- **Modifiers** — `pub` / `extern` / `weak` / `owned` / `unowned` /
  `consuming` / `escaping` / `readonly` / `default` / `notify` /
  `constant` / `bindable` / `bind` / `fresh` / `value`
- **Constants** — `true` / `false` / `nil` / `self` / `ok` / `err` /
  `some` / `none`
- **Built-in types** — `Int` / `Float` / `Bool` / `String` /
  `ByteArray` / `Date` / `DateTime` / `Url` / `Regex` / `List` /
  `Map` / `Set` / `Hash` / `Future` / `Slice` / `ModelList` /
  `Void`, plus any `PascalCase` identifier (catches user-defined
  classes, structs, and Qt binding types)
- Comments (`# ...`), double-quoted strings, `#{...}` interpolation
  with full nested-expression highlighting, escape sequences
  (`\xNN`, `\u{...}`), `:symbol` literals, and decimal / hex /
  binary / float numerics with `_` separators

## License

MIT
