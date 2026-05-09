# Language tour

The [Anatomy](anatomy.md) walkthrough covered `class`
defaulting to QObject, `pub prop` auto-deriving its NOTIFY signal,
`var ( ... )` block fields, bare member access in class methods,
string interpolation, top-level `fn`, `style { ... }` with QSS
shorthand, and `widget Main { ... }`. The sections below cover
the rest of the language — what the calculator doesn't reach for.
Each entry stands on its own; pick whichever you need.

## `prop` extras — `notify`, `constant`, `signal`, `slot`, visibility

The Anatomy walked through `pub prop X : T, default: V`. Three
modifiers and two non-prop forms cover the rest of the meta-object
surface:

```cute
class Counter {
  pub prop count : Int, default: 0                           # auto NOTIFY = countChanged
  pub prop label : String, default: "", notify: :renamed     # custom NOTIFY name
  pub prop id    : String, default: "", constant             # CONSTANT — no NOTIFY

  pub signal clicked                          # hand-written signal, not a prop's notify pair
  pub signal errorOccurred(reason: String)

  pub fn increment { count = count + 1 }
  pub slot reset   { count = 0 }              # callable from QML via onClicked: counter.reset()
}
```

- **`, notify: :customName`** overrides the auto-derived
  `<X>Changed`. Pair with `pub signal customName` if the signal
  has parameters.
- **`, constant`** opts into a CONSTANT Q_PROPERTY: writable in
  the constructor only, no NOTIFY, QML treats reads as one-shot.
  Useful for genuinely immutable metadata that QML still needs to
  bind to.
- **Hand-written `pub signal`** is for transient events that
  aren't a prop's notify pair (`clicked`, `errorOccurred`). Emit
  with `emit signalName(args)`. **Do not** follow a `prop = ...`
  write with a manual `emit propChanged` — the setter already
  fires it, and double-emitting confuses bindings.
- **`pub slot fn-name { ... }`** is identical to `pub fn` but
  additionally emits a Q_SLOT declaration so QML can call it
  (`onClicked: counter.reset()`). Inside Cute, `slot` and `fn`
  behave the same from the caller's side.
- **Visibility shortcuts.** Top-level decls are file-private;
  class members are class-private. `pub` opens a name to other
  Cute files / QML / external observers — apply it on the
  QML-bound surface (`prop`, `signal`, `slot`, externally-called
  `fn`).
- **Naming follows Qt house style:** camelCase for member /
  method / signal / slot / field names, PascalCase for type
  names.

Reads / writes inside class methods go through the auto-generated
accessors. `count = count + 1` lowers to `setCount(count() + 1)`;
the setter is the standard Q_PROPERTY shape:

```cpp
void Counter::setCount(int value) {
    if (m_count == value) return;
    m_count = value;
    emit countChanged();      // ← auto-emitted by the setter
}
```

If a member name collides with a fn parameter, the parameter wins
inside that fn body (`init(label: String) { name = label }`
correctly assigns the param to the member). Reach a shadowed
member via `self.<name>()`.

### `prop` vs `let` vs `var` — when to use which

Three different storage forms with different visibility surfaces:

| Decl | Mutable? | Q_PROPERTY? | NOTIFY? | QML-visible? | Use for |
|---|---|---|---|---|---|
| `let x : T = init` | only inside `init` | ❌ | ❌ | ❌ | const field, internal id, handle |
| `var x : T = init` | ✅ anywhere | ❌ | ❌ | ❌ | mutable internal state, work buffer, cache |
| `prop x : T, default: V` | ✅ via setter | ✅ | ✅ (auto `xChanged`) | ✅ | reactive value bound to UI / external observers |
| `prop x : T, default: V, constant` | ❌ (set in ctor only) | ✅ | ❌ | ✅ | metadata QML reads but never observes |

```cute
class Editor {
  let id        : Int  = nextEditorId()  # immutable; init only
  var dirty     : Bool = false           # private mutable; internal flag
  pub var label : String = ""            # public mutable; getter + setter,
                                         # but NOT a Q_PROPERTY (no QML reactivity)
  pub prop text : String, default: ""    # full Q_PROPERTY: QML binds,
                                         # auto NOTIFY = textChanged

  pub fn typeChar(c: String) {
    text  = text + c     # routes through setter → fires textChanged
    dirty = true         # plain field write, no signal
  }
}
```

