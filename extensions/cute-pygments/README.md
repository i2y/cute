# cute-pygments

Pygments lexer for the [Cute](https://github.com/i2y/cute)
programming language. Registers `cute` as a `pygments.lexers`
entry-point, so any tool that delegates to Pygments — Material
for MkDocs / [Zensical](https://zensical.org) /
[Sphinx](https://www.sphinx-doc.org) / `pymdownx.highlight`
markdown processors / `pygmentize` CLI — picks it up
automatically.

## Why

Off-the-shelf highlighters (Ruby / Rust / Swift / Vala / …) all
miss the Cute-specific keywords (`prop`, `state`, `signal`,
`slot`, `store`, `suite`, …). With this lexer installed the full
keyword set carries the right Pygments token classes and
documentation builders render Cute samples with the same
quality as their built-in languages.

## Install

From the Cute repo root, in whatever environment your docs
toolchain runs in (a Zensical / mkdocs-material venv, a Sphinx
project's requirements, your local CLI, …):

```sh
pip install -e extensions/cute-pygments/
```

Editable mode means edits to `cute_pygments/__init__.py` take
effect on the next process start; reinstall isn't needed for
live iteration. Drop the `-e` for a regular install once the
lexer stops moving.

## Verify

```sh
pygmentize -L lexer | grep cute       # → "* cute: Cute (filenames *.cute)"
pygmentize -l cute -f raw foo.cute    # → token classes
pygmentize -l cute -f html -O nowrap foo.cute   # → <span class="kd">prop</span>...
```

## Use in markdown

After installing in the docs build environment, fence any Cute
sample with `cute` and the renderer carries the highlighting:

````markdown
```cute
class Counter {
  pub prop count : Int, default: 0
  pub fn increment { count = count + 1 }
}
```
````

The renderer's Pygments theme decides which token class lands on
which colour. Material for MkDocs / Zensical ship a couple of
defaults; their docs cover swapping themes.

## Token map

| Cute construct | Pygments token | Material theme colour (default) |
|---|---|---|
| `class` `struct` `arc` `enum` `flags` `error` `trait` `impl` `view` `widget` `style` `store` `suite` `test` `fn` `prop` `signal` `slot` `init` `deinit` `let` `var` `state` | `Keyword.Declaration` (`kd`) | bold |
| `pub` `extern` `escaping` `consuming` `weak` `owned` `unowned` `readonly` | `Keyword.Reserved` (`kr`) | bold |
| `if` `else` `case` `when` `match` `for` `while` `break` `continue` `return` `try` `emit` `batch` `async` `await` | `Keyword` (`k`) | bold |
| `bindable` `bind` `fresh` `notify` `default` `constant` `model` `of` (contextual) | `Keyword.Pseudo` (`kp`) | bold italic |
| `use` `import` (incl. `use qml "…"`) | `Keyword.Namespace` (`kn`) | bold |
| `Int` `Float` `Bool` `String` `Void` `Self` `List` `Map` `Slice` `ModelList` `Future` `Result` | `Keyword.Type` (`kt`) | colour-2 |
| PascalCase identifiers (user types) | `Name.Class` (`nc`) | colour-3 |
| `true` `false` `nil` | `Keyword.Constant` (`kc`) | colour-4 |
| `self` | `Name.Builtin.Pseudo` (`bp`) | italic |
| `:foo` symbol literals | `String.Symbol` (`ss`) | colour-5 |
| `# comment` | `Comment.Single` (`c1`) | grey italic |
| `"…"` strings, `\x..` / `\u{…}` escapes | `String` (`s`) + `String.Escape` (`se`) | colour-6 |
| `#{ expr }` interpolation | `String.Interpol` (`si`) (markers) + nested root tokens | mixed |
| Numbers (`123`, `3.14`, `0xFF`, `0b101`) | `Number.{Integer,Float,Hex,Bin}` | colour-7 |

`value` is intentionally NOT in the modifier keyword list —
it's contextual only after `extern` (`extern value Foo { … }`)
and clashes with the common `value` identifier name elsewhere.
The leading `extern` is enough to mark the surrounding form.

## Upstream submission

The Pygments project accepts community-contributed lexers via
PR; the standard process is documented at
<https://pygments.org/docs/lexerdevelopment/>. Once submitted +
landed there, GitHub's [linguist](https://github.com/github-linguist/linguist)
can register `Cute` as a recognised language and ` ```cute ` 
fences will render with proper colouring directly on
github.com / GitLab / etc.

Until that happens, this plugin works for any local /
self-hosted documentation pipeline that uses Pygments — which
covers Zensical, Material for MkDocs, Sphinx, Hugo's chroma
fallback, and most others.

## License

MIT OR Apache-2.0. Same as the parent Cute repository.
