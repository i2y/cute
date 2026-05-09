// Cute runtime: `cute::function_ref<F>` — a non-owning callable wrapper.
//
// This is the default lowering target for Cute fn-typed parameters
// without `@escaping`. It models a borrowed callable: two pointers
// wide (callable address + type-erased thunk) and never copies the
// underlying callable. The contract is the same as Swift's default
// closure-param semantics — the callable must outlive the call;
// storing it in a field, returning it, or passing it into another
// `@escaping` slot is a compile-time escape error (caught at the
// Cute layer, not here).
//
// Compared to `std::function<F>`:
// - No allocation. `std::function` may small-buffer-optimise for
//   small captures but otherwise heap-allocates; `function_ref` never
//   does.
// - Doesn't keep the callable alive. The address it stores is
//   borrowed; if you outlive the callable, you UB.
// - Trivially copyable. Costs are equivalent to copying two raw
//   pointers.
//
// Cute users opt into the owning `std::function` form by writing
// `@escaping` on the param.
//
// C++17 header-only.

#pragma once

#include <memory>
#include <type_traits>
#include <utility>

namespace cute {

template <typename Sig>
class function_ref;

template <typename R, typename... Args>
class function_ref<R(Args...)> {
public:
    constexpr function_ref() noexcept = default;
    constexpr function_ref(std::nullptr_t) noexcept : ctx_(nullptr), thunk_(nullptr) {}

    /// Construct from any callable. The callable's address is
    /// borrowed — no copy is made. Caller is responsible for
    /// keeping the callable alive for the lifetime of this
    /// `function_ref`.
    template <typename F,
              typename = std::enable_if_t<
                  !std::is_same_v<std::decay_t<F>, function_ref>
                  && std::is_invocable_r_v<R, F&, Args...>>>
    function_ref(F&& f) noexcept
        : ctx_(const_cast<void*>(static_cast<const void*>(std::addressof(f)))),
          thunk_(&invoker<std::remove_reference_t<F>>) {}

    constexpr function_ref(const function_ref&) noexcept = default;
    constexpr function_ref& operator=(const function_ref&) noexcept = default;

    R operator()(Args... args) const {
        return thunk_(ctx_, std::forward<Args>(args)...);
    }

    explicit operator bool() const noexcept { return thunk_ != nullptr; }

private:
    void* ctx_ = nullptr;
    R (*thunk_)(void*, Args...) = nullptr;

    template <typename F>
    static R invoker(void* ctx, Args... args) {
        return (*static_cast<F*>(ctx))(std::forward<Args>(args)...);
    }
};

}  // namespace cute