When in doubt: **start with `prop` if anything outside the class
needs to react to the value; use `var` when only the class itself
reads and writes; use `let` for values fixed at construction.**
Adding `pub` to a `let` / `var` exposes the C++ getter (and
setter for `var`) but does NOT make it a Q_PROPERTY — QML still
can't bind to it. Promote to `prop` when QML reactivity matters.

## View-local state (`state X : T = init`)

When you don't need a separate model class — you just want a primitive
reactive cell scoped to one view / widget — declare it at the head of
the body. Cute's analogue of SwiftUI's `@State`:

```cute
view Main {
  state count : Int = 0

  ApplicationWindow {
    Label  { text: "count: " + count }
    Button { text: "+1"; onClicked: { count = count + 1 } }
    Button { text: "reset"; onClicked: { count = 0 } }
  }
}
```

Same surface in QtWidgets (`widget Main { state count : Int = 0 ; QMainWindow { ... } }`)
and in cute_ui's GPU path (`widget Main { state count : Int = 0 ; Column { ... } }`).
The lowering is target-specific:

- **`view` (QML)** → root-level `property int count: 0`. References resolve
  via QML's own scoping; assignment fires the auto-generated `countChanged`.
- **`widget` (QtWidgets / cute_ui)** → desugared into a synthesized
  `__<Widget>State < QObject` class with `pub prop count : Int, default: 0`
  plus a hidden `let __cute_state = __<Widget>State()`. Bare references
  inside the body get rewritten to `__cute_state.count`; the reactive
  binding emitter / cute_ui rebuild loop pick up the auto-derived
  NOTIFY signal exactly the way they do for any other class-on-prop.

Use `state` for primitive reactive cells (Int / Double / Bool / String /
Url / Color / `var`-fallback for List or unknown types) that don't need to
be reused across views. Use `let counter = Counter()` + `pub prop count`
on a class for shared state, multi-view bindings, or genuinely
model-shaped data. Demos: [`examples/counter_state`](examples/counter_state/)
(QML), [`examples/widgets_counter_state`](examples/widgets_counter_state/)
(QtWidgets), [`examples/gpu_state`](examples/gpu_state/) (cute_ui).

## Singleton state (`store X { ... }`)

For state that lives outside any one view — current user, theme, navigation
routes, app-wide caches — declare a `store`. One block collapses class +
singleton instantiation + per-member `pub` markings:

```cute
store Application {
  state user : User? = nil
  state theme : Theme = Theme.Light

  fn login(u: User) { user = u }
  fn logout         { user = nil }
}
```

Anywhere in the program, `Application.user` / `Application.login(u)` works.
Bare `Application` references resolve through a `Q_GLOBAL_STATIC` accessor
the desugar synthesizes — `Application()->user()` in the emitted C++. Each
`state X : T = init` lowers to `prop X : T, notify: :XChanged, default:
init` + `signal XChanged`, so QML / widget bindings on `Application.X`
re-render reactively when X changes. Every member is forced `pub` (a
singleton is global by definition); Object-kind state fields like
`let X = sub_obj()` are rejected at parse time — use `state` for primitive
cells or write the init in `init { ... }`. Demo:
[`examples/store_demo`](examples/store_demo/).

`store` is sugar for the `class Foo { ... } + let APP : Foo = Foo.new()`
pattern when there's exactly one instance per process. The hand-written
form is still valid (and useful when the type is reused with multiple
instances elsewhere); `store` is the recommended shape for true
singletons.

## Reactive props (Qt 6 binding system)

`prop` carries four shapes that map onto Qt 6's full property surface:

