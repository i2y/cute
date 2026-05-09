# Anatomy of a Cute app

The screenshot below is `examples/calculator/calculator.cute` —
one file, 219 lines, that produces a fully-themed QtWidgets
calculator binary. The walkthrough that follows dissects that
file part by part: a QObject model class, a pure compute helper,
the style palette, and the widget tree that wires them together.

![Cute Calculator on openSUSE Tumbleweed (KDE Plasma 6)](images/calculator.png)

Concepts that come along for the ride: `class` defaulting to
QObject, `prop` auto-deriving its NOTIFY signal, `var ( ... )`
block-form sibling fields, bare member access in class methods,
string interpolation with format specs, top-level `fn`, `style`
blocks with QSS-shorthand keys, style composition with `+`, and
the `widget Main { ... }` app intrinsic.

## Model class — `class Calculator`

```cute
class Calculator {
  pub prop display : String, default: ""

  var (
    current       : Float  = 0.0
    decimalFactor : Float  = 0.0
    acc           : Float  = 0.0
    op            : String = ""
    fresh         : Bool   = false
  )

  pub fn digit(d: Int) {
    if fresh {
      current = 1.0 * d
      decimalFactor = 0.0
      fresh = false
    } else {
      if decimalFactor == 0.0 {
        current = current * 10.0 + 1.0 * d
      } else {
        current = current + 1.0 * d * decimalFactor
        decimalFactor = decimalFactor * 0.1
      }
    }
    display = "#{current:.10g}"
  }

  # pub fn dot / pressOp / equals / clear / negate — see Full source
}
```

- **`class X { ... }` defaults to `class X < QObject`.** Cute
  classes extend QObject by default — you get signals, slots,
  properties, and Qt parent-tree ownership for free. Spell the
  super only when overriding (`class Foo < SomeOtherBase`).
- **`pub prop display : String, default: ""` declares a
  Q_PROPERTY** with an auto-generated getter / setter (`display()` /
  `setDisplay(v)`) and a NOTIFY signal whose name is conventional:
  `displayChanged`. Writing `display = "..."` inside a method
  routes through the setter, which dirty-checks and emits the
  signal — no manual `notify:`, `pub signal`, or `emit`.
- **`var ( ... )` is the block form for sibling fields.** Each
  line declares one private mutable field; visually grouping them
  is a typographic nicety. These are plain mutable state owned by
  the class, not Q_PROPERTYs.
- **Bare member access in class methods.** `current = current *
  10.0 + 1.0 * d` reads and writes the `current` field directly —
  no `@field` sigil, no `self.` prefix. The compiler walks each
  class method body and resolves bare identifiers that match a
  member name to that member.
- **Reads vs writes.** A read like `current` lowers to the trivial
  accessor (back to `m_current` in optimised builds). A write like
  `display = "..."` lowers to `setDisplay("...")`, which runs the
  auto-generated setter, dirty-checks, and fires `displayChanged`.
- **`"#{current:.10g}"` is string interpolation with format specs.**
  `:.10g` is a Python-style mini-language (general format, 10-digit
  precision). Lowers to a `cute::str::format` chain that returns a
  `QString`.

## Pure helpers — `fn compute`

```cute
fn compute(a: Float, op: String, b: Float) Float {
  if op == "+" { return a + b }
  if op == "-" { return a - b }
  if op == "*" { return a * b }
  if op == "/" {
    if b == 0.0 { return 0.0 }
    return a / b
  }
  b
}
```

- **Top-level `fn` is a free function.** No class, no QObject
  overhead, no metaobject machinery. Use these for pure compute,
  helpers, and parsers that don't belong to an object.
- **Return type goes after the parameter list** — `fn compute(a:
  Float, ...) Float { ... }`. No `->` arrow. Void fns omit the
  type entirely (`fn shutdown { ... }`).
- **Implicit return of the last expression.** The trailing `b`
  (no `return` keyword) is the fallthrough return value when `op`
  matched none of the four cases. Cute fn bodies are expressions;
  the last value falls out as the return.
