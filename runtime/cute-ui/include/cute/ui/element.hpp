#pragma once

#include "paint.hpp"

#include <QFont>
#include <QFontMetricsF>
#include <QObject>
#include <QPointF>
#include <QRectF>
#include <QSizeF>
#include <QString>
#include <QVariant>

#include <Qt>

#include <functional>
#include <memory>
#include <vector>

// Forward-declare Yoga types so the public header doesn't pull <yoga/Yoga.h>
// (yogacore is a PRIVATE link dependency of cute_ui).
struct YGNode;
typedef YGNode* YGNodeRef;

class QKeyEvent;
class QInputMethodEvent;

namespace cute::ui {

/// Retained-mode tree node returned by Component::build().
/// Owned by std::unique_ptr; lifetime is independent of the Qt parent-child
/// graph that owns Components.
class Element {
public:
    Element();
    virtual ~Element();

    Element(const Element&) = delete;
    Element& operator=(const Element&) = delete;

    YGNodeRef yogaNode() const noexcept { return yoga_node_; }

    /// Configures Yoga style and attaches child Yoga nodes. Window calls this
    /// at the start of every frame, just before YGNodeCalculateLayout.
    /// Subclasses override to set flex direction, padding, sizing, etc.
    virtual void layout(PaintCtx& ctx);

    /// Pulls Yoga's calculated rect into frame_ recursively. Window calls this
    /// after YGNodeCalculateLayout, before paint().
    void syncFrameFromYoga(QPointF parentOrigin = QPointF(0, 0));

    virtual void paint(PaintCtx& ctx) = 0;

    /// Intrinsic content size. Containers fall back to even distribution
    /// when this returns (0, 0). Takes a PaintCtx because measuring text
    /// goes through Canvas Painter.
    virtual QSizeF preferredSize(PaintCtx& /*ctx*/) const {
        return QSizeF(0, 0);
    }

    virtual bool hitTest(QPointF localPos) const {
        return frame_.contains(localPos);
    }

    /// Attaches the child both to the cute::ui tree and to this element's
    /// Yoga node. Defined out-of-line so the YGNodeInsertChild call can
    /// stay in element.cpp where Yoga is included. Subclasses may override
    /// (StackElement does, to apply absolute positioning).
    virtual void addChild(std::unique_ptr<Element> child);

    Element* onClick(std::function<void()> fn) { onClick_ = std::move(fn); return this; }
    Element* onHover(std::function<void(bool)> fn) { onHover_ = std::move(fn); return this; }

    /// Walks children back-to-front (z-order), firing the deepest hit's onClick.
    virtual bool dispatchClick(QPointF localPos) {
        if (!hitTest(localPos)) return false;
        for (auto it = children_.rbegin(); it != children_.rend(); ++it) {
            if ((*it)->dispatchClick(localPos)) return true;
        }
        if (onClick_) {
            onClick_();
            return true;
        }
        return false;
    }

    virtual Element* findFocusableAt(QPointF localPos) {
        if (!hitTest(localPos)) return nullptr;
        for (auto it = children_.rbegin(); it != children_.rend(); ++it) {
            if (auto* found = (*it)->findFocusableAt(localPos)) return found;
        }
        return acceptsFocus() ? this : nullptr;
    }

    Element* findFocused();

    virtual bool acceptsFocus() const { return false; }
    bool isFocused() const noexcept { return focused_; }
    virtual void setFocused(bool f) { focused_ = f; }

    virtual void keyPressEvent(QKeyEvent*) {}
    virtual void inputMethodEvent(QInputMethodEvent*) {}
    virtual QVariant inputMethodQuery(Qt::InputMethodQuery) const { return {}; }

    /// Default no-op; stateful elements (caret pos, scroll offset, ...) copy
    /// transient state from their old-tree counterpart so a rebuild doesn't
    /// reset it. Component::rebuildSelf walks the tree pairwise via
    /// transferStateRecursive.
    virtual void transferStateFrom(Element& /*old*/) {}