```cute
class Position {
  pub prop entryPrice   : Float, bindable, default: 150.0
  pub prop currentPrice : Float, bindable, default: 175.5
  pub prop quantity     : Int,   bindable, default: 100
  pub prop fees         : Float, bindable, default: 12.0
  # auto NOTIFY = entryPriceChanged / currentPriceChanged / quantityChanged / feesChanged

  # Derived: re-evaluates lazily, deps tracked automatically.
  pub prop netPl : Float, bind { (currentPrice - entryPrice) * quantity - fees }

  # Function-style: re-runs every read; for state Qt's binding system can't
  # observe (file size, current time, third-party getters).
  pub prop now : String, fresh { QDateTime.currentDateTime().toString("HH:mm:ss") }

  # Atomic multi-prop update — bindable writes inside the block defer their
  # notifications until scope exit, so dependent bindings re-evaluate exactly
  # once with a fully consistent state (glitch-free).
  pub fn loadAapl {
    batch {
      entryPrice = 150.0; currentPrice = 175.5
      quantity = 100;     fees = 12.0
    }
  }
}
```

| Modifier | Lowering | Use |
|---|---|---|
| (none) | bare `T m_x;` | plain non-bindable storage |
| `bindable` | `Q_OBJECT_BINDABLE_PROPERTY` | input you write to from outside |
| `bind { e }` | `QObjectBindableProperty` + `setBinding(lambda)` | derived value, deps auto-tracked, lazy-cached |
| `fresh { e }` | `Q_OBJECT_COMPUTED_PROPERTY` | function-style getter, re-runs every read |

The `gpu_pnl` / `widgets_pnl` / `qml_pnl` examples render the same `Position`
class through three different UI backends (cute_ui canvas, QtWidgets, QML);
the `gpu_fresh` example puts `bind` and `fresh` side-by-side. See
`examples/README.md`.

### Internal `prop` for encapsulated reactive state

Q_PROPERTY without `pub` is class-private but still participates in
the binding system, so a public derived prop can take a dependency on
internal state without exposing it:

```cute
class Document {
  # Internal — bindable so derived props observe writes, but
  # no `pub` means callers can't reach `changeCount` directly.
  prop changeCount   : Int, bindable, default: 0
  prop pristineCount : Int, bindable, default: 0

  # Public derived views; each `bind { ... }` re-evaluates
  # automatically when either internal counter changes.
  pub prop isDirty   : Bool,   bind { changeCount != pristineCount }
  pub prop saveLabel : String, bind { if isDirty { "Save *" } else { "Save" } }

  pub fn touch { changeCount = changeCount + 1 }
  pub fn save  { pristineCount = changeCount }
}
```

External code only sees `touch()` / `save()` / `isDirty` / `saveLabel`.
The dirty-bit mechanics stay encapsulated, but UI bindings on
`saveLabel` still update reactively. Demo:
[`examples/internal_prop_demo`](examples/internal_prop_demo/).

## Collections as item models (`ModelList<T>`)

`pub prop xs : ModelList<T>` exposes the list as a single
`QAbstractItemModel*` adapter (a public-derived `QRangeModel`) so
QML's `ListView` / `Repeater` / `TableView` AND QtWidgets' `QListView`
/ `QTableView` can consume it directly — **no hand-written
`QAbstractListModel` subclass**:

```cute
class Library {
  pub prop books : ModelList<Book>, default: [...]
  #                ^^^^^^^^^^^^^^^
  # lowers to:  Q_PROPERTY(::cute::ModelList<Book*>* books READ books CONSTANT)
  #             — pointer is stable; mutate via books.append(b) /
  #             books.removeAt(i) / books.clear() / books.replace(...)
  pub fn appendBook(b: Book) { books.append(b) }
}

view Main {
  let library = Library()
  ApplicationWindow {
    ListView {
      model: library.books
      delegate: Item {
        Label { text: model.title }            # role names auto-derived
        Label { text: "by " + model.author }   # from Book's Q_PROPERTYs
      }
    }
  }
}
```

