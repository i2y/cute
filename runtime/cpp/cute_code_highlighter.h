// cute::CodeHighlighter — a concrete QSyntaxHighlighter that takes a
// list of (regex, format) rules at runtime instead of requiring a
// per-language subclass. Sidesteps the Cute language gap that
// virtual-override emission for arbitrary `class X < QSyntaxHighlighter`
// isn't wired yet — users instantiate this concrete class and feed
// rules via `addRule(...)`, no subclassing required.
//
// Mirrors the canonical pattern from the Qt docs' "Syntax Highlighter
// example", just generalized so the rule set is data not code.
//
// No Q_OBJECT macro: the class doesn't declare its own signals /
// slots / Q_INVOKABLE methods, so it doesn't need its own
// metaobject. The base `QSyntaxHighlighter` keeps its Q_OBJECT,
// which means `QObject::connect` / parent ownership / `deleteLater`
// all work through the inherited `staticMetaObject`. Skipping
// Q_OBJECT here also means we don't need AUTOMOC turned on (Cute
// builds keep it off and emit metaobject tables for Cute-defined
// classes themselves).
//
// Bundled with the cute runtime (`runtime/cpp/`) and stamped into
// every cute build cache that pulls it via `#include "cute_code_highlighter.h"`.

#pragma once

#include <QColor>
#include <QFont>
#include <QPlainTextEdit>
#include <QRegularExpression>
#include <QString>
#include <QSyntaxHighlighter>
#include <QTextCharFormat>
#include <QTextDocument>
#include <QTextEdit>
#include <QVariant>
#include <QVector>

// Qt6::Quick is only on the link line for QML / CuteUi builds;
// pure QtWidgets consumers don't pull it. Conditional include
// keeps cute_code_highlighter.h compiling on widget-only paths
// while still wiring `attachToQmlTextItem` when QtQuick is reachable.
#if __has_include(<QQuickTextDocument>)
#include <QQuickTextDocument>
#define CUTE_HIGHLIGHTER_HAS_QUICK 1
#endif

namespace cute {

class CodeHighlighter : public QSyntaxHighlighter {
public:
    explicit CodeHighlighter(QTextDocument* parent = nullptr)
        : QSyntaxHighlighter(parent) {}

    // Convenience constructors that take an editor widget directly
    // — the rule-set lives on the highlighter, but the highlight
    // events fire against the editor's internal QTextDocument.
    // Lets Cute users skip the `.document()` step that
    // qtextedit.qpi doesn't currently expose. Pulling these in
    // means CodeHighlighter consumers link Qt6::Widgets even on
    // CLI / QML builds — the cute-driver `detect_build_extras`
    // pass adds Widgets automatically when this class is referenced.
    explicit CodeHighlighter(QPlainTextEdit* editor)
        : QSyntaxHighlighter(editor != nullptr ? editor->document() : nullptr) {}

    explicit CodeHighlighter(QTextEdit* editor)
        : QSyntaxHighlighter(editor != nullptr ? editor->document() : nullptr) {}

#ifdef CUTE_HIGHLIGHTER_HAS_QUICK
    // QML hook: build a CodeHighlighter for any QML TextEdit /
    // TextArea passed in via `Component.onCompleted: chat.attach(this)`.
    // Pulls the `textDocument` Q_PROPERTY (QQuickTextDocument*),
    // unwraps to QTextDocument, hands it to the QSyntaxHighlighter
    // ctor, and pre-loads the fenced-code preset so chat-style
    // assistant bubbles light up immediately when their markdown
    // body lands.
    //
    // Returns the new highlighter parented to `qmlTextItem`, so it
    // dies with the bubble. Returns nullptr if the receiver doesn't
    // expose a usable textDocument (Label / unrelated QML object).
    //
    // Compiled out for QtWidgets-only builds where QQuickTextDocument
    // isn't reachable — the `__has_include` guard at the top of this
    // header sets `CUTE_HIGHLIGHTER_HAS_QUICK` only when QtQuick is
    // on the link surface.
    static CodeHighlighter* attachToQmlTextItem(QObject* qmlTextItem) {
        if (qmlTextItem == nullptr) {
            return nullptr;
        }
        QVariant qtdVar = qmlTextItem->property("textDocument");
        auto* qtd = qtdVar.value<QQuickTextDocument*>();
        if (qtd == nullptr) {
            return nullptr;
        }
        QTextDocument* doc = qtd->textDocument();
        if (doc == nullptr) {
            return nullptr;
        }
        auto* hl = new CodeHighlighter(doc);
        hl->setParent(qmlTextItem);
        hl->useFencedPreset();
        return hl;
    }
#endif