    /// Identity key used by the diff algorithm to keep stable subtrees alive
    /// across rebuilds.
    Element* setKey(QVariant k) { key_ = std::move(k); return this; }
    const QVariant& key() const noexcept { return key_; }

    QRectF frame() const noexcept { return frame_; }
    void setFrame(QRectF r) { frame_ = r; }

    Element* parent() const noexcept { return parent_; }

    const std::vector<std::unique_ptr<Element>>& children() const noexcept { return children_; }
    std::vector<std::unique_ptr<Element>>& mutableChildren() { return children_; }

protected:
    std::vector<std::unique_ptr<Element>> children_;
    Element* parent_ = nullptr;
    QRectF frame_;
    QVariant key_;
    std::function<void()> onClick_;
    std::function<void(bool)> onHover_;
    YGNodeRef yoga_node_ = nullptr;
    bool focused_ = false;
};

/// Lockstep walk: when classes match at the same tree position, the new
/// element inherits transient state from its old counterpart via
/// `transferStateFrom`. Positional only — reordered children lose state.
void transferStateRecursive(Element* old_tree, Element* new_tree);

class RectElement : public Element {
public:
    explicit RectElement(QColor fill, float radius = 0.f)
        : fill_(fill), radius_(radius) {}

    RectElement* setRadius(float r)         { radius_ = r; return this; }
    RectElement* setFill(QColor c)          { fill_ = c; return this; }
    RectElement* setStroke(QColor c, float w) { strokeColor_ = c; strokeWidth_ = w; return this; }
    RectElement* setSize(QSizeF s)          { fixedSize_ = s; return this; }

    void paint(PaintCtx& ctx) override;

private:
    QColor fill_;
    QColor strokeColor_;
    float radius_ = 0.f;
    float strokeWidth_ = 0.f;
    QSizeF fixedSize_;
};

class TextElement : public Element {
public:
    explicit TextElement(QString text)
        : text_(std::move(text)),
          font_(QStringLiteral("Helvetica"), 14) {}

    TextElement* setText(QString t)         { text_ = std::move(t); return this; }
    TextElement* setFont(QFont f)           { font_ = std::move(f); return this; }
    TextElement* setFontSize(qreal pt)      { font_.setPointSizeF(pt); return this; }
    TextElement* setColor(QColor c)         { color_ = c; explicit_color_ = true; return this; }
    /// Convenience for the `color: "#rrggbb"` / `color: "red"` styling
    /// surfaced through cute_ui.qpi. QColor handles the parsing; an
    /// invalid string leaves the prior color in place.
    TextElement* setColor(const QString& s) {
        QColor c(s);
        if (c.isValid()) { color_ = c; explicit_color_ = true; }
        return this;
    }

    QString text() const                    { return text_; }
    void paint(PaintCtx& ctx) override;
    QSizeF preferredSize(PaintCtx& ctx) const override;

private:
    QString text_;
    QFont font_;
    QColor color_;
    bool explicit_color_ = false;
};

class ButtonElement : public Element {
public:
    explicit ButtonElement(QString label)
        : label_(std::move(label)),
          font_(QStringLiteral("Helvetica"), 14) {}

    ButtonElement* setLabel(QString s)      { label_ = std::move(s); return this; }
    QString label() const                   { return label_; }

    void paint(PaintCtx& ctx) override;
    QSizeF preferredSize(PaintCtx& ctx) const override;
    bool hitTest(QPointF localPos) const override { return frame_.contains(localPos); }

    void setPressed(bool p)                 { pressed_ = p; }
    void setHovered(bool h)                 { hovered_ = h; }
    bool buttonPressed() const noexcept     { return pressed_; }
    bool buttonHovered() const noexcept     { return hovered_; }
    void transferStateFrom(Element& old) override;

private:
    QString label_;
    QFont font_;
    bool hovered_ = false;
    bool pressed_ = false;
    // *_t_ tween 0→1 toward the binary state over a couple of frames so
    // the visual reacts smoothly even on a tap (press + release < 16ms).
    float press_t_ = 0.f;
    float hover_t_ = 0.f;
    qint64 last_tick_ms_ = 0;
};

class TextFieldElement : public Element {
public:
    TextFieldElement();