Backed by Qt 6.11's [`QRangeModel`](https://doc.qt.io/qt-6/qrangemodel.html);
the `<QRangeModel>` include and a `MultiRoleItem` row-options specialization
are emitted only when a class actually declares a `ModelList<T>` prop, so
projects on Qt < 6.11 that never use it stay unaffected. v1 requires the row
type to be a QObject subclass; element-level mutations (`book.title = "..."`)
flow through the row's Q_PROPERTY notify signals via QRangeModel's
auto-watching. `cute::ModelList<T>::data()` falls through `Qt::DisplayRole`
to the row's first auto-derived role so default `QListView` / `QTableView`
delegates render usable text out of the box. See
[`examples/qrange_model`](examples/qrange_model/) (QML) and
[`examples/widgets_books`](examples/widgets_books/) (QtWidgets) for the full
demos.

## Pattern matching with error unions

```cute
fn parseInt(s: String) !Int { ... }

case parseInt(input) {
  when ok(n)  { println("parsed: #{n}") }
  when err(e) { println("rejected: #{e}") }
}
```

`!T` is "T or error". `try expr` unwraps and early-returns the
error up through the surrounding `!T` fn (Zig-style). Errors
themselves are declared with `error MyErr { kind1, kind2(detail:
String) }`. `T?` is "T or nil"; chain through it with the postfix
null-safe accessor `recv?.member` / `recv?.method(args)`.

## Generics

```cute
arc Box<T> {
  pub var item : T
}

fn first<T>(xs: List<T>) T? {
  if xs.isEmpty { nil } else { xs[0] }
}

let b : Box<Int>    = Box.new()
let s : Box<String> = Box.new()
```

Generic classes lower to C++ templates; generic fns become
function templates. Type args are inferred at call sites from
the surrounding context (let / var annotation, fn parameter,
return type).

## Traits and impls

```cute
trait Summarizable {
  fn summary String
  # Default body: impls that omit `loudSummary` get this for free.
  fn loudSummary String { "(no summary available)" }
}

class Person {
  pub prop name : String, default: ""
  pub prop age  : Int,    default: 0
}

impl Summarizable for Person {
  fn summary String { "person: #{self.name()}, age #{self.age()}" }
  fn loudSummary String { "PERSON >> #{self.name()}" }
}

# Generic-bound fn: the body can call any method declared on the
# trait. Method dispatch is monomorphized per T at C++ template
# instantiation time.
fn announce<T: Summarizable>(thing: T) {
  println(thing.summary())
  println(thing.loudSummary())
}
```

- **Nominal traits** (Swift `protocol` / Rust `trait` style): retroactive
  `impl Trait for ExistingClass` works on any class — including Qt
  binding types (`impl Magnitude for QPoint`) and builtin generics
  (`impl<T> Sized for List<T>`).
- **Generic bounds** (`<T: Trait>`) are enforced at the call site: a
  type that doesn't implement the listed trait is rejected with a
  Cute-source diagnostic, not a downstream C++ template error.
- **`Self`** in a trait method's signature substitutes to the bound
  `T` in generic-bound bodies and to the concrete for-type at impl
  emission. `trait Cloneable { fn cloned Self }` lets you write
  `fn duplicate<T: Cloneable>(t: T) T { t.cloned() }`.
- **Specialization**: parametric vs concrete impls on a non-class
  base (`impl<T> Foo for List<T>` plus `impl Foo for List<Int>`)
  both emit as namespace overloads and C++ overload resolution
  picks the most specific at the call site. On user classes, only
  one impl per `(trait, base)` is allowed for now.
- **Default-bodied trait methods** are inherited by impls that omit
  them; impl-supplied versions override.
- See [`examples/traits`](examples/traits/) and
  [`examples/traits_extern`](examples/traits_extern/) for end-to-end
  demos covering both QObject classes and `extern value` types.

## String interpolation + format specs

```cute
let msg = "count: #{n}"           # interpolation
let f   = "#{value:.10g}"         # Python-style mini-language
let pad = "[#{name:>10}]"         # right-align width 10
```

Lowers to a `cute::str::format` chain, which is `QString`-native
under the hood.

## Async / await (Qt 6.5+ coroutines)

```cute
async fn fetch(url: String) Future(String) {
  let resp = await network.get(url)
  resp.body
}

async fn main_async Future(Void) {
  let body = await fetch("https://example.com")
  println(body)
}
```

