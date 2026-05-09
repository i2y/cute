#pragma once

#include <QtCanvasPainter/QCanvasPainter>
#include <QtCanvasPainter/QCanvasBoxShadow>
#include <QtCanvasPainter/QCanvasImage>
#include <QColor>
#include <QFont>
#include <QPointF>
#include <QRectF>
#include <QSizeF>
#include <QString>
#include <QTransform>

#include <vector>

namespace cute::ui {

/// Light/dark visual theme. Selected once per Window and threaded through
/// `PaintCtx::style()`; flip via Window::setTheme to switch live.
enum class Theme { Dark, Light };

/// Semantic color tokens — every Element pulls from this instead of
/// hardcoding QColor literals so the Window's theme controls the look.
/// Add fields here only when an Element actually needs a new token.
struct Style {
    QColor windowBg;
    QColor surface;        // TextField / ListView panel
    QColor border;
    QColor borderFocused;
    QColor text;           // primary foreground
    QColor textDim;        // placeholder / secondary
    QColor accent;         // button rest
    QColor accentHover;
    QColor accentPressed;
    QColor onAccent;       // text on accent
    QColor selection;      // text selection highlight
    QColor scrollbar;
    QColor shadow;

    static Style dark();
    static Style light();
    static Style forTheme(Theme t) {
        return t == Theme::Light ? light() : dark();
    }

    /// Per-field lerp between two Styles. Used by Window's theme-crossfade
    /// during the Cmd+T transition.
    static Style blend(const Style& a, const Style& b, float t);
};

/// Linear interpolation between two QColors. `t` is clamped to [0, 1].
inline QColor lerpColor(QColor a, QColor b, float t)
{
    if (t <= 0.f) return a;
    if (t >= 1.f) return b;
    return QColor(
        int(a.red()   + (b.red()   - a.red())   * t),
        int(a.green() + (b.green() - a.green()) * t),
        int(a.blue()  + (b.blue()  - a.blue())  * t),
        int(a.alpha() + (b.alpha() - a.alpha()) * t));
}

class PaintCtx {
public:
    PaintCtx(QCanvasPainter& painter, QSizeF window_size, const Style& style,
             qint64 elapsed_ms)
        : p_(painter), window_size_(window_size), style_(&style),
          elapsed_ms_(elapsed_ms) {}

    const Style& style() const noexcept { return *style_; }

    /// Monotonic clock starting at Window construction. Use modulo for
    /// periodic effects (caret blink) or as a baseline for tween elapsed.
    qint64 elapsedMs() const noexcept { return elapsed_ms_; }

    /// Tells Window the painter wants another frame on the next vsync.
    /// Called from paint() during animations; Window schedules a
    /// QTimer-driven requestUpdate so the loop only runs while at least
    /// one element is animating.
    void requestAnimationFrame() const noexcept { needs_more_frames_ = true; }
    bool needsMoreFrames() const noexcept { return needs_more_frames_; }

    QCanvasPainter& raw() noexcept { return p_; }

    void setFill(QColor c)                 { p_.setFillStyle(c); }
    void setStroke(QColor c, float w = 1.f) { p_.setStrokeStyle(c); p_.setLineWidth(w); }

    void fillRect(QRectF r)                { p_.fillRect(r); }
    void strokeRect(QRectF r)              { p_.strokeRect(r); }
    void fillRoundRect(QRectF r, float radius) {
        p_.beginPath();
        p_.roundRect(r, radius);
        p_.fill();
    }
    void strokeRoundRect(QRectF r, float radius) {
        p_.beginPath();
        p_.roundRect(r, radius);
        p_.stroke();
    }

    void strokeLine(QPointF a, QPointF b) {
        p_.beginPath();
        p_.moveTo(a);
        p_.lineTo(b);
        p_.stroke();
    }

    void fillCircle(QPointF center, float radius) {
        p_.beginPath();
        p_.circle(center, radius);
        p_.fill();
    }
    void strokeCircle(QPointF center, float radius) {
        p_.beginPath();
        p_.circle(center, radius);
        p_.stroke();
    }

    /// Draws a shadow behind a (rounded) filled rect.
    void fillRectShadow(QRectF rect, QColor fillColor, float radius,
                        QPointF shadowOffset, float blur, QColor shadowColor) {
        QCanvasBoxShadow shadow(rect.translated(shadowOffset), radius, blur, shadowColor);
        p_.drawBoxShadow(shadow);
        p_.setFillStyle(fillColor);
        if (radius > 0.f) {
            p_.beginPath();
            p_.roundRect(rect, radius);
            p_.fill();
        } else {
            p_.fillRect(rect);
        }
    }

    void setFont(const QFont& f)           { p_.setFont(f); }
    void setTextAlign(QCanvasPainter::TextAlign a)        { p_.setTextAlign(a); }
    void setTextBaseline(QCanvasPainter::TextBaseline b)  { p_.setTextBaseline(b); }
    void fillText(const QString& s, QPointF pos) { p_.fillText(s, pos); }
    void fillTextInRect(const QString& s, QRectF rect)    { p_.fillText(s, rect); }
    QRectF textBoundingBox(const QString& s, QPointF pos = QPointF()) {
        return p_.textBoundingBox(s, pos);
    }
    QSizeF measureText(const QString& s, const QFont& f) {
        p_.save();
        p_.setFont(f);
        QRectF bb = p_.textBoundingBox(s, QPointF());
        p_.restore();
        return bb.size();
    }

    void drawImage(const QCanvasImage& image, QRectF dst) { p_.drawImage(image, dst); }
    QCanvasImage addImage(const QImage& image)            { return p_.addImage(image); }

    void save()                            { p_.save(); }
    void restore()                         { p_.restore(); }

    void translate(QPointF t)              { p_.translate(t); }
    void scale(float sx, float sy)         { p_.scale(sx, sy); }
    void rotate(float deg)                 { p_.rotate(deg); }

    /// Canvas Painter only supports rectangular clipping (no path clip).
    /// Qt 6.11 Tech Preview's QCanvasPainter does NOT preserve clip state
    /// across save/restore, and resetClipping() doesn't reliably clear an
    /// active clip mid-frame either. Workaround: track an explicit stack
    /// and on the outermost popClip re-issue a full-window setClipRect to
    /// restore the unclipped state. Without this, a clipping element (e.g.
    /// TextField) leaks its clip rect and prevents subsequent siblings
    /// from rendering.
    void pushClipRect(QRectF r) {
        clip_stack_.push_back(r);
        p_.setClipRect(r);
    }
    void popClip() {
        clip_stack_.pop_back();
        if (clip_stack_.empty()) {
            p_.setClipRect(QRectF(QPointF(0, 0), window_size_));
        } else {
            p_.setClipRect(clip_stack_.back());
        }
    }

private:
    QCanvasPainter& p_;
    QSizeF window_size_;
    const Style* style_;
    qint64 elapsed_ms_ = 0;
    mutable bool needs_more_frames_ = false;
    std::vector<QRectF> clip_stack_;
};

} // namespace cute::ui
