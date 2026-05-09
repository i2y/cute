# Cute — examples

Each example is a single `.cute` file compiled by `cute build` into a native
binary. **No `moc` invocation, no hand-written `main.cpp`, no `CMakeLists.txt`
written by the user** — `cute build` drives an internal cmake project so the
user's interaction is one line:

```sh
cute build app.cute    # → ./app
./app
```

## Reactive trade calculator (Q_PROPERTY BINDABLE showcase)

The `*_pnl` triplet renders the same `class Position` model — 4 bindable
inputs, 6 derived (computed) figures, 4 atomic batch presets — through three
different UI backends. They're the canonical demo of what landed with
the Qt 6 binding-system surface (`bindable`, `bind { ... }`, `batch { ... }`):

| Demo | Backend | Files |
|---|---|---|
| [`gpu_pnl/`](gpu_pnl/) | cute_ui (Qt 6.11 Canvas Painter, no QML) | `gpu_pnl.cute` |
| [`widgets_pnl/`](widgets_pnl/) | QtWidgets (`QMainWindow` / `QVBoxLayout` / `QLabel` / `QPushButton`) | `widgets_pnl.cute` |
| [`qml_pnl/`](qml_pnl/) | QtQuick.Controls / QML | `qml_pnl.cute` |

Same `class Position` in all three. Click `+$1` → `current_price` ticks; the
binding system invalidates only the dependent computed figures and re-evaluates
on the next read. Click an `AAPL` / `TSLA` / `BTC` preset → all five inputs
commit atomically inside `batch { ... }` and the screen redraws once with a
fully consistent state (no intermediate `new symbol with old price` flicker).

## `bind` vs `fresh` ([`gpu_fresh/`](gpu_fresh/))

A small cute_ui demo that puts `bind { tick * 2 }` (auto-tracked +
cached, backed by `QObjectBindableProperty.setBinding`) next to
`fresh { QDateTime.currentDateTime().toString(...) }` (function-style,
re-evaluates every read, backed by `QObjectComputedProperty`). Click `ping`
and watch the timestamp jump to wherever the wall clock is now — `fresh`
exists exactly for state Qt's binding system can't observe.

## QRangeModel-backed list view ([`qrange_model/`](qrange_model/))

`prop books : ModelList<Book>, default: [...]` exposes a
`QAbstractItemModel*` adapter (a `QRangeModel`-backed `ModelList<T>`) that
QML's `ListView` consumes directly via `model: library.books`. No
hand-written `QAbstractListModel` subclass; role names auto-derive from
`Book`'s `Q_PROPERTY`s so the delegate just reads `model.title` /
`model.author`. Backed by Qt 6.11's `QRangeModel` (the `<QRangeModel>`
include and a `MultiRoleItem` row-options specialization are emitted only
when at least one class in the module actually declares a `ModelList<T>`
prop).

## Earlier demos

- [`reading_list/`](reading_list/) — Kirigami dashboard. `prop ratio = (1.0 *
  page) / total, bind { ... }` powers the percent label without manual
  `emit`.
- [`todomv/`](todomv/) — QML Material TodoMV: ListView, add form, toggle, remove.
  49-line `.cute` source, zero hand-written C++.
- [`todomv_cli/`](todomv_cli/) — Same model classes driven from a `cli_app { ... }`
  block, no GUI link.
- [`widgets_counter/`](widgets_counter/), [`widgets_hello/`](widgets_hello/),
  [`notes/`](notes/), [`calculator/`](calculator/), [`charts/`](charts/) —
  QtWidgets via the `widget Main { QMainWindow { ... } }` DSL.
- [`gpu_notes/`](gpu_notes/), [`gpu_modal/`](gpu_modal/), [`gpu_chart/`](gpu_chart/),
  [`gpu_huge_list/`](gpu_huge_list/), [`gpu_progress/`](gpu_progress/),
  [`gpu_scroll/`](gpu_scroll/), [`gpu_svg/`](gpu_svg/), [`gpu_table/`](gpu_table/)
  — cute_ui showcases: TextField + IME, Modal, BarChart / LineChart,
  10k-row virtualized ListView, ProgressBar / Spinner, scrollbar drag, SVG,
  DataTable.
- [`kirigami_hello/`](kirigami_hello/), [`calculator_kirigami/`](calculator_kirigami/)
  — KDE Kirigami via `use qml "org.kde.kirigami"`. macOS runtime needs
  codesign + DYLD env (`cute build` handles both automatically when the
  manifest pulls `KF6Kirigami`).