    // Append a coloring rule. `pattern` is a Qt regular expression
    // string; matches across the active text block get painted with
    // `color` (and bolded when `bold` is true). Rules apply in
    // append order so users can stack a broad rule (identifiers)
    // and then a narrower one (keywords) to refine the result.
    void addRule(const QString& pattern, const QColor& color, bool bold = false) {
        Rule r;
        r.pattern = QRegularExpression(pattern);
        r.format.setForeground(color);
        if (bold) {
            r.format.setFontWeight(QFont::DemiBold);
        }
        rules_.append(r);
        rehighlight();
    }

    // Drop every rule and refresh — useful when swapping languages
    // on the same editor (e.g. Python → Cute).
    void clearRules() {
        rules_.clear();
        rehighlight();
    }

    // Convenience: load the standard "fenced code block" colour
    // profile (keywords, strings, line comments, numbers). Picks
    // a single neutral palette that reads against both light and
    // dark backgrounds — the highlighted markdown bubbles in the
    // LLM chat showcase use this preset.
    void useFencedPreset() {
        clearRules();
        // Strings — match first so quoted keywords stay green.
        addRule(QStringLiteral("\"[^\"]*\""), QColor("#16a34a"), false);
        addRule(QStringLiteral("'[^']*'"), QColor("#16a34a"), false);
        // Numbers
        addRule(QStringLiteral("\\b[0-9]+(\\.[0-9]+)?\\b"), QColor("#9333ea"), false);
        // Common keywords across Python / JS / Rust / Cute / C-likes.
        // The over-broad pattern is intentional — chat code blocks
        // mix languages, so a generic keyword bucket is friendlier
        // than per-language palettes the user has to pick.
        addRule(
            QStringLiteral(
                "\\b(fn|let|var|if|else|for|while|return|class|struct|"
                "import|from|as|def|in|true|false|null|nil|None|True|False|"
                "and|or|not|new|pub|use|signal|prop|view|widget|mut|"
                "match|case|when|do|end|public|private|extern|enum|trait|"
                "impl|self|this|throw|try|catch|async|await)\\b"),
            QColor("#0a84ff"),
            true);
        // Single-line comments — `#`, `//`, `--`. Match-anchored to
        // line start when possible; otherwise mid-line `#` (rare in
        // most languages but common in Python / shell / Cute).
        addRule(QStringLiteral("#[^\\n]*"), QColor("#737373"), false);
        addRule(QStringLiteral("//[^\\n]*"), QColor("#737373"), false);
    }

protected:
    void highlightBlock(const QString& text) override {
        for (const auto& rule : rules_) {
            QRegularExpressionMatchIterator it = rule.pattern.globalMatch(text);
            while (it.hasNext()) {
                const auto m = it.next();
                setFormat(static_cast<int>(m.capturedStart()),
                          static_cast<int>(m.capturedLength()),
                          rule.format);
            }
        }
    }

private:
    struct Rule {
        QRegularExpression pattern;
        QTextCharFormat format;
    };
    QVector<Rule> rules_;
};

}  // namespace cute

// Bring the class into the global namespace so Cute's binding can
// refer to it as `CodeHighlighter` (the .qpi declares `class
// CodeHighlighter < QObject`). Cute lowers `CodeHighlighter.new(doc)`
// to `new CodeHighlighter(doc)`; without the alias that would name-
// resolve to nothing, since the C++ class lives under `cute::`.
using CodeHighlighter = cute::CodeHighlighter;

