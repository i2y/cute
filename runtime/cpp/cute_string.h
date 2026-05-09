// Cute runtime: String aliases the Cute primitive `String` to QString.
//
// Spec: `property text : String` lowers to `QString m_text;` etc. This header
// is the canonical "what does Cute String mean to C++" answer.
//
// String interpolation `"hello #{name}"` is lowered by cute-lower into
// concatenation against `cute::str::to_string(...)` for non-QString
// arguments; the helpers live here.

#pragma once

#include <QString>
#include <QStringList>

#include <QVariant>

namespace cute {

using String = QString;

namespace str {

inline QString to_string(const QString& v) { return v; }
inline QString to_string(qlonglong v) { return QString::number(v); }
inline QString to_string(qulonglong v) { return QString::number(v); }
inline QString to_string(int v) { return QString::number(v); }
inline QString to_string(double v) { return QString::number(v, 'g', 17); }
inline QString to_string(bool v) { return v ? QStringLiteral("true") : QStringLiteral("false"); }
// QVariant-of-anything prints via its inner value's `toString()` rather
// than the default `QVariant(QString, "...")` debug wrapper. Untyped
// `List` / `Map` collections (which lower to QVariantList / QVariantMap)
// iterate as QVariant elements - this overload is what makes
// `for x in items { println(x) }` print "bread" instead of
// `QVariant(QString, "bread")`.
inline QString to_string(const QVariant& v) { return v.toString(); }

// ---- format-spec interpolation helpers -----------------------------
//
// `"#{value:fmt}"` lowers to `::cute::str::format(value, "fmt")`. The
// spec grammar is a curated subset of Python's PEP 3101:
//
//   spec    := [[fill]align]? ["0"]? [width]? ["." precision]? [type]?
//   align   := "<" | ">" | "^"
//   type    := "d" | "x" | "X" | "o" | "b" | "f" | "e" | "g" | "%" | "s"
//
// Examples:
//   "#{n:08d}"   -> zero-pad to width 8 ("00000042")
//   "#{x:.2f}"   -> fixed precision 2     ("3.14")
//   "#{name:>20}" -> right-align width 20
//   "#{r:.0%}"   -> percent ("85%")
//
// The spec is parsed at runtime (rather than at codegen time) so that
// every overload sees the same parser and the resulting code is just
// one function call per interp.
struct FormatSpec {
    QChar fill = QChar(' ');
    QChar align = QChar(0);   // 0 = no explicit align
    int width = 0;
    int precision = -1;       // -1 = no precision
    QChar type = QChar(0);    // 0 = no explicit type
    bool zero = false;
};

inline FormatSpec parse_spec(const char* spec_str) {
    FormatSpec r;
    if (spec_str == nullptr) return r;
    QString text = QString::fromUtf8(spec_str);
    int i = 0;
    int n = text.length();

    // [[fill]align]: align is one of "<", ">", "^". If a non-special
    // character appears at position 0 followed by an align char at 1,
    // that first char is the fill.
    if (n >= 2 && (text[1] == '<' || text[1] == '>' || text[1] == '^')) {
        r.fill = text[0];
        r.align = text[1];
        i = 2;
    } else if (n >= 1 && (text[0] == '<' || text[0] == '>' || text[0] == '^')) {
        r.align = text[0];
        i = 1;
    }

    // ["0"]: zero-pad flag. Implies fill='0' and right-alignment if
    // align wasn't otherwise specified.
    if (i < n && text[i] == '0') {
        r.zero = true;
        if (r.align == QChar(0)) {
            r.align = QChar('>');
            r.fill = QChar('0');
        }
        i++;
    }

    // [width]: decimal integer.
    while (i < n && text[i].isDigit()) {
        r.width = r.width * 10 + text[i].digitValue();
        i++;
    }

    // ["." precision]: decimal integer after a literal period.
    if (i < n && text[i] == '.') {
        i++;
        r.precision = 0;
        while (i < n && text[i].isDigit()) {
            r.precision = r.precision * 10 + text[i].digitValue();
            i++;
        }
    }

    // [type]: single letter (d/x/X/o/b/f/e/g/%/s). Anything else is
    // ignored — the spec is best-effort.
    if (i < n) {
        r.type = text[i];
    }
    return r;
}

inline QString apply_align(const QString& text, const FormatSpec& spec) {
    if (spec.width <= text.length()) return text;
    int pad = spec.width - text.length();
    QChar align = spec.align == QChar(0) ? QChar('>') : spec.align;
    if (align == '<') {
        return text + QString(pad, spec.fill);
    } else if (align == '^') {
        int left = pad / 2;
        int right = pad - left;
        return QString(left, spec.fill) + text + QString(right, spec.fill);
    } else {  // '>' or default
        return QString(pad, spec.fill) + text;
    }
}

inline QString apply_zero_pad(const QString& text, const FormatSpec& spec) {
    if (spec.width <= text.length()) return text;
    int pad = spec.width - text.length();
    // Negative-sign aware zero-padding: "-42" -> "-0042" not "00-42".
    if (text.startsWith('-') || text.startsWith('+')) {
        return text.left(1) + QString(pad, '0') + text.mid(1);
    }
    return QString(pad, '0') + text;
}

inline QString format_int_spec(qlonglong v, const FormatSpec& spec) {
    QString text;
    QChar t = spec.type;
    if (t == 'x') {
        text = (v < 0 ? "-" : "") + QString::number(std::abs(v), 16);
    } else if (t == 'X') {
        text = (v < 0 ? "-" : "") + QString::number(std::abs(v), 16).toUpper();
    } else if (t == 'b') {
        text = (v < 0 ? "-" : "") + QString::number(std::abs(v), 2);
    } else if (t == 'o') {
        text = (v < 0 ? "-" : "") + QString::number(std::abs(v), 8);
    } else {
        text = QString::number(v);
    }
    if (spec.zero) {
        return apply_zero_pad(text, spec);
    }
    return apply_align(text, spec);
}

inline QString format_float_spec(double v, const FormatSpec& spec) {
    QString text;
    QChar t = spec.type;
    int prec = spec.precision < 0 ? 6 : spec.precision;
    if (t == 'f') {
        text = QString::number(v, 'f', prec);
    } else if (t == 'e') {
        text = QString::number(v, 'e', prec);
    } else if (t == 'g') {
        text = QString::number(v, 'g', prec);
    } else if (t == '%') {
        text = QString::number(v * 100.0, 'f', prec) + QStringLiteral("%");
    } else if (spec.precision >= 0) {
        // No type but precision given → fixed-point.
        text = QString::number(v, 'f', prec);
    } else {
        text = QString::number(v, 'g', 17);
    }
    if (spec.zero) {
        return apply_zero_pad(text, spec);
    }
    return apply_align(text, spec);
}

inline QString format_string_spec(const QString& s, const FormatSpec& spec) {
    QString text = s;
    if (spec.precision >= 0 && spec.precision < text.length()) {
        text = text.left(spec.precision);
    }
    return apply_align(text, spec);
}

inline QString format(qlonglong v, const char* spec_str) {
    FormatSpec spec = parse_spec(spec_str);
    if (spec.type == 'f' || spec.type == 'e' || spec.type == 'g' || spec.type == '%') {
        return format_float_spec(static_cast<double>(v), spec);
    }
    return format_int_spec(v, spec);
}
inline QString format(int v, const char* spec_str) {
    return format(static_cast<qlonglong>(v), spec_str);
}
inline QString format(qulonglong v, const char* spec_str) {
    return format(static_cast<qlonglong>(v), spec_str);
}
inline QString format(double v, const char* spec_str) {
    FormatSpec spec = parse_spec(spec_str);
    return format_float_spec(v, spec);
}
inline QString format(bool v, const char* spec_str) {
    FormatSpec spec = parse_spec(spec_str);
    return format_string_spec(
        v ? QStringLiteral("true") : QStringLiteral("false"), spec);
}
inline QString format(const QString& s, const char* spec_str) {
    FormatSpec spec = parse_spec(spec_str);
    return format_string_spec(s, spec);
}
inline QString format(const QVariant& v, const char* spec_str) {
    // Variant: dispatch by what the inner type can convert to. Numbers
    // win over strings so `:.2f` works for an Int wrapped in QVariant.
    if (v.canConvert<double>() && v.metaType().id() != QMetaType::QString) {
        bool ok = false;
        double d = v.toDouble(&ok);
        if (ok) {
            FormatSpec spec = parse_spec(spec_str);
            // If spec wants integer formatting, route through the int path.
            if (spec.type == QChar(0) || spec.type == 'd' || spec.type == 'x'
                || spec.type == 'X' || spec.type == 'o' || spec.type == 'b') {
                if (d == static_cast<double>(static_cast<qlonglong>(d))) {
                    return format_int_spec(static_cast<qlonglong>(d), spec);
                }
            }
            return format_float_spec(d, spec);
        }
    }
    return format(v.toString(), spec_str);
}

}  // namespace str

}  // namespace cute
