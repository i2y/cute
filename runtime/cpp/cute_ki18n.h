// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Cute-side wrapper for the KF6 KI18n `i18n()` / `i18nc()` /
// `i18np()` / `i18ncp()` / `ki18n()` macros. The KF6 originals
// take `const char *`, which Cute can't pass without explicit
// conversion (Cute's `String` type is `QString`). These inline
// wrappers do the `qPrintable(...)` bridge once at the call
// site so user .cute code can write
//
//     let s = i18n("Hello, KI18n!")
//
// and have it lower to a working KF6 call. The wrappers are
// ASCII-safe (matching the practical case where the source
// strings live in .cute literals); for runtime-built non-ASCII
// strings, prefer the deferred-resolution `ki18n(...).subs(...)
// .toString()` chain.
//
// Pulled in only when the user's cute.toml lists this header
// under `[cpp] includes`. Including it transitively pulls in
// `<KLocalizedString>` so the underlying KF6 macros are visible.

#pragma once

#include <KLocalizedString>
#include <QString>

namespace cute_kf6_i18n {

inline QString i18n(const QString& text) {
    return ::i18n(qPrintable(text));
}

inline QString i18nc(const QString& context, const QString& text) {
    return ::i18nc(qPrintable(context), qPrintable(text));
}

inline QString i18np(const QString& singular, const QString& plural, qint64 n) {
    return ::i18np(qPrintable(singular), qPrintable(plural), n);
}

inline QString i18ncp(const QString& context,
                      const QString& singular,
                      const QString& plural,
                      qint64 n) {
    return ::i18ncp(qPrintable(context),
                    qPrintable(singular),
                    qPrintable(plural),
                    n);
}

inline KLocalizedString ki18n(const QString& text) {
    return ::ki18n(qPrintable(text));
}

inline KLocalizedString ki18nc(const QString& context, const QString& text) {
    return ::ki18nc(qPrintable(context), qPrintable(text));
}

inline KLocalizedString ki18np(const QString& singular, const QString& plural) {
    return ::ki18np(qPrintable(singular), qPrintable(plural));
}

}  // namespace cute_kf6_i18n

// Bring just the wrapped names into the including TU's scope, so the
// Cute-generated code's unqualified `i18n("...")` / `ki18n("...")`
// call sites resolve here. Per-name `using` is preferred over
// `using namespace cute_kf6_i18n;` — the latter pollutes the TU
// with everything the namespace might gain in the future.
using cute_kf6_i18n::i18n;
using cute_kf6_i18n::i18nc;
using cute_kf6_i18n::i18np;
using cute_kf6_i18n::i18ncp;
using cute_kf6_i18n::ki18n;
using cute_kf6_i18n::ki18nc;
using cute_kf6_i18n::ki18np;