- **No error union here on purpose.** `compute` picks "0.0 on
  divide-by-zero" rather than surfacing an error, because the
  display has no slot for one. Error-aware code would declare
  `fn compute(...) !Float` and call sites would `case compute(...)
  when ok(n) ... when err(e) ...` — see
  [Pattern matching with error unions](tour.md#pattern-matching-with-error-unions)
  in the Language tour.

## Style declarations

```cute
style BtnBase {
  minimumWidth: 64
  minimumHeight: 64
  border: "none"
  borderRadius: 32
}

style NumLook {
  background: "#333333"
  color: "#ffffff"
  fontSize: 26
  fontWeight: 500
  hover.background: "#3d3d3d"
  pressed.background: "#555555"
}

style OpLook {
  background: "#ff9f0a"
  color: "#ffffff"
  fontSize: 30
  fontWeight: 600
  hover.background: "#ffae33"
  pressed.background: "#cc7f08"
}

style NumBtn = BtnBase + NumLook
style OpBtn  = BtnBase + OpLook
```

- **`style X { key: value; ... }` is a compile-time bag of
  (key, value) entries.** Reference one from a UI element with
  `style: X` (you'll see this in the widget tree below) and the
  entries are inlined at codegen time — no runtime cost, no
  runtime registry, no QSS strings authored by hand.
- **QtWidgets path: QSS shorthand.** Recognised keys (`color`,
  `background`, `borderRadius`, `fontSize`, `fontWeight`,
  `padding*`, `margin*`, `border*`, `textAlign`) collapse into a
  single `setStyleSheet(...)` call per element at codegen.
  Length-typed keys auto-suffix `px` (`borderRadius: 32` →
  `border-radius: 32px`); numeric keys (`fontWeight: 500`) pass
  through; bare strings emit verbatim.
- **Pseudo-class prefixes: `hover.`, `pressed.`, `focus.`,
  `disabled.`, `checked.`** lower into `:hover`, `:pressed`, etc.
  selectors. So `hover.background: "#3d3d3d"` becomes a
  `:hover { background: #3d3d3d }` rule under the hood.
- **Real `QWidget` setters (`minimumWidth`, `minimumHeight`,
  `border`)** fall through to the regular `setX(...)` path, so
  shorthand and genuine setters coexist in the same block.
  `minimumWidth: 64` lowers to `btn->setMinimumWidth(64)`.
- **`style NumBtn = BtnBase + NumLook` composes two styles.**
  Right-wins merge — when both name the same key, the right side
  overrides. Sizing lives in `BtnBase` (shared across all
  buttons); per-variant look (Number vs Function vs Operator)
  lives in `NumLook` / `FnLook` / `OpLook`. `+` lets you mix and
  match without copy-paste.
- **`cute check` validates each entry against the expected shape.**
  `borderRadius: true` and `color: 42` are rejected up-front with
  a typed diagnostic, not silently as broken QSS at runtime.
- **Other paths.** The QML (`view`) path lowers each entry to a
  literal QML property line (`color: "#1a1a1a"` becomes
  `color: "#1a1a1a"` in the generated `.qml`). The cute_ui
  (`gpu_app`) path lowers each entry to a setter call on the
  cute_ui Element type. See
  [Style declarations](tour.md#style-declarations) in the
  Language tour for the cross-path comparison.

## Widget tree

```cute
widget Main {
  let calc = Calculator()

  QMainWindow {
    style: Window

    QVBoxLayout {
      spacing: 10

      QLabel {
        style: Display
        text: calc.display
      }

      QHBoxLayout {
        spacing: 10
        QPushButton { style: FnBtn; text: "C"; onClicked: calc.clear() }
        QPushButton { style: FnBtn; text: "±"; onClicked: calc.negate() }
        QPushButton { style: OpBtn; text: "÷"; onClicked: calc.pressOp("/") }
        QPushButton { style: OpBtn; text: "×"; onClicked: calc.pressOp("*") }
      }

      # ... 4 more rows: 7-9 / 4-6 / 1-3 / 0 . — see Full source
    }
  }
}
```

- **`widget Main { ... }` is the QtWidgets app intrinsic.** When
  the compiler sees a `widget` block, it auto-synthesizes an
  `int main(int argc, char **argv)` that constructs a
  `QApplication`, instantiates `Main`, calls `Main::show()`, and
  runs `app.exec()`. No `main.cpp` to write yourself.
- **`let calc = Calculator()` inside the widget body** owns one
  `Calculator` instance for the lifetime of `Main`. The Qt
  parent-tree wires `Main` as `calc`'s parent automatically
  (because `Main` is a QObject and we're in its body), so `calc`'s
  lifetime tracks the window's. No manual `delete`, no dangling
  pointer.
- **`QMainWindow { ... }` opens the widget tree.** Children nest
  by syntactic containment: `QMainWindow → QVBoxLayout → (QLabel +
  5 × QHBoxLayout)`. Codegen turns each block into
  `new QMainWindow(this)`, `new QVBoxLayout(...)`,
  `vbox->addLayout(hbox)`, `vbox->addWidget(label)`, etc. Layouts
  flex by default — `QHBoxLayout` distributes children evenly
  across the width; no explicit sizing math.
- **`style: Display` and `style: NumBtn`** apply the `style`
  blocks from the previous section. The QSS shorthand in those
  blocks is collapsed into one `setStyleSheet(...)` per element
  at codegen.
- **`text: calc.display` is a binding.** Cute spots that the
  right-hand side reads a `prop`, hooks the auto-derived
  `displayChanged` signal, and re-evaluates the binding whenever
  the prop changes. So when `digit(7)` runs `display = "..."`,
  the label refreshes automatically. No manual `connect(...)`,
  no `Q_DECLARE_METATYPE`.
- **`onClicked: calc.digit(7)` connects the button's `clicked()`
  signal** to a slot that calls `calc.digit(7)`. The compiler
  emits the connection directly into the generated C++ — no
  `moc`, no `Q_OBJECT` macro to maintain, no manual
  `connect(button, &QPushButton::clicked, calc, ...)`.

## Full source

The four sections above are slices. The complete file is right
below — every method of `class Calculator`, all six `style` blocks,
and all five widget rows.

<details>
<summary>Full source — <code>examples/calculator/calculator.cute</code> (219 lines)</summary>

```cute
# Cute on QtWidgets — calculator with proper decimal input.
# QHBoxLayout / QVBoxLayout flex by default on QtWidgets, so the
# buttons fill their rows evenly without explicit sizing.

class Calculator {
  pub prop display : String, default: ""

  # Internal state machine. Block form groups the sibling vars; the
  # readable surface for outside callers is the `display` prop
  # above. These track the next-keystroke decision (current
  # accumulator, pending op, decimal-input position, fresh-after-op
  # flag).
  var (
    current : Float = 0.0
    decimalFactor : Float = 0.0
    acc : Float = 0.0
    op : String = ""
    fresh : Bool = false
  )
  pub fn digit(d: Int) {
    if fresh {
      current = 1.0 * d
      decimalFactor = 0.0
      fresh = false
    } else {
      if decimalFactor == 0.0 {
        current = current * 10.0 + 1.0 * d
      } else {
        current = current + 1.0 * d * decimalFactor
        decimalFactor = decimalFactor * 0.1
      }
    }
    display = "#{current:.10g}"
  }

  pub fn dot {
    if fresh {
      current = 0.0
      fresh = false
    }
    if decimalFactor == 0.0 {
      decimalFactor = 0.1
      display = "#{current:.10g}."
    }
  }

  pub fn pressOp(o: String) {
    if op == "" {
      acc = current
    } else {
      acc = compute(acc, op, current)
    }
    op = o
    fresh = true
    decimalFactor = 0.0
    display = "#{acc:.10g}"
  }

  pub fn equals {
    # No pending op = nothing to compute. Re-fire the change signal
    # so any listener that missed the last update can refresh; the
    # auto-emit doesn't kick in here because there's no `display =`
    # write in this branch.
    if op == "" {
      emit displayChanged
    } else {
      acc = compute(acc, op, current)
      op = ""
      fresh = true
      decimalFactor = 0.0
      display = "#{acc:.10g}"
    }
  }

  pub fn clear {
    current = 0.0
    acc = 0.0
    op = ""
    fresh = true
    decimalFactor = 0.0
    display = "0"
  }

  pub fn negate {
    current = 0.0 - current
    display = "#{current:.10g}"
  }
}

fn compute(a: Float, op: String, b: Float) Float {
  if op == "+" { return a + b }
  if op == "-" { return a - b }
  if op == "*" { return a * b }
  if op == "/" {
    if b == 0.0 { return 0.0 }
    return a / b
  }
  b
}

# Compile-time style palette. On the widget path the recognised
# QSS-shorthand keys (`color`, `background`, `borderRadius`,
# `fontSize`, `fontWeight`, `padding*`, `margin*`, `border*`,
# `textAlign`) plus pseudo-class prefixes (`hover.X`, `pressed.X`,
# `focus.X`, `disabled.X`, `checked.X`) collapse into a single
# `setStyleSheet(...)` call at codegen time — no QSS strings in
# user code, no objectName tagging, no cascade-specificity tricks.
# Length-typed values auto-suffix `px`; non-length numeric values
# (e.g. `fontWeight`) pass through.

style BtnBase {
  minimumWidth: 64
  minimumHeight: 64
  border: "none"
  borderRadius: 32
}

style NumLook {
  background: "#333333"
  color: "#ffffff"
  fontSize: 26
  fontWeight: 500
  hover.background: "#3d3d3d"
  pressed.background: "#555555"
}

style FnLook {
  background: "#a5a5a5"
  color: "#000000"
  fontSize: 22
  fontWeight: 600
  hover.background: "#b8b8b8"
  pressed.background: "#d4d4d4"
}

style OpLook {
  background: "#ff9f0a"
  color: "#ffffff"
  fontSize: 30
  fontWeight: 600
  hover.background: "#ffae33"
  pressed.background: "#cc7f08"
}

style NumBtn = BtnBase + NumLook

style FnBtn  = BtnBase + FnLook

style OpBtn  = BtnBase + OpLook

style Display {
  color: "#ffffff"
  background: "transparent"
  fontSize: 56
  fontWeight: 200
  paddingTop: 24
  paddingBottom: 8
  paddingLeft: 18
  paddingRight: 18
  textAlign: "right"
}

style Window {
  windowTitle: "Cute Calculator"
  minimumWidth: 340
  minimumHeight: 520
  background: "#1c1c1e"
}

widget Main {
  let calc = Calculator()

  QMainWindow {
    style: Window

    QVBoxLayout {
      spacing: 10

      QLabel {
        style: Display
        text: calc.display
      }

      QHBoxLayout {
        spacing: 10
        QPushButton { style: FnBtn; text: "C"; onClicked: calc.clear() }
        QPushButton { style: FnBtn; text: "±"; onClicked: calc.negate() }
        QPushButton { style: OpBtn; text: "÷"; onClicked: calc.pressOp("/") }
        QPushButton { style: OpBtn; text: "×"; onClicked: calc.pressOp("*") }
      }
      QHBoxLayout {
        spacing: 10
        QPushButton { style: NumBtn; text: "7"; onClicked: calc.digit(7) }
        QPushButton { style: NumBtn; text: "8"; onClicked: calc.digit(8) }
        QPushButton { style: NumBtn; text: "9"; onClicked: calc.digit(9) }
        QPushButton { style: OpBtn;  text: "−"; onClicked: calc.pressOp("-") }
      }
      QHBoxLayout {
        spacing: 10
        QPushButton { style: NumBtn; text: "4"; onClicked: calc.digit(4) }
        QPushButton { style: NumBtn; text: "5"; onClicked: calc.digit(5) }
        QPushButton { style: NumBtn; text: "6"; onClicked: calc.digit(6) }
        QPushButton { style: OpBtn;  text: "+"; onClicked: calc.pressOp("+") }
      }
      QHBoxLayout {
        spacing: 10
        QPushButton { style: NumBtn; text: "1"; onClicked: calc.digit(1) }
        QPushButton { style: NumBtn; text: "2"; onClicked: calc.digit(2) }
        QPushButton { style: NumBtn; text: "3"; onClicked: calc.digit(3) }
        QPushButton { style: OpBtn;  text: "="; onClicked: calc.equals() }
      }
      QHBoxLayout {
        spacing: 10
        QPushButton { style: NumBtn; text: "0"; onClicked: calc.digit(0) }
        QPushButton { style: NumBtn; text: "."; onClicked: calc.dot() }
      }
    }
  }
}
```

</details>
