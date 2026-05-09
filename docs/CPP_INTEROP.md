# C++ interop in Cute

Cute compiles to ordinary Qt 6 C++. Cute classes emit Q_OBJECT-style
metaobject data, ARC classes are plain C++ templates, and the
generated `.h` / `.cpp` pair link against `cute_runtime` (a header-
only C++ library that ships in the cute binary). That makes Cute a
drop-in for any C++ codebase that already uses Qt — you can call
into Cute from C++, call into C++ from Cute, or do both.

This document covers the three interop modes:

1. **Use a third-party C++ library from Cute** — bind once, call
   forever. The path most projects start with.
2. **Embed Cute output in an existing C++ project** — `build_file`
   emits `.h` + `.cpp` that you `#include` from C++ build systems.
3. **Call Cute classes from C++** — the symbols Cute exports and
   how to drive them from hand-written C++.

The mechanism is the same in all three: `.qpi` binding files
describe the type surface, `cute.toml` wires up the build system,
and `cute_runtime` provides the small set of headers (Arc, error
union, format) that user code shares.

---

## Mode 1 — use a third-party C++ library from Cute

Drop a `cute.toml` next to your `.cute` source. The file has four
sections relevant to interop:

```toml
[bindings]
paths = ["bindings/spdlog.qpi"]

[cmake]
find_package = ["spdlog"]
link_libraries = ["spdlog::spdlog"]

[cpp]
includes = ["<spdlog/spdlog.h>"]
```

| Section | Effect |
|---|---|
| `[bindings] paths` | Extra `.qpi` files to load on top of the stdlib bindings. Resolved relative to the directory that contains `cute.toml`. |
| `[cmake] find_package` | Each entry becomes `find_package(<value> REQUIRED)` in the generated CMakeLists. |
| `[cmake] link_libraries` | Appended to `target_link_libraries(<binary> PRIVATE Qt6::... <values>)`. |
| `[cpp] includes` | Each entry becomes a `#include <value>` in the generated header. The brackets / quotes are user-supplied so umbrella vs specific is your call. |

Then write the binding. A `.qpi` file is a strict subset of Cute
syntax: `class Name { ... }` with member declarations but no
function bodies.

`bindings/spdlog.qpi`:

```ruby
# spdlog: header-only C++ logging library. We bind the parts we
# actually call from .cute source — most apps just want
# info/warn/error and that's it.

class spdlog_logger < QObject {
  fn info(msg: String)
  fn warn(msg: String)
  fn error(msg: String)
  fn debug(msg: String)
  fn flush
  fn set_level(level: Int)
}
```

Now from `.cute` source:

```ruby
fn main {
  let log = spdlog_logger()
  log.info("hello from cute")
  log.error("something went wrong")
}
```

`cute build app.cute` produces a binary that links against spdlog
exactly like a hand-written C++ project. The cute frontend runs the
type-check against the binding, codegen emits the C++ method calls,
and CMake handles the link.

### Choosing a binding shape

Three forms cover almost every C++ class you'll bind:

| C++ class shape | `.qpi` form | Notes |
|---|---|---|
| Q_OBJECT-derived (signals, slots, Q_PROPERTY) | `class Foo < QObject { prop ... signal ... fn ... }` | Inheritance is honoured; declare the real super (`< QWidget`, `< QAbstractButton`) when it's bound elsewhere. |
| Plain class (no inheritance, members accessed by value) | `extern value Foo { fn ... }` | `Foo.new(args)` lowers to `Foo(args)`; no Arc / no metaobject. The QtCore value types (QPoint, QRect, QColor, QDate, ...) ship in this form. |
| Plain class but you want Cute to track it via Arc anyway | `class Foo { fn ... }` (no super clause) | The fallback for non-Q_OBJECT classes that you nevertheless want to allocate on the heap. Most users go with `extern value` instead. |

Cute treats binding classes as opaque type surfaces — codegen
never sees them — so the inheritance you declare in `.qpi` only
matters for type-check resolution. If you mis-state it, callers
just lose the parent's API surface but everything still compiles.

