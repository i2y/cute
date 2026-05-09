# Reference

## Memory model

Cute matches Qt's existing semantics rather than imposing a new model:

| Form | Lifetime | Generated C++ |
|---|---|---|
| `class X { ... }` (default) | Qt parent-tree (auto `new T(this)` in class methods + widget state fields) | raw `T*` |
| `struct X { ... }` (user-defined) | Stack value, copy on assignment, no metaobject, no inheritance | bare `X` |
| `arc X { ... }` | ARC reference counting (final, no signals/slots) | `class X : public ::cute::ArcBase` + `cute::Arc<T>` |
| `T?` (nullable QObject) | Auto-nulls when target dies | `QPointer<T>` |
| `extern value Foo { ... }` (binding) | Stack-allocated value | bare `Foo` (e.g. `QPoint`, `QColor`, `QSize`) |
| `String` / `List<U>` / `Map<K,V>` / ... | Qt implicit sharing (COW) | `QString` / `QList<U>` / `QMap<K,V>` / ... |
| `Slice<T>` | `std::shared_ptr<QList<T>>` keepalive — view never dangles | `cute::Slice<T>` (`arr[a..b]` lowers to `make_slice`) |

Pattern matching catches the common error cases:

```cute
fn parseInt(s: String) !Int { ... }

case parseInt(input) {
  when ok(n)   { println("parsed: #{n}") }
  when err(e)  { println("rejected") }
}
```

---

## Consuming external C++ libraries

Cute has no package manager — instead, drop a `cute.toml` next to your
`.cute` source and Cute will add to its CMake configuration:

```toml
# cute.toml
[bindings]
paths = ["bindings/qtcharts.qpi"]

[cmake]
find_package = ["Qt6 COMPONENTS Charts"]
link_libraries = ["Qt6::Charts"]

[cpp]
includes = ["<QtCharts>"]
```

This works for any C++ library with `find_package` support: vcpkg /
Conan packages, KDE Frameworks, header-only libs. See
[`examples/charts`](https://github.com/i2y/cute/tree/main/examples/charts) for a working QtCharts integration,
and [`docs/CPP_INTEROP.md`](https://github.com/i2y/cute/blob/main/docs/CPP_INTEROP.md) for the full reference
(binding shapes, embed-into-existing-C++ workflow, calling Cute from C++).

The `.qpi` binding files can be **hand-written** for small surfaces,
or **auto-generated from C++ headers** via `cute-qpi-gen`:

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
include = ["info", "warn", "error", "debug", "flush"]
```

```bash
cargo run -p cute-qpi-gen -- --typesystem bindings/spdlog.toml > bindings/spdlog.qpi
```

The generator scrapes `Q_PROPERTY` / `Q_SIGNALS` from header tokens,
expands default arguments into Cute overloads, lowers enums and
`QFlags<...>` to `Int`, and detects inheritance via libclang. Built-in
defaults for `${qt_core}` / `${qt_gui}` / `${qt_widgets}` etc. cover
macOS Homebrew framework layout and Linux distro `/usr/include/qt6/...`
out of the box. `just qpi-regen` regenerates every `.qpi` under
`stdlib/qt/`; `just qpi-check` is the byte-for-byte verification gate.

When the generator detects Qt's `bool *ok = nullptr` out-parameter
pattern (`QString::toInt`, `QByteArray::toDouble`, ...), it emits the
fn with a `@lifted_bool_ok` marker after the return type and lifts the
result into `!T`. At each call site, codegen wraps the call into an
IIFE that owns a fresh `bool _ok` and produces
`Result<T, QtBoolError>` — Cute users get clean
`case bytes.toInt(10) { when ok(n) ... when err(_) ... }` instead of
the manual out-arg plumbing.

---

## Embedding assets — `embed("path")`

Compile-time asset embedding, in the spirit of Go's `//go:embed`:

```cute
let GREETING : ByteArray = embed("greeting.txt")
let LICENSE  : ByteArray = embed("../../LICENSE")

fn main {
  cli_app {
    println("greeting.txt embedded #{GREETING.size()} bytes at compile time.")
  }
}
```

`cute build` reads the file at codegen time and inlines its bytes
as a `static constexpr unsigned char[]` in the generated C++. The
Cute expression evaluates to a zero-copy `QByteArray` (via
`fromRawData`). Paths are relative to the .cute source containing
the call.

`embed` returns `ByteArray` unconditionally; compose for typed
shapes:

```cute
let icon = QImage::fromData(embed("assets/icon.png"))
let cfg  = QString::fromUtf8(embed("config/default.json"))
```

Qt's `.qrc` / `rcc` covers the broader case — a virtual
filesystem with `:/path/…` lookups, translation `.qm`
registration, and large multi-file resource trees. `embed("path")`
is a complementary shape for the simpler case where you just want
one file inlined into source: the path and the resulting type
participate in the same type-check as any other expression, and
there's no separate build step. Reach for `rcc` when you need
runtime resource lookup; reach for `embed` when a static asset
belongs next to the code that uses it. See
[`examples/embed_demo/`](https://github.com/i2y/cute/tree/main/examples/embed_demo).

---

## Sharing Cute code as libraries

Reusable `.cute` code is distributed the same way as any other C++ /
Qt library — via CMake `find_package`. No separate package manager,
no central registry. Cute compiles a library project to a shared lib
+ public header + binding file + cmake config, all installed to
`~/.cache/cute/libraries/<Name>/<version>/<triple>/`.

### Authoring a library

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
  pub prop count : Int, notify: :countChanged, default: 0
  pub signal countChanged
  pub fn increment { count = count + 1 }
}
```

`cute build mylib.cute` (with `[library]` in the manifest) emits a
shared library, a public C++ header, a `.qpi` binding, and a
`<Name>Config.cmake`, then installs them so other projects can find
them. `fn main` and `*_app` intrinsics are rejected — libraries
have no entry point.

### Consuming a library

```toml
# myapp/cute.toml
[cute_libraries]
deps = ["MyLib"]
```

```cute
# myapp/main.cute
fn main { cli_app { let c = MyLibCounter.new(); c.increment() } }
```

`cute build` resolves each dep through the install cache, loads the
library's `.qpi` so the type checker sees its surface, auto-adds
`find_package(MyLib)` + `target_link_libraries(... MyLib::MyLib)`
to the generated CMakeLists, and bakes the install path into the
binary's `INSTALL_RPATH`. Result: `./myapp` runs without
`DYLD_LIBRARY_PATH` / `LD_LIBRARY_PATH` env.

### Fetching libraries — `cute install`

```bash
cute install /path/to/mylib                  # local dir, build in place
cute install https://github.com/x/y.git@v0.2 # git clone + build (rev optional)
cute install                                 # walk cwd's cute.toml,
                                             # install every spec
```

The detailed-spec form lets a consumer pin a remote source:

```toml
# myapp/cute.toml
[cute_libraries.MyLib]
git = "https://github.com/foo/mylib"
rev = "v0.2.0"        # tag / branch / commit; optional

[cute_libraries.LocalLib]
path = "../local-lib" # for side-by-side dev
```

`cute install` (no args) walks every `[cute_libraries.<Name>]` spec
in the current directory's manifest and installs each in turn.

See [`examples/lib_counter/`](https://github.com/i2y/cute/tree/main/examples/lib_counter) (library) +
[`examples/lib_counter_app/`](https://github.com/i2y/cute/tree/main/examples/lib_counter_app) (consumer)
for the full, runnable round-trip.

**Not yet covered**: lock files, transitive resolver, ABI versioning,
`cute uninstall`. Library updates require re-building consumers.
