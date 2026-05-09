//! QSS (Qt Style Sheet) shorthand for the `widget` (QtWidgets) path.
//!
//! Most "visual" Qt styling lives in QSS, not in QWidget setters —
//! `QPushButton` has no `setBackground` / `setBorderRadius` /
//! `setFontWeight`, only `setStyleSheet(QString)`. Without this
//! module, every visual property has to be hand-rolled into a single
//! `styleSheet: "..."` string literal, which loses the style-bag
//! ergonomics Cute offers for QML (`font.pixelSize: 28`) and forces
//! users to mix Cute and CSS-flavored syntax in the same file.
//!
//! With this module, the widget path recognizes a fixed shorthand
//! vocabulary (camelCase keys + dotted pseudo-class prefixes) and
//! aggregates them at codegen time into one `setStyleSheet(...)` call:
//!
//! ```cute,ignore
//! style NumBtn {
//!   minimumWidth: 64           # genuine QWidget setter -> setMinimumWidth(64)
//!   background: "#333"         # QSS-only -> aggregated
//!   color: "#fff"              # QSS-only -> aggregated
//!   borderRadius: 32           # QSS-only, length -> "32px"
//!   hover.background: "#3d3d3d"   # pseudo-class -> :hover bucket
//!   pressed.background: "#555"    # pseudo-class -> :pressed bucket
//! }
//! ```
//!
//! Lowers (synthesised QSS, single setter call):
//!
//! ```cpp,ignore
//! _w->setMinimumWidth(64);
//! _w->setStyleSheet(QStringLiteral(
//!     "QPushButton { background: #333; color: #fff; border-radius: 32px; } "
//!     "QPushButton:hover { background: #3d3d3d; } "
//!     "QPushButton:pressed { background: #555; }"));
//! ```
//!
//! If the user also writes a literal `styleSheet: "..."` on the same
//! element, the synthesized rules are emitted first and the user's
//! string is concatenated after (so the user's QSS rules win on
//! later-rule-wins QSS specificity ties).
//!
//! Pseudo-class prefixes recognised: `hover`, `pressed`, `focus`,
//! `disabled`, `checked`. Anything else with a dot in the key falls
//! through to the regular setter path.

use cute_syntax::ast::{Expr, ExprKind, StrPart};
use cute_types::qss::{QssValueShape, shape_for};

/// QSS pseudo-class for ordering and selector composition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum QssPseudo {
    Base,
    Hover,
    Pressed,
    Focus,
    Disabled,
    Checked,
}

impl QssPseudo {
    fn suffix(self) -> &'static str {
        match self {
            QssPseudo::Base => "",
            QssPseudo::Hover => ":hover",
            QssPseudo::Pressed => ":pressed",
            QssPseudo::Focus => ":focus",
            QssPseudo::Disabled => ":disabled",
            QssPseudo::Checked => ":checked",
        }
    }

    fn from_prefix(s: &str) -> Option<Self> {
        match s {
            "hover" => Some(QssPseudo::Hover),
            "pressed" => Some(QssPseudo::Pressed),
            "focus" => Some(QssPseudo::Focus),
            "disabled" => Some(QssPseudo::Disabled),
            "checked" => Some(QssPseudo::Checked),
            _ => None,
        }
    }
}

/// CamelCase Cute key -> kebab-case QSS property. Returns `None`
/// when the key isn't part of the shorthand vocabulary; the caller
/// then routes the property through the normal `setX(...)` path.
fn kebab_for(camel: &str) -> Option<&'static str> {
    Some(match camel {
        "color" => "color",
        "background" => "background",
        "backgroundColor" => "background-color",
        "border" => "border",
        "borderRadius" => "border-radius",
        "borderColor" => "border-color",
        "borderWidth" => "border-width",
        "borderStyle" => "border-style",
        "fontSize" => "font-size",
        "fontWeight" => "font-weight",
        "fontFamily" => "font-family",
        "fontStyle" => "font-style",
        "padding" => "padding",
        "paddingLeft" => "padding-left",
        "paddingRight" => "padding-right",
        "paddingTop" => "padding-top",
        "paddingBottom" => "padding-bottom",
        "margin" => "margin",
        "marginLeft" => "margin-left",
        "marginRight" => "margin-right",
        "marginTop" => "margin-top",
        "marginBottom" => "margin-bottom",
        "textAlign" => "qproperty-alignment",
        _ => return None,
    })
}

