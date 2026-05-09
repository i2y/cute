#include "cute/ui/element.hpp"
#include "cute/ui/paint.hpp"

#include <yoga/Yoga.h>

#include <QClipboard>
#include <QCoreApplication>
#include <QFileInfo>
#include <QFontMetricsF>
#include <QGuiApplication>
#include <QImage>
#include <QImageReader>
#include <QInputMethod>
#include <QInputMethodEvent>
#include <QKeyEvent>
#include <QPainter>
#include <QSvgRenderer>
#include <QtMath>

#include <algorithm>
#include <string>
#include <typeinfo>
#include <unordered_map>
#include <vector>

namespace cute::ui {

void transferStateRecursive(Element* old_tree, Element* new_tree)
{
    if (!old_tree || !new_tree) return;
    if (typeid(*old_tree) == typeid(*new_tree)) {
        new_tree->transferStateFrom(*old_tree);
    }
    const auto& oc = old_tree->children();
    auto& nc = new_tree->mutableChildren();

    // Keyed-diff pass: for each new child whose key matches an old
    // child's key (regardless of position), pair them by key. This
    // preserves caret / scroll / press state across list reorders —
    // the textbook case is `for it in items { TextField { ... } }`
    // where appending or removing an item shouldn't blow away the
    // focused field's caret. Old children without a key, and new
    // children whose key didn't match, fall back to the positional
    // pairing the previous behaviour used.
    //
    // Only build the map when at least one old child carries a key —
    // the unkeyed-only case (the common one) keeps the existing
    // O(n) walk with no allocations.
    bool any_old_keyed = false;
    for (const auto& c : oc) {
        if (c && c->key().isValid()) {
            any_old_keyed = true;
            break;
        }
    }

    if (any_old_keyed) {
        // Map key (as QString comparison — QVariant doesn't have a
        // hash that round-trips for arbitrary inner types) to the
        // old-child index. First-occurrence wins for duplicate keys
        // so the diff is deterministic.
        std::unordered_map<std::string, size_t> by_key;
        by_key.reserve(oc.size());
        for (size_t i = 0; i < oc.size(); ++i) {
            if (oc[i] && oc[i]->key().isValid()) {
                std::string k = oc[i]->key().toString().toStdString();
                by_key.try_emplace(std::move(k), i);
            }
        }

        // Track which old children have already been paired so we
        // don't transfer state from one old child into multiple new
        // children — that would clone the caret across instances.
        std::vector<bool> old_used(oc.size(), false);

        // First pass: pair every new keyed child with its old
        // same-key child.
        for (size_t i = 0; i < nc.size(); ++i) {
            if (!nc[i]) continue;
            if (!nc[i]->key().isValid()) continue;
            std::string k = nc[i]->key().toString().toStdString();
            auto it = by_key.find(k);
            if (it == by_key.end()) continue;
            size_t oi = it->second;
            if (old_used[oi]) continue;
            old_used[oi] = true;
            transferStateRecursive(oc[oi].get(), nc[i].get());
        }

        // Second pass: positional pairing for any new child that
        // wasn't keyed (or whose key didn't match), falling back to
        // the next available unused old child at the same index.
        // We pair `nc[i]` with `oc[i]` only when both are unused —
        // otherwise the keyed pass already handled it.
        for (size_t i = 0; i < nc.size() && i < oc.size(); ++i) {
            if (!nc[i]) continue;
            if (nc[i]->key().isValid()) continue; // handled above
            if (old_used[i]) continue;            // old slot already moved away
            old_used[i] = true;
            transferStateRecursive(oc[i].get(), nc[i].get());
        }
        return;
    }

    // No keys anywhere: stay on the original positional pairing for
    // a tight inner loop on common static trees.
    const size_t n = std::min(oc.size(), nc.size());
    for (size_t i = 0; i < n; ++i) {
        transferStateRecursive(oc[i].get(), nc[i].get());
    }
}

Element* Element::findFocused()
{
    if (focused_) return this;
    for (auto& child : children_) {
        if (auto* f = child->findFocused()) return f;
    }
    return nullptr;
}

namespace {
/// Resolves an asset path against the running binary's directory when it
/// looks relative, so a `Image { source: "logo.png" }` works regardless of
/// the launching shell's cwd. Absolute and Qt-resource (`:/foo`) paths
/// pass through unchanged.
QString resolveAssetPath(const QString& path)
{
    if (path.isEmpty() || path.startsWith(QLatin1Char(':')) || QFileInfo(path).isAbsolute()) {
        return path;
    }
    return QCoreApplication::applicationDirPath() + QLatin1Char('/') + path;
}
}   // namespace

Element::Element()
{
    yoga_node_ = YGNodeNew();
}

Element::~Element()
{
    if (yoga_node_) {
        YGNodeRemoveAllChildren(yoga_node_);
        YGNodeFree(yoga_node_);
        yoga_node_ = nullptr;
    }
}

void Element::layout(PaintCtx& ctx)
{
    QSizeF p = preferredSize(ctx);
    if (p.width() > 0.f) {
        YGNodeStyleSetWidth(yoga_node_, float(p.width()));
    }
    if (p.height() > 0.f) {
        YGNodeStyleSetHeight(yoga_node_, float(p.height()));
    }
    for (auto& child : children_) {
        child->layout(ctx);
    }
}

void Element::addChild(std::unique_ptr<Element> child)
{
    child->parent_ = this;
    YGNodeInsertChild(yoga_node_, child->yoga_node_, children_.size());
    children_.push_back(std::move(child));
}

void Element::syncFrameFromYoga(QPointF parentOrigin)
{
    const float left = YGNodeLayoutGetLeft(yoga_node_);
    const float top = YGNodeLayoutGetTop(yoga_node_);
    const float w = YGNodeLayoutGetWidth(yoga_node_);
    const float h = YGNodeLayoutGetHeight(yoga_node_);
    QPointF origin(parentOrigin.x() + left, parentOrigin.y() + top);
    frame_ = QRectF(origin, QSizeF(w, h));
    for (auto& child : children_) {
        child->syncFrameFromYoga(origin);
    }
}

void RectElement::paint(PaintCtx& ctx)
{
    ctx.setFill(fill_);
    if (radius_ > 0.f) {
        ctx.fillRoundRect(frame_, radius_);
    } else {
        ctx.fillRect(frame_);
    }
    if (strokeWidth_ > 0.f && strokeColor_.alpha() > 0) {
        ctx.setStroke(strokeColor_, strokeWidth_);
        if (radius_ > 0.f) {
            ctx.strokeRoundRect(frame_, radius_);
        } else {
            ctx.strokeRect(frame_);
        }
    }
}

void TextElement::paint(PaintCtx& ctx)
{
    ctx.setFont(font_);
    // Default text color follows the theme; setColor() still wins if the
    // user explicitly set one.
    ctx.setFill(explicit_color_ ? color_ : ctx.style().text);
    ctx.setTextBaseline(QCanvasPainter::TextBaseline::Top);
    ctx.setTextAlign(QCanvasPainter::TextAlign::Left);
    ctx.fillText(text_, QPointF(frame_.left(), frame_.top()));
}

QSizeF TextElement::preferredSize(PaintCtx& ctx) const
{
    return ctx.measureText(text_, font_);
}

void ButtonElement::paint(PaintCtx& ctx)
{
    const Style& s = ctx.style();

    // ~120ms time-to-target via 1 - exp(-speed*dt) shape; snappy without
    // looking digital. Both press_t_ and hover_t_ share the same clock.
    const qint64 now = ctx.elapsedMs();
    const float dt = last_tick_ms_ ? std::min<float>(0.05f, (now - last_tick_ms_) / 1000.f) : 0.f;
    last_tick_ms_ = now;
    constexpr float speed = 14.f;
    auto tween = [&](float& current, float target) {
        float diff = target - current;
        if (std::abs(diff) > 0.005f) {
            current += diff * std::min(1.f, dt * speed);
            ctx.requestAnimationFrame();
        } else {
            current = target;
        }
    };
    tween(press_t_, pressed_ ? 1.f : 0.f);
    tween(hover_t_, hovered_ ? 1.f : 0.f);

    // Press wins when both are active: lerp accent → hover, then →
    // pressed weighted by press_t_. Yields rest → hover → press.
    QColor bg = lerpColor(s.accent, s.accentHover, hover_t_);
    bg = lerpColor(bg, s.accentPressed, press_t_);
    ctx.fillRectShadow(frame_, bg, 8.f, QPointF(0, 2), 4.f, s.shadow);

    ctx.setFont(font_);
    ctx.setFill(s.onAccent);
    ctx.setTextAlign(QCanvasPainter::TextAlign::Center);
    ctx.setTextBaseline(QCanvasPainter::TextBaseline::Middle);
    ctx.fillText(label_, frame_.center());
}

QSizeF ButtonElement::preferredSize(PaintCtx& ctx) const
{
    QSizeF text = ctx.measureText(label_, font_);
    return QSizeF(text.width() + 24.0, std::max(text.height() + 16.0, 32.0));
}

void ButtonElement::transferStateFrom(Element& old)
{
    if (auto* o = dynamic_cast<ButtonElement*>(&old)) {
        // Press / hover state + the in-flight tweens survive a rebuild —
        // without this the user-visible animation snaps back to 0 every
        // time the store rebuilds, which happens on every button click.
        pressed_ = o->pressed_;
        hovered_ = o->hovered_;
        press_t_ = o->press_t_;
        hover_t_ = o->hover_t_;
        last_tick_ms_ = o->last_tick_ms_;
    }
}

TextFieldElement::TextFieldElement()
    : font_(QStringLiteral("Helvetica"), 14),
      font_metrics_(font_)
{
    YGNodeStyleSetMinHeight(yoga_node_, 32.f);
}

TextFieldElement* TextFieldElement::setFont(QFont f)
{
    font_ = std::move(f);
    font_metrics_ = QFontMetricsF(font_);
    return this;
}

TextFieldElement* TextFieldElement::setText(QString t)
{
    if (t == text_) return this;
    text_ = std::move(t);
    caret_pos_ = int(text_.size());
    selection_anchor_ = caret_pos_;
    preedit_.clear();
    preedit_caret_ = 0;
    return this;
}

void TextFieldElement::transferStateFrom(Element& old)
{
    if (auto* o = dynamic_cast<TextFieldElement*>(&old)) {
        text_ = o->text_;
        const int len = int(text_.size());
        caret_pos_ = std::min(o->caret_pos_, len);
        selection_anchor_ = std::min(o->selection_anchor_, len);
        focused_ = o->focused_;
        preedit_ = o->preedit_;
        preedit_caret_ = o->preedit_caret_;
        focus_t_ = o->focus_t_;
        last_tick_ms_ = o->last_tick_ms_;
    }
}

void TextFieldElement::emitTextChanged()
{
    if (on_text_changed_) on_text_changed_(text_);
}

void TextFieldElement::replaceSelectionWith(const QString& s)
{
    int lo = std::min(selection_anchor_, caret_pos_);
    int hi = std::max(selection_anchor_, caret_pos_);
    text_.replace(lo, hi - lo, s);
    caret_pos_ = lo + int(s.size());
    selection_anchor_ = caret_pos_;
}

void TextFieldElement::keyPressEvent(QKeyEvent* e)
{
    const bool hasSel = (selection_anchor_ != caret_pos_);
    const int len = int(text_.size());
    const auto mods = e->modifiers();
    const bool cmd = mods & (Qt::ControlModifier | Qt::MetaModifier);
    const auto selectionRange = [this] {
        int lo = std::min(selection_anchor_, caret_pos_);
        int hi = std::max(selection_anchor_, caret_pos_);
        return std::pair<int, int>(lo, hi);
    };
    auto extendOrMove = [&](int newPos) {
        caret_pos_ = std::clamp(newPos, 0, len);
        if (!(mods & Qt::ShiftModifier)) {
            selection_anchor_ = caret_pos_;
        }
    };

    switch (e->key()) {
    case Qt::Key_Left:      extendOrMove(caret_pos_ - 1); return;
    case Qt::Key_Right:     extendOrMove(caret_pos_ + 1); return;
    case Qt::Key_Home:      extendOrMove(0);              return;
    case Qt::Key_End:       extendOrMove(len);            return;
    case Qt::Key_Backspace:
        if (hasSel) {
            replaceSelectionWith(QString());
        } else if (caret_pos_ > 0) {
            text_.remove(caret_pos_ - 1, 1);
            --caret_pos_;
            selection_anchor_ = caret_pos_;
        } else {
            return;
        }
        emitTextChanged();
        return;
    case Qt::Key_Delete:
        if (hasSel) {
            replaceSelectionWith(QString());
        } else if (caret_pos_ < len) {
            text_.remove(caret_pos_, 1);
        } else {
            return;
        }
        emitTextChanged();
        return;
    case Qt::Key_A:
        if (cmd) {
            selection_anchor_ = 0;
            caret_pos_ = len;
            return;
        }
        break;
    case Qt::Key_C:
        if (cmd && hasSel) {
            auto [lo, hi] = selectionRange();
            QGuiApplication::clipboard()->setText(text_.mid(lo, hi - lo));
            return;
        }
        break;
    case Qt::Key_X:
        if (cmd && hasSel) {
            auto [lo, hi] = selectionRange();
            QGuiApplication::clipboard()->setText(text_.mid(lo, hi - lo));
            replaceSelectionWith(QString());
            emitTextChanged();
            return;
        }
        break;
    case Qt::Key_V:
        if (cmd) {
            QString clip = QGuiApplication::clipboard()->text();
            if (!clip.isEmpty()) {
                replaceSelectionWith(clip);
                emitTextChanged();
            }
            return;
        }
        break;
    default:
        break;
    }

    QString t = e->text();
    if (!t.isEmpty() && t[0].isPrint()) {
        replaceSelectionWith(t);
        emitTextChanged();
    }
}

void TextFieldElement::inputMethodEvent(QInputMethodEvent* e)
{
    const QString commit = e->commitString();
    if (!commit.isEmpty()) {
        replaceSelectionWith(commit);
        preedit_.clear();
        preedit_caret_ = 0;
        emitTextChanged();
    }
    preedit_ = e->preeditString();
    preedit_caret_ = int(preedit_.size());
    for (const QInputMethodEvent::Attribute& a : e->attributes()) {
        if (a.type == QInputMethodEvent::Cursor) {
            preedit_caret_ = std::clamp<int>(a.start, 0, int(preedit_.size()));
        }
    }
    e->accept();
}

QVariant TextFieldElement::inputMethodQuery(Qt::InputMethodQuery query) const
{
    switch (query) {
    case Qt::ImEnabled:
        return true;
    case Qt::ImHints:
        return int(Qt::ImhNone);
    case Qt::ImSurroundingText:
        return text_;
    case Qt::ImCursorPosition:
        return caret_pos_;
    case Qt::ImAnchorPosition:
        return selection_anchor_;
    case Qt::ImCurrentSelection: {
        int lo = std::min(selection_anchor_, caret_pos_);
        int hi = std::max(selection_anchor_, caret_pos_);
        return text_.mid(lo, hi - lo);
    }
    case Qt::ImCursorRectangle: {
        // Window-relative rect so QInputMethod can anchor IME popups.
        qreal x = frame_.left() + 8.0
                  + font_metrics_.horizontalAdvance(text_.left(caret_pos_));
        qreal y = frame_.top() + (frame_.height() - font_metrics_.height()) / 2.0;
        return QRectF(x, y, 1.0, font_metrics_.height());
    }
    default:
        return {};
    }
}

void TextFieldElement::paint(PaintCtx& ctx)
{
    const Style& s = ctx.style();

    // Tween focus_t_ ~120ms to/from `focused_` so the border / glow ease
    // in instead of snapping. Same easing curve as ButtonElement.
    const qint64 now = ctx.elapsedMs();
    const float dt = last_tick_ms_ ? std::min<float>(0.05f, (now - last_tick_ms_) / 1000.f) : 0.f;
    last_tick_ms_ = now;
    constexpr float speed = 14.f;
    const float target = focused_ ? 1.f : 0.f;
    if (std::abs(target - focus_t_) > 0.005f) {
        focus_t_ += (target - focus_t_) * std::min(1.f, dt * speed);
        ctx.requestAnimationFrame();
    } else {
        focus_t_ = target;
    }

    ctx.setFill(s.surface);
    ctx.fillRoundRect(frame_, 6.f);
    // Soft outer glow ring while focusing in — bordered halo using the
    // accent color faded by focus_t_.
    if (focus_t_ > 0.01f) {
        QColor glow = s.borderFocused;
        glow.setAlpha(int(60 * focus_t_));
        ctx.setStroke(glow, 4.f);
        ctx.strokeRoundRect(frame_.adjusted(-2, -2, 2, 2), 8.f);
    }
    QColor border = lerpColor(s.border, s.borderFocused, focus_t_);
    ctx.setStroke(border, 1.f + focus_t_);
    ctx.strokeRoundRect(frame_, 6.f);

    ctx.pushClipRect(frame_.adjusted(2, 2, -2, -2));

    const qreal padX = 8.0;
    const qreal ty = frame_.center().y() - font_metrics_.height() / 2.0;

    QString display = text_;
    if (!preedit_.isEmpty()) {
        display = text_.left(caret_pos_) + preedit_ + text_.mid(caret_pos_);
    }

    if (display.isEmpty() && !placeholder_.isEmpty()) {
        ctx.setFont(font_);
        ctx.setFill(s.textDim);
        ctx.setTextBaseline(QCanvasPainter::TextBaseline::Top);
        ctx.setTextAlign(QCanvasPainter::TextAlign::Left);
        ctx.fillText(placeholder_, QPointF(frame_.left() + padX, ty));
    } else {
        if (focused_ && selection_anchor_ != caret_pos_ && preedit_.isEmpty()) {
            int lo = std::min(selection_anchor_, caret_pos_);
            int hi = std::max(selection_anchor_, caret_pos_);
            qreal x0 = frame_.left() + padX + font_metrics_.horizontalAdvance(text_.left(lo));
            qreal x1 = frame_.left() + padX + font_metrics_.horizontalAdvance(text_.left(hi));
            ctx.setFill(s.selection);
            ctx.fillRect(QRectF(x0, ty, x1 - x0, font_metrics_.height()));
        }
        ctx.setFont(font_);
        ctx.setFill(s.text);
        ctx.setTextBaseline(QCanvasPainter::TextBaseline::Top);
        ctx.setTextAlign(QCanvasPainter::TextAlign::Left);
        ctx.fillText(display, QPointF(frame_.left() + padX, ty));
    }

    if (focused_) {
        // 1.06s period (530ms on / 530ms off) matches the macOS / Wayland
        // standard caret blink and stays out of resonance with 60Hz frame
        // ticks. requestAnimationFrame keeps the paint loop alive while
        // focused; idle textfields go back to event-driven painting.
        const bool caret_on = (ctx.elapsedMs() % 1060) < 530 || !preedit_.isEmpty();
        ctx.requestAnimationFrame();
        if (caret_on) {
            QString prefix = text_.left(caret_pos_);
            if (!preedit_.isEmpty()) {
                prefix += preedit_.left(preedit_caret_);
            }
            qreal cx = frame_.left() + padX + font_metrics_.horizontalAdvance(prefix);
            ctx.setStroke(s.text, 1.5f);
            ctx.strokeLine(QPointF(cx, ty), QPointF(cx, ty + font_metrics_.height()));
        }
    }

    ctx.popClip();
}

QSizeF TextFieldElement::preferredSize(PaintCtx& /*ctx*/) const
{
    qreal w = std::max<qreal>(160.0, font_metrics_.horizontalAdvance(text_) + 24.0);
    qreal h = std::max<qreal>(32.0, font_metrics_.height() + 12.0);
    return QSizeF(w, h);
}

namespace {
// Time-based ease shared by both chart types.
void tweenList(QList<qreal>& current, const QList<qreal>& target, float dt,
               PaintCtx& ctx)
{
    while (current.size() < target.size()) current.push_back(0.0);
    while (current.size() > target.size()) current.removeLast();
    constexpr float speed = 10.f;  // ~150ms time-to-target
    bool any = false;
    for (int i = 0; i < target.size(); ++i) {
        qreal diff = target[i] - current[i];
        if (std::abs(diff) > 0.5) {
            current[i] += diff * std::min<qreal>(1.0, dt * speed);
            any = true;
        } else {
            current[i] = target[i];
        }
    }
    if (any) ctx.requestAnimationFrame();
}

qreal listMax(const QList<qreal>& xs)
{
    qreal m = 0;
    for (auto v : xs) if (v > m) m = v;
    return m;
}
}   // namespace

ProgressBarElement* ProgressBarElement::setValue(qreal v)
{
    value_ = std::clamp<qreal>(v, 0.0, 1.0);
    return this;
}

ProgressBarElement* ProgressBarElement::setSize(QSizeF s)
{
    explicit_size_ = s;
    return this;
}

void ProgressBarElement::transferStateFrom(Element& old)
{
    if (auto* o = dynamic_cast<ProgressBarElement*>(&old)) {
        animated_value_ = o->animated_value_;
        last_tick_ms_ = o->last_tick_ms_;
    }
}

QSizeF ProgressBarElement::preferredSize(PaintCtx& /*ctx*/) const
{
    if (!explicit_size_.isEmpty()) return explicit_size_;
    return QSizeF(240, 12);
}

void ProgressBarElement::paint(PaintCtx& ctx)
{
    const Style& s = ctx.style();

    const qint64 now = ctx.elapsedMs();
    const float dt = last_tick_ms_ ? std::min<float>(0.05f, (now - last_tick_ms_) / 1000.f) : 0.f;
    last_tick_ms_ = now;
    constexpr float speed = 10.f;
    qreal diff = value_ - animated_value_;
    if (std::abs(diff) > 0.002) {
        animated_value_ += diff * std::min<qreal>(1.0, dt * speed);
        ctx.requestAnimationFrame();
    } else {
        animated_value_ = value_;
    }

    qreal radius = std::min<qreal>(frame_.height() / 2.0, 6.0);
    ctx.setFill(lerpColor(s.surface, s.windowBg, 0.4f));
    ctx.fillRoundRect(frame_, radius);
    if (animated_value_ > 0.0) {
        QRectF fill(frame_.left(), frame_.top(),
                    frame_.width() * animated_value_, frame_.height());
        ctx.setFill(s.accent);
        ctx.fillRoundRect(fill, radius);
    }
}

SpinnerElement* SpinnerElement::setSize(QSizeF s)
{
    explicit_size_ = s;
    return this;
}

QSizeF SpinnerElement::preferredSize(PaintCtx& /*ctx*/) const
{
    if (!explicit_size_.isEmpty()) return explicit_size_;
    return QSizeF(32, 32);
}

void SpinnerElement::paint(PaintCtx& ctx)
{
    const Style& s = ctx.style();
    QPointF center = frame_.center();
    qreal r = std::min(frame_.width(), frame_.height()) / 2.0 - 2.0;
    if (r <= 0) return;

    // Background ring.
    ctx.setStroke(lerpColor(s.surface, s.windowBg, 0.4f), 3.f);
    ctx.raw().beginPath();
    ctx.raw().circle(center, float(r));
    ctx.raw().stroke();

    // Rotating arc covering ~120° at 1 revolution per second.
    const qreal sweep_deg = 120.0;
    const qreal start_deg = (ctx.elapsedMs() % 1000) * 0.36;   // 0..360
    ctx.raw().beginPath();
    ctx.raw().arc(center, float(r),
                  float(qDegreesToRadians(start_deg)),
                  float(qDegreesToRadians(start_deg + sweep_deg)));
    ctx.setStroke(s.accent, 3.f);
    ctx.raw().stroke();
    ctx.requestAnimationFrame();
}

BarChartElement* BarChartElement::setData(QList<qreal> data)
{
    data_ = std::move(data);
    return this;
}

BarChartElement* BarChartElement::setLabels(QStringList labels)
{
    labels_ = std::move(labels);
    return this;
}

BarChartElement* BarChartElement::setSize(QSizeF s)
{
    explicit_size_ = s;
    return this;
}

void BarChartElement::transferStateFrom(Element& old)
{
    if (auto* o = dynamic_cast<BarChartElement*>(&old)) {
        animated_ = o->animated_;
        last_tick_ms_ = o->last_tick_ms_;
    }
}

QSizeF BarChartElement::preferredSize(PaintCtx& /*ctx*/) const
{
    if (!explicit_size_.isEmpty()) return explicit_size_;
    return QSizeF(320, 180);
}

void BarChartElement::paint(PaintCtx& ctx)
{
    const Style& s = ctx.style();
    ctx.setFill(s.surface);
    ctx.fillRoundRect(frame_, 6.f);
    ctx.setStroke(s.border, 1.f);
    ctx.strokeRoundRect(frame_, 6.f);

    const qint64 now = ctx.elapsedMs();
    const float dt = last_tick_ms_ ? std::min<float>(0.05f, (now - last_tick_ms_) / 1000.f) : 0.f;
    last_tick_ms_ = now;
    tweenList(animated_, data_, dt, ctx);

    const qreal padX = 12.0;
    const qreal padY = 12.0;
    const qreal label_h = labels_.isEmpty() ? 0 : 18.0;
    QRectF plot = frame_.adjusted(padX, padY, -padX, -(padY + label_h));
    if (plot.isEmpty()) return;

    qreal max_val = listMax(data_);
    if (max_val <= 0) max_val = 1.0;

    const int n = animated_.size();
    if (n == 0) return;
    const qreal slot = plot.width() / n;
    const qreal bar_w = std::max<qreal>(2.0, slot * 0.7);

    QFontMetricsF fm(QFont(QStringLiteral("Helvetica"), 11));
    for (int i = 0; i < n; ++i) {
        qreal h = plot.height() * (animated_[i] / max_val);
        if (h < 0) h = 0;
        const qreal cx = plot.left() + slot * (i + 0.5);
        QRectF bar(cx - bar_w / 2.0, plot.bottom() - h, bar_w, h);
        ctx.setFill(s.accent);
        ctx.fillRoundRect(bar, 3.f);
        if (i < labels_.size()) {
            ctx.setFont(QFont(QStringLiteral("Helvetica"), 11));
            ctx.setFill(s.textDim);
            ctx.setTextAlign(QCanvasPainter::TextAlign::Center);
            ctx.setTextBaseline(QCanvasPainter::TextBaseline::Top);
            ctx.fillText(labels_[i], QPointF(cx, plot.bottom() + 3));
        }
    }
}

LineChartElement* LineChartElement::setData(QList<qreal> data)
{
    data_ = std::move(data);
    return this;
}

LineChartElement* LineChartElement::setLabels(QStringList labels)
{
    labels_ = std::move(labels);
    return this;
}

LineChartElement* LineChartElement::setSize(QSizeF s)
{
    explicit_size_ = s;
    return this;
}

void LineChartElement::transferStateFrom(Element& old)
{
    if (auto* o = dynamic_cast<LineChartElement*>(&old)) {
        animated_ = o->animated_;
        last_tick_ms_ = o->last_tick_ms_;
    }
}

QSizeF LineChartElement::preferredSize(PaintCtx& /*ctx*/) const
{
    if (!explicit_size_.isEmpty()) return explicit_size_;
    return QSizeF(320, 180);
}

void LineChartElement::paint(PaintCtx& ctx)
{
    const Style& s = ctx.style();
    ctx.setFill(s.surface);
    ctx.fillRoundRect(frame_, 6.f);
    ctx.setStroke(s.border, 1.f);
    ctx.strokeRoundRect(frame_, 6.f);

    const qint64 now = ctx.elapsedMs();
    const float dt = last_tick_ms_ ? std::min<float>(0.05f, (now - last_tick_ms_) / 1000.f) : 0.f;
    last_tick_ms_ = now;
    tweenList(animated_, data_, dt, ctx);

    const qreal padX = 12.0;
    const qreal padY = 12.0;
    const qreal label_h = labels_.isEmpty() ? 0 : 18.0;
    QRectF plot = frame_.adjusted(padX, padY, -padX, -(padY + label_h));
    if (plot.isEmpty()) return;

    qreal max_val = listMax(data_);
    if (max_val <= 0) max_val = 1.0;

    const int n = animated_.size();
    if (n == 0) return;
    auto sample_x = [&](int i) {
        return n == 1 ? plot.center().x()
                      : plot.left() + plot.width() * qreal(i) / qreal(n - 1);
    };
    auto sample_y = [&](int i) {
        return plot.bottom() - plot.height() * (animated_[i] / max_val);
    };

    if (n >= 2) {
        ctx.setStroke(s.accent, 2.f);
        ctx.raw().beginPath();
        ctx.raw().moveTo(QPointF(sample_x(0), sample_y(0)));
        for (int i = 1; i < n; ++i) {
            ctx.raw().lineTo(QPointF(sample_x(i), sample_y(i)));
        }
        ctx.raw().stroke();
    }
    for (int i = 0; i < n; ++i) {
        ctx.setFill(s.accent);
        ctx.fillCircle(QPointF(sample_x(i), sample_y(i)), 3.5f);
    }
    if (!labels_.isEmpty()) {
        ctx.setFont(QFont(QStringLiteral("Helvetica"), 11));
        ctx.setFill(s.textDim);
        ctx.setTextAlign(QCanvasPainter::TextAlign::Center);
        ctx.setTextBaseline(QCanvasPainter::TextBaseline::Top);
        for (int i = 0; i < n && i < labels_.size(); ++i) {
            ctx.fillText(labels_[i], QPointF(sample_x(i), plot.bottom() + 3));
        }
    }
}

SvgElement::SvgElement(QString source) : source_(std::move(source)) {}

SvgElement* SvgElement::setSource(QString s)
{
    if (s != source_) {
        source_ = std::move(s);
        upload_attempted_ = false;
        canvas_image_ = QCanvasImage();
        last_pixel_size_ = QSize();
    }
    return this;
}

SvgElement* SvgElement::setSize(QSizeF s)
{
    if (s != explicit_size_) {
        explicit_size_ = s;
        // Force re-rasterize next paint at the new resolution.
        upload_attempted_ = false;
        last_pixel_size_ = QSize();
    }
    return this;
}

void SvgElement::rasterizeIfNeeded(PaintCtx& ctx, QSize px)
{
    if (upload_attempted_ && px == last_pixel_size_) return;
    QSvgRenderer renderer(resolveAssetPath(source_));
    if (!renderer.isValid()) {
        upload_attempted_ = true;
        return;
    }
    QImage img(px, QImage::Format_ARGB32_Premultiplied);
    img.fill(Qt::transparent);
    QPainter p(&img);
    renderer.render(&p);
    p.end();
    canvas_image_ = ctx.addImage(img);
    last_pixel_size_ = px;
    upload_attempted_ = true;
}

void SvgElement::paint(PaintCtx& ctx)
{
    QSize px(int(frame_.width()), int(frame_.height()));
    if (px.isEmpty()) return;
    rasterizeIfNeeded(ctx, px);
    if (!last_pixel_size_.isValid()) return;
    ctx.drawImage(canvas_image_, frame_);
}

QSizeF SvgElement::preferredSize(PaintCtx& /*ctx*/) const
{
    if (!explicit_size_.isEmpty()) return explicit_size_;
    return QSizeF(64, 64);
}

ImageElement::ImageElement(QString source) : source_(std::move(source)) {}

ImageElement* ImageElement::setSource(QString s)
{
    if (s != source_) {
        source_ = std::move(s);
        upload_attempted_ = false;
        canvas_image_ = QCanvasImage();
        natural_size_ = QSizeF();
    }
    return this;
}

void ImageElement::paint(PaintCtx& ctx)
{
    if (!upload_attempted_) {
        QImageReader reader(resolveAssetPath(source_));
        QImage img = reader.read();
        if (!img.isNull()) {
            canvas_image_ = ctx.addImage(img);
            natural_size_ = QSizeF(img.size());
        }
        upload_attempted_ = true;
    }
    if (natural_size_.isEmpty()) return;

    QRectF dst = frame_;
    if (fit_ == Qt::KeepAspectRatio && !natural_size_.isEmpty()) {
        QSizeF scaled = natural_size_;
        scaled.scale(frame_.size(), Qt::KeepAspectRatio);
        dst = QRectF(frame_.left() + (frame_.width() - scaled.width()) / 2.0,
                     frame_.top() + (frame_.height() - scaled.height()) / 2.0,
                     scaled.width(), scaled.height());
    }
    ctx.drawImage(canvas_image_, dst);
}

QSizeF ImageElement::preferredSize(PaintCtx& /*ctx*/) const
{
    if (!explicit_size_.isEmpty()) return explicit_size_;
    if (!natural_size_.isEmpty()) return natural_size_;
    // First frame before upload — give layout a small placeholder so the
    // window doesn't collapse to zero.
    return QSizeF(64, 64);
}

namespace {
void paintChildren(Element& self, PaintCtx& ctx)
{
    for (auto& child : self.mutableChildren()) {
        child->paint(ctx);
    }
}
}   // namespace

ColumnElement::ColumnElement()
{
    YGNodeStyleSetFlexDirection(yoga_node_, YGFlexDirectionColumn);
    YGNodeStyleSetGap(yoga_node_, YGGutterAll, 4.f);
    YGNodeStyleSetPadding(yoga_node_, YGEdgeAll, 8.f);
}

void ColumnElement::paint(PaintCtx& ctx)
{
    paintChildren(*this, ctx);
}

ColumnElement* ColumnElement::setSpacing(qreal v)
{
    YGNodeStyleSetGap(yoga_node_, YGGutterAll, static_cast<float>(v));
    return this;
}

ColumnElement* ColumnElement::setPadding(qreal v)
{
    YGNodeStyleSetPadding(yoga_node_, YGEdgeAll, static_cast<float>(v));
    return this;
}

RowElement::RowElement()
{
    YGNodeStyleSetFlexDirection(yoga_node_, YGFlexDirectionRow);
    YGNodeStyleSetGap(yoga_node_, YGGutterAll, 4.f);
    YGNodeStyleSetAlignItems(yoga_node_, YGAlignCenter);
}

void RowElement::paint(PaintCtx& ctx)
{
    paintChildren(*this, ctx);
}

RowElement* RowElement::setSpacing(qreal v)
{
    YGNodeStyleSetGap(yoga_node_, YGGutterAll, static_cast<float>(v));
    return this;
}

RowElement* RowElement::setPadding(qreal v)
{
    YGNodeStyleSetPadding(yoga_node_, YGEdgeAll, static_cast<float>(v));
    return this;
}

void StackElement::addChild(std::unique_ptr<Element> child)
{
    YGNodeStyleSetPositionType(child->yogaNode(), YGPositionTypeAbsolute);
    YGNodeStyleSetPosition(child->yogaNode(), YGEdgeAll, 0);
    Element::addChild(std::move(child));
}

void StackElement::paint(PaintCtx& ctx)
{
    paintChildren(*this, ctx);
}

ModalElement::ModalElement()
{
    YGNodeStyleSetPositionType(yoga_node_, YGPositionTypeAbsolute);
    YGNodeStyleSetPosition(yoga_node_, YGEdgeLeft, 0.f);
    YGNodeStyleSetPosition(yoga_node_, YGEdgeTop, 0.f);
    YGNodeStyleSetPosition(yoga_node_, YGEdgeRight, 0.f);
    YGNodeStyleSetPosition(yoga_node_, YGEdgeBottom, 0.f);
    YGNodeStyleSetFlexDirection(yoga_node_, YGFlexDirectionColumn);
    YGNodeStyleSetJustifyContent(yoga_node_, YGJustifyCenter);
    YGNodeStyleSetAlignItems(yoga_node_, YGAlignCenter);

    // dialog_ is a private inner Column that holds user children. It gives
    // the dialog its own padding/gap and lets ModalElement paint a single
    // surface card behind it.
    auto dialog = std::make_unique<ColumnElement>();
    dialog_ = dialog.get();
    Element::addChild(std::move(dialog));
}

void ModalElement::addChild(std::unique_ptr<Element> child)
{
    dialog_->addChild(std::move(child));
}

void ModalElement::paint(PaintCtx& ctx)
{
    ctx.setFill(QColor(0, 0, 0, 130));
    ctx.fillRect(frame_);

    const Style& s = ctx.style();
    ctx.fillRectShadow(dialog_->frame(), s.surface, 12.f,
                       QPointF(0, 8), 24.f, s.shadow);
    dialog_->paint(ctx);
}

bool ModalElement::dispatchClick(QPointF localPos)
{
    if (!hitTest(localPos)) return false;
    // Dialog handles clicks within itself; clicks on the dim background
    // get swallowed so they don't reach widgets behind the modal.
    dialog_->dispatchClick(localPos);
    return true;
}

Element* ModalElement::findFocusableAt(QPointF localPos)
{
    if (!hitTest(localPos)) return nullptr;
    // Only focusable targets inside the dialog itself; clicking the dim
    // background drops focus instead of leaking it to the page underneath.
    return dialog_->findFocusableAt(localPos);
}

DataTableElement::DataTableElement()
{
    YGNodeStyleSetFlexDirection(yoga_node_, YGFlexDirectionColumn);
    YGNodeStyleSetGap(yoga_node_, YGGutterAll, 0.f);
    YGNodeStyleSetPadding(yoga_node_, YGEdgeAll, 0.f);
}

void DataTableElement::paint(PaintCtx& ctx)
{
    const Style& s = ctx.style();
    ctx.setFill(s.surface);
    ctx.fillRoundRect(frame_, 6.f);

    const QColor stripe = lerpColor(s.surface, s.windowBg, 0.4f);
    const QColor headerBg = lerpColor(s.surface, s.border, 0.3f);
    const QColor divider = s.border;

    for (size_t i = 0; i < children_.size(); ++i) {
        const QRectF rf = children_[i]->frame();
        const bool is_header = (i == 0);
        if (is_header) {
            ctx.setFill(headerBg);
            ctx.fillRect(rf);
        } else if (i % 2 == 0) {
            ctx.setFill(stripe);
            ctx.fillRect(rf);
        }
        if (i + 1 < children_.size()) {
            ctx.setStroke(divider, is_header ? 1.5f : 0.5f);
            ctx.strokeLine(QPointF(rf.left(), rf.bottom()),
                           QPointF(rf.right(), rf.bottom()));
        }
    }

    paintChildren(*this, ctx);

    ctx.setStroke(s.border, 1.f);
    ctx.strokeRoundRect(frame_, 6.f);
}

ListViewElement::ListViewElement(ScrollAxis axis) : axis_(axis)
{
    YGNodeStyleSetFlexDirection(yoga_node_,
        axis_ == ScrollAxis::Horizontal ? YGFlexDirectionRow : YGFlexDirectionColumn);
    YGNodeStyleSetGap(yoga_node_, YGGutterAll, 4.f);
    YGNodeStyleSetPadding(yoga_node_, YGEdgeAll, 8.f);
    // flexBasis=0 + flexGrow=1 is the canonical "scroll viewport" recipe:
    // it tells Yoga to ignore children's intrinsic content size when
    // sizing the listview itself, then absorb whatever space the parent
    // has left. Without flexBasis=0, Yoga grows the listview to fit its
    // content (e.g. 60 rows × 14px = 1092px inside a 480px window),
    // frame == content, max_off = 0, and scrolling silently dies.
    YGNodeStyleSetFlexBasis(yoga_node_, 0.f);
    YGNodeStyleSetFlexGrow(yoga_node_, 1.f);
    YGNodeStyleSetFlexShrink(yoga_node_, 1.f);
    if (axis_ == ScrollAxis::Vertical) {
        YGNodeStyleSetMinHeight(yoga_node_, 80.f);
    } else {
        YGNodeStyleSetMinWidth(yoga_node_, 80.f);
    }
}

void ListViewElement::transferStateFrom(Element& old)
{
    if (auto* o = dynamic_cast<ListViewElement*>(&old)) {
        scroll_pos_ = o->scroll_pos_;
        scroll_target_ = o->scroll_target_;
        scroll_last_tick_ms_ = o->scroll_last_tick_ms_;
        focused_ = o->focused_;
    }
}

bool ListViewElement::setScrollOffset(qreal v)
{
    qreal viewport = (axis_ == ScrollAxis::Vertical) ? frame_.height() : frame_.width();
    qreal max_off = std::max<qreal>(0.0, content_extent_ - viewport);
    qreal clamped = std::clamp<qreal>(v, 0.0, max_off);
    if (clamped == scroll_pos_ && clamped == scroll_target_) return false;
    // Programmatic jump — both target and visible position snap together.
    scroll_pos_ = scroll_target_ = clamped;
    return true;
}

void ListViewElement::advanceScrollTween(PaintCtx& ctx)
{
    if (scroll_pos_ == scroll_target_) return;
    const qint64 now = ctx.elapsedMs();
    const float dt = scroll_last_tick_ms_
        ? std::min<float>(0.05f, (now - scroll_last_tick_ms_) / 1000.f) : 0.f;
    scroll_last_tick_ms_ = now;
    constexpr float speed = 14.f;  // ~120ms time-to-target
    qreal diff = scroll_target_ - scroll_pos_;
    if (std::abs(diff) < 0.5) {
        scroll_pos_ = scroll_target_;
    } else {
        scroll_pos_ += diff * std::min<qreal>(1.0, dt * speed);
        ctx.requestAnimationFrame();
    }
}

namespace {
// Yoga has no viewport concept, so the scroll offset is applied as a
// post-pass that walks every descendant frame. Free function (no captures)
// avoids per-paint std::function allocation.
void shiftFrame(Element* el, qreal dx, qreal dy)
{
    QRectF f = el->frame();
    f.translate(dx, dy);
    el->setFrame(f);
    for (auto& gc : el->mutableChildren()) shiftFrame(gc.get(), dx, dy);
}
}

void ListViewElement::applyScrollOffsetToChildren()
{
    if (scroll_pos_ == 0) return;
    qreal dx = (axis_ == ScrollAxis::Horizontal) ? -scroll_pos_ : 0;
    qreal dy = (axis_ == ScrollAxis::Vertical) ? -scroll_pos_ : 0;
    for (auto& child : children_) {
        shiftFrame(child.get(), dx, dy);
    }
}

void ListViewElement::layout(PaintCtx& ctx)
{
    if (virtualized_) {
        // Skip the per-child Yoga walk entirely; in virtualized mode each
        // visible row's frame is computed directly in paint() from the
        // row index + item_height_. Yoga still owns the listview's own
        // outer layout via the base preferredSize() / size code path.
        QSizeF p = preferredSize(ctx);
        if (p.width() > 0.f) YGNodeStyleSetWidth(yoga_node_, float(p.width()));
        if (p.height() > 0.f) YGNodeStyleSetHeight(yoga_node_, float(p.height()));
        return;
    }
    Element::layout(ctx);
}

qreal ListViewElement::recomputeScrollExtent()
{
    qreal max_edge = 0;
    qreal min_edge = (axis_ == ScrollAxis::Vertical) ? frame_.top() : frame_.left();
    for (const auto& child : children_) {
        qreal edge = (axis_ == ScrollAxis::Vertical)
                       ? child->frame().bottom()
                       : child->frame().right();
        if (edge > max_edge) max_edge = edge;
    }
    content_extent_ = max_edge - min_edge;
    qreal viewport = (axis_ == ScrollAxis::Vertical) ? frame_.height() : frame_.width();
    qreal max_off = std::max<qreal>(0.0, content_extent_ - viewport);
    if (scroll_pos_ > max_off) scroll_pos_ = max_off;
    if (scroll_target_ > max_off) scroll_target_ = max_off;
    return max_off;
}

void ListViewElement::paintScrollbar(PaintCtx& ctx, qreal max_off)
{
    if (max_off <= 0.0) return;
    QRectF thumb = scrollbarThumbRect();
    if (thumb.isEmpty()) return;
    ctx.setFill(ctx.style().scrollbar);
    ctx.fillRoundRect(thumb, 2.f);
}

QRectF ListViewElement::scrollbarThumbRect() const
{
    qreal viewport = (axis_ == ScrollAxis::Vertical) ? frame_.height() : frame_.width();
    qreal max_off = std::max<qreal>(0.0, content_extent_ - viewport);
    if (max_off <= 0.0) return {};
    const qreal thick = 4.f;
    if (axis_ == ScrollAxis::Vertical) {
        const qreal track_x = frame_.right() - thick - 2.0;
        const qreal track_y = frame_.top() + 2.0;
        const qreal track_h = frame_.height() - 4.0;
        const qreal thumb_h = std::max<qreal>(20.0,
            track_h * (frame_.height() / (max_off + frame_.height())));
        const qreal thumb_y = track_y + (track_h - thumb_h) * (scroll_pos_ / max_off);
        return QRectF(track_x, thumb_y, thick, thumb_h);
    } else {
        const qreal track_y = frame_.bottom() - thick - 2.0;
        const qreal track_x = frame_.left() + 2.0;
        const qreal track_w = frame_.width() - 4.0;
        const qreal thumb_w = std::max<qreal>(20.0,
            track_w * (frame_.width() / (max_off + frame_.width())));
        const qreal thumb_x = track_x + (track_w - thumb_w) * (scroll_pos_ / max_off);
        return QRectF(thumb_x, track_y, thumb_w, thick);
    }
}

bool ListViewElement::hitScrollbarThumb(QPointF localPos) const
{
    QRectF thumb = scrollbarThumbRect();
    if (thumb.isEmpty()) return false;
    // Inflate by a couple of pixels so a precise click on the 4-px-thick
    // thumb still registers — matches what most native scrollbars do.
    QRectF hit = thumb.adjusted(-3.0, -3.0, 3.0, 3.0);
    return hit.contains(localPos);
}

void ListViewElement::beginScrollbarDrag(QPointF localPos)
{
    dragging_scrollbar_ = true;
    drag_start_pos_ = localPos;
    drag_start_scroll_ = scroll_pos_;
}

void ListViewElement::updateScrollbarDrag(QPointF localPos)
{
    if (!dragging_scrollbar_) return;
    qreal viewport = (axis_ == ScrollAxis::Vertical) ? frame_.height() : frame_.width();
    qreal max_off = std::max<qreal>(0.0, content_extent_ - viewport);
    if (max_off <= 0.0) return;

    // Translate mouse delta in track coordinates back to a scroll
    // offset. Track usable length excludes the thumb itself: dragging
    // the thumb across (track_h - thumb_h) pixels covers the full
    // 0..max_off range.
    const qreal thick = 4.f;
    const qreal track_extent = (axis_ == ScrollAxis::Vertical)
        ? (frame_.height() - 4.0)
        : (frame_.width() - 4.0);
    const qreal viewport_extent = (axis_ == ScrollAxis::Vertical)
        ? frame_.height() : frame_.width();
    const qreal thumb_extent = std::max<qreal>(20.0,
        track_extent * (viewport_extent / (max_off + viewport_extent)));
    const qreal usable_track = std::max<qreal>(1.0, track_extent - thumb_extent);

    qreal mouse_delta = (axis_ == ScrollAxis::Vertical)
        ? (localPos.y() - drag_start_pos_.y())
        : (localPos.x() - drag_start_pos_.x());
    qreal new_scroll = drag_start_scroll_ + mouse_delta * (max_off / usable_track);
    new_scroll = std::clamp<qreal>(new_scroll, 0.0, max_off);
    // Snap both pos and target so the easing tween doesn't lag the
    // user's drag.
    scroll_pos_ = new_scroll;
    scroll_target_ = new_scroll;
    (void)thick; // kept for symmetry with paintScrollbar's geometry.
}

void ListViewElement::paint(PaintCtx& ctx)
{
    const Style& s = ctx.style();
    if (virtualized_) {
        // Compute extent from item count alone — children frames are
        // never set outside the visible window, so the usual
        // recomputeScrollExtent walk would see all-zero frames.
        const qreal padding = 8.0;
        const int n = int(children_.size());
        content_extent_ = padding * 2.0 + n * item_height_;
        const qreal viewport = frame_.height();
        const qreal max_off = std::max<qreal>(0.0, content_extent_ - viewport);
        if (scroll_pos_ > max_off) scroll_pos_ = max_off;
        if (scroll_target_ > max_off) scroll_target_ = max_off;
        advanceScrollTween(ctx);

        ctx.setFill(s.surface);
        ctx.fillRoundRect(frame_, 6.f);
        ctx.setStroke(s.border, 1.f);
        ctx.strokeRoundRect(frame_, 6.f);

        ctx.pushClipRect(frame_);
        const qreal inner_top = frame_.top() + padding;
        const qreal inner_left = frame_.left() + padding;
        const qreal inner_w = frame_.width() - 2 * padding;
        // ±1 row of slack so partially-visible rows at the edges paint.
        const int start = std::max<int>(0, int((scroll_pos_) / item_height_) - 1);
        const int end = std::min<int>(n,
            int((scroll_pos_ + viewport) / item_height_) + 2);
        for (int i = start; i < end; ++i) {
            qreal y = inner_top + i * item_height_ - scroll_pos_;
            Element* c = children_[i].get();
            c->setFrame(QRectF(inner_left, y, inner_w, item_height_));
            c->paint(ctx);
        }
        ctx.popClip();

        paintScrollbar(ctx, max_off);
        return;
    }

    qreal max_off = recomputeScrollExtent();
    advanceScrollTween(ctx);
    applyScrollOffsetToChildren();

    ctx.setFill(s.surface);
    ctx.fillRoundRect(frame_, 6.f);
    ctx.setStroke(s.border, 1.f);
    ctx.strokeRoundRect(frame_, 6.f);

    ctx.pushClipRect(frame_);
    paintChildren(*this, ctx);
    ctx.popClip();

    paintScrollbar(ctx, max_off);
}

ScrollViewElement::ScrollViewElement(ScrollAxis axis) : ListViewElement(axis) {}

void ScrollViewElement::paint(PaintCtx& ctx)
{
    qreal max_off = recomputeScrollExtent();
    advanceScrollTween(ctx);
    applyScrollOffsetToChildren();

    ctx.pushClipRect(frame_);
    paintChildren(*this, ctx);
    ctx.popClip();

    paintScrollbar(ctx, max_off);
}

bool ListViewElement::dispatchWheel(QPointF localPos, qreal dx, qreal dy)
{
    if (!hitTest(localPos)) return false;
    for (auto it = children_.rbegin(); it != children_.rend(); ++it) {
        if (auto* lv = dynamic_cast<ListViewElement*>(it->get())) {
            if (lv->dispatchWheel(localPos, dx, dy)) return true;
        }
    }
    qreal delta = (axis_ == ScrollAxis::Horizontal) ? dx : dy;
    qreal viewport = (axis_ == ScrollAxis::Vertical) ? frame_.height() : frame_.width();
    qreal max_off = std::max<qreal>(0.0, content_extent_ - viewport);
    qreal new_target = std::clamp<qreal>(scroll_target_ + delta, 0.0, max_off);
    if (new_target == scroll_target_) return false;
    // Move only the target — paint() eases scroll_pos_ toward it.
    scroll_target_ = new_target;
    return true;
}

} // namespace cute::ui