`async fn f T` (or the explicit `async fn f Future(T)`) lowers to a
coroutine whose C++ return type is `QFuture<T>`; `await expr` is
`co_await expr` under the hood. Cute ships
`runtime/cpp/cute_async.h`, which adds the
`std::coroutine_traits<QFuture<T>>` specialization Qt 6.11 lacks
plus `operator co_await(QFuture<T>)`, so the lowered C++ compiles
and runs against stock Qt. `cli_app` automatically lifts its body
into a `QFuture<void>` coroutine driven by `QCoreApplication::exec`
when it spots an `await`; synchronous bodies stay on the original
zero-event-loop path. Plays naturally with Qt's existing async
surfaces (QFuture, QtConcurrent). See `examples/async_demo/` for a
runnable end-to-end demo.

## Slicing (`arr[a..b]` → `Slice<T>`)

```cute
fn sum(xs: Slice<Int>) Int {
  var total: Int = 0
  for x in xs { total = total + x }
  total
}

fn main {
  cli_app {
    let arr: List<Int> = [1, 2, 3, 4, 5]
    let s = arr[1..4]              # Slice<Int>, length 3
    println(sum(arr[1..4]))        # 9
    println(s[0])                  # 2
    let sub = s[0..2]              # zero-copy sub-slice
  }
}
```

`arr[a..b]` (exclusive) and `arr[a..=b]` (inclusive) lower to
`::cute::make_slice(arr, a, b)`, returning a `cute::Slice<T>` value
backed by a `std::shared_ptr<QList<T>>`. The slice can be returned
from a fn, stored in a struct field, indexed (`s[i]`), iterated
(`for x in s`), and length-queried (`s.length`). Sub-slicing is
zero-copy. The shared backing keeps the source alive, so a slice
never dangles — pass slices around freely without lifetime
annotations. The current scope is read-only views; mutating the source
through a slice (Go-style shared semantics) is planned for a future
release. Demo: `examples/slice_demo/`.

## Range loops (`std::views::iota`)

`for i in 0..N` and `for i in 0..=N` lower to
`for (qint64 i : std::views::iota(s, e))`. `Slice<T>` iteration
uses range-based for too, so both sources share the same C++20
ranges path — future pipeline operators (`xs | filter | map`) will
compose against either.

## Style declarations

`style X { ... }` is a compile-time bag of `(key, value)` entries.
Reference it from a UI element with `style: X` and the entries are
**inlined at codegen time** — no runtime cost, no QML / QtWidgets
plumbing. `style Y = A + B` composes with right-wins merge so
shared sizing and per-variant looks stay decoupled.

The **syntax** (`style { ... }`, `style: X`, `+` merge) is the same
across all three UI paths Cute targets, but the **vocabulary** of
keys you can write inside differs because each backend has a
different property/setter surface. A `style` block is therefore
typically authored against one specific path:

| Path | Vocabulary | Aggregation |
|---|---|---|
| `view` (QML) | QML property names: `color`, `font.pixelSize`, `radius`, `anchors.X`, `border.color`, ... | passthrough — each entry becomes a literal QML property line |
| `widget` (QtWidgets) | genuine QWidget setters (`minimumWidth`, `windowTitle`, ...) **+ QSS shorthand** (`color`, `borderRadius`, `hover.X`, ...) | shorthand keys auto-aggregated into one `setStyleSheet(...)` call |
| `widget` over cute_ui (`gpu_app`) | the cute_ui Element's setter API (`color`, `text`, `spacing`, ...; varies per element type) | passthrough — each entry calls the matching `setX(...)` on the Element |

Sharing a single `style` between paths is **not** supported today
(`fontSize: 28` works on QtWidgets shorthand but is silently
ignored by QML, which wants `font.pixelSize: 28`). If you target
multiple paths, write per-path style blocks. A target-aware
modifier namespace is on the roadmap (SPEC §4.5 future scope).

### `view` (QML) path

