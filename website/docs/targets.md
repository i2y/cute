# App targets

Calculator (in [Anatomy of a Cute app](anatomy.md)) is one shape
Cute compiles to. The same `class` / `prop` / `style` / binding
mechanics apply across other targets — only the top-level wrapper
element + `fn main` intrinsic change.

| Target | Wrapper | `fn main` | Demo |
|---|---|---|---|
| QML / Material / Plasma 6 | `view Main { ApplicationWindow { ... } }` | (auto, synthesised from `view`) | [`examples/counter`](https://github.com/i2y/cute/tree/main/examples/counter) |
| QtWidgets desktop | `widget Main { QMainWindow { ... } }` | (auto, synthesised from `widget`) | [`examples/calculator`](https://github.com/i2y/cute/tree/main/examples/calculator) |
| KDE Kirigami (mobile-ready) | `view Main { Kirigami.ApplicationWindow { ... } }` | (auto) | [`examples/calculator_kirigami`](https://github.com/i2y/cute/tree/main/examples/calculator_kirigami) |
| Charts / data-vis | `widget Main { QChartView { ... } }` | (auto) | [`examples/charts`](https://github.com/i2y/cute/tree/main/examples/charts) |
| GPU canvas (cute_ui) | `widget Main { Column { ... } }` | `gpu_app(window: Main, theme: light)` | [`examples/gpu_notes`](https://github.com/i2y/cute/tree/main/examples/gpu_notes) |
| HTTP server / REST | (no wrapper) | `server_app { QHttpServer.new() ... }` | [`examples/http_hello`](https://github.com/i2y/cute/tree/main/examples/http_hello) |
| CLI tool with typed flags | (no wrapper) | `fn main(args: List) { cli_app { QCommandLineParser ... } }` | [`examples/cli_args`](https://github.com/i2y/cute/tree/main/examples/cli_args) |

Same `cute build foo.cute && ./foo` workflow for every row.

## `fn main` intrinsics

The compiler picks the wrapper from the shape of `fn main` (or
auto-synthesises one when a `view` / `widget` exists at top level
and no explicit `fn main` is written):

| Intrinsic | Wraps body in | Use for |
|---|---|---|
| `qml_app(view: Main)` (auto when a `view` exists) | `QGuiApplication` + `QQmlApplicationEngine` + qrc-bundled QML | QtQuick / Material / Plasma 6 / Kirigami GUI |
| `widget_app(window: Main)` (auto when a `widget` exists) | `QApplication` + `<Main> w; w.show();` | QtWidgets / OS-native GUI |
| `gpu_app(window: Main, theme: light)` | `cute::ui::App` + `cute::ui::Window` (QRhi + Canvas Painter) | GPU-accelerated UI without QML / Widgets — see below |
| `server_app { ... }` | `QCoreApplication` + body + `app.exec()` | HTTP server, signal/timer-driven services |
| `cli_app { ... }` | `QCoreApplication` + body + `return 0;` (sync body) **or** body lifted into `QFuture<void>` coroutine + `app.exec()` (when body uses `await`) | CLI tools — async-aware |
| (none — generic main) | `int main(...)` only | Plain batch processing |

`fn main` with a single parameter (`fn main(args: List) { ... }`)
automatically lifts `argc/argv` into a `QStringList` bound to that
parameter — works with any of the above intrinsics.

## `server_app` — HTTP server in 22 lines

```cute
fn main {
  server_app {
    let server = QHttpServer.new()

    server.route("/") {
      "Hello from Cute!\n"
    }

    server.route("/cute") {
      "Cute is a general-purpose Qt language.\n"
    }

    let tcp = QTcpServer.new()
    tcp.listen()
    server.bind(tcp)

    println("listening on http://localhost:#{tcp.serverPort}")
  }
}
```

```sh
$ cute build http_hello.cute && ./http_hello
listening on http://localhost:55060
$ curl http://localhost:55060/
Hello from Cute!
```

`QHttpServer` (Qt 6.4+) is in scope without imports. Routes are
trailing-block lambdas returning strings (or `QHttpServerResponse`
for status codes / headers). Qt6::HttpServer + Qt6::Network are
linked automatically. Same Qt event loop drives QML and widgets.

## `gpu_app` — custom-rendered UI (`cute_ui` runtime)

When you want a custom-rendered UI without pulling in QML / V4 /
the JS engine or QtWidgets' CPU painter, Cute ships a small
reactive UI runtime (`cute_ui`) that renders through Qt 6.11's
[Canvas Painter](https://doc.qt.io/qt-6/qcanvaspainter.html)
(Tech Preview) on top of QRhi. The widget DSL stays the same —
you write `widget Main { Column { ... } }` and codegen targets
cute_ui classes instead of QtWidgets when `fn main` calls
`gpu_app(...)`.

```cute
widget Main {
  state count : Int = 0
  Column {
    Text { text: "Count: #{count}" }
    Button { text: "+1"; onClick: { count = count + 1 } }
  }
}

fn main { gpu_app(window: Main, theme: light) }
```

```sh
$ cute install-cute-ui          # one-time: builds + installs the runtime
$ cute build counter.cute && ./counter
```

Runtime state — caret position, scroll offset, button press /
hover, focus, theme tween — survives reactive rebuilds via
positional state transfer; no manual diff bookkeeping. Cmd /
Ctrl + T toggles the theme live.

| Widget | Notes | Demo |
|---|---|---|
| `Text` / `Button` | Themed colors, hover + press tweens | [`examples/gpu_notes`](https://github.com/i2y/cute/tree/main/examples/gpu_notes) |
| `TextField` | Caret blink, selection, copy/paste, IME (`QInputMethodEvent`) | [`examples/gpu_notes`](https://github.com/i2y/cute/tree/main/examples/gpu_notes) |
| `Image` / `Svg` | `QImage` / `QSvgRenderer` rasterized into a `QCanvasImage` | [`examples/gpu_svg`](https://github.com/i2y/cute/tree/main/examples/gpu_svg) |
| `ListView` / `ScrollView` / `HScrollView` | Vertical / horizontal scroll, scrollbar thumb, eased ~120ms | [`examples/gpu_scroll`](https://github.com/i2y/cute/tree/main/examples/gpu_scroll) |
| `DataTable` | Header row + alternating stripes + dividers | [`examples/gpu_table`](https://github.com/i2y/cute/tree/main/examples/gpu_table) |
| `Modal` | Dim overlay + centered surface + click-outside swallow | [`examples/gpu_modal`](https://github.com/i2y/cute/tree/main/examples/gpu_modal) |
| `BarChart` / `LineChart` | Per-bar / per-point eased data swap | [`examples/gpu_chart`](https://github.com/i2y/cute/tree/main/examples/gpu_chart) |
| `ProgressBar` / `Spinner` | Determinate fill + indeterminate arc | [`examples/gpu_progress`](https://github.com/i2y/cute/tree/main/examples/gpu_progress) |

`gpu_app` is **Qt 6.11+ only** (Canvas Painter is new there) and
on macOS additionally needs `brew install qtcanvaspainter` until
KDE Craft picks the module up. `cute install-cute-ui` builds and
installs the runtime as a static lib once per machine. Linux uses
Qt's Vulkan QRhi backend (auto-wired via `QVulkanInstance`); the
Qt build must have Vulkan support enabled (default on most distro
packages).

`widget` over a cute_ui root (`Column`, `Row`, ...) gets a
**Flutter / Castella-style stateful / stateless split** that QML
and QtWidgets don't have: `Component` is your StatefulWidget, the
`Element` tree it returns from `build()` is disposable, and a
`transferStateFrom` walk preserves caret position / scroll offset
/ press / hover / focus tweens across rebuilds. See
[`docs/CUTE_UI.md`](https://github.com/i2y/cute/blob/main/docs/CUTE_UI.md)
for the architecture deep-dive and the cross-framework comparison
table.

## KDE Frameworks beyond Kirigami

Bundled `.qpi` bindings cover several KF6 modules (KConfig, KI18n,
KNotifications, KCoreAddons, KIO, KItemModels) — runtime libraries
any of the targets above can pull in via `cute.toml`'s `[cmake]`
section. Three demos illustrate the surfaces:

| Demo | What it shows |
|---|---|
| [`examples/kf6_config`](https://github.com/i2y/cute/tree/main/examples/kf6_config) | Persistent counter via `KSharedConfig.openConfig(...)` + `KConfigGroup.readEntryInt` / `writeEntryInt` |
| [`examples/kf6_i18n`](https://github.com/i2y/cute/tree/main/examples/kf6_i18n) | Translation lookups via `KLocalizedString.i18n` / `i18nc` / `i18np` / `i18ncp` + the deferred `ki18n(...).subs(...).toString()` chain |
| [`examples/kf6_notifications`](https://github.com/i2y/cute/tree/main/examples/kf6_notifications) | Desktop notification via `KNotification` + `QTimer.singleShot { QCoreApplication.quit() }` (both static-fn calls; see [Language tour › Static methods](tour.md#static-methods--static-fn)) |

Linking is per-module: each demo's `cute.toml` declares the
relevant `find_package` / `link_libraries` (`KF6Config`,
`KF6I18n`, `KF6Notifications`). Build prerequisites are the KDE
Frameworks 6 dev packages — `sudo zypper install kf6-config-devel
kf6-i18n-devel kf6-notifications-devel` on Tumbleweed, `craft
kconfig ki18n knotifications` via [Craft](https://community.kde.org/Craft)
on macOS.
