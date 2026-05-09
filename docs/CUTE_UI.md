# cute_ui — GPU-accelerated UI runtime

`cute_ui` is the third UI substrate Cute can target, alongside QML
(`view`) and QtWidgets (`widget` with a Qt class root). It exists to
support `widget` declarations whose root is one of cute_ui's own
container classes (`Column`, `Row`, ...). Selection happens
automatically: `cute build` looks at the root class of each `widget`,
and if it's a cute_ui name it lowers to `cute::ui::Component`
subclasses; otherwise it lowers to QtWidgets as before. The `fn main`
intrinsic — `gpu_app(window: Main, theme: light)` — boots a
`cute::ui::App` (a thin `QGuiApplication`) that runs a custom render
loop with no QML / V4 / QtWidgets dependency.

This doc covers the parts that aren't obvious from the README's
[GPU-accelerated UI section](../README.md#gpu-accelerated-ui-gpu_app--cute_ui-runtime).

## Why a third UI path

| | QtQuick (`view`) | QtWidgets (`widget` over Qt) | cute_ui (`widget` over Column / Row / ...) |
|--|--|--|--|
| Module deps | `Qt6::Qml` + `Qt6::Quick` + V4 JS engine | `Qt6::Widgets` + CPU `QPainter` | `Qt6::Gui` + `Qt6::CanvasPainter` (Tech Preview) |
| Renderer | scene graph, `QSGNode` | CPU rasterizer | QRhi (Metal / D3D12 / Vulkan / OpenGL ES) |
| Output binary size | large (V4) | medium | small (~3-5 MB lib) |
| Reactive layer | property bindings + JS | none — manual `update()` | Component → build() → Element diff |

QtQuick gives you the richest GUI ecosystem but ships V4 and Qt Quick
Controls; QtWidgets is light but CPU-painted with no reactive model.
cute_ui plugs the gap: GPU-accelerated, reactive, no JS, ~5 MB. The
tradeoff is **Qt 6.11+ only** because Canvas Painter is new there.

## Widget model: Flutter / Castella / QtWidgets / QML / cute_ui

This is the part that's confused enough people that it deserves its
own table. Qt by itself has no equivalent of Flutter's
`StatelessWidget` / `StatefulWidget` split — both QtWidgets and QML
treat every widget as a long-lived stateful object. cute_ui
intentionally adopts the Flutter / Castella split (it's the model
that makes `if cond { Modal { ... } }` and incremental list updates
fall out for free) and layers it on top of `QObject`.

|  | Flutter / Castella | QtWidgets | QML / QtQuick | cute_ui |
|--|--|--|--|--|
| Widget identity | `StatelessWidget` / `StatefulWidget` (immutable description) | `QWidget` instance (mutable, lives forever) | `QQuickItem` instance (mutable, lives forever) | `Component` subclass (`QObject`) — instance lives, but its `build()` output is disposable |
| What `build()` returns | a fresh `Widget` tree | (n/a — the QWidget *is* the painted object) | (n/a — the item tree *is* the painted scene) | a fresh `unique_ptr<Element>` tree |
| Where state lives | inside `State<T>`, which the Element keeps alive across rebuilds | as `QWidget` member fields | as Item properties + JS-driven internal state | `prop` / state-field on `Component`; transient render state (caret, scroll, focus) on `Element` |
| Rebuild trigger | `setState()` → framework re-runs `build` | manual `update()`; never structural rebuild | property change → binding system re-evaluates | state-field's notify signal → codegen-emitted `connect(... requestRebuild)` |
| Transient state on rebuild | preserved by Element diff (key / type) | n/a (no rebuild) | n/a (no rebuild) | preserved by `Element::transferStateFrom` walk |

So when you write

```ruby
widget Main {
  let store = Store()
  Column {
    Text { text: store.message }
    Button { text: "click"; onClick: store.bump() }
  }
}
```

the mental model is "this is a `StatefulWidget` whose State holds
`store`". `Main` itself is a `Component` subclass that lives for the
lifetime of the window; `store.bump()` modifies state, fires a
notify signal that codegen wired to `requestRebuild()`, and on the
next event loop tick `build()` runs again to produce a fresh
`Element` tree. Because the new tree is positionally similar to the
old one, the Button's `pressed_` flag, the TextField's caret
position, and the ListView's scroll offset all transfer over via
`transferStateFrom`. That is the cute_ui equivalent of Flutter's
`Element` keeping `State<T>` alive while throwing the Widget away.

## Component / Element / Window split

```
cute::ui::Component (QObject subclass — your `widget` lowers to this)
      │
      │  std::unique_ptr<Element> build() override
      ▼
cute::ui::Element  (root of one render tree)
      │  Yoga node, frame, optional onClick, paint(PaintCtx&)
      └─ children: vector<unique_ptr<Element>>

cute::ui::Window (QWindow subclass)
      │  owns QRhi swap chain + QCanvasPainterFactory + BuildOwner
      ▼
  ─ exposeEvent / mousePressEvent / keyPressEvent / wheelEvent
  ─ renderFrame: layout → YGNodeCalculateLayout → paint
```

The asymmetry is on purpose:

- `Component` is `QObject`-derived so it slots into Qt's signal /
  slot world (state-field props with `notify:` work because the
  Component itself owns the moc data).
- `Element` is plain C++ owned through `unique_ptr`. Lifetime is
  decoupled from the Qt parent-tree, which lets `build()` rebuild
  the entire tree cheaply each tick.
- `Window` knows about both: it owns the `BuildOwner` queue, calls
  `Component::rebuildSelf` lazily, and runs Yoga + paint over the
  current `Element` tree.

## Reactivity: how a click becomes a redraw

```
Button onClick: store.add()                        (you wrote this)
        │
        ▼
ButtonElement::dispatchClick
        │  fires onClick lambda → store->add()
        ▼
Store::add():  m_count = m_count + 1; emit count_changed
        │
        ▼
QObject::connect(store, &Store::count_changed,
                 this, [this]{ requestRebuild(); })   (codegen emitted in the Component ctor)
        │
        ▼
Component::requestRebuild → BuildOwner::scheduleBuild → Window::requestUpdate
        │
        ▼
Next renderFrame: build() runs, transferStateRecursive copies caret
/ scroll / press_t_ / focus_ / hover_t_ / focus_t_ over,
syncFrameFromYoga + paint.
```

All of "click handler", "state change", "requestRebuild", "diff
transfer" is automatic — no manual `setState`, no `notify_listeners`,
no observers. The Component constructor that codegen emits walks
each state field's class signal list and connects every signal to
`requestRebuild`, so any reactive change in any state field
propagates to a rebuild without per-binding bookkeeping.

The current scheme is coarse-grained: any signal on any state field
re-runs the entire widget's `build()`. Per-element bind() exists in
the runtime (`Element::bind<S, Sig>`) for finer dispatch but the
codegen doesn't use it yet because the per-component rebuild is
fast enough on the demos we have.

## State that survives a rebuild

`transferStateRecursive` walks old and new trees in lockstep. When
classes match at the same tree position, the new element inherits
transient state from its old counterpart via `transferStateFrom`.
v1 is positional only — reordered children lose state — and that's
all `gpu_*` demos need.

What gets transferred today:

| Element | Carried over |
|--|--|
| `TextFieldElement` | `text_`, `caret_pos_`, `selection_anchor_`, `focused_`, `preedit_`, `focus_t_` (focus-ring tween), `last_tick_ms_` |
| `ButtonElement` | `pressed_`, `hovered_`, `press_t_`, `hover_t_`, `last_tick_ms_` |
| `ListViewElement` / `ScrollViewElement` | `scroll_pos_`, `scroll_target_`, `scroll_last_tick_ms_`, `focused_` |
| `BarChartElement` / `LineChartElement` | `animated_` list, `last_tick_ms_` |
| `ProgressBarElement` | `animated_value_`, `last_tick_ms_` |

If you're adding an Element with transient state, override
`transferStateFrom(Element& old)` and copy whatever shouldn't snap
back to default each rebuild. The base default is a no-op.

## Animation tick

`PaintCtx` exposes `elapsedMs()` (a monotonic clock that starts at
window construction) and `requestAnimationFrame()`. After paint,
Window schedules a 16ms `QTimer::singleShot` if any element called
`requestAnimationFrame`, giving a ~60 Hz tick that runs only while
something is animating. Idle UIs go back to event-driven painting —
no busy loop, no CPU wakeups.

Existing consumers all share the same time-based easing curve
(`current += diff * std::min(1.f, dt * speed)`):

- TextField caret blink (530 ms on / 530 ms off cycle)
- Button press_t_ + hover_t_ (~120 ms)
- TextField focus ring (~120 ms)
- Theme crossfade (~250 ms over `Style::blend`)
- ListView / ScrollView scroll inertia (~120 ms)
- BarChart / LineChart per-bar / per-point eases (~150 ms)
- ProgressBar fill (~200 ms)
- Spinner (1 rev / sec, indeterminate)

If you want a new animated effect, you don't need a separate
animation framework — just hold a `float my_t_` on your Element,
ease it toward a target in `paint()`, and call
`ctx.requestAnimationFrame()` while the tween is in flight.

## Style and theming

`Style` is a struct of semantic color tokens (windowBg, surface,
border, text, accent, ...). `Style::dark()` and `Style::light()` are
the two presets; `Window::setTheme(Theme)` swaps with a 250 ms
crossfade via per-field `lerpColor` (see `Style::blend`). PaintCtx
hands the active Style to every element on every paint, so live
theme switching is free — no rebuild needed.

`gpu_app` accepts an initial theme:

```ruby
fn main { gpu_app(window: Main, theme: light) }
```

`Cmd / Ctrl + T` toggles at runtime in any `gpu_app` window.

## Limitations and gotchas

A few worth flagging up front; the rest live in commit messages /
internal memory.

- **Qt 6.11+ only.** Canvas Painter is new in Qt 6.11; older Qt
  versions don't ship the `qcanvaspainter.h` header.
- **macOS extra step:** `brew install qtcanvaspainter` is needed
  until KDE Craft picks the module up. Linux / Windows are fine if
  the distro Qt is 6.11+.
- **Multi-arg slots:** worked through cute-meta in `540eee4`; arity
  is no longer capped.
- **Vulkan on Linux:** `Window::defaultGraphicsApi()` returns
  `QRhi::Vulkan` on Linux now; the runtime auto-creates a shared
  `QVulkanInstance` on first construction. Qt has to be built with
  Vulkan support (default on every distro package this repo verifies
  against — Tumbleweed, Arch). Tested in CI but not as exhaustively
  as the macOS Metal path.
- **Element diff is positional, not keyed.** Reordering siblings
  loses transient state.
- **No keyboard focus traversal yet.** Tab between TextFields isn't
  wired up; click-to-focus only.
- **`QCanvasPainter` clip state is not save/restore preserved**, so
  `PaintCtx::pushClipRect` / `popClip` track an explicit stack and
  re-issue `setClipRect(full window)` on outermost pop. Without
  that workaround, a clipping element leaks its clip and prevents
  every subsequent sibling from rendering.

## Where to look in the source

| Concern | File |
|--|--|
| `Component` / `BuildOwner` / `Element::bind` | `runtime/cute-ui/{include,src}/cute/ui/component.hpp,cpp` |
| Element base + every concrete element | `runtime/cute-ui/include/cute/ui/element.hpp` + `src/element.cpp` |
| `App` / `Window` / event routing / render loop | `runtime/cute-ui/{include,src}/cute/ui/app.hpp,cpp` and `window.hpp,cpp` |
| Style tokens + Style::blend | `runtime/cute-ui/{include,src}/cute/ui/paint.hpp` + `src/style.cpp` |
| Codegen for `widget` over cute_ui roots, `gpu_app` intrinsic, leaf emitters | `crates/cute-codegen/src/cpp.rs` (search for `cute_ui` / `gpu_app`) |
| `cute_ui.qpi` widget binding declarations | `stdlib/cute_ui/cute_ui.qpi` |

For commit-archaeology, the `feat(cute-ui)` series in `git log`
covers each milestone in order — M4 widgets, reactive bind() codegen,
Style/Theme, animation tick, scroll inertia, charts, indicators.
