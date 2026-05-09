// Cute runtime: Slice<T> — non-dangling array view.
//
// `arr[1..3]` lowers to `::cute::make_slice(arr, 1, 2)` and returns
// a `Slice<T>` value that the user can index, iterate, return from a
// fn, or store in a struct field. The slice keeps its backing storage
// alive through a `std::shared_ptr<QList<T>>`, so unlike a raw
// `QSpan<T>` it cannot dangle even if the original local goes out of
// scope.
//
// Why std::shared_ptr (not cute::Arc): cute::Arc<T> requires
// `T : ArcBase`, which QList<T> isn't and shouldn't be. shared_ptr
// is the lifetime tool that fits a non-intrusive control block;
// Slice<T> is only used as a structural carrier for the backing
// pointer, never as a Cute-surface ARC handle.
//
// v1.0 semantics — read-only view backed by a private copy:
//
//   let arr = [1, 2, 3, 4, 5]
//   let s   = arr[1..4]    // Slice<Int>: { backing = copy of arr, off=1, len=3 }
//   s[0]                   // 2 (cheap; no further allocations)
//   for x in s { ... }     // range-based; reuses the backing
//
// The slicing operation copies `arr` into a fresh shared backing the
// FIRST time it's sliced. Subsequent sub-slices of an already-Slice
// value reuse the same backing (cheap pointer + offset arithmetic).
// Mutation through Slice is **not exposed at the surface** in v1.0;
// adding a `MutSlice<T>` later, plus auto-promotion of locals to
// shared backings so `arr[1..3]` and the original `arr` share state
// (true Go semantics), are tracked for v1.x.
//
// What works today: indexing, .size(), iteration, passing as fn
// param (`fn sum(xs: Slice<Int>) -> Int`), returning, storing.

#pragma once

#include <QList>

#include <cstddef>
#include <iterator>
#include <memory>
#include <utility>

namespace cute {

template <typename T>
class Slice {
public:
    using value_type = T;
    using size_type  = qsizetype;

    Slice() = default;

    // Take ownership of an existing shared backing. Used by sub-slicing
    // (`Slice<T>::slice(off, len)`) where the caller already holds a
    // shared_ptr and just wants to clone the handle.
    Slice(std::shared_ptr<QList<T>> backing, qsizetype offset, qsizetype length) noexcept
        : backing_(std::move(backing)), offset_(offset), length_(length) {}

    // Copy a QList<T> into a fresh shared backing. Used for the
    // `arr[a..b]` lowering on a non-shared source (the common case).
    // Note: QList<T> is implicit-shared / CoW, so the "copy" at the
    // call boundary is an atomic refcount bump — not a deep copy —
    // unless someone has already detached the source.
    static Slice from_list(QList<T> source, qsizetype offset, qsizetype length) {
        auto backing = std::make_shared<QList<T>>(std::move(source));
        return Slice(std::move(backing), offset, length);
    }

    qsizetype size() const noexcept { return length_; }
    qsizetype length() const noexcept { return length_; }
    bool empty() const noexcept { return length_ == 0; }

    // Read-only indexing only. Routing through `std::as_const` avoids
    // QList<T>::operator[]'s non-const detach path (which would deep-
    // copy the shared backing on first access — silently breaking the
    // v1.0 read-only-view promise). MutSlice<T> in v1.x will reintroduce
    // a non-const accessor that opts into the detach intentionally.
    const T& operator[](qsizetype i) const {
        return std::as_const(*backing_)[offset_ + i];
    }

    // Sub-slicing on an already-Slice: zero-copy, reuses the backing.
    Slice slice(qsizetype off, qsizetype len) const {
        return Slice(backing_, offset_ + off, len);
    }

    // Iteration uses the const iterators on QList<T> for the same
    // detach-avoidance reason. begin()/end() match what range-based
    // for sees so `for (auto x : slice)` reads through the shared
    // backing without forcing a copy.
    auto begin() const noexcept { return backing_->cbegin() + offset_; }
    auto end()   const noexcept { return backing_->cbegin() + offset_ + length_; }

    auto cbegin() const noexcept { return backing_->cbegin() + offset_; }
    auto cend()   const noexcept { return backing_->cbegin() + offset_ + length_; }

private:
    std::shared_ptr<QList<T>> backing_;
    qsizetype offset_ = 0;
    qsizetype length_ = 0;
};

// Codegen hook for `arr[start..end]` (exclusive range). The `[start, end)`
// range becomes a Slice with `len = end - start`. Lives as a free fn so
// CTAD picks T from the QList<T> argument without the user having to
// spell it.
template <typename T>
inline Slice<T> make_slice(QList<T> source, qsizetype start, qsizetype end) {
    if (start < 0)        start = 0;
    if (end < start)      end = start;
    if (end > source.size()) end = source.size();
    return Slice<T>::from_list(std::move(source), start, end - start);
}

// Codegen hook for `arr[start..=end]` (inclusive range). Symmetric to
// `make_slice` but bumps `end` by one before clamping.
template <typename T>
inline Slice<T> make_slice_inclusive(QList<T> source, qsizetype start, qsizetype end) {
    return make_slice(std::move(source), start, end + 1);
}

// Sub-slicing on a Slice<T> source — zero-copy. Mirrors `make_slice`
// for chained `s[1..3][0..1]` shapes.
template <typename T>
inline Slice<T> make_slice(Slice<T> source, qsizetype start, qsizetype end) {
    if (start < 0)               start = 0;
    if (end < start)             end = start;
    if (end > source.size())     end = source.size();
    return source.slice(start, end - start);
}

template <typename T>
inline Slice<T> make_slice_inclusive(Slice<T> source, qsizetype start, qsizetype end) {
    return make_slice(std::move(source), start, end + 1);
}

}  // namespace cute
