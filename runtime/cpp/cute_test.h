// Cute test framework runtime — milestone 1.
//
// Intentionally Qt-light: only what `cute::str::to_string` needs to
// stringify the failure message.

#pragma once

#include <algorithm>
#include <cstdio>
#include <exception>
#include <string>
#include <type_traits>

#include <QString>
#include <QStringList>

#include "cute_string.h"

namespace cute::test {

class AssertionFailure : public std::exception {
public:
    explicit AssertionFailure(std::string msg) : m_msg(std::move(msg)) {}
    const char* what() const noexcept override { return m_msg.c_str(); }

private:
    std::string m_msg;
};

// Format a unified-style diff between two QStrings. Lines that match
// at the same index render with two leading spaces; mismatched or
// extra lines render with `- ` (only in `actual`) or `+ ` (only in
// `expected`). The alignment is positional — no LCS — which is
// cheap and readable enough for typical small-difference cases.
inline QString format_qstring_diff(const QString& actual, const QString& expected) {
    const QStringList a_lines = actual.split(QChar('\n'));
    const QStringList e_lines = expected.split(QChar('\n'));
    QString out = QStringLiteral("  diff (- actual / + expected):");
    const int n = std::max(a_lines.size(), e_lines.size());
    for (int i = 0; i < n; ++i) {
        const bool has_a = i < a_lines.size();
        const bool has_e = i < e_lines.size();
        if (has_a && has_e && a_lines[i] == e_lines[i]) {
            out += QStringLiteral("\n    %1").arg(a_lines[i]);
        } else {
            if (has_a) {
                out += QStringLiteral("\n  - %1").arg(a_lines[i]);
            }
            if (has_e) {
                out += QStringLiteral("\n  + %1").arg(e_lines[i]);
            }
        }
    }
    return out;
}

template <typename A, typename B>
inline void assert_eq(const A& actual, const B& expected, const char* file, int line) {
    if (actual == expected) {
        return;
    }
    // QString-vs-QString failures get a per-line diff. The single-
    // line case still falls through to the simpler `actual: ...`
    // shape since a diff of one line is just noise. Other types
    // (numbers, bools, containers) keep the original format —
    // line-splitting them would be misleading.
    if constexpr (std::is_same_v<A, QString> && std::is_same_v<B, QString>) {
        const bool multiline = actual.contains(QChar('\n'))
            || expected.contains(QChar('\n'));
        if (multiline) {
            QString msg = QStringLiteral("assertion failed at %1:%2\n%3")
                .arg(QString::fromUtf8(file))
                .arg(line)
                .arg(format_qstring_diff(actual, expected));
            throw AssertionFailure(msg.toStdString());
        }
    }
    QString msg = QStringLiteral("assertion failed at %1:%2\n  actual:   %3\n  expected: %4")
        .arg(QString::fromUtf8(file))
        .arg(line)
        .arg(::cute::str::to_string(actual))
        .arg(::cute::str::to_string(expected));
    throw AssertionFailure(msg.toStdString());
}

template <typename A, typename B>
inline void assert_neq(const A& actual, const B& unexpected, const char* file, int line) {
    if (!(actual == unexpected)) {
        return;
    }
    QString msg = QStringLiteral("assertion failed at %1:%2\n  expected values to differ, but both were: %3")
        .arg(QString::fromUtf8(file))
        .arg(line)
        .arg(::cute::str::to_string(actual));
    throw AssertionFailure(msg.toStdString());
}

inline void assert_true(bool cond, const char* file, int line) {
    if (cond) {
        return;
    }
    QString msg = QStringLiteral("assertion failed at %1:%2\n  expected: true\n  actual:   false")
        .arg(QString::fromUtf8(file))
        .arg(line);
    throw AssertionFailure(msg.toStdString());
}

inline void assert_false(bool cond, const char* file, int line) {
    if (!cond) {
        return;
    }
    QString msg = QStringLiteral("assertion failed at %1:%2\n  expected: false\n  actual:   true")
        .arg(QString::fromUtf8(file))
        .arg(line);
    throw AssertionFailure(msg.toStdString());
}

// `assert_throws { body }` succeeds when `body` throws any exception
// (we catch `std::exception` plus `...`). The codegen wraps the user
// block in a nullary lambda and routes it here so the try/catch lives
// in the runtime, not in every emitted call site.
template <typename F>
inline void assert_throws(F&& body, const char* file, int line) {
    try {
        body();
    } catch (const AssertionFailure&) {
        // Re-raise: a failed inner assertion is still a test failure;
        // we only want to swallow user-domain exceptions.
        throw;
    } catch (const std::exception&) {
        return;
    } catch (...) {
        return;
    }
    QString msg = QStringLiteral("assertion failed at %1:%2\n  expected the block to throw, but it returned normally")
        .arg(QString::fromUtf8(file))
        .arg(line);
    throw AssertionFailure(msg.toStdString());
}

// Run a single test and print TAP-lite output. Returns 0 on success,
// 1 on failure. Centralizing the try/catch here keeps the codegen-
// emitted runner main short — one call per test instead of an inline
// catch ladder.
inline int run_one(int n, const char* name, void (*fn)()) {
    try {
        fn();
        std::printf("ok %d - %s\n", n, name);
        return 0;
    } catch (const AssertionFailure& e) {
        std::printf("not ok %d - %s: %s\n", n, name, e.what());
    } catch (const std::exception& e) {
        std::printf("not ok %d - %s: unexpected exception: %s\n", n, name, e.what());
    } catch (...) {
        std::printf("not ok %d - %s: unknown exception\n", n, name);
    }
    return 1;
}

}  // namespace cute::test
