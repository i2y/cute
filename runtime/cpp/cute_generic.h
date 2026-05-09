// Cute generic-fn body helpers.
//
// Codegen wraps method/property accesses on a generic type parameter
// (e.g. `xs.method()` inside `fn use_it<T: Foo>(xs: T)`) with
// `::cute::deref(...)`. At C++ template instantiation time, the
// `if constexpr` picks the right form: for a pointer T (typical
// QObject case) the helper dereferences to a value reference; for a
// value T the helper returns the reference unchanged. The user-side
// member-access syntax stays uniform: `xs.method()` always.
//
// Without this, a `fn use_it<T: Foo>(xs: T)` instantiated with
// `T = Person*` would emit `xs.method()` against a pointer and fail
// to compile. Cute's existing pointer-vs-value lowering rule
// (`recv->method` vs `recv.method`) needs the receiver class to be
// known statically, which a generic parameter isn't.

#pragma once

#include <type_traits>

namespace cute {

// Forward declaration so the `Arc<T>` deref overloads below can be
// declared without forcing a circular include with cute_arc.h.
// (Generic-bound bodies frequently exercise both ARC and raw-
// pointer Ts, so deref needs to know about both shapes.)
template <typename T>
class Arc;

template <typename T>
inline auto& deref(T& x) {
    if constexpr (std::is_pointer_v<std::remove_reference_t<T>>) {
        return *x;
    } else {
        return x;
    }
}

template <typename T>
inline const auto& deref(const T& x) {
    if constexpr (std::is_pointer_v<std::remove_reference_t<T>>) {
        return *x;
    } else {
        return x;
    }
}

// Arc<T> is the smart pointer used by ARC (non-QObject) classes.
// Generic-bound bodies need to reach the underlying T to call
// trait methods on it, so unwrap one level here. Without these
// overloads, `fn use_it<T: Foo>(thing: T)` instantiated with
// `T = cute::Arc<MyClass>` would call `.method()` on the Arc
// itself and fail to compile.
template <typename T>
inline T& deref(Arc<T>& x) {
    return *x;
}

template <typename T>
inline const T& deref(const Arc<T>& x) {
    return *x;
}

}  // namespace cute
