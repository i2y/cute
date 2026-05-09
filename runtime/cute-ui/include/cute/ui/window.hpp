#pragma once

#include "paint.hpp"

#include <QElapsedTimer>
#include <QWindow>
#include <QSize>
#include <Qt>
#include <QtCanvasPainter/QCanvasPainterFactory>

#include <rhi/qrhi.h>

#include <memory>

class QKeyEvent;
class QInputMethodEvent;

namespace cute::ui {

class Component;
class BuildOwner;
class Element;
class ButtonElement;
class ListViewElement;

/// QWindow that owns a QRhi swap chain and a QCanvasPainterFactory, and
/// drives a `layout → calculate → paint` cycle on root's Element tree
/// each frame.
class Window : public QWindow {
    Q_OBJECT
public:
    explicit Window(Component* root,
                    QRhi::Implementation api = defaultGraphicsApi(),
                    QWindow* parent = nullptr);
    ~Window() override;

    Component* rootComponent() const noexcept { return root_; }
    BuildOwner* buildOwner() noexcept { return build_owner_.get(); }

    /// macOS → Metal, Windows → D3D12, otherwise → Null (Linux/Vulkan support
    /// requires a QVulkanInstance and is not enabled here yet).
    static QRhi::Implementation defaultGraphicsApi();

    Theme theme() const noexcept { return theme_; }
    /// Switches the active theme; the next paint pulls fresh colors via
    /// Style::forTheme. Cheap — no rebuild needed since paint() always
    /// reads from the live PaintCtx style.
    void setTheme(Theme t);

protected:
    void exposeEvent(QExposeEvent*) override;
    void mousePressEvent(QMouseEvent*) override;
    void mouseReleaseEvent(QMouseEvent*) override;
    void mouseMoveEvent(QMouseEvent*) override;
    void keyPressEvent(QKeyEvent*) override;
    void wheelEvent(QWheelEvent*) override;
    bool event(QEvent*) override;

private:
    void initRhi();
    void resizeSwapChain();
    void releaseResources();
    void renderFrame();
    /// QWindow has no virtual for InputMethod / InputMethodQuery; event()
    /// routes them to these helpers.
    void handleInputMethod(QInputMethodEvent* e);
    void handleInputMethodQuery(QEvent* e);
    /// Pass nullptr to drop focus. Toggles per-element `focused_` flags and
    /// pings QInputMethod so IME repositions.
    void setFocusElement(Element* e);
    void onRootRebuilt();
    /// Recursively finds a hit-tested ListViewElement and forwards both
    /// wheel axes; the listview consumes whichever matches its axis_.
    /// Returns true iff the scroll offset actually changed.
    bool dispatchWheelToListView(Element* el, QPointF pos, qreal dx, qreal dy);
    /// Common tail for keyboard / IME handling: pings QInputMethod and
    /// requests a repaint.
    void notifyFocusedElementMutated(Qt::InputMethodQueries queries);

    Component* root_;
    QRhi::Implementation graphicsApi_;
    std::unique_ptr<QRhi> rhi_;
    std::unique_ptr<QRhiSwapChain> sc_;
    std::unique_ptr<QRhiRenderBuffer> ds_;
    std::unique_ptr<QRhiRenderPassDescriptor> rp_;
    std::unique_ptr<QCanvasPainterFactory> canvasFactory_;
    std::unique_ptr<BuildOwner> build_owner_;
    bool hasSwapChain_ = false;
    bool notExposed_ = false;
    QSize lastPixelSize_;
    Theme theme_ = Theme::Dark;
    Theme prev_theme_ = Theme::Dark;
    /// Tween position for the active theme crossfade. 1.f = fully on
    /// `theme_`, < 1 = blending from `prev_theme_`. Bumped to 0 by
    /// setTheme; advanced toward 1 in renderFrame.
    float theme_anim_t_ = 1.f;
    qint64 theme_last_tick_ms_ = 0;
    QElapsedTimer clock_;
    Element* focused_element_ = nullptr;
    /// Set on mousePress when the cursor lands on a Button; setPressed(true)
    /// is called for the visual sink, and the click handler is deferred to
    /// mouseRelease so the press tint is visible for the duration of the
    /// gesture. Reset to nullptr on release.
    ButtonElement* pressed_button_ = nullptr;
    ButtonElement* hovered_button_ = nullptr;
    /// Set on mousePress when the cursor lands on a ListView/ScrollView
    /// scrollbar thumb. mouseMove translates the press delta into a
    /// scroll offset; mouseRelease clears the field. While set, hover /
    /// click handling is bypassed so the user's drag isn't interrupted
    /// by a button under the cursor mid-gesture.
    ListViewElement* dragging_scrollbar_listview_ = nullptr;
};

} // namespace cute::ui
