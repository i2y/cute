// Cute runtime: helpers for the postfix `?.` (null-safe chain) operator.
//
// `recv?.member` lowers to an immediately-invoked lambda that null-tests
// the receiver and either returns the member access lifted into a
// nullable form, or the empty/null sentinel of that nullable form.
//
// The trait `cute::nullable_lift<T>` answers "given the static type of
// the inner expression, what nullable shell wraps it, and what is its
// empty value?":
//
//   nullable_lift<T*>            => T*            (empty: nullptr)
//   nullable_lift<optional<T>>   => optional<T>   (empty: nullopt)
//   nullable_lift<QPointer<T>>   => QPointer<T>   (empty: default-constructed)
//   nullable_lift<Arc<T>>        => Arc<T>        (empty: default-constructed)
//   nullable_lift<T>             => optional<T>   (empty: nullopt)
//
// The pointer / optional / QPointer specializations FLATTEN — without
// them, `recv?.maybe` where `maybe : T?` would lift the
// already-nullable inner result to `optional<optional<T>>`, which
// doesn't implicitly convert back to the surface `T?` Cute's
// type checker computed.
//
// Codegen emits the IIFE shape:
//
//   [&]() {
//       auto __r = <receiver>;
//       using __NL = ::cute::nullable_lift<decltype(__r-><member>(<args>))>;
//       return __r ? __NL::make(__r-><member>(<args>)) : __NL::none();
//   }()
//
// The two `__r-><member>(<args>)` occurrences are textually identical:
// the first is in `decltype` (unevaluated), the second is in the
// non-null branch of the ternary (evaluated at most once). `args`
// only run in the materialized branch — no double-evaluation.

#pragma once

#include <optional>
#include <utility>

#include <QPointer>

#include "cute_arc.h"

namespace cute {

template <typename T>
struct nullable_lift {
    using type = std::optional<T>;
    static type make(T v) { return type(std::move(v)); }
    static type none() { return std::nullopt; }
    // Pattern-test helpers used by `if let some(v) = e { ... }` and
    // `case e { when some(v); when nil }`. Kept on the same trait so
    // codegen can dispatch via `decltype(__tmp)`.
    static bool has_value(const std::optional<T>& v) { return v.has_value(); }
    static const T& value(const std::optional<T>& v) { return *v; }
};

template <typename T>
struct nullable_lift<T*> {
    using type = T*;
    static type make(T* v) { return v; }
    static type none() { return nullptr; }
    static bool has_value(T* v) { return v != nullptr; }
    static T* value(T* v) { return v; }
};

template <typename T>
struct nullable_lift<std::optional<T>> {
    using type = std::optional<T>;
    static type make(std::optional<T> v) { return v; }
    static type none() { return std::nullopt; }
    static bool has_value(const std::optional<T>& v) { return v.has_value(); }
    static const T& value(const std::optional<T>& v) { return *v; }
};

template <typename T>
struct nullable_lift<QPointer<T>> {
    using type = QPointer<T>;
    static type make(QPointer<T> v) { return v; }
    static type none() { return QPointer<T>{}; }
    static bool has_value(const QPointer<T>& v) { return !v.isNull(); }
    static T* value(const QPointer<T>& v) { return v.data(); }
};

/// `cute::Arc<T>` is a smart pointer with `operator bool` that flips
/// to `false` once the strong refcount hits 0 (also serves as the
/// "expired" signal when adopted from a `cute::Weak<T>::lock()`
/// result). Lifting it as a nullable is a no-op shell: `value()`
/// returns the Arc itself so users binding `when some(p) -> ...`
/// keep the strong reference (and the smart pointer's `operator->`
/// gives them member access in the body).
template <typename T>
struct nullable_lift<::cute::Arc<T>> {
    using type = ::cute::Arc<T>;
    static type make(::cute::Arc<T> v) { return v; }
    static type none() { return ::cute::Arc<T>{}; }
    static bool has_value(const ::cute::Arc<T>& v) { return static_cast<bool>(v); }
    static ::cute::Arc<T> value(const ::cute::Arc<T>& v) { return v; }
};

}  // namespace cute
