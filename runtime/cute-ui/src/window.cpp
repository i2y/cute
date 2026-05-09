#include "cute/ui/window.hpp"
#include "cute/ui/component.hpp"
#include "cute/ui/element.hpp"
#include "cute/ui/paint.hpp"

#include <yoga/Yoga.h>

#include <QExposeEvent>
#include <QGuiApplication>
#include <QInputMethod>
#include <QInputMethodEvent>
#include <QInputMethodQueryEvent>
#include <QKeyEvent>
#include <QMouseEvent>
#include <QResizeEvent>
#include <QPlatformSurfaceEvent>
#include <QTimer>
#include <QWheelEvent>
#include <QtCanvasPainter/QCanvasPainter>
#include <QtCanvasPainter/QCanvasRhiPaintDriver>
#include <QColor>
#include <QDebug>

#if defined(Q_OS_LINUX) && QT_CONFIG(vulkan)
#  include <QVulkanInstance>
#endif

namespace cute::ui {

#if defined(Q_OS_LINUX) && QT_CONFIG(vulkan)
/// Single QVulkanInstance shared across every Window in the process —
/// Qt requires one and only one for the lifetime of the GUI app.
static QVulkanInstance* sharedVulkanInstance()
{
    static QVulkanInstance inst;
    static bool created = false;
    if (!created) {
        inst.setApiVersion(QVersionNumber(1, 2));
        if (!inst.create()) {
            qWarning() << "cute::ui: QVulkanInstance::create() failed:"
                       << inst.errorCode();
        }
        created = true;
    }
    return &inst;
}
#endif

void Window::setTheme(Theme t)
{
    if (theme_ == t) return;
    prev_theme_ = theme_;
    theme_ = t;
    theme_anim_t_ = 0.f;
    theme_last_tick_ms_ = 0;
    requestUpdate();
}

QRhi::Implementation Window::defaultGraphicsApi()
{
#if defined(Q_OS_MACOS)
    return QRhi::Metal;
#elif defined(Q_OS_WIN)
    return QRhi::D3D12;
#elif defined(Q_OS_LINUX) && QT_CONFIG(vulkan)
    return QRhi::Vulkan;
#else
    return QRhi::Null;
#endif
}

Window::Window(Component* root, QRhi::Implementation api, QWindow* parent)
    : QWindow(parent), root_(root), graphicsApi_(api),
      build_owner_(std::make_unique<BuildOwner>())
{
    switch (api) {
    case QRhi::Metal:        setSurfaceType(QSurface::MetalSurface); break;
    case QRhi::D3D11:
    case QRhi::D3D12:        setSurfaceType(QSurface::Direct3DSurface); break;
    case QRhi::Vulkan:       setSurfaceType(QSurface::VulkanSurface); break;
    case QRhi::OpenGLES2:    setSurfaceType(QSurface::OpenGLSurface); break;
    case QRhi::Null:         setSurfaceType(QSurface::RasterSurface); break;
    }
#if defined(Q_OS_LINUX) && QT_CONFIG(vulkan)
    if (api == QRhi::Vulkan) {
        setVulkanInstance(sharedVulkanInstance());
    }
#endif

    if (root_) {
        root_->setBuildOwner(build_owner_.get());
        connect(root_, &Component::rebuilt, this, [this] { onRootRebuilt(); });
    }
    // requestUpdate fires once when a Component schedules a rebuild, so the
    // render loop only runs when there's something to do.
    build_owner_->setOnDirty([this]() { requestUpdate(); });
    clock_.start();
    resize(640, 480);
}

Window::~Window()
{
    releaseResources();
}

void Window::initRhi()
{
    QRhi* rawRhi = nullptr;
#if QT_CONFIG(metal)
    if (graphicsApi_ == QRhi::Metal) {
        QRhiMetalInitParams params;
        rawRhi = QRhi::create(QRhi::Metal, &params);
    }
#endif
#if defined(Q_OS_WIN)
    if (graphicsApi_ == QRhi::D3D12) {
        QRhiD3D12InitParams params;
        rawRhi = QRhi::create(QRhi::D3D12, &params);
    }
#endif
#if defined(Q_OS_LINUX) && QT_CONFIG(vulkan)
    if (graphicsApi_ == QRhi::Vulkan) {
        QRhiVulkanInitParams params;
        params.inst = sharedVulkanInstance();
        params.window = this;
        rawRhi = QRhi::create(QRhi::Vulkan, &params);
    }
#endif
    if (graphicsApi_ == QRhi::Null) {
        QRhiInitParams params;
        rawRhi = QRhi::create(QRhi::Null, &params);
    }
    rhi_.reset(rawRhi);
    if (!rhi_) {
        qFatal("cute::ui::Window: QRhi::create() failed for selected graphics API.");
    }

    sc_.reset(rhi_->newSwapChain());
    ds_.reset(rhi_->newRenderBuffer(QRhiRenderBuffer::DepthStencil,
                                    QSize(),
                                    1,
                                    QRhiRenderBuffer::UsedWithSwapChainOnly));
    sc_->setWindow(this);
    sc_->setDepthStencil(ds_.get());
    sc_->setSampleCount(1);
    rp_.reset(sc_->newCompatibleRenderPassDescriptor());
    sc_->setRenderPassDescriptor(rp_.get());

    canvasFactory_ = std::make_unique<QCanvasPainterFactory>();
    canvasFactory_->create(rhi_.get());
    if (!canvasFactory_->isValid()) {
        qFatal("cute::ui::Window: QCanvasPainterFactory::create() failed.");
    }

    if (root_) {
        root_->rebuildSelf();
    }
}

void Window::releaseResources()
{
    canvasFactory_.reset();
    rp_.reset();
    ds_.reset();
    sc_.reset();
    rhi_.reset();
    hasSwapChain_ = false;
}

void Window::resizeSwapChain()
{
    hasSwapChain_ = sc_->createOrResize();
}

void Window::exposeEvent(QExposeEvent*)
{
    if (isExposed() && !rhi_) {
        initRhi();
        resizeSwapChain();
        lastPixelSize_ = sc_->surfacePixelSize();
    }
    const QSize surfaceSize = sc_ ? sc_->surfacePixelSize() : QSize();
    if ((!isExposed() || (hasSwapChain_ && surfaceSize.isEmpty())) && rhi_) {
        notExposed_ = true;
    }
    if (isExposed() && rhi_ && hasSwapChain_ && !surfaceSize.isEmpty()) {
        notExposed_ = false;
        if (surfaceSize != lastPixelSize_) {
            resizeSwapChain();
            lastPixelSize_ = surfaceSize;
        }
        renderFrame();
    }
}

namespace {
ButtonElement* findButtonAt(Element* el, QPointF pos)
{
    if (!el->frame().contains(pos)) return nullptr;
    for (auto it = el->mutableChildren().rbegin();
         it != el->mutableChildren().rend(); ++it) {
        if (auto* b = findButtonAt(it->get(), pos)) return b;
    }
    return dynamic_cast<ButtonElement*>(el);
}
}

// Walk an element subtree depth-first, top-to-bottom in paint order
// (last child wins because it paints on top), and find the topmost
// ListView/ScrollView whose scrollbar thumb contains `pos`.
static ListViewElement* findScrollbarThumbAt(Element* el, QPointF pos)
{
    if (!el->frame().contains(pos)) return nullptr;
    for (auto it = el->mutableChildren().rbegin();
         it != el->mutableChildren().rend(); ++it) {
        if (auto* lv = findScrollbarThumbAt(it->get(), pos)) return lv;
    }
    if (auto* lv = dynamic_cast<ListViewElement*>(el)) {
        if (lv->hitScrollbarThumb(pos)) return lv;
    }
    return nullptr;
}

void Window::mousePressEvent(QMouseEvent* e)
{
    if (!root_ || !root_->currentRoot()) return;
    if (e->button() != Qt::LeftButton) return;
    Element* elem = root_->currentRoot();
    // The actual root frame is set in renderFrame() by syncFrameFromYoga;
    // hit-testing on click happens between frames so seed it from window size.
    elem->setFrame(QRectF(0, 0, width(), height()));

    // Scrollbar thumb takes priority over focus / button click — the
    // thumb visually overlaps content that may also be focusable, but
    // the user clicked the scrollbar, not the content underneath.
    if (auto* lv = findScrollbarThumbAt(elem, e->position())) {
        dragging_scrollbar_listview_ = lv;
        lv->beginScrollbarDrag(e->position());
        // The thumb is just an indicator, not a focusable widget — don't
        // disturb the existing focus on press.
        requestUpdate();
        return;
    }

    Element* hit_focusable = elem->findFocusableAt(e->position());
    setFocusElement(hit_focusable);

    // If a Button was hit, defer its click to mouseReleaseEvent so the
    // pressed tint is visible for the gesture's duration.
    if (auto* b = findButtonAt(elem, e->position())) {
        pressed_button_ = b;
        b->setPressed(true);
        requestUpdate();
        return;
    }

    if (elem->dispatchClick(e->position())) {
        // The click handler's state changes emit signals codegen has
        // wired to requestRebuild — no manual rebuild needed here.
        requestUpdate();
    } else if (hit_focusable) {
        requestUpdate();
    }
}

void Window::mouseMoveEvent(QMouseEvent* e)
{
    if (!root_ || !root_->currentRoot()) {
        QWindow::mouseMoveEvent(e);
        return;
    }
    // Active scrollbar drag wins over hover handling — keep the user's
    // gesture smooth even when the cursor passes over a button.
    if (dragging_scrollbar_listview_) {
        dragging_scrollbar_listview_->updateScrollbarDrag(e->position());
        // Force a rebuild so virtualized rows re-pick the visible
        // window. Wheel scrolling intentionally avoids this (wheel
        // input always moves the target only, easing is visible-only)
        // but a drag should feel instant.
        if (root_) root_->requestRebuild();
        requestUpdate();
        return;
    }
    Element* elem = root_->currentRoot();
    elem->setFrame(QRectF(0, 0, width(), height()));
    ButtonElement* under = findButtonAt(elem, e->position());
    if (under == hovered_button_) return;
    if (hovered_button_) hovered_button_->setHovered(false);
    hovered_button_ = under;
    if (hovered_button_) hovered_button_->setHovered(true);
    requestUpdate();
}

void Window::mouseReleaseEvent(QMouseEvent* e)
{
    if (e->button() != Qt::LeftButton) {
        QWindow::mouseReleaseEvent(e);
        return;
    }
    if (dragging_scrollbar_listview_) {
        dragging_scrollbar_listview_->endScrollbarDrag();
        dragging_scrollbar_listview_ = nullptr;
        requestUpdate();
        return;
    }
    if (!pressed_button_) {
        QWindow::mouseReleaseEvent(e);
        return;
    }
    ButtonElement* b = pressed_button_;
    pressed_button_ = nullptr;
    b->setPressed(false);
    // Only fire onClick if the cursor is still over the same button (the
    // user can drag away to cancel — standard button UX). Click handlers
    // emit signals that codegen wired to requestRebuild.
    b->dispatchClick(e->position());
    requestUpdate();
}

void Window::notifyFocusedElementMutated(Qt::InputMethodQueries queries)
{
    if (auto* im = QGuiApplication::inputMethod()) {
        im->update(queries);
    }
    requestUpdate();
}

void Window::keyPressEvent(QKeyEvent* e)
{
    // Temporary diagnostic: Cmd/Ctrl+T toggles dark/light. Will be replaced
    // by a proper API surface once theme-from-Cute hookups land.
    if (e->key() == Qt::Key_T
        && (e->modifiers() & (Qt::ControlModifier | Qt::MetaModifier))) {
        setTheme(theme_ == Theme::Dark ? Theme::Light : Theme::Dark);
        return;
    }

    // Tab / Shift+Tab walk a DFS-collected list of focusables. Forms with
    // multiple TextFields rely on this; native Qt would use focusNext /
    // focusPrev on QWidget but our Element tree isn't a QWidget tree.
    if ((e->key() == Qt::Key_Tab || e->key() == Qt::Key_Backtab)
        && root_ && root_->currentRoot()) {
        const bool backward = e->key() == Qt::Key_Backtab
            || (e->modifiers() & Qt::ShiftModifier);
        std::vector<Element*> focusables;
        std::function<void(Element*)> walk = [&](Element* el) {
            if (el->acceptsFocus()) focusables.push_back(el);
            for (auto& c : el->mutableChildren()) walk(c.get());
        };
        walk(root_->currentRoot());
        if (!focusables.empty()) {
            int idx = -1;
            for (size_t i = 0; i < focusables.size(); ++i) {
                if (focusables[i] == focused_element_) { idx = int(i); break; }
            }
            const int n = int(focusables.size());
            int next_idx;
            if (idx < 0) {
                next_idx = backward ? n - 1 : 0;
            } else {
                next_idx = backward ? (idx - 1 + n) % n : (idx + 1) % n;
            }
            setFocusElement(focusables[next_idx]);
            requestUpdate();
        }
        return;
    }

    if (focused_element_) {
        focused_element_->keyPressEvent(e);
        // The user's onTextChanged callback (if any) emits store signals
        // that codegen has connected to requestRebuild — no manual rebuild
        // needed here. We still poke the input method and request a paint
        // so the caret / selection redraws even when no rebuild fires.
        notifyFocusedElementMutated(Qt::ImQueryInput);
        return;
    }
    QWindow::keyPressEvent(e);
}

void Window::wheelEvent(QWheelEvent* e)
{
    if (!root_ || !root_->currentRoot()) {
        QWindow::wheelEvent(e);
        return;
    }
    Element* elem = root_->currentRoot();
    elem->setFrame(QRectF(0, 0, width(), height()));

    QPoint pixelDelta = e->pixelDelta();
    // angleDelta is 1/8 degree; negate so positive wheel moves content up/left.
    qreal dx = pixelDelta.isNull() ? -(e->angleDelta().x() / 8.0)
                                   : -pixelDelta.x();
    qreal dy = pixelDelta.isNull() ? -(e->angleDelta().y() / 8.0)
                                   : -pixelDelta.y();

    if (dispatchWheelToListView(elem, e->position(), dx, dy)) {
        requestUpdate();
        return;
    }
    QWindow::wheelEvent(e);
}

bool Window::dispatchWheelToListView(Element* el, QPointF pos, qreal dx, qreal dy)
{
    if (!el->frame().contains(pos)) return false;
    for (auto it = el->mutableChildren().rbegin();
         it != el->mutableChildren().rend(); ++it) {
        if (dispatchWheelToListView(it->get(), pos, dx, dy)) return true;
    }
    if (auto* lv = dynamic_cast<ListViewElement*>(el)) {
        return lv->dispatchWheel(pos, dx, dy);
    }
    return false;
}

void Window::handleInputMethod(QInputMethodEvent* e)
{
    if (!focused_element_) {
        e->ignore();
        return;
    }
    focused_element_->inputMethodEvent(e);
    notifyFocusedElementMutated(Qt::ImQueryInput);
}

void Window::handleInputMethodQuery(QEvent* e)
{
    auto* q = static_cast<QInputMethodQueryEvent*>(e);
    Qt::InputMethodQueries todo = q->queries();
    while (todo) {
        // Iterate one set bit at a time; QInputMethodQueryEvent passes a
        // mask of N queries, but typically only 2-3 are actually requested.
        Qt::InputMethodQueries bit = Qt::InputMethodQueries(uint(todo) & -int(uint(todo)));
        todo &= ~bit;
        Qt::InputMethodQuery one = Qt::InputMethodQuery(uint(bit));
        QVariant value;
        if (focused_element_) {
            value = focused_element_->inputMethodQuery(one);
        } else if (one == Qt::ImEnabled) {
            value = false;
        }
        q->setValue(one, value);
    }
    q->accept();
}

void Window::setFocusElement(Element* e)
{
    if (focused_element_ == e) return;
    if (focused_element_) focused_element_->setFocused(false);
    focused_element_ = e;
    if (focused_element_) focused_element_->setFocused(true);
    if (auto* im = QGuiApplication::inputMethod()) {
        im->update(Qt::ImQueryAll);
    }
}

void Window::onRootRebuilt()
{
    // The old focused_element_ pointer was just invalidated; state-transfer
    // copied focused_=true to the new tree's matching element. Find it.
    if (root_ && root_->currentRoot()) {
        Element* root_el = root_->currentRoot();
        focused_element_ = root_el->findFocused();
        // Same dangling-pointer story for press / hover button pointers.
        pressed_button_ = nullptr;
        hovered_button_ = nullptr;
        std::function<void(Element*)> walk = [&](Element* el) {
            if (auto* b = dynamic_cast<ButtonElement*>(el)) {
                if (b->buttonPressed()) pressed_button_ = b;
                if (b->buttonHovered()) hovered_button_ = b;
            }
            for (auto& c : el->mutableChildren()) walk(c.get());
        };
        walk(root_el);
    } else {
        focused_element_ = nullptr;
    }
    if (auto* im = QGuiApplication::inputMethod()) {
        im->update(Qt::ImQueryAll);
    }
}

bool Window::event(QEvent* e)
{
    switch (e->type()) {
    case QEvent::UpdateRequest:
        renderFrame();
        break;
    case QEvent::PlatformSurface:
        if (static_cast<QPlatformSurfaceEvent*>(e)->surfaceEventType()
            == QPlatformSurfaceEvent::SurfaceAboutToBeDestroyed) {
            releaseResources();
        }
        break;
    case QEvent::InputMethod:
        handleInputMethod(static_cast<QInputMethodEvent*>(e));
        return true;
    case QEvent::InputMethodQuery:
        handleInputMethodQuery(e);
        return true;
    default:
        break;
    }
    return QWindow::event(e);
}

void Window::renderFrame()
{
    if (!hasSwapChain_ || notExposed_) return;

    if (build_owner_ && build_owner_->hasDirty()) {
        build_owner_->flush();
    }

    const QSize currentPixelSize = sc_->surfacePixelSize();
    if (currentPixelSize != lastPixelSize_) {
        resizeSwapChain();
        lastPixelSize_ = currentPixelSize;
        if (!hasSwapChain_) return;
    }

    QRhi::FrameOpResult r = rhi_->beginFrame(sc_.get());
    if (r == QRhi::FrameOpSwapChainOutOfDate) {
        resizeSwapChain();
        if (!hasSwapChain_) return;
        r = rhi_->beginFrame(sc_.get());
    }
    if (r != QRhi::FrameOpSuccess) {
        requestUpdate();
        return;
    }

    QRhiCommandBuffer* cb = sc_->currentFrameCommandBuffer();
    QRhiRenderTarget* rt = sc_->currentFrameRenderTarget();

    QCanvasRhiPaintDriver* driver = canvasFactory_->paintDriver();
    QCanvasPainter* painter = canvasFactory_->painter();

    driver->resetForNewFrame();

    // Theme crossfade: while theme_anim_t_ < 1, blend prev / current Style
    // and advance the tween. ~250ms feels distinct without dragging.
    Style style;
    if (theme_anim_t_ >= 1.f || prev_theme_ == theme_) {
        style = Style::forTheme(theme_);
    } else {
        const qint64 now = clock_.elapsed();
        const float dt = theme_last_tick_ms_
            ? std::min<float>(0.05f, (now - theme_last_tick_ms_) / 1000.f) : 0.f;
        theme_last_tick_ms_ = now;
        theme_anim_t_ = std::min(1.f, theme_anim_t_ + dt * 4.f);  // ~250ms
        style = Style::blend(Style::forTheme(prev_theme_),
                             Style::forTheme(theme_),
                             theme_anim_t_);
    }
    const float dpr = static_cast<float>(devicePixelRatio());
    driver->beginPaint(cb, rt, style.windowBg, size(), dpr);

    bool needs_more_frames = (theme_anim_t_ < 1.f);
    if (root_ && root_->currentRoot()) {
        // Two-pass: layout configures Yoga style + child links, then
        // YGNodeCalculateLayout fills the absolute frame, then paint draws.
        Element* elem = root_->currentRoot();
        PaintCtx ctx{*painter, QSizeF(width(), height()), style, clock_.elapsed()};
        elem->layout(ctx);
        YGNodeCalculateLayout(elem->yogaNode(),
                              float(width()),
                              float(height()),
                              YGDirectionLTR);
        elem->syncFrameFromYoga(QPointF(0, 0));
        elem->paint(ctx);
        needs_more_frames = needs_more_frames || ctx.needsMoreFrames();
    }

    driver->endPaint();
    rhi_->endFrame(sc_.get());

    if (needs_more_frames) {
        // ~60Hz; close enough without going to QRhi's vsync-driven path.
        // The animation loop only runs while at least one element calls
        // ctx.requestAnimationFrame() — idle UIs stay event-driven.
        QTimer::singleShot(16, this, [this]{ requestUpdate(); });
    }
}

} // namespace cute::ui
