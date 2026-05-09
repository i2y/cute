#include "cute/ui/paint.hpp"

namespace cute::ui {

Style Style::dark()
{
    Style s;
    s.windowBg       = QColor(20, 22, 30);
    s.surface        = QColor(30, 33, 42);
    s.border         = QColor(80, 90, 110);
    s.borderFocused  = QColor(120, 170, 240);
    s.text           = QColor(230, 230, 235);
    s.textDim        = QColor(140, 140, 150);
    s.accent         = QColor(80, 130, 200);
    s.accentHover    = QColor(90, 150, 220);
    s.accentPressed  = QColor(60, 100, 160);
    s.onAccent       = QColor(255, 255, 255);
    s.selection      = QColor(70, 110, 180, 160);
    s.scrollbar      = QColor(120, 130, 150, 180);
    s.shadow         = QColor(0, 0, 0, 80);
    return s;
}

Style Style::blend(const Style& a, const Style& b, float t)
{
    Style out;
    out.windowBg      = lerpColor(a.windowBg,      b.windowBg,      t);
    out.surface       = lerpColor(a.surface,       b.surface,       t);
    out.border        = lerpColor(a.border,        b.border,        t);
    out.borderFocused = lerpColor(a.borderFocused, b.borderFocused, t);
    out.text          = lerpColor(a.text,          b.text,          t);
    out.textDim       = lerpColor(a.textDim,       b.textDim,       t);
    out.accent        = lerpColor(a.accent,        b.accent,        t);
    out.accentHover   = lerpColor(a.accentHover,   b.accentHover,   t);
    out.accentPressed = lerpColor(a.accentPressed, b.accentPressed, t);
    out.onAccent      = lerpColor(a.onAccent,      b.onAccent,      t);
    out.selection     = lerpColor(a.selection,     b.selection,     t);
    out.scrollbar     = lerpColor(a.scrollbar,     b.scrollbar,     t);
    out.shadow        = lerpColor(a.shadow,        b.shadow,        t);
    return out;
}

Style Style::light()
{
    Style s;
    s.windowBg       = QColor(245, 246, 248);
    s.surface        = QColor(255, 255, 255);
    s.border         = QColor(200, 205, 215);
    s.borderFocused  = QColor(70, 130, 220);
    s.text           = QColor(30, 32, 40);
    s.textDim        = QColor(130, 135, 145);
    s.accent         = QColor(80, 130, 200);
    s.accentHover    = QColor(70, 120, 190);
    s.accentPressed  = QColor(55, 95, 160);
    s.onAccent       = QColor(255, 255, 255);
    s.selection      = QColor(70, 130, 220, 90);
    s.scrollbar      = QColor(160, 165, 175, 200);
    s.shadow         = QColor(0, 0, 0, 50);
    return s;
}

} // namespace cute::ui