### Generating bindings from C++ headers (cute-qpi-gen)

Hand-writing `.qpi` files works for small surfaces but gets
repetitive for full classes. The `cute-qpi-gen` tool walks a C++
header via libclang and emits the matching `.qpi` from a
declarative `typesystem.toml`:

```toml
# bindings/spdlog.toml
[clang]
includes = ["/usr/include"]
std = "c++17"

[[classes]]
name = "spdlog_logger"
kind = "object"
super_name = "QObject"
header = "/usr/include/spdlog/logger.h"
include = ["info", "warn", "error", "debug", "flush", "set_level"]
```

```bash
DYLD_FALLBACK_LIBRARY_PATH=/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib \
  cargo run -p cute-qpi-gen -- \
    --typesystem bindings/spdlog.toml \
    > bindings/spdlog.qpi
```

The tool handles:

- **Q_PROPERTY scraping** (the macro is gone after preprocessor
  expansion, so it tokenises the class body and parses
  `Q_PROPERTY(type name READ ... WRITE ...)` directly).
- **`Q_SIGNALS:` / `signals:` access section detection** — methods
  on those source lines emit as `signal X(...)` instead of `fn`.
- **Default-arg expansion** — `foo(a, b = 1)` becomes both
  `foo(a)` and `foo(a, b)` in Cute.
- **Enum / `QFlags<...>` types** map to `Int` automatically.
- **Inheritance** auto-detected via libclang; `super_name`
  overrides when the real C++ super isn't bound (e.g. QGraphicsView).
- **OS-aware path defaults** — `${qt_core}` / `${qt_gui}` /
  `${qt_widgets}` / `${qt_charts}` / `${qt_quick}` / `${qt_qml}` /
  `${qt_svg}` / `${qt_svg_widgets}` / `${qt_network}` /
  `${qt_multimedia}` resolve to `/opt/homebrew/lib/Qt<X>.framework/Headers`
  on macOS and `/usr/include/qt6/Qt<X>` on Linux. Override per
  variable via `CUTE_QPI_<UPPERCASE>` env if your install differs.

The tool is what generates every `.qpi` under `stdlib/qt/` —
65+ Qt classes across the QtWidgets widget tree, the QLayout
family, the model-view chain, QtCharts / QtMultimedia / QtSvg /
QtNetwork basics, QSettings, QTimer, QProcess,
QRegularExpression, QtCore value types (QPoint / QSize / QRect /
QColor / QDate / QUrl …), and the QJsonDocument family. Run
`just qpi-regen` from the repo root to refresh them all; `just
qpi-check` is the pre-commit gate that verifies committed
output matches generation byte-for-byte.

### Namespaced C++ APIs

`.qpi` class names are bare identifiers. If the C++ class lives in
a namespace (`spdlog::logger`, `nlohmann::json`, ...), the binding
class name doesn't carry the namespace, and the generated C++ won't
either. Workaround: write a small wrapper header that pulls the
namespaced name into the global / using-declared scope, and list
*that* header in `[cpp] includes`. For example:

```cpp
// bindings/spdlog_glue.h
#pragma once
#include <spdlog/spdlog.h>
using spdlog_logger = spdlog::logger;
```

```toml
[cpp]
includes = ["\"bindings/spdlog_glue.h\""]
```

Then `class spdlog_logger < QObject { ... }` in the `.qpi` resolves
the same way at compile time. This is a small one-time cost per
namespaced library; once the alias is in place, the rest of the
integration is identical to a flat-namespace binding.

### Header-only quirks

Some header-only libraries pollute the global namespace with macros
(`#define SPDLOG_TRACE`, etc.). Today every `[cpp] includes` line
ends up in the generated `.h`, which the cute output `.cpp` then
includes — there's no `cpp_only_includes` distinction yet. If that's
a problem, the workaround is to wrap the noisy header in a small
glue header that hides what it can:

```cpp
// bindings/spdlog_clean.h
#pragma once
#undef SPDLOG_TRACE   // suppress macros that leak into user TUs
#include <spdlog/spdlog.h>
using spdlog_logger = spdlog::logger;
```