    TextFieldElement* setText(QString t);
    TextFieldElement* setPlaceholder(QString s) { placeholder_ = std::move(s); return this; }
    TextFieldElement* setFont(QFont f);
    TextFieldElement* setOnTextChanged(std::function<void(QString)> fn) {
        on_text_changed_ = std::move(fn); return this;
    }

    QString text() const { return text_; }

    void paint(PaintCtx& ctx) override;
    QSizeF preferredSize(PaintCtx& ctx) const override;
    bool acceptsFocus() const override { return true; }
    void keyPressEvent(QKeyEvent* e) override;
    void inputMethodEvent(QInputMethodEvent* e) override;
    QVariant inputMethodQuery(Qt::InputMethodQuery query) const override;
    void transferStateFrom(Element& old) override;

private:
    void emitTextChanged();
    void replaceSelectionWith(const QString& s);

    QString text_;
    QString placeholder_;
    QFont font_;
    // QFontMetricsF is a glyph-cache lookup on construction; cache it instead
    // of paying the cost in paint(), preferredSize() and inputMethodQuery().
    QFontMetricsF font_metrics_;
    int caret_pos_ = 0;
    int selection_anchor_ = 0;
    // IME pre-edit text rendered inline at the caret but not yet part of text_.
    QString preedit_;
    int preedit_caret_ = 0;
    // focus_t_ tweens 0→1 toward `focused_`, lerping border color + width
    // for a smooth focus ring. Same time-based easing as Button press_t_.
    float focus_t_ = 0.f;
    qint64 last_tick_ms_ = 0;
    std::function<void(QString)> on_text_changed_;
};

/// Vector image rendered via QSvgRenderer. Rasterized once at the
/// configured size (or 64x64 default) into a QCanvasImage on first paint.
/// Resizing the element re-rasterizes lazily.
class SvgElement : public Element {
public:
    explicit SvgElement(QString source);

    SvgElement* setSource(QString s);
    SvgElement* setSize(QSizeF s);

    void paint(PaintCtx& ctx) override;
    QSizeF preferredSize(PaintCtx& ctx) const override;

private:
    void rasterizeIfNeeded(PaintCtx& ctx, QSize px);

    QString source_;
    QSizeF explicit_size_;
    mutable bool upload_attempted_ = false;
    mutable QSize last_pixel_size_;
    mutable QCanvasImage canvas_image_;
};

/// Determinate progress bar. `value_` ∈ [0, 1]; the fill width eases
/// from its previous value via the shared animation tick.
class ProgressBarElement : public Element {
public:
    ProgressBarElement() = default;

    ProgressBarElement* setValue(qreal v);
    ProgressBarElement* setSize(QSizeF s);

    void paint(PaintCtx& ctx) override;
    QSizeF preferredSize(PaintCtx& ctx) const override;
    void transferStateFrom(Element& old) override;

private:
    qreal value_ = 0;
    qreal animated_value_ = 0;
    qint64 last_tick_ms_ = 0;
    QSizeF explicit_size_;
};

/// Indeterminate spinner — a partial accent arc that rotates
/// continuously. Idle when not in the visual tree; loops while painted.
class SpinnerElement : public Element {
public:
    SpinnerElement() = default;
    SpinnerElement* setSize(QSizeF s);

    void paint(PaintCtx& ctx) override;
    QSizeF preferredSize(PaintCtx& ctx) const override;

private:
    QSizeF explicit_size_;
};

/// Vertical bar chart. Bars animate from their previous height when
/// data changes — the painter eases each bar toward its target via the
/// shared per-frame tick.
class BarChartElement : public Element {
public:
    BarChartElement() = default;

