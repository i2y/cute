// Cute runtime: ARC (automatic reference counting) for non-QObject types.
//
// Spec: "通常クラス(非 QObject) → ARC + weak". This header provides the
// `cute::Arc<T>` smart pointer + `cute::ArcBase` intrusive base, plus
// `cute::Weak<T>` for non-owning references that don't keep the pointee
// alive. Together they cover the cycle-breaking case (parent owns child
// strongly, child holds parent via weak) without any GC machinery.
//
// Design notes:
// - Intrusive: the strong refcount lives on the object itself, like Qt's
//   QSharedData. Avoids a control-block allocation per Arc<T>.
// - Atomic refs: Cute does not statically prove single-threaded use, so
//   refcount ops are atomic. The cost is negligible for the access patterns
//   we care about (ARC traffic at scope boundaries).
// - Weak<T> uses a *separate*, lazy-allocated control block (cute::WeakCtl)
//   that survives the object's destruction as long as any weak ref still
//   holds it. The block is allocated on the first weak ref via CAS — objects
//   that are never weak'd pay no allocation cost.
//
// C++17 header-only.

#pragma once

#include <atomic>
#include <cstddef>
#include <type_traits>
#include <utility>

namespace cute {

class ArcBase;

/// Internal: weak-reference control block. Allocated lazily on the first
/// weak ref to an `ArcBase`-derived object. Lifetime: as long as any weak
/// ref holds it OR the object is alive — it's freed when both counts
/// hit zero.
struct WeakCtl {
    /// Total holds: count of live `Weak<T>` instances pointing here,
    /// plus 1 if the object is still alive. The "alive" hold is
    /// dropped when the strong refcount on the object reaches 0, at
    /// which point `obj` is also nulled out.
    std::atomic<int> weak_refs;
    /// Object pointer, or nullptr after the object has been destroyed.
    /// Lock attempts read this and bail if null.
    std::atomic<ArcBase*> obj;

    explicit WeakCtl(ArcBase* o) noexcept : weak_refs(1), obj(o) {}
};

/// Tag type for the `Arc<T>(T*, AdoptTag)` constructor that adopts an
/// already-retained pointer without bumping the refcount. Used by
/// `Weak<T>::lock()` after a successful `retain_if_alive`.
struct AdoptTag {};

class ArcBase {
public:
    ArcBase() noexcept : refs_(0), weak_ctl_(nullptr) {}
    ArcBase(const ArcBase&) noexcept : refs_(0), weak_ctl_(nullptr) {}
    ArcBase& operator=(const ArcBase&) noexcept { return *this; }
    virtual ~ArcBase() = default;

    void retain() const noexcept {
        refs_.fetch_add(1, std::memory_order_relaxed);
    }

    void release() const noexcept {
        if (refs_.fetch_sub(1, std::memory_order_acq_rel) == 1) {
            std::atomic_thread_fence(std::memory_order_acquire);
            // Notify weak observers BEFORE deletion: any concurrent
            // Weak<T>::lock() seeing obj == nullptr will return null.
            WeakCtl* ctl = weak_ctl_.load(std::memory_order_acquire);
            if (ctl) {
                ctl->obj.store(nullptr, std::memory_order_release);
                // Drop the alive hold; if no weak refs remain, free
                // the control block too.
                if (ctl->weak_refs.fetch_sub(1, std::memory_order_acq_rel) == 1) {
                    delete ctl;
                }
            }
            delete this;
        }
    }

    int ref_count() const noexcept {
        return refs_.load(std::memory_order_acquire);
    }

    /// Implementation detail used by `cute::Weak<T>::lock()`:
    /// atomically bumps the strong refcount only if the object is
    /// still alive (refs > 0). Returns true on success.
    bool retain_if_alive() const noexcept {
        int cur = refs_.load(std::memory_order_acquire);
        while (cur > 0) {
            if (refs_.compare_exchange_weak(
                    cur, cur + 1,
                    std::memory_order_acq_rel,
                    std::memory_order_acquire)) {
                return true;
            }
            // CAS failed: cur was reloaded; loop with the fresh value.
        }
        return false;
    }

    /// Implementation detail: lazy-allocate the weak control block on
    /// the first weak ref. Idempotent and thread-safe via CAS — every
    /// caller observes the same WeakCtl* once published.
    WeakCtl* ensure_weak_ctl() const noexcept {
        WeakCtl* ctl = weak_ctl_.load(std::memory_order_acquire);
        if (ctl) return ctl;
        WeakCtl* fresh = new WeakCtl(const_cast<ArcBase*>(this));
        WeakCtl* expected = nullptr;
        if (weak_ctl_.compare_exchange_strong(
                expected, fresh,
                std::memory_order_acq_rel,
                std::memory_order_acquire)) {
            return fresh;
        }
        // Lost the race; another thread published `expected` first.
        delete fresh;
        return expected;
    }

private:
    mutable std::atomic<int> refs_;
    mutable std::atomic<WeakCtl*> weak_ctl_;
};

template <typename T>
class Arc {
    static_assert(std::is_base_of<ArcBase, T>::value,
                  "cute::Arc<T> requires T to inherit cute::ArcBase");

public:
    constexpr Arc() noexcept : ptr_(nullptr) {}
    constexpr Arc(std::nullptr_t) noexcept : ptr_(nullptr) {}

    explicit Arc(T* p) noexcept : ptr_(p) {
        if (ptr_) ptr_->retain();
    }

    /// Adopts an already-retained pointer without bumping the
    /// refcount. The caller is responsible for the +1.
    Arc(T* p, AdoptTag) noexcept : ptr_(p) {}