Then `[cpp] includes = ["\"bindings/spdlog_clean.h\""]` and the
binding sees `spdlog_logger` resolved cleanly. A native
`[cpp] cpp_only_includes` knob is on the roadmap.

---

## Mode 2 — embed Cute output in an existing C++ project

`cute build foo.cute --out-dir ./gen/` runs the same frontend
(parse → resolve → typecheck → codegen) but stops before invoking
CMake. Output:

```
gen/
  foo.h        # public Cute classes, signals, properties
  foo.cpp      # implementations + cute_meta-emitted MetaObject data
```

Add the pair to your existing C++ build (CMake, Bazel, qmake — any
Qt-aware system), link `cute_runtime` (header-only, copy
`runtime/cpp/` into your tree or set up an include path), and you
can `#include "foo.h"` from C++.

Driver entry point:

```rust
cute_driver::build_file(input_path, out_dir)
```

Returns the list of files written. No CMake invocation, no compiler
launch — pure codegen. The cute-cli wrapper exposes this through
`cute build foo.cute --out-dir <dir>`.

### Runtime dependency

The generated code depends on a handful of headers from
`runtime/cpp/`:

| Header | Purpose |
|---|---|
| `cute_arc.h` | `cute::Arc<T>` — the ARC pointer for `arc X { ... }` classes |
| `cute_error.h` | `cute::Result<T, E>` — the error-union runtime (`ok(v)` / `err(e)` factories) |
| `cute_string.h` | `cute::str::format` — string interpolation runtime |
| `cute_meta.h` | `qt_create_metaobjectdata<Tag>` — moc replacement |

Copy these into your project (or set an include path) before
compiling the emitted `.cpp`. They are header-only and have no
non-Qt dependencies.

---

## Mode 3 — call Cute classes from C++

Once Mode 2 has produced `foo.h`, you can drive the Cute types
from hand-written C++.

### QObject classes

