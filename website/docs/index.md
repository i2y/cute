---
hide:
  - navigation
  - toc
---

# Cute

## The language dedicated to the Qt and KDE ecosystem

A **statically typed**, **compiled** language designed for Qt
and KDE — first-class properties, signals, slots, and reactive
bindings. QtQuick desktop, KDE Plasma 6, Kirigami mobile, Qt for
WebAssembly, embedded Linux, automotive HMIs, HTTP servers, CLI
tools — one source, anywhere Qt runs.

[Quick start](installation.md){ .md-button .md-button--primary }
[Anatomy of a Cute app](anatomy.md){ .md-button }
[Examples](https://github.com/i2y/cute/tree/main/examples){ .md-button }
[GitHub](https://github.com/i2y/cute){ .md-button }

![Cute Calculator on openSUSE Tumbleweed (KDE Plasma 6)](images/calculator.png)

*[`examples/calculator`](https://github.com/i2y/cute/tree/main/examples/calculator)
— a QtWidgets calculator running natively on KDE Plasma 6. One
`.cute` file, 219 lines: model class + style blocks + widget tree.
The dark Material-style theme is declared with `style { ... }`
blocks; no hand-written `setStyleSheet(...)`.*

A small slice — one style block and the top of the widget tree:

```cute
style NumBtn {
  background: "#333333"
  color: "#ffffff"
  fontSize: 26
  borderRadius: 32
  hover.background:   "#3d3d3d"
  pressed.background: "#555555"
}

widget Main {
  let calc = Calculator()
  QMainWindow {
    QVBoxLayout {
      QLabel { style: Display; text: calc.display }
      QHBoxLayout {
        QPushButton { style: NumBtn; text: "7"; onClicked: calc.digit(7) }
        QPushButton { style: NumBtn; text: "8"; onClicked: calc.digit(8) }
        QPushButton { style: NumBtn; text: "9"; onClicked: calc.digit(9) }
        QPushButton { style: OpBtn;  text: "−"; onClicked: calc.pressOp("-") }
      }
      # ...
    }
  }
}
```

```sh
$ cute build calculator.cute   # → ./calculator
$ ./calculator
```

Full source and a part-by-part walkthrough in [Anatomy of a Cute app](anatomy.md).

---

## Why Cute?

<div class="grid cards" markdown>

-   :zap: __Qt's binding system, woven in__

    `pub prop count : Int, default: 0` is a reactive cell wired
    into Qt's binding system. Inputs propagate to derived values,
    derived values propagate to bound QML / widget views —
    automatically, per-property.

-   :earth_africa: __Anywhere Qt runs__

    KDE Plasma 6, Kirigami mobile, Qt for WebAssembly, Qt for
    Automotive, embedded Linux, traditional desktop. The same
    model classes drive every surface; pick the front-end per
    `fn main`.

-   :battery: __Qt is the standard library__

    `String` is `QString`, `List<T>` is `QList<T>`, `HttpServer`
    is `QHttpServer`. Everything Qt provides — `QSqlDatabase`,
    `QJsonDocument`, `QtConcurrent`, `QCommandLineParser` — is in
    scope without imports or wrappers.

-   :shield: __Statically typed, safe by default__

    Every value carries a compile-checked type. `!T` error unions
    plus exhaustive pattern matching surface missing cases ahead
    of time. Generics preserve safety across module boundaries;
    `~Copyable` types catch use-after-move.

</div>

---

## Where next

<div class="grid cards" markdown>

-   :rocket: __[Installation](installation.md)__

    Get `cute` on your `PATH` in three commands. macOS (Homebrew)
    and Linux (Tumbleweed / Fedora rawhide / Debian sid / Arch).

-   :microscope: __[Anatomy of a Cute app](anatomy.md)__

    Walk through `examples/calculator` part by part — model class,
    style blocks, widget tree, full source.

-   :package: __[App targets](targets.md)__

    Same `class` / `prop` / `style` mechanics across QML / Kirigami /
    GPU canvas / HTTP server / CLI tool.

-   :book: __[Language tour](tour.md)__

    Every language feature — reactive props, `ModelList`, pattern
    matching, generics, traits, async, slicing, tests, multi-file.

-   :hammer_and_wrench: __[Toolchain](toolchain.md)__

    `cute build` / `check` / `test` / `fmt` / `watch` / `doctor` /
    `cute-lsp` — one binary, one `cute.toml`.

-   :books: __[Reference](reference.md)__

    Memory model, C++ interop, asset embedding, library sharing.

</div>

---

_Cute is an independent, community-developed project. It is not
affiliated with, endorsed by, or sponsored by The Qt Company /
Qt Group, the Qt Project, or KDE e.V. "Qt" is a trademark of
The Qt Company; "KDE" / "Plasma" / "Kirigami" are trademarks of
KDE e.V._