    BarChartElement* setData(QList<qreal> data);
    BarChartElement* setLabels(QStringList labels);
    BarChartElement* setSize(QSizeF s);

    void paint(PaintCtx& ctx) override;
    QSizeF preferredSize(PaintCtx& ctx) const override;
    void transferStateFrom(Element& old) override;

private:
    QList<qreal> data_;
    QStringList labels_;
    QList<qreal> animated_;     // tween values, lerped toward data_
    qint64 last_tick_ms_ = 0;
    QSizeF explicit_size_;
};

/// Polyline chart over evenly-spaced data with a small dot at each
/// sample. Same animation tick as BarChartElement.
class LineChartElement : public Element {
public:
    LineChartElement() = default;

    LineChartElement* setData(QList<qreal> data);
    LineChartElement* setLabels(QStringList labels);
    LineChartElement* setSize(QSizeF s);

    void paint(PaintCtx& ctx) override;
    QSizeF preferredSize(PaintCtx& ctx) const override;
    void transferStateFrom(Element& old) override;

private:
    QList<qreal> data_;
    QStringList labels_;
    QList<qreal> animated_;
    qint64 last_tick_ms_ = 0;
    QSizeF explicit_size_;
};

class ImageElement : public Element {
public:
    explicit ImageElement(QString source);

    ImageElement* setSource(QString s);
    ImageElement* setSize(QSizeF s)        { explicit_size_ = s; return this; }
    ImageElement* setFit(Qt::AspectRatioMode m) { fit_ = m; return this; }

    void paint(PaintCtx& ctx) override;
    QSizeF preferredSize(PaintCtx& ctx) const override;

private:
    QString source_;
    QSizeF explicit_size_;
    Qt::AspectRatioMode fit_ = Qt::KeepAspectRatio;
    // Lazy upload on first paint: QCanvasImage handles are tied to the
    // PaintCtx's driver. natural_size_ doubles as the loaded-flag — an
    // empty size means either "not uploaded yet" or "load failed".
    mutable bool upload_attempted_ = false;
    mutable QCanvasImage canvas_image_;
    mutable QSizeF natural_size_;
};

class ColumnElement : public Element {
public:
    ColumnElement();
    void paint(PaintCtx& ctx) override;
    ColumnElement* setSpacing(qreal v);
    ColumnElement* setPadding(qreal v);
};

class RowElement : public Element {
public:
    RowElement();
    void paint(PaintCtx& ctx) override;
    RowElement* setSpacing(qreal v);
    RowElement* setPadding(qreal v);
};

class StackElement : public Element {
public:
    StackElement() = default;
    void addChild(std::unique_ptr<Element> child) override;
    void paint(PaintCtx& ctx) override;
};

/// Full-window dim overlay with a centered rounded surface that holds the
/// children. Yoga-positioned absolute, so a Modal inside any container is
/// painted on top regardless of sibling ordering. Clicks inside the modal
/// are dispatched to the dialog children; clicks on the dim background are
/// swallowed so the page underneath doesn't fire its handlers.
class ModalElement : public Element {
public:
    ModalElement();
    void addChild(std::unique_ptr<Element> child) override;
    void paint(PaintCtx& ctx) override;
    bool dispatchClick(QPointF localPos) override;
    Element* findFocusableAt(QPointF localPos) override;

private:
    Element* dialog_ = nullptr;
};

/// Tabular container. Children are expected to be Row elements; the first
/// Row is rendered as the header (stronger background + bottom divider),
/// the rest as data rows with an alternating stripe color and a row
/// separator beneath each. Surrounding rounded border.
class DataTableElement : public Element {
public:
    DataTableElement();
    void paint(PaintCtx& ctx) override;
};

enum class ScrollAxis { Vertical, Horizontal };

/// Single-axis scrolling container. A scroll offset clips content outside
/// the viewport along the chosen axis. Window routes wheel events via
/// dispatchWheel; the scroll-state plumbing is shared with
/// ScrollViewElement (ListView adds a surface card + border, ScrollView
/// paints children only).
class ListViewElement : public Element {
public:
    explicit ListViewElement(ScrollAxis axis = ScrollAxis::Vertical);