A `class Counter < QObject { ... }` (note: leading uppercase makes
the class exported under Cute's Go-style visibility rule) emits a
normal Q_OBJECT-style class:

```cpp
#include "foo.h"

void from_cpp(QObject* parent) {
    Counter* c = new Counter(parent);
    c->setCount(42);                    // public property setter
    QObject::connect(c, &Counter::countChanged,
                     parent, [c]{ qDebug() << c->Count(); });
    QMetaObject::invokeMethod(c, "Increment");  // public slot
}
```

The shape mirrors what moc would have produced, with one difference:
the metaobject data lives in a `qt_create_metaobjectdata<Tag>`
template specialization (Qt 6.9+ form). `cute_meta.h` provides the
specialization; nothing else changes for callers.

### ARC classes (`arc X { ... }`)

`arc Buffer { ... }` uses Cute's reference-counted class system.
(Like `class`, leading uppercase exports it across modules.) The
C++ surface is `cute::Arc<Buffer>`:

```cpp
#include "foo.h"

void from_cpp() {
    auto buf = cute::Arc<Buffer>(new Buffer(/* ctor args */));
    buf->Push(1.0);
    buf->Push(2.0);
    // refcount drops, destruction follows, when buf goes out of scope
}
```

`cute::Arc<T>` is the same type the codegen uses internally — it's
an intrusive ref-counter (the count lives on the object itself
via `cute::ArcBase`), so you can pass the same value across the
Cute / C++ boundary without bridging.

### Free functions

A `fn Frobnicate(x: Int) Int { ... }` (leading uppercase = exported)
becomes a free function in the generated namespace (currently the file's
module name, e.g. `foo::Frobnicate` for `foo.cute`).

---

## Calling Cute output from a C++ project — worked example

Project layout:

```
my_qt_app/
├── CMakeLists.txt
├── main.cpp                # hand-written, drives the Cute class
└── cute_part/
    ├── counter.cute        # the Cute source
    └── gen/                # `cute build --out-dir gen/` output
        ├── counter.h
        └── counter.cpp
```

`cute_part/counter.cute`:

```ruby
pub class Counter {
  pub prop count : Int, notify: :countChanged
  pub signal countChanged

  pub fn increment {
    count = count + 1
  }
}
```

Generate the C++:

```sh
cute build cute_part/counter.cute --out-dir cute_part/gen/
```

`main.cpp`:

```cpp
#include <QCoreApplication>
#include <QDebug>
#include "counter.h"

int main(int argc, char** argv) {
    QCoreApplication app(argc, argv);

    Counter c;
    QObject::connect(&c, &Counter::countChanged,
                     [&] { qDebug() << "count is now" << c.Count(); });
    c.Increment();   // → "count is now 1"
    c.Increment();   // → "count is now 2"
    return 0;
}
```

`CMakeLists.txt`:

```cmake
cmake_minimum_required(VERSION 3.21)
project(my_qt_app CXX)

# Cute's runtime headers (cute_async.h / cute_error.h) require
# C++20 — coroutines for async/await, three-way comparison on
# Result<T,E>. Set this on the consuming target as well.
set(CMAKE_CXX_STANDARD 20)
find_package(Qt6 REQUIRED COMPONENTS Core)

add_executable(my_qt_app
    main.cpp
    cute_part/gen/counter.cpp
)

target_include_directories(my_qt_app PRIVATE
    cute_part/gen
    ${CUTE_REPO}/runtime/cpp        # path to runtime/cpp/ in the cute repo
)

target_link_libraries(my_qt_app PRIVATE Qt6::Core)
```

That's the whole integration. No moc, no qmake, no `.pro` — just
plain CMake plus four header files copied from Cute's `runtime/cpp/`.

---

## Known gaps

These are missing today.

### `raw_cpp { ... }` escape hatch (Task #28)

There is no inline-C++ form yet. Every C++ snippet has to go
through a `.qpi` declaration and a separate C++ source file. For
a 5-line glue function this is overkill. The proposed item-level
form (no expression-level — too easy to break the type system):

```ruby
raw_cpp_header {
  #include <atomic>
  std::atomic<int> g_counter{0};
}

extern fn bump_counter Int = raw_cpp { return g_counter.fetch_add(1); }
```

Deferred until a real user has a real ask. Until then, write the
glue in a sibling `.cpp` file and bind it via `.qpi`.

### `[cpp] cpp_only_includes`

For libraries whose headers pollute the global namespace, you'd
want their `#include` line in the generated `.cpp` only — not the
`.h`, which transitively pollutes every TU that pulls Cute output.
Today the only `[cpp] includes` knob places them all in the `.h`.
Workaround: hide the noisy include behind a sibling `.cpp`
wrapper.

### Cute → C++ ABI

The names that show up in the emitted `.h` (mangled module-class
form `<module>__<class>`, the ARC wrapper `cute::Arc<T>`, the
format helper `cute::str::format`) are unstable across Cute
versions. Mode-3 callers should treat them as private until we
lock the public API. Tracked under the broader "ABI stability"
item — no concrete plan yet, but the rule is: stable for
uppercase-led (exported) items at the source level, brittle in
their generated C++ form.

---

## File reference

| What | Where |
|---|---|
| Driver pipeline | `crates/cute-driver/src/lib.rs::frontend` |
| `cute.toml` schema | `crates/cute-driver/src/lib.rs::Manifest` |
| `.qpi` parser | `crates/cute-binding/src/lib.rs::parse_qpi` |
| Stdlib bindings | `stdlib/qt/*.qpi` |
| Stdlib typesystems | `stdlib/qt/typesystem/*.toml` |
| `cute-qpi-gen` (binding generator) | `crates/cute-qpi-gen/src/` |
| `qpi-regen` / `qpi-check` recipes | `justfile` |
| Runtime headers | `runtime/cpp/cute_*.h` |
| `build_file` (embed mode) | `crates/cute-driver/src/lib.rs::build_file` |
| CMake template | `crates/cute-driver/src/lib.rs::generate_cmake` |
| moc replacement | `crates/cute-meta/` |
