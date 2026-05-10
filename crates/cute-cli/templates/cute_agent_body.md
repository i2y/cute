
# Cute language — agent guide

Cute compiles `.cute` source to C++ + a `QMetaObject` data table, then drives `cmake` to produce a native Qt 6 binary. **It does not invoke `moc`** — Cute generates the Q_OBJECT-equivalent metadata directly via Qt 6.9's `qt_create_metaobjectdata` template specialization.

## Hard requirements

- **Qt 6.9 or newer.** Qt 6.4–6.8 lack `QtCore/qtmochelpers.h`; cmake build fails with "fatal error: QtCore/qtmochelpers.h: No such file or directory". Distros: Ubuntu 24.04 LTS Qt 6.4 ❌, Debian trixie Qt 6.7 ❌, openSUSE Tumbleweed Qt 6.11 ✅, Arch Qt 6.11 ✅, macOS via KDE Craft Qt 6.11 ✅. (`gpu_app` / CuteUI mode pins Qt 6.11+ for QtCanvasPainter.)
- **Rust 1.85+** to build the compiler.
- **CMake ≥ 3.21.**
- **`just`** for the canonical install recipe.

## Installing the compiler

There is no `brew install cute` / curl-installer / AUR package yet — distribution channels are on the v1.x roadmap. Until then, `just install` from a clone is the supported route on every platform:

```sh
git clone https://github.com/i2y/cute && cd cute
just install        # release build → ~/.cargo/bin/cute (= cargo install --path crates/cute-cli --force)
```

Then verify the runtime deps with:

```sh
cute doctor
```