    Arc(const Arc& other) noexcept : ptr_(other.ptr_) {
        if (ptr_) ptr_->retain();
    }

    Arc(Arc&& other) noexcept : ptr_(other.ptr_) {
        other.ptr_ = nullptr;
    }

    template <typename U,
              typename = std::enable_if_t<std::is_convertible<U*, T*>::value>>
    Arc(const Arc<U>& other) noexcept : ptr_(other.get()) {
        if (ptr_) ptr_->retain();
    }

    ~Arc() noexcept {
        if (ptr_) ptr_->release();
    }

    Arc& operator=(const Arc& other) noexcept {
        if (other.ptr_) other.ptr_->retain();
        T* old = ptr_;
        ptr_ = other.ptr_;
        if (old) old->release();
        return *this;
    }

    Arc& operator=(Arc&& other) noexcept {
        if (this != &other) {
            T* old = ptr_;
            ptr_ = other.ptr_;
            other.ptr_ = nullptr;
            if (old) old->release();
        }
        return *this;
    }

    Arc& operator=(std::nullptr_t) noexcept {
        if (ptr_) ptr_->release();
        ptr_ = nullptr;
        return *this;
    }

    T* get() const noexcept { return ptr_; }
    T& operator*() const noexcept { return *ptr_; }
    T* operator->() const noexcept { return ptr_; }
    explicit operator bool() const noexcept { return ptr_ != nullptr; }

    void reset(T* p = nullptr) noexcept {
        if (p) p->retain();
        T* old = ptr_;
        ptr_ = p;
        if (old) old->release();
    }

    template <typename... Args>
    static Arc<T> make(Args&&... args) {
        return Arc<T>(new T(std::forward<Args>(args)...));
    }

private:
    T* ptr_;
};

/// Non-owning weak reference to an `ArcBase`-derived object. Doesn't
/// keep the pointee alive. Use `lock()` to attempt to obtain a strong
/// reference; returns null if the object has already been destroyed.
template <typename T>
class Weak {
    static_assert(std::is_base_of<ArcBase, T>::value,
                  "cute::Weak<T> requires T to inherit cute::ArcBase");

public:
    constexpr Weak() noexcept : ctl_(nullptr) {}
    constexpr Weak(std::nullptr_t) noexcept : ctl_(nullptr) {}

    Weak(const Arc<T>& a) noexcept : ctl_(nullptr) {
        attach_(a.get());
    }

    Weak(const Weak& other) noexcept : ctl_(other.ctl_) {
        if (ctl_) ctl_->weak_refs.fetch_add(1, std::memory_order_relaxed);
    }

    Weak(Weak&& other) noexcept : ctl_(other.ctl_) {
        other.ctl_ = nullptr;
    }

    ~Weak() noexcept { release_(); }

    Weak& operator=(const Weak& other) noexcept {
        if (other.ctl_) other.ctl_->weak_refs.fetch_add(1, std::memory_order_relaxed);
        WeakCtl* old = ctl_;
        ctl_ = other.ctl_;
        if (old) Weak::release_ctl_(old);
        return *this;
    }

    Weak& operator=(Weak&& other) noexcept {
        if (this != &other) {
            WeakCtl* old = ctl_;
            ctl_ = other.ctl_;
            other.ctl_ = nullptr;
            if (old) Weak::release_ctl_(old);
        }
        return *this;
    }

    Weak& operator=(std::nullptr_t) noexcept {
        release_();
        return *this;
    }

    Weak& operator=(const Arc<T>& a) noexcept {
        WeakCtl* old = ctl_;
        ctl_ = nullptr;
        attach_(a.get());
        if (old) Weak::release_ctl_(old);
        return *this;
    }

    /// Try to obtain a strong reference. Returns a null `Arc<T>` if
    /// the pointee has already been destroyed (or this Weak was
    /// default-constructed).
    Arc<T> lock() const noexcept {
        if (!ctl_) return Arc<T>{};
        ArcBase* obj = ctl_->obj.load(std::memory_order_acquire);
        if (!obj) return Arc<T>{};
        if (obj->retain_if_alive()) {
            return Arc<T>(static_cast<T*>(obj), AdoptTag{});
        }
        return Arc<T>{};
    }

    bool expired() const noexcept {
        if (!ctl_) return true;
        return ctl_->obj.load(std::memory_order_acquire) == nullptr;
    }

    explicit operator bool() const noexcept { return !expired(); }

    void reset() noexcept {
        release_();
        ctl_ = nullptr;
    }

private:
    WeakCtl* ctl_;

    void attach_(T* p) noexcept {
        if (!p) return;
        ctl_ = p->ensure_weak_ctl();
        ctl_->weak_refs.fetch_add(1, std::memory_order_relaxed);
    }

    void release_() noexcept {
        if (!ctl_) return;
        Weak::release_ctl_(ctl_);
        ctl_ = nullptr;
    }

    static void release_ctl_(WeakCtl* ctl) noexcept {
        if (ctl->weak_refs.fetch_sub(1, std::memory_order_acq_rel) == 1) {
            delete ctl;
        }
    }
};

template <typename T, typename U>
inline bool operator==(const Arc<T>& a, const Arc<U>& b) noexcept {
    return a.get() == b.get();
}
template <typename T>
inline bool operator==(const Arc<T>& a, std::nullptr_t) noexcept {
    return a.get() == nullptr;
}
template <typename T>
inline bool operator==(std::nullptr_t, const Arc<T>& a) noexcept {
    return a.get() == nullptr;
}
template <typename T, typename U>
inline bool operator!=(const Arc<T>& a, const Arc<U>& b) noexcept {
    return !(a == b);
}

}  // namespace cute