```cute
style Heading { font.pixelSize: 28; color: "#1a1a1a" }
style Centered { anchors.centerIn: parent }
style HeadingCentered = Heading + Centered    # composes; right wins

view Main {
  Label { style: HeadingCentered; text: "hi" }
}
```

Each entry lowers to a QML property line (`font.pixelSize: 28`,
`color: "#1a1a1a"`). Unknown keys are emitted verbatim and the QML
engine warns at runtime — Cute itself does no schema check on QML
property names today.

### `widget` (QtWidgets) path — same syntax, QSS shorthand built in

QtWidgets has a much narrower setter API than QML — `QPushButton`
has no `setBackground` / `setBorderRadius` / `setFontWeight`, only
`setStyleSheet(QString)`. So the widget path recognises a fixed
**QSS shorthand vocabulary** (camelCase, optionally with a
pseudo-class prefix) and aggregates every recognised entry into one
`setStyleSheet(...)` call at codegen time. The
[Anatomy](anatomy.md) above shows this in action against
`examples/calculator`; the table below is the authoritative key
reference.

Recognised shorthand keys:

| Category | Keys |
|---|---|
| Color | `color`, `background`, `backgroundColor`, `borderColor` |
| Border | `border`, `borderRadius`, `borderWidth`, `borderStyle` |
| Font | `fontSize`, `fontWeight`, `fontFamily`, `fontStyle` |
| Padding / margin | `padding`, `paddingLeft`/`Right`/`Top`/`Bottom`, `margin*` |
| Alignment | `textAlign` (`"right"` → `qproperty-alignment: AlignRight`) |
| Pseudo-class prefix | `hover.X`, `pressed.X`, `focus.X`, `disabled.X`, `checked.X` |

Length-typed keys (`borderRadius`, `padding*`, `fontSize`, ...)
auto-suffix `px` when given an `Int` / `Float` literal; numeric
keys (`fontWeight`) emit the number raw; bare strings (`"32px"`,
`"50%"`, `"bold"`) pass through verbatim. Unrecognised keys fall
through to the regular `setX(...)` path so genuine widget setters
(`minimumWidth: 64`, `windowTitle: "..."`) keep working alongside
shorthand in the same block.

`cute check` validates each entry's value against the expected
shape — `borderRadius: true` and `color: 42` are rejected up-front
with a typed diagnostic, instead of silently producing malformed
QSS that Qt then discards at runtime.

A user-written literal `styleSheet: "..."` on the same element is
preserved: the synth runs first, the literal is concatenated after
with `+`, so QSS later-rule-wins specificity gives the user's
hand-written rules the final say.

### `widget` over cute_ui (`gpu_app`) path

The `gpu_app` runtime draws via Qt 6.11 Canvas Painter, not QSS,
so the widget shorthand above does **not** apply here. Instead,
each `style` entry lowers to a setter call on the cute_ui Element
type the key targets — `color: "#fff"` on a `Text` becomes
`_t->setColor("#fff")` because `cute::ui::TextElement` exposes
`setColor`. Pseudo-class prefixes (`hover.X` etc.) and length-key
auto-`px`-suffixing are QtWidgets-only and have no analogue here.

```cute
style BodyText { color: "#1a1a1a" }      # TextElement::setColor
style Padded   { spacing: 8 }            # column/row's setSpacing

widget Main {
  Column {
    style: Padded
    Text { style: BodyText; text: "hi" }
  }
}

fn main { gpu_app(window: Main) }
```

The exact key set depends on the cute_ui element type — see the
`runtime/cute-ui/include/cute/ui/element.hpp` setter declarations
for the authoritative list per element. Mismatched keys (e.g.
`borderRadius` on a Text) surface as a C++ compile error from the
generated code, not a Cute-level diagnostic.

## Tests (`test fn` / `suite "X" { test "y" { ... } }`)

Cute ships a built-in test runner — no extra dependency, no separate
test crate. Three surface forms coexist:

```cute
# Compact: name is an identifier. Right tool when a snake_case name
# describes the test.
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

`cute test foo.cute` (or `cute test` to walk cwd) builds a TAP-lite
runner and runs it:

```
1..3
ok 1 - addition
ok 2 - compute / adds positive numbers
ok 3 - compute / handles zero divisor as zero
# 3 passed, 0 failed
```

Failed assertions surface their `actual=… / expected=… at <file>:<line>`
directly in the line and the binary exits non-zero, so CI can gate on it.
Available out of the box: `assert_eq` / `assert_neq` / `assert_true` /
`assert_false` / `assert_throws { body }`. Inside a suite body, only the
string-named form is accepted (compact-form tests declare at the top
level alongside the suite). Nested suites, `pub suite`, and `pub test`
are rejected. `before` / `after` hooks are deferred to v1.y; for shared
setup today, call helper fns from each test or reset `store` state by
hand. Demo: [`examples/test_demo`](examples/test_demo/) (covers all
three shapes).

## Trailing-block lambdas

```cute
xs.each { |x| println(x) }
server.route("/health") { "ok\n" }
QTimer.singleShot(1000) { println("tick") }
```

The `{ |args| body }` form is a closure passed as the call's last
argument. Used heavily by QHttpServer routes, QtConcurrent
callbacks, and QML signal handlers.

## Multi-file projects

```
counter/
├── cute.toml          # optional manifest (deps, include paths)
├── counter.cute       # `use model` + `view Main { ... }`
├── model.cute         # `pub class Counter { ... }`
└── styles.cute        # `pub style Heading { ... }`
```

`use model` brings every `pub` item from `model.cute` into local
scope (no `pub` = file-private); `use model.{Counter as C}` does
selective + renamed import. Both `cute build` and `cute-lsp` walk
the dependency graph transitively.

## Value types — `struct`

```cute
struct Point {
  var x : Int = 0
  var y : Int = 0

  fn magnitudeSq Int { self.x * self.x + self.y * self.y }
  fn shifted(dx: Int, dy: Int) Point {
    Point.new(self.x + dx, self.y + dy)
  }
}

fn main {
  let p : Point = Point.new(3, 4)
  let q : Point = p                # copy — `q` and `p` are independent
  let r : Point = p.shifted(10, 20)
  println("(#{r.x}, #{r.y})")       # (13, 24)
}
```

`struct` types are **value types**: stack-allocated, copy on
assignment, no metaobject, no inheritance. Use them for plain-old
data — coordinates, config records, computation results. No
reference cycles to worry about, no QObject overhead, no
`Q_OBJECT` machinery. Methods are inline; field access is direct
(`p.x`, no `()`).

## Reference-counted classes — `arc` + `weak`

```cute
arc Document {
  pub var title : String = ""
  pub var lines : List<String> = []

  init(t: String) { title = t }
  deinit { println("Document dropped: #{title}") }

  pub fn appendLine(s: String) { lines.append(s) }
}

arc Editor {
  weak let document : Document?     # weak ref — breaks the cycle

  init(d: Document) { document = d }

  pub fn show {
    case document {
      when some(d) { println("editing #{d.title}") }
      when nil     { println("(document released)") }
    }
  }
}
```

`arc Foo { ... }` is a **heap-allocated, reference-counted** type
(`cute::Arc<T>` — Swift / Rust `Arc` semantics). Multiple bindings
share the same object; the runtime cleans up on last release.
`init` / `deinit` are user-defined construction and destruction.
`weak let` flips a field to `cute::Weak<T>` to break cycles —
reads auto-yield `T?` so `case` can match `some(p)` / `nil`.

Choose `arc` when you need shared ownership without Qt's binding
machinery (`signal` / `slot` / parent tree are class-only).
Choose `class` (the default) when you need QObject reactivity.
The [Memory model](reference.md#memory-model) table summarises
every ownership form including `T?` (auto-nulling pointer) and
Qt's COW collection types.

## Auto-`this` for `T.new()` in class methods

```cute
class Counter {
  prop child : Counter?

  pub fn spawn { child = Counter.new() }    # parent = self, injected
}
```

Inside a class method, `T.new(args)` lowers to `new T(this, args)`
so the Qt parent-tree wiring is automatic. Outside class methods,
`T.new()` is `new T(nullptr)` and the caller is responsible for
parenting.