`cute doctor` enumerates the Qt6 / KF6 / CuteUI modules a build needs, checks each against the local install, and prints the platform-specific install command for anything missing (single `brew install qt` on macOS via Homebrew's umbrella formula; one `sudo zypper`/`dnf`/`apt`/`pacman` line on Linux for Tumbleweed / Fedora / Debian-Ubuntu / Arch). When you see a `find_package(Qt6) Could NOT be found` from `cute build`, run `cute doctor` first — its output is what the user actually needs to copy-paste, not the raw CMake error.

For Kirigami / KF6 builds on macOS the prerequisite is KDE Craft (`craft qtbase qtdeclarative qthttpserver qtcharts kirigami` into `~/CraftRoot`), not Homebrew — `cute doctor` flags this and points at the Craft bootstrap. Optional follow-ups: `cargo install --path crates/cute-lsp` for editor LSP; `cute install-cute-ui` for the `gpu_app` runtime.

## Source-shape gotchas (VERY common AI mistakes)

These four are the things AI assistants get wrong most often when writing Cute. Internalize them before producing code.

### 1. The keyword is `prop`, not `property`

```ruby
class Counter {
  pub prop count : Int, default: 0   # ✅
  pub property count : Int           # ❌ parse error
}
```

`property` was removed. The lexer rejects it. Plain `pub prop X : T, default: V` auto-derives a `<X>Changed` NOTIFY signal — no manual `notify:` / `pub signal` lines needed (rule 4 below covers the details).

### 2. `use qml "..."` is REQUIRED for every QML module

The auto-injected QtQuick / QtQuick.Controls / Material / Layouts imports were dropped (commit 71ccc23). Every demo with a `view` block declares them at the top of the file:

```ruby
use qml "QtQuick"
use qml "QtQuick.Controls"
use qml "QtQuick.Controls.Material"   # for Material colors / accent
use qml "QtQuick.Layouts"              # for HBox / VBox / Layout.fillWidth
use qml "org.kde.kirigami" as Kirigami # for Kirigami.ApplicationWindow

view Main { ApplicationWindow { ... } }
```

Without these the generated `.qml` has no `import` lines and **fails at QML runtime, not at compile time** — `cute check` and `cute build` both succeed but the app dies with "ApplicationWindow is not a type" on launch.

### 3. Visibility is the **`pub` keyword** + Qt camelCase / PascalCase convention

```ruby
pub class Counter {
  pub prop count : Int, default: 0     # exported, reactive (auto NOTIFY = countChanged)
      prop secret : Int, default: 0    # private (no `pub`) — module / class internal

  pub fn increment { count = count + 1 }   # exported method (camelCase)
      fn helper    { ... }                 # private helper

  pub signal clicked                       # transient signal (not a prop's notify pair)
      signal internalDone                  # internal-only transient signal
}
```

Rules:

- **`pub`** on the declaration controls export. No `pub` = visible only inside the declaring module / class.
- **Type names** (class / struct / arc / extern value / trait): **PascalCase** (`Counter`, `LibCounter`, `Position`).
- **Member / method / signal / slot / fn names**: **camelCase** (`countChanged`, `nudgePrice`, `applyBull`) — Qt convention. The leading letter does NOT control visibility.
- The view body `Label { text: counter.count }` is fine even though `count` starts lowercase, as long as `count` is `pub` on the class. Same for `Button { onClicked: counter.increment() }`.

`pub` is also the toggle for top-level decls — `pub class Foo` makes `Foo` reachable from other modules; bare `class Foo` keeps it private to the declaring file.

### 4. NOTIFY + change-signal are auto-derived; bare member names; auto-emit

```ruby
class Counter {
  pub prop count : Int, default: 0   # auto NOTIFY = countChanged + signal injected
  pub fn increment {
    count = count + 1                # bare LHS → setCount(...) → auto emit countChanged
  }
}
```

Plain `pub prop X : T, default: V` synthesizes a conventional `<X>Changed` NOTIFY signal AND injects the matching signal declaration into the metaobject. The setter does the dirty-check and fires the signal on every value change. **Don't write `, notify: :countChanged` + `pub signal countChanged` for the common case** — they're redundant. **Don't write `emit countChanged` after `count = ...`** — the setter already fired it; a manual `emit` would fire it twice (a v1 lint warns).

Bare names inside class methods auto-resolve to the member: `count` reads `m_count` directly, `count = expr` rewrites to the auto-generated setter call. (`@x` is not valid syntax — write the bare name.)

Three escape hatches when the auto-derive doesn't fit:

- `pub prop x : T, notify: :customName` — explicit signal name. Pair with a `pub signal customName` declaration if you want to attach parameters; the auto-injection only adds the signal when no `pub signal` matches.
- `pub prop x : T, default: V, constant` — opts out entirely. Lowers to a Qt CONSTANT Q_PROPERTY: settable in the constructor only, no NOTIFY, no signal. QML treats reads as one-shot.
- `pub prop x : T, bindable` / `bind { ... }` / `fresh { ... }` — Qt 6 binding-system shapes (auto-NOTIFY also applies, just with QObjectBindableProperty / QObjectComputedProperty storage).

External writes (`counter.count = 42` from outside the class) go through the same setter and auto-emit.

### 5. `state X : T = init` — view-local reactive cell, no class needed

Inside a `view` / `widget` body's head section (sibling to `let counter = Counter()`), `state X : T = init` declares a primitive reactive cell — Cute's analogue of SwiftUI's `@State`. No wrapper class, no signal, no manual emit.

```ruby
view Main {
  state count : Int = 0

  ApplicationWindow {
    Label  { text: "count: " + count }
    Button { text: "+1"; onClicked: { count = count + 1 } }
    Button { text: "reset"; onClicked: { count = 0 } }
  }
}
```

Same surface in QtWidgets (`widget Main { state count : Int = 0 ; QMainWindow { ... } }`) and cute_ui's GPU path (`widget Main { state count : Int = 0 ; Column { ... } }`). The lowering is target-specific — QML view emits a root-level `property int count: 0`; widget / cute_ui desugar into a synthesized `__<Widget>State < QObject` class (so the existing reactive-binding emitter / cute_ui rebuild loop pick the auto-derived NOTIFY up uniformly). For primitive cells (Int / Double / Bool / String / Url / Color / `var`-fallback) that don't need to be reused across views, prefer `state` over a wrapper class.

### 6. `store X { ... }` — process-lifetime singleton (Mint-inspired)

For global app state that lives outside any view / widget — current
user, theme, navigation routes — declare a `store`. One block
collapses class declaration, singleton instantiation, and per-member
`pub`:

```ruby
store Application {
  state user : User? = nil
  state theme : Theme = Theme.Light

  fn login(u: User) { user = u }
  fn logout         { user = nil }
}
```

Anywhere in the program, `Application.user` / `Application.login(u)`
works — bare `Application` references the synthesized
`Q_GLOBAL_STATIC` accessor (`Application()->user()` in C++). `state X
: T = init` inside the body lowers to `prop X : T, notify: :XChanged,
default: init` + `signal XChanged` so QML / widget bindings on
`Application.X` re-render reactively when X mutates. Every member is
forced `pub`. Object-kind state fields (`let X = sub_obj()`) are
rejected — singletons have no parent-injection lifetime model; use
`state` for primitive cells or write the init in `init { ... }`.

Use `store` instead of `let APP : Application = Application.new()` +
hand-written `class Application` whenever there's exactly one
instance for the process. The let-form pattern is still valid (and
useful when the type is reused with multiple instances elsewhere),
but `store` is the recommended shape for true singletons.

### 7. `static fn` — class-scoped static methods

Many Qt / KF6 APIs are static: `QTimer.singleShot(msec, callback)`,
`QCoreApplication.quit()`, `KSharedConfig.openConfig(...)`,
`KLocalizedString.setApplicationDomain(...)`. Declare them with
`static fn` in the class body; call sites use `ClassName.method(args)`
without an instance receiver.

```ruby
class Counter {
  pub prop count : Int, default: 0

  pub static fn loaded(initial: Int) Counter {
    let c = Counter()
    c.count = initial
    c
  }
}

# Cute-side
let c = Counter.loaded(42)

# Binding-side (`static fn` declared in the .qpi)
QTimer.singleShot(500) { println("hi") }
QCoreApplication.quit()
```

Lowering: `T.method(args)` → `T::method(args)` in C++. The header emits
`static {ret} {name}(...)` instead of the default `Q_INVOKABLE`.
`static fn` bodies have NO `self` — instance fields / props / methods
aren't reachable. Coexists with same-named `prop` / instance method
(Qt's `QTimer` exposes both `Q_PROPERTY(bool singleShot ...)` and the
static `singleShot(int, Functor)`; the type checker dispatches by
receiver kind — `t.singleShot = true` is the prop write,
`QTimer.singleShot(500) { ... }` is the static call).

The type checker rejects mismatched call forms upfront:
`someTimer.singleShot(500) {...}` (static called on instance) and
`QTimer.start()` (instance method called via type) both produce a
directional error rather than passing through to a C++ compile failure.

## Tests — `suite "X" { test "y" { ... } }` and `test fn`

Cute has a built-in test runner. Two surface forms:

```ruby
# Compact form — name is an identifier. Right tool when a snake_case
# name says it.
test fn addition {
  assert_eq(2 + 2, 4)
}

# Free-form-string-named, optionally grouped under a suite. Display
# label in TAP output is `<suite> / <test>`.
suite "compute" {
  test "adds positive numbers" {
    assert_eq(compute(3.0, "+", 4.0), 7.0)
  }
  test "handles zero divisor as zero" {
    assert_eq(compute(1.0, "/", 0.0), 0.0)
  }
}
```

`cute test foo.cute` (or `cute test` to walk cwd) builds a TAP runner
that prints `1..N` then one `ok N - <label>` / `not ok N - <label>:
<reason>` per test. Failed assertions surface their
`actual=… expected=… at <file>:<line>` directly in the line. Inside a
suite body, only the string-named form is accepted (compact-form
tests declare at top level alongside the suite). Nested suites,
`pub suite`, and `pub test` are rejected. `before` / `after` hooks
are deferred to v1.y; for shared setup / teardown today, call helper
fns from each test or reset `store` state explicitly.

Asserts available out of the box: `assert_eq(actual, expected)`,
`assert_neq`, `assert_true`, `assert_false`, `assert_throws { body }`.

## Memory model — Qt-native, three forms

| Cute | Lifetime | Generated C++ |
|---|---|---|
| `class X { ... }` (default super = `QObject`) | Qt parent-tree (auto `new T(this)` in class methods) | raw `T*` |
| `class X < QSomething { ... }` (explicit super) | Same as above, but inherits from any QObject-derived Qt class (`QPlainTextEdit`, `QAbstractListModel`, …) | `class X : public QSomething` + Q_OBJECT |
| `arc X { ... }` | Intrusive ARC (final, no signals/slots) | `class X : public ::cute::ArcBase` + `cute::Arc<T>` |
| `T?` | Auto-nulls when target dies | `QPointer<T>` |
| `extern value Foo { ... }` (binding-only) | Stack value | bare `Foo` (e.g., `QPoint`, `QColor`) |
| `String` / `List<T>` / `Map<K,V>` | Qt implicit-shared (COW) | `QString` / `QList<T>` / `QMap<K,V>` |
| `Slice<T>` | shared-pointer keepalive — non-dangling array view | `cute::Slice<T>` (`arr[a..b]` lowers to `make_slice`) |

`T.new(args)` inside a class method auto-injects `this` as Qt parent. Outside, the caller must explicitly parent or risk a leak.

`class X < QPlainTextEdit { let hl : CodeHighlighter = CodeHighlighter.new(self) }` is a working pattern for "subclass a Qt widget, own a helper as a member field" — used by `examples/code_highlight`. The Qt umbrella header from the build mode (`<QtWidgets>` for widget mode, `<QtQuick>` for QML) reaches the super class without manual includes.

## Type system — Qt is the standard library

`String` IS `QString` (no wrapper). `List<T>` IS `QList<T>`. `Future<T>` IS `QFuture<T>`. `Slice<T>` IS `cute::Slice<T>`. Method names follow Qt conventions: **`isEmpty` not `is_empty`**, `length` (or `size`), `mid(start)` not `slice` for List<T>, etc. The `.qpi` binding files in `stdlib/qt/` are the source of truth.

### Slicing — `arr[a..b]` returns `Slice<T>`

`arr[1..3]` (exclusive) and `arr[1..=3]` (inclusive) lower to a `cute::Slice<T>` value backed by `std::shared_ptr<QList<T>>`. The slice can be returned from a fn, stored in a struct field, indexed (`s[i]`), iterated (`for x in s`), and length-queried (`s.length`). Sub-slicing (`s[0..2]`) is zero-copy. The shared backing keeps the source alive, so a slice never dangles — pass slices around without lifetime annotations.

```ruby
fn sum(xs: Slice<Int>) Int {
  var total: Int = 0
  for x in xs { total = total + x }
  total
}

let arr: List<Int> = [1, 2, 3, 4, 5]
let s = arr[1..4]              # Slice<Int>, length 3
println(sum(arr[1..4]))        # 9
println(s[0])                  # 2
let sub = s[0..2]              # zero-copy sub-slice
```

The current scope is read-only views (mutating the source through a slice is planned for a future release). String / ByteArray slicing is not auto-wired yet.

### Range loops use `std::views::iota`

`for i in 0..N` / `for i in 0..=N` lower to `for (qint64 i : std::views::iota(s, e))`. `Slice<T>` iteration uses range-based for too, so both share the same C++20 ranges path.

## Traits and impls

Cute has nominal traits (Swift `protocol` / Rust `trait` style) with retroactive `impl Trait for Type`. Trait declarations have no runtime cost — dispatch is monomorphized at C++ template instantiation time.

```ruby
trait Summarizable {
  fn summary String                         # required (no body)
  fn loudSummary String { "(no summary)" }  # default body, optional override
}

class Person {
  pub prop name : String, default: ""
}

impl Summarizable for Person {
  fn summary String { "person: #{self.name()}" }
  # loudSummary omitted → inherits default
}

# Generic-bound fn: body can call any method declared on the trait.
fn announce<T: Summarizable>(thing: T) { println(thing.summary()) }
```

### Surface rules to internalize

- **`impl` body methods follow the same `pub` rule** as plain class members. Add `pub fn summary String { ... }` so `announce(p)` can dispatch into it from another module.
- **Receiver is lowercase `self`** (the Cute keyword), not `Self`. `Self` (capital S) is a *type* placeholder used in trait method signatures only.
- **`Self` substitutes** to the bound `T` in generic-bound bodies and to the impl's for-type at impl emission. So `trait Cloneable { fn cloned Self }` lets you write `pub fn dup<T: Cloneable>(t: T) T { t.cloned() }`.
- **Default-bodied trait methods** are inherited by impls that omit them — no need to copy them in. Override by declaring the same method name in the impl.
- **`impl` itself has no visibility modifier**: reach is decided by the trait + for-type's own visibility.

### Where impl works

| For-type | Works? | Notes |
|---|---|---|
| `class X { ... }` (user) | ✅ | impl methods are spliced into the class body |
| `arc X<T> { ... }` (user generic) | ✅ | parametric impl `impl<T> Foo for X<T>` lands on the class template |
| `extern value Y` (QPoint, QColor, …) | ✅ | emits as `cute::trait_impl::Trait::method(Y& self, …)` namespace overload |
| Builtin generic (`List<T>`, `Map<K,V>`) | ✅ | parametric impl emits a templated namespace overload |
| Qt binding class (`QStringList` …) | ✅ | namespace overload (no class body to splice) |

### Specialization rule

`impl<T> Foo for List<T>` and `impl Foo for List<Int>` both work — the concrete one wins when both are candidates (C++ overload resolution at the namespace dispatch picks the most specific). Same for `extern value` types and binding classes.

For **user generic classes** the splice path can only carry one definition per method, so parametric + concrete overlap is rejected. Two parametric impls on the same base are also rejected (ambiguous; would cause C++ ODR violations).

### Common pitfalls

- Calling a trait method on a generic-bound `T` whose method-level generics need explicit type args (e.g. `fn mapTo<U>(f: fn(Int) -> U) U`) used to fail — now it doesn't, the type-checker records inferred `U` and codegen emits `<X>` at the dispatch automatically. No user action needed.
- `impl Foo for QPoint` emits `Foo::method(QPoint& self, …)` (value-flavored). If you mistakenly read it as expecting a pointer (because Qt usually deals in pointers), remember `extern value` types are pass-by-value/reference, not pass-by-pointer.
- `Self` in a trait return type is fine; `Self` as a *standalone variable* is not (it's a type, not an expression). Use `self` (lowercase) for the receiver.

## App intrinsics — pick one per `fn main`

| Intrinsic | Wraps body in | Use for |
|---|---|---|
| `qml_app(view: Main)` (auto-synthesized when a `view` exists) | `QGuiApplication` + `QQmlApplicationEngine` + qrc-bundled QML | QtQuick / Material / Plasma 6 / Kirigami GUI |
| `widget_app(window: Main)` (auto-synthesized when a `widget` exists with a Qt root) | `QApplication` + `Main w; w.show();` | QtWidgets / OS-native GUI |
| `gpu_app(window: Main, theme: light)` | `cute::ui::App` + `cute::ui::Window` (QRhi + Canvas Painter) | GPU-accelerated UI without QML or QtWidgets — see GPU section below |
| `server_app { ... }` | `QCoreApplication` + body + `app.exec()` | HTTP servers, signal/timer-driven services, **streaming HTTP clients** (`QNetworkAccessManager` + `readyRead` chunks) — see `examples/llm_chat` |
| `cli_app { ... }` | `QCoreApplication` + body + `return 0;` (sync body) **OR** body lifted into `QFuture<void>` coroutine + `app.exec()` (when body uses `await`) | CLI tools — async-aware |
| (none) | bare `int main(...)` | batch processing |

`Qt6::Network` is auto-linked when the generated C++ references
`QNetworkAccessManager` / `QNetworkRequest` / `QNetworkReply` — no
`cute.toml [cmake]` entry is needed for the typical HTTP client.
Same for `Qt6::Gui` + `Qt6::Widgets` when a `cute::CodeHighlighter`
shows up.

## QML view body sugar

```ruby
Row    { ... }   # → QML's Row (sequential, no flex)
Column { ... }   # → QML's Column
Grid   { ... }   # → QML's Grid
HBox   { ... }   # → RowLayout    (flex; needs `use qml "QtQuick.Layouts"`)
VBox   { ... }   # → ColumnLayout (flex)
HGrid  { ... }   # → GridLayout   (flex)
```

Use `HBox` / `VBox` when you need `Layout.fillWidth: true` / `Layout.fillHeight: true` flex behavior. Use `Row` / `Column` for the QtQuick non-flex variants.

## GPU-accelerated UI (`gpu_app` + cute_ui runtime)

A third UI substrate for `widget` declarations: when the root container is one of cute_ui's classes (`Column` / `Row` / `Stack` / etc.), codegen lowers the widget to a `cute::ui::Component` subclass instead of a QtWidgets class, and `fn main { gpu_app(window: Main) }` boots a custom QRhi + Qt 6.11 Canvas Painter render loop. No QML, no V4, no QtWidgets, no `moc`.

```ruby
widget Main {
  state count : Int = 0
  Column {
    Text { text: "Count: #{count}" }
    Button { text: "+1"; onClick: { count = count + 1 } }
  }
}

fn main { gpu_app(window: Main, theme: light) }
```

(Or with a wrapper class when state is shared across widgets:)

```ruby
class Counter {
  pub prop count : Int, default: 0          # auto NOTIFY = countChanged
  pub fn increment { count = count + 1 }    # bare LHS auto-emits via setter
}

widget Main {
  let counter = Counter()
  Column {
    Text { text: "Count: #{counter.count}" }
    Button { text: "+1"; onClick: counter.increment() }
  }
}
```

### Hard requirements (gpu_app only)

- **Qt 6.11+** — Canvas Painter is new there; older Qt versions don't ship `QtCanvasPainter/QCanvasPainter`.
- **macOS:** `brew install qtcanvaspainter` until KDE Craft picks the module up.
- **`cute install-cute-ui`** builds and installs the C++20 runtime as a static lib under `~/.cache/cute/cute-ui-runtime/<version>/<triple>/` (the workspace-local fallback `runtime/cute-ui/install/` is also probed). Run it once per machine before any `cute build` of a gpu_app project.

### Available cute_ui widgets

| Element | Purpose |
|---|---|
| `Column` / `Row` / `Stack` | Yoga flexbox containers |
| `Text` | Themed label |
| `Button` | Click target with hover + press color tweens, `onClick` |
| `TextField` | Editable text with caret blink, selection, copy/paste, IME, focus ring tween, `onTextChanged: s.update(text)`, Tab / Shift+Tab traversal |
| `Image` | `QImage` rendered via QCanvasImage; `source`, `width`, `height` |
| `Svg` | `QSvgRenderer`-rasterized vector icon; `source`, `width`, `height` |
| `ListView` / `ScrollView` / `HScrollView` | Scrollable container (vertical or horizontal); wheel + scrollbar thumb + ~120 ms scroll inertia. ListView optionally `virtualized: true` + `itemHeight: 24.0` for 10k+ row lists |
| `DataTable` | Vertical container; first Row = header (stronger bg + thicker bottom divider), subsequent Rows alternate stripe |
| `Modal` | Full-window dim overlay + centered rounded surface; clicks outside the dialog are swallowed |
| `BarChart` / `LineChart` | `data: List<Float>`, `labels: List<String>`; bars / line eased on data swap |
| `ProgressBar` | `value: Float` 0..1, eased fill |
| `Spinner` | Indeterminate accent arc, 1 rev/sec |

`gpu_app` accepts `theme: dark` or `theme: light` (default dark). `Cmd / Ctrl + T` toggles live with a 250 ms crossfade. Demos: `examples/gpu_notes`, `gpu_modal`, `gpu_svg`, `gpu_table`, `gpu_scroll`, `gpu_chart`, `gpu_progress`. Architecture deep-dive: `docs/CUTE_UI.md`.

### Stateful / Stateless model (Flutter-style)

Unlike QtWidgets / QML, cute_ui has the same Component / Element split as Flutter or Castella:

- The `widget Foo { ... }` you write becomes a **`Component`** (StatefulWidget equivalent). Its state lives on `Component` as `prop` / state-field and is preserved across rebuilds.
- The `Column { ... }` tree returned from `build()` is a fresh **`Element`** tree on every rebuild. Codegen wires every state-field signal to `requestRebuild()` automatically — no manual `setState`.
- Transient state (caret position, scroll offset, button press / hover / focus tweens, chart animation buffers) survives a rebuild via `transferStateRecursive`'s positional walk. Reordered children lose state; this matches Flutter's "no key" behavior.

So `let s = Store()` inside `widget Main` is the equivalent of `final s = Store()` in Flutter's `State<Main>`. Mutating `s` via a method that fires its notify signal triggers exactly one rebuild on the next event-loop tick.

### gpu_app gotchas AI assistants hit

- **Don't confuse cute_ui's `Column` / `Text` / `Button` with the QML ones.** A `Column { spacing: 12 }` style QML attribute does not exist on cute_ui's Column — its layout comes from Yoga + the element's preferredSize. Inside `widget Main { Column { ... } }` of a gpu_app project, every child is a cute_ui Element constructed through the runtime DSL.
- **List / Map literals lower differently in widget body vs slot body.**
  - In a widget body's property position (`labels: ["Mon", "Tue", ...]`) the list literal becomes C++ brace-init `{a, b, c}` and the receiving setter's type (`QStringList`, `QList<qreal>`, ...) drives deduction. This works.
  - In a slot body, `values = [1.0, 2.0]` against a typed `prop values: List<Float>` lowers to `QList<double>{1.0, 2.0}` directly — no more `QVariantList` workaround. Same for `m = { a: 1 }` against `prop m: Map<String, Int>` → `QMap<::cute::String, qint64>{{QStringLiteral("a"), 1}}`. The hint is **recursive**: `Map<String, List<Int>>` with `{ xs: [1,2,3] }` propagates `List<Int>` into the inner array, and `List<List<Int>>` / `List<Map<String, Int>>` combinations work the same way. The same LHS-driven hint applies to `let xs : List<Int> = [...]` / `let m : Map<String, Int> = {...}`.
  - Untyped `let xs = [...]` / `let m = {...}` still lowers to `QVariantList` / `QVariantMap` (no element-type to infer).
  - **Map keys must be identifiers or string literals** — keys are always lowered as `QString`, so `Map<Int, V>` literal-construction isn't supported yet (use `m.insert(k, v)` for non-string keys).
- **Multi-arg slots are fine again** since `540eee4`. `fn add(name: String, age: Int, city: String)` works; the qt_gotchas note that capped them was lifted.
- **`onClick` not `onClicked`** in cute_ui. (`onClicked` is QML's signal-handler convention; cute_ui's Button uses `onClick:` to match the JS convention used elsewhere in the runtime.)

## Toolchain

| Command | What it does |
|---|---|
| `cute init <name> [--qml\|--widget\|--server\|--cli\|--kirigami\|--gpu] [--vscode]` | Scaffolds a new project with a working hello-world entry, `cute.toml`, `.gitignore`, optional `.vscode/settings.json`. `--gpu` sets up a `gpu_app` project (Qt 6.11 Canvas Painter; needs `cute install-cute-ui`). |
| `cute build foo.cute` | Full pipeline: parse → check → codegen → cmake configure + build → native binary in cwd. |
| `cute build foo.cute --out-dir gen/` | Writes `.h` + `.cpp` only; skips cmake. For embedding into existing C++ projects. |
| `cute check foo.cute` | Parse + resolve + type-check; renders diagnostics; non-zero exit on errors. Fast — no cmake. |
| `cute test [foo.cute]` | Builds a TAP-lite runner from every `test fn` / `test "..."` / `suite "X"` block (single file or every `.cute` under cwd) and runs it. Exits non-zero on any failure. |
| `cute fmt foo.cute` | Canonical formatter (preserves comments at item / member / stmt boundaries). `--check` for CI. |
| `cute install [<path>\|<git-url>[@rev]]` | Builds + installs a Cute library to `~/.cache/cute/libraries/<Name>/<version>/<triple>/`. No-arg form walks the cwd's `[cute_libraries.<Name>]` specs. See "Sharing Cute code as libraries" below. |
| `cute install-cute-ui` | Builds + installs the cute_ui runtime once per machine. Required before `cute build` of any `gpu_app` project. |
| `cute doctor [foo.cute]` | Diagnoses Qt 6 / KF 6 / CuteUI module presence for a Cute build, and prints the platform-specific install command for anything missing. Run this when `cute build` fails with `find_package(Qt6) Could NOT be found` — its output is what to copy-paste. |
| `cute install-skills [--claude] [--cursor] [--codex]` | Drops the Cute-aware AI agent skill into `~/.claude/skills/cute/`, `~/.cursor/rules/`, and/or `~/.codex/`. No flag = install to all three. |
| `cute watch [foo.cute]` | Builds + spawns the binary, then auto-rebuilds + relaunches when any `.cute` / `cute.toml` in its directory is saved. Not in-process hot reload (state not preserved), but ~2–3 s edit/run loop. |
| `cute-lsp` | Stdio LSP server: diagnostics, hover, go-to-definition, completion (member access via fn parameter type or `let c = Counter()` local), multi-file project resolution. |

## Multi-file project layout

```
my_app/
├── cute.toml         # optional manifest (deps + cmake + cpp config)
├── main.cute         # entry; has `use model` + view/widget
├── model.cute        # `pub class Counter { ... }` for cross-module use
└── styles.cute       # `pub style Heading { ... }`
```

`use model` brings in every `pub` item from `model.cute` (no `pub` =
file-private). `use model.{Counter as C}` does selective + renamed
import. Project root = nearest `cute.toml`'s directory or entry's
parent.

## Pulling external C++ libraries via cute.toml

```toml
[bindings]
paths = ["bindings/spdlog.qpi"]   # describe the C++ surface in Cute syntax

[cmake]
find_package = ["spdlog"]
link_libraries = ["spdlog::spdlog"]

[cpp]
includes = ["<spdlog/spdlog.h>"]
```

`.qpi` files declare classes / methods / props in Cute syntax (no bodies); the codegen uses them for type-check only — they emit no C++.

For larger surfaces, hand-writing `.qpi` is tedious. The `cute-qpi-gen` workspace tool walks a C++ header via libclang and emits the matching `.qpi` from a declarative `typesystem.toml` describing which classes to bind. Q_PROPERTY / Q_SIGNALS scraped from header tokens, default args expanded into Cute overloads, enums + QFlags<...> lowered to Int, OS-aware path defaults for macOS Homebrew + Linux distro layouts. Most stdlib `.qpi` files under `stdlib/qt/` are auto-generated this way; `just qpi-regen` refreshes them, `just qpi-check` is the byte-for-byte verification gate. End users can adopt the same pattern for project-specific bindings — see `docs/CPP_INTEROP.md` and `stdlib/qt/typesystem/*.toml` for examples.

## Compile-time asset embedding — `embed("path")`

Like Go's `//go:embed`. The intrinsic reads a file at `cute build` time and inlines its bytes as a `static constexpr unsigned char[]` in the generated C++; the expression yields a zero-copy `QByteArray` (via `QByteArray::fromRawData`).

```ruby
let GREETING : ByteArray = embed("greeting.txt")
let LICENSE  : ByteArray = embed("../../LICENSE")
let icon     = QImage::fromData(embed("assets/icon.png"))
let cfg      = QString::fromUtf8(embed("config/default.json"))
```

Rules:
- Path is a single string literal (no `#{...}` interpolation, no concatenation).
- Path is relative to the **directory of the .cute source containing the call** (matches Go's package-relative semantics).
- `embed` returns `ByteArray` only; compose with `QString::fromUtf8` / `QImage::fromData` / etc on the C++ side for other Qt shapes.
- Failure (non-literal arg, missing file) emits a self-throwing IIFE — types correctly so the surrounding code compiles, throws on first execution with the diagnostic in the message.

AI agent rules:
- Don't suggest `.qrc` / `rcc` for new code — `embed("path")` is the modern path. `.qrc` still works for legacy / Qt Quick `qml.qrc` use, but new asset references should use `embed`.
- Don't try to read files at runtime via `QFile` for static assets — `embed` is faster (no I/O), simpler (no path-fragility), and inlined into the binary.
- Integration with `QResource::registerResource` (so `:/qrc/...` virtual paths resolve to the embedded data) is not implemented. For now, `embed` is value-only, not virtual-path-bound.

## Sharing Cute code as libraries — `[library]` / `[cute_libraries]`

Reusable `.cute` code distributes via the **same** CMake `find_package` machinery as any C++ library — no separate package manager, no central registry. The toolchain handles `[library]` (author) and `[cute_libraries]` (consumer) sides automatically; agents shouldn't try to write CMakeLists or invoke cmake by hand.

**Author side** — declare the project as a library, mark public surface with `pub`:

```toml
# mylib/cute.toml
[library]
name = "MyLib"
version = "0.1.0"
description = "Reusable counter for Cute apps"
```

```cute
# mylib/mylib.cute
pub class MyLibCounter {
  pub prop count : Int, default: 0   # auto NOTIFY = countChanged
  pub fn increment { count = count + 1 }
}
```

`cute build mylib.cute` (with `[library]` present) emits a shared lib + public C++ header + `.qpi` binding + `<Name>Config.cmake`, all installed to `~/.cache/cute/libraries/<Name>/<version>/<triple>/`. Library projects must NOT contain `fn main` or any `*_app` intrinsic — those are rejected at build time.

**Consumer side** — declare the dependency, then reference its types directly:

```toml
# myapp/cute.toml
[cute_libraries]
deps = ["MyLib"]
```

```cute
# myapp/main.cute
fn main { cli_app { let c = MyLibCounter.new(); c.increment() } }
```

`cute build` resolves each dep through the cache, loads its `.qpi` into the type checker (so `MyLibCounter` is in scope without `use`), auto-adds `find_package(<Name>) REQUIRED` + `target_link_libraries(... <Name>::<Name>)`, and bakes the install path into the binary's `INSTALL_RPATH`. The result runs without `DYLD_LIBRARY_PATH` / `LD_LIBRARY_PATH`.

**`cute install`** — three forms:

```bash
cute install /path/to/lib                      # local dir, build in place
cute install https://github.com/x/y.git@v0.2   # git clone + build (rev = tag/branch/commit)
cute install                                   # walk cwd's [cute_libraries.<Name>] specs
```

The detailed-spec form for `cute install` no-arg / pinning a remote source:

```toml
[cute_libraries.MyLib]
git = "https://github.com/foo/mylib"
rev = "v0.2.0"        # optional; default = remote HEAD

[cute_libraries.LocalLib]
path = "../local-lib" # for side-by-side dev
```

**AI agent rules**:
- Don't write `cmake_minimum_required(...)` / `add_library(...)` / `find_package(...)` by hand for Cute libraries — `[library]` + `[cute_libraries]` drives all of it.
- Don't suggest `pip` / `npm` / `cargo`-style `cute add <name>`. The lookup is filesystem-based against `~/.cache/cute/libraries/`; the right command is `cute install <path-or-url>` followed by `[cute_libraries] deps = ["<Name>"]` in the consumer's manifest.
- Don't put `fn main` in a `[library]` project — it's a build error. Mention this if a user mixes them.
- The `.qpi` emitter only covers `pub class` (prop / signal / fn / slot signatures), `pub struct`, top-level `pub fn`. Generic params, init / deinit, default values, weak / unowned modifiers, trait impls do NOT round-trip through library bindings yet — work around by keeping those internal or hand-writing a `.qpi`.

## Common error messages and what they mean

| Error | Likely cause |
|---|---|
| `expected item (use/class/struct/error/fn/view/widget), found Str(...)` | Forgot the `qml` keyword: write `use qml "QtQuick"`, not `use "QtQuick"` |
| ``\`X\` is private to class \`Y\` `` | Class member is missing the `pub` modifier — add `pub` to the declaration to make it accessible from outside the class |
| ``\`X\` is not exported from module \`m\` `` | Top-level decl is missing `pub` — add `pub class Foo` / `pub fn foo` to export across modules |
| ``no signal \`X\` declared in class \`Y\`(did you mean \`Z\`?)`` | Typo; the formatter shows did-you-mean suggestions |
| `function expects 3 argument(s), got 2 — signature: (Float, String, Float) -> Float` | Arity mismatch + signature note |
| `fatal error: QtCore/qtmochelpers.h: No such file or directory` | Qt < 6.9; need 6.9+ |
| QML "X is not a type" at runtime | Missing `use qml "..."` for the module that defines X |
| Material colors don't show up | Missing `use qml "QtQuick.Controls.Material"` |
| `expected expression, found While` | n/a anymore — `while cond { ... }` loops parse since 371014a |
| `/* TODO range outside for-loop unsupported */` in generated C++ | A bare range expression `a..b` / `a..=b` was used outside `for x in <range>` AND outside `arr[a..b]` slicing — those are the two supported positions today. Standalone `let r = 0..3` (Range as a first-class value) is not yet supported |
| `/* TODO widget_lower: <variant> */` in generated C++ | A widget property value used an `ExprKind` the widget lowerer doesn't support yet (statement-bearing Block / Lambda, Case-as-expression, raw Element). Hoist the value into a state field or slot and assign by name. |

## When writing or editing Cute code, ALWAYS

1. Check the `use qml` block at top of file matches every QML element used in `view` / `widget` (QtQuick path only — gpu_app projects don't use `use qml`).
2. Mark any class member referenced from a `view` / `widget` body or another class as **`pub`**. Same for top-level decls used across modules and `impl` methods called via generic-bound dispatch from another module. No `pub` = scope-private. Member names stay camelCase (`count`, `nudgePrice`); type names are PascalCase (`Counter`).
3. Use `prop` not `property`.
4. Use `isEmpty` / `size` / `mid` etc. (Qt camelCase), not Rust/Python idioms. Auto-synth notify signals follow Qt convention `<propName>Changed`, e.g. `pub prop count` → `:countChanged`.
5. **Plain `pub prop X : T, default: V` auto-derives the `<X>Changed` NOTIFY + signal** — no manual `, notify:` / `pub signal` lines needed for the common case. Bare LHS writes inside class methods (`count = count + 1`) go through the auto-generated setter, which emits the signal exactly once. **Don't add a manual `emit countChanged`** — a v1 lint warns; firing the signal twice causes double-redraw bugs. Override the conventional name with `, notify: :customName` (then pair it with `pub signal customName` if you need parameters); use `, constant` to opt out for genuinely immutable storage. For view-local primitive state, prefer `state X : T = init` at the head of the `view` / `widget` body over a wrapper class.
6. For `gpu_app` projects: confirm `cute install-cute-ui` has been run, use cute_ui widget names (Column / Row / Text / Button / TextField / Image / Svg / ListView / ScrollView / HScrollView / DataTable / Modal / BarChart / LineChart / ProgressBar / Spinner), `onClick` not `onClicked`. Slot-body list literals are typed when the LHS is a `prop List<T>` or a typed `let xs : List<T> = ...` — only untyped `let`/`var` falls back to QVariantList.
7. For `impl` blocks: receiver is lowercase `self` (the Cute keyword); `Self` (capital) is a type placeholder for the for-type, used in trait method signatures only. `impl` itself has no visibility modifier.
8. For class-scoped static methods (factories, Qt static utilities like `QTimer.singleShot`, `QCoreApplication.quit`), declare with `static fn` and call as `ClassName.method(args)`. Don't try to call them on an instance, and don't call instance methods via the class name — both are type errors now.
9. After the user changes anything, run `cute check` to verify before claiming done.

## Repository

Source: https://github.com/i2y/cute · cute_ui architecture: `docs/CUTE_UI.md` · C++ interop: `docs/CPP_INTEROP.md` · Demos: `examples/` (60+ working `.cute` projects: qml / widget / gpu_app / server / cli / Kirigami / KF6 (`kf6_config` / `kf6_i18n` / `kf6_notifications`) / charts / multi-file / generics / async / pattern matching / enums / errors / slices / embed / libraries). Tight edit / run loop: `cute watch foo.cute`.