- [`traits/`](traits/), [`traits_extern/`](traits_extern/) — nominal traits
  + impls (Swift/Rust style) on user classes and on extern Qt value types.
- [`two_counters/`](two_counters/) — multi-module demo: two `Counter` classes
  in different modules, instantiated side-by-side from QML.
- [`embed_demo/`](embed_demo/) — `embed("path")` compile-time asset
  embedding. `cute build` reads the file at codegen time and
  inlines its bytes as a `static constexpr unsigned char[]` in
  the generated C++. The Cute expression yields a zero-copy
  `QByteArray` (via `QByteArray::fromRawData`). Like Go's
  `//go:embed`, but folded into the type system — no separate
  `.qrc` / `rcc` pre-pass.
- [`lib_counter/`](lib_counter/) + [`lib_counter_app/`](lib_counter_app/) —
  Cute library round-trip. `lib_counter/` declares
  `[library] name = "LibCounter"` and ships a `pub class LibCounter <
  QObject` as a shared library + `.qpi` binding +
  `LibCounterConfig.cmake` (installed under
  `~/.cache/cute/libraries/`). `lib_counter_app/` consumes it via
  `[cute_libraries] deps = ["LibCounter"]` — `cute build` auto-loads
  the binding for type-check, auto-generates `find_package` + link,
  and bakes the install path into rpath, so the CLI runs without any
  DYLD env. See the README's "Sharing Cute code as libraries"
  section for the full author / consumer / `cute install` flows.

Run individual demos via `./run.sh` in each directory, or rebuild from scratch
with `cute build <name>.cute && ./<name>`. `just demos-all` from the repo root
builds every example in sequence.

## Reactive prop surface (the recent addition)

The `gpu_pnl` / `widgets_pnl` / `qml_pnl` / `gpu_fresh` / `qrange_model`
demos exercise five attributes that landed for `prop`:

| Attribute | Lowering | Use |
|---|---|---|
| `prop x : T` | bare `T m_x;` member | plain non-bindable storage |
| `prop x : T, bindable` | `Q_OBJECT_BINDABLE_PROPERTY(...)` | input fields you write to from outside |
| `prop x : T, bind { e }` | `QObjectBindableProperty` + `setBinding(lambda)` | derived value, deps auto-tracked, lazy-cached |
| `prop x : T, fresh { e }` | `Q_OBJECT_COMPUTED_PROPERTY(...)` | function-style getter, re-runs every read (file size, wall clock, third-party getters) |
| `prop xs : ModelList<T>` | `Q_PROPERTY` returning a `QRangeModel`-backed `cute::ModelList<T*>*` | exposes the collection as a `QAbstractItemModel*` for QML views |

Plus `batch { x = ...; y = ...; }` lowers to `QScopedPropertyUpdateGroup` so
multiple bindable writes commit atomically (glitch-free).

## What the compiler does

Cute's compiler (in `crates/cute-codegen/`) takes the Cute AST and emits
C++17 directly:

- **`class < QObject`** lowers to a `Q_OBJECT` C++ class with a moc-output
  equivalent (`qt_create_metaobjectdata<Tag>()` template specialization in
  Qt 6.9+ form). No `moc` runs; AUTOMOC is off.
- **`prop` + `signal`** become `Q_PROPERTY(...)` + `signals:` whose metadata
  is registered in the template specialization above. The `bindable` / `bind`
  / `fresh` modifiers extend Q_PROPERTY with `BINDABLE` / synthesized NOTIFY
  as appropriate (see the table above).
- **Class types referenced as values** render as `T*` (QObject subclasses are
  heap-only and parent-tree managed).
- **`T.new(args)`** rewrites to `new T(args)` for QObjects; subsequent
  `let item = T.new(...)` is tracked so `item.method()` becomes
  `item->method()` (pointer dispatch).
- **`Type.staticMethod(args)`** on extern value classes (`QDateTime`,
  `QColor`, …) rewrites to `Type::staticMethod(args)` so static factories
  like `QDateTime.currentDateTime()` work from `.cute` source.
- **`fn main { qml_app(...) }`** / **`fn main { cli_app { ... } }`** /
  **`fn main { gpu_app(window: Main) }`** are the compiler-recognized
  entry-point intrinsics. They wrap the standard Qt boilerplate so the user
  never writes `main.cpp`.