/// Length-typed keys auto-suffix `px` when the user passes an int /
/// float literal (`borderRadius: 32` → `border-radius: 32px;`). For
/// non-length keys (`fontWeight: 500`) the number is emitted as-is.
/// Vocabulary lives in `cute_types::qss::shape_for` so the type
/// checker and codegen share a single source of truth.
fn is_length_key(camel: &str) -> bool {
    matches!(shape_for(camel), Some(QssValueShape::Length))
}

/// Map a `textAlign:` string value to the Qt::Alignment enum that
/// `qproperty-alignment` expects. Unknown strings pass through so
/// custom enum names like `"AlignCenter"` keep working.
fn align_value(s: &str) -> String {
    match s {
        "left" | "Left" => "AlignLeft".into(),
        "right" | "Right" => "AlignRight".into(),
        "center" | "Center" => "AlignCenter".into(),
        "justify" | "Justify" => "AlignJustify".into(),
        other => other.into(),
    }
}

/// Try to format a literal expression as a QSS value. Returns `None`
/// for non-literal expressions (e.g. interpolated strings, idents) —
/// those force the property back onto the regular setter path so we
/// don't silently drop reactive data.
fn format_value(camel_key: &str, e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Str(parts) => {
            if parts.len() != 1 {
                return None;
            }
            let StrPart::Text(t) = &parts[0] else {
                return None;
            };
            if camel_key == "textAlign" {
                Some(align_value(t))
            } else {
                Some(t.clone())
            }
        }
        ExprKind::Int(n) => {
            if is_length_key(camel_key) {
                Some(format!("{n}px"))
            } else {
                Some(n.to_string())
            }
        }
        ExprKind::Float(v) => {
            let stem = if v.fract() == 0.0 && v.is_finite() {
                format!("{}", *v as i64)
            } else {
                v.to_string()
            };
            if is_length_key(camel_key) {
                Some(format!("{stem}px"))
            } else {
                Some(stem)
            }
        }
        _ => None,
    }
}