    void layout(PaintCtx& ctx) override;
    void paint(PaintCtx& ctx) override;
    void transferStateFrom(Element& old) override;

    /// Window forwards both axes; the listview consumes whichever matches
    /// its axis_. Returns true iff the offset actually changed.
    bool dispatchWheel(QPointF localPos, qreal dx, qreal dy);

    /// Visual scroll position — what the user sees, eased toward
    /// `scroll_target_` over a couple of frames.
    qreal scrollOffset() const noexcept { return scroll_pos_; }
    /// Snaps both the target and the visual position. Use when calling
    /// programmatically (e.g. "jump to bottom"). Wheel input goes through
    /// dispatchWheel which moves only the target so paint can ease in.
    bool setScrollOffset(qreal v);

    /// Opt-in fixed-height row virtualization: only the rows whose row
    /// index falls in the visible range get laid out + painted. Cuts
    /// paint cost from O(N) to O(viewport / item_h). Children must be
    /// fixed-height (defaults to itemHeight = 24px) and self-contained
    /// (their subtree is laid out directly without Yoga).
    void setVirtualized(bool v) noexcept { virtualized_ = v; }
    void setItemHeight(qreal h) noexcept { item_height_ = h; }

    ScrollAxis axis() const noexcept { return axis_; }

    // ---- Scrollbar thumb drag ----------------------------------------
    /// True when `localPos` falls inside the currently-painted scrollbar
    /// thumb rectangle. Lets Window route a mouse-press on the thumb to
    /// `beginScrollbarDrag` instead of the normal click / focus path.
    bool hitScrollbarThumb(QPointF localPos) const;
    /// Capture the press position and current scroll target as drag
    /// origin. Subsequent `updateScrollbarDrag` deltas translate the
    /// thumb (and the scroll position) proportionally.
    void beginScrollbarDrag(QPointF localPos);
    /// Apply a mouse-move delta against the captured origin. Snaps both
    /// scroll_pos_ and scroll_target_ so the visual doesn't tween while
    /// the user is actively dragging.
    void updateScrollbarDrag(QPointF localPos);
    /// Release the drag. Subsequent moves go through the normal hover
    /// / click handlers.
    void endScrollbarDrag() noexcept { dragging_scrollbar_ = false; }
    bool isDraggingScrollbar() const noexcept { return dragging_scrollbar_; }

protected:
    void applyScrollOffsetToChildren();
    void paintScrollbar(PaintCtx& ctx, qreal max_off);
    /// Recomputes `content_extent_` from child frames along the active
    /// axis and clamps both pos and target. Returns the new max offset.
    qreal recomputeScrollExtent();
    /// Eases `scroll_pos_` toward `scroll_target_` and requests another
    /// frame if the tween is in flight. Called from paint().
    void advanceScrollTween(PaintCtx& ctx);
    /// Compute the current thumb rectangle in the same coordinate
    /// system as `paintScrollbar`. Returns an empty rect when there's
    /// no overflow (no thumb to draw).
    QRectF scrollbarThumbRect() const;

    ScrollAxis axis_;
    qreal scroll_pos_ = 0;
    qreal scroll_target_ = 0;
    qreal content_extent_ = 0;
    qint64 scroll_last_tick_ms_ = 0;
    bool virtualized_ = false;
    qreal item_height_ = 24.0;
    bool dragging_scrollbar_ = false;
    QPointF drag_start_pos_ {};
    qreal drag_start_scroll_ = 0.0;
};

/// Scrollable container without ListView's surface card. Use to scroll
/// a generic Column / DataTable when content overflows the viewport.
class ScrollViewElement : public ListViewElement {
public:
    explicit ScrollViewElement(ScrollAxis axis = ScrollAxis::Vertical);
    void paint(PaintCtx& ctx) override;
};

} // namespace cute::ui
