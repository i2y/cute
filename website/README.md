# Cute documentation site

Source for the user-facing Cute documentation, built with
[Zensical](https://zensical.org) (the Material for MkDocs
successor by the squidfunk team).

The repo's `docs/` directory holds dev-facing specs
(`CPP_INTEROP.md`, `CUTE_UI.md`, …) — those stay where they are
and aren't part of this site.

## Layout

```
website/
├── zensical.toml              # site config
├── docs/
│   └── index.md               # home page (currently the only page)
└── README.md                  # you're reading it
```

`zensical.toml` references the local `cute-pygments` plugin from
`extensions/cute-pygments/` so ` ```cute ` fences in markdown carry
proper syntax highlighting (`prop` / `state` / `signal` / `slot` /
`store` / `suite` etc. classified as Pygments `Keyword.Declaration`,
`pub` as `Keyword.Reserved`, `bindable` / `bind` / `default` as
`Keyword.Pseudo`, and so on).

## Develop locally

The repo's `justfile` wraps the venv + install + run lifecycle:

```sh
just docs-serve     # http://localhost:8000, watches & re-renders
just docs-build     # static site → website/build/
just docs-clean     # remove build/ and .venv/
```

First run of `docs-serve` / `docs-build` calls `docs-setup` which
creates `website/.venv/` (Python 3.11 via [`uv`](https://docs.astral.sh/uv/))
and pip-installs `zensical` + the local `cute-pygments` lexer
editable. Subsequent runs reuse the venv, so iterations are
near-instant. Editable-install of `cute-pygments` means edits to
`extensions/cute-pygments/cute_pygments/__init__.py` take effect
on the next `zensical serve` restart — no reinstall needed.

If you'd rather drive everything by hand:

```sh
uv venv --python 3.11 .venv
uv pip install --python .venv/bin/python zensical
uv pip install --python .venv/bin/python -e ../extensions/cute-pygments/
.venv/bin/zensical serve
```

## Build static site

```sh
just docs-build
# or: cd website && .venv/bin/zensical build
```

Output lands in `website/build/` (per `site_dir` in
`zensical.toml`); deploy that directory to any static host.

## Pages

The home page (`docs/index.md`) is patterned after Mint's
homepage layout:

1. Hero (headline + tagline + CTA + introductory code sample)
2. Properties, signals, slots — no moc
3. Singleton state with `store`
4. One source, multiple UI backends (table)
5. Collections as item models — `ModelList<T>`
6. Pattern matching + error unions
7. Built-in test framework
8. Toolchain
9. Trivia

Additional pages (Tutorial, Reference, Install, …) are
intentionally not yet wired up; the home page links into them so
the structure is in place for follow-up content.

## Deployment

`site_url` in `zensical.toml` is set to
`https://i2y.github.io/cute/`. Wire up a GitHub Pages workflow
(or any static-site host) when ready; Zensical's
`zensical new .` scaffold generates a sample
`.github/workflows/docs.yml` that can be adapted.