/// Classification of one element-property entry against the shorthand.
pub enum QssClass {
    /// Recognised shorthand. `(pseudo, kebab_property, formatted_value)`
    Shorthand(QssPseudo, &'static str, String),
    /// Not part of the shorthand — caller emits via the regular
    /// `setX(...)` path.
    Passthrough,
}

/// Decide whether `(key, value)` belongs in the synthesized QSS
/// stylesheet. Dotted pseudo-class prefix is honored
/// (`hover.background` → `(Hover, "background", ...)`).
pub fn classify(key: &str, value: &Expr) -> QssClass {
    let (pseudo, base_key) = match key.split_once('.') {
        Some((prefix, rest)) => match QssPseudo::from_prefix(prefix) {
            Some(p) => (p, rest),
            None => return QssClass::Passthrough,
        },
        None => (QssPseudo::Base, key),
    };
    let Some(kebab) = kebab_for(base_key) else {
        return QssClass::Passthrough;
    };
    let Some(formatted) = format_value(base_key, value) else {
        return QssClass::Passthrough;
    };
    QssClass::Shorthand(pseudo, kebab, formatted)
}

/// Accumulator for shorthand entries collected while walking an
/// element's members.
#[derive(Default)]
pub struct QssBag {
    base: Vec<(&'static str, String)>,
    hover: Vec<(&'static str, String)>,
    pressed: Vec<(&'static str, String)>,
    focus: Vec<(&'static str, String)>,
    disabled: Vec<(&'static str, String)>,
    checked: Vec<(&'static str, String)>,
}

impl QssBag {
    pub fn push(&mut self, pseudo: QssPseudo, key: &'static str, value: String) {
        let bucket = match pseudo {
            QssPseudo::Base => &mut self.base,
            QssPseudo::Hover => &mut self.hover,
            QssPseudo::Pressed => &mut self.pressed,
            QssPseudo::Focus => &mut self.focus,
            QssPseudo::Disabled => &mut self.disabled,
            QssPseudo::Checked => &mut self.checked,
        };
        // Last write wins per (pseudo, key) — a later style block in
        // a `+` chain should override an earlier one's same key.
        bucket.retain(|(k, _)| *k != key);
        bucket.push((key, value));
    }

    pub fn is_empty(&self) -> bool {
        self.base.is_empty()
            && self.hover.is_empty()
            && self.pressed.is_empty()
            && self.focus.is_empty()
            && self.disabled.is_empty()
            && self.checked.is_empty()
    }

    /// Render to a single QSS string, scoped via `selector` (the
    /// element's class name, e.g. `QPushButton`). Pseudo buckets
    /// emit in a fixed order so output is deterministic.
    pub fn render(&self, selector: &str) -> String {
        let mut out = String::new();
        for (pseudo, bucket) in [
            (QssPseudo::Base, &self.base),
            (QssPseudo::Hover, &self.hover),
            (QssPseudo::Pressed, &self.pressed),
            (QssPseudo::Focus, &self.focus),
            (QssPseudo::Disabled, &self.disabled),
            (QssPseudo::Checked, &self.checked),
        ] {
            if bucket.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(selector);
            out.push_str(pseudo.suffix());
            out.push_str(" { ");
            for (k, v) in bucket {
                out.push_str(k);
                out.push_str(": ");
                out.push_str(v);
                out.push_str("; ");
            }
            out.push('}');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cute_syntax::span::Span;

    fn lit_str(s: &str) -> Expr {
        Expr {
            kind: ExprKind::Str(vec![StrPart::Text(s.into())]),
            span: Span::dummy(),
        }
    }
    fn lit_int(n: i64) -> Expr {
        Expr {
            kind: ExprKind::Int(n),
            span: Span::dummy(),
        }
    }

    #[test]
    fn shorthand_color_passes_through_string_value() {
        let cls = classify("color", &lit_str("#fff"));
        let QssClass::Shorthand(pseudo, kebab, val) = cls else {
            panic!("expected shorthand");
        };
        assert_eq!(pseudo, QssPseudo::Base);
        assert_eq!(kebab, "color");
        assert_eq!(val, "#fff");
    }

    #[test]
    fn length_key_auto_appends_px() {
        let cls = classify("borderRadius", &lit_int(32));
        let QssClass::Shorthand(_, _, val) = cls else {
            panic!("expected shorthand");
        };
        assert_eq!(val, "32px");
    }

    #[test]
    fn font_weight_int_no_px_suffix() {
        let cls = classify("fontWeight", &lit_int(500));
        let QssClass::Shorthand(_, _, val) = cls else {
            panic!("expected shorthand");
        };
        assert_eq!(val, "500");
    }

    #[test]
    fn hover_prefix_routes_to_hover_bucket() {
        let cls = classify("hover.background", &lit_str("#3d3d3d"));
        let QssClass::Shorthand(pseudo, kebab, _) = cls else {
            panic!("expected shorthand");
        };
        assert_eq!(pseudo, QssPseudo::Hover);
        assert_eq!(kebab, "background");
    }

    #[test]
    fn unknown_key_is_passthrough() {
        assert!(matches!(
            classify("notARealQssKey", &lit_str("v")),
            QssClass::Passthrough
        ));
    }

    #[test]
    fn dotted_non_pseudo_is_passthrough() {
        // `font.pixelSize` is a QML idiom; widget path leaves it alone
        // so codegen emits the (broken) `setFont.pixelSize(...)` line
        // the user's existing code expects, rather than silently
        // hijacking it.
        assert!(matches!(
            classify("font.pixelSize", &lit_int(28)),
            QssClass::Passthrough
        ));
    }

    #[test]
    fn text_align_maps_to_qproperty_alignment() {
        let cls = classify("textAlign", &lit_str("right"));
        let QssClass::Shorthand(_, kebab, val) = cls else {
            panic!("expected shorthand");
        };
        assert_eq!(kebab, "qproperty-alignment");
        assert_eq!(val, "AlignRight");
    }

    #[test]
    fn render_groups_by_pseudo() {
        let mut bag = QssBag::default();
        bag.push(QssPseudo::Base, "background", "#333".into());
        bag.push(QssPseudo::Base, "color", "#fff".into());
        bag.push(QssPseudo::Hover, "background", "#3d3d3d".into());
        bag.push(QssPseudo::Pressed, "background", "#555".into());
        let qss = bag.render("QPushButton");
        assert!(qss.contains("QPushButton { background: #333; color: #fff; }"));
        assert!(qss.contains("QPushButton:hover { background: #3d3d3d; }"));
        assert!(qss.contains("QPushButton:pressed { background: #555; }"));
    }

    #[test]
    fn last_write_wins_per_key() {
        // A `+` merge with `Card + Big` may emit `padding` twice with
        // the right side intended to win; the bag preserves only the
        // latest value to match `merge_entries` semantics.
        let mut bag = QssBag::default();
        bag.push(QssPseudo::Base, "padding", "4px".into());
        bag.push(QssPseudo::Base, "padding", "32px".into());
        let qss = bag.render("QLabel");
        assert!(qss.contains("padding: 32px"));
        assert!(!qss.contains("padding: 4px"));
    }
}
