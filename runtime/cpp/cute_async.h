// Cute async/await runtime — makes QFuture<T> usable as a C++20
// coroutine return type AND awaitable.
//
// Qt 6.11's QFuture<T> ships neither `coroutine_traits<...>::promise_type`
// (so `QFuture<int> foo() { co_return 42; }` fails with "no member named
// 'promise_type'") nor `operator co_await` (so `co_await someFuture`
// fails with "no member named 'await_ready'"). This header closes both
// gaps:
//
// 1. A `std::coroutine_traits<QFuture<T>>` specialization that routes
//    the coroutine body through `QPromise<T>` — Qt's canonical bridge
//    between a coroutine and a QFuture observer.
// 2. A free `operator co_await(QFuture<T>)` that wraps the future in
//    a `QFutureAwaiter<T>`, which uses QFutureWatcher::finished to
//    resume the coroutine when the future completes.
//
// Cute's `async fn f T` (and `async fn f Future<T>`) lowers to
// `QFuture<T> f() { ... co_return v; }`, so this header is the missing
// piece that lets that lowering compile and run against Qt 6.11.
//
// When Qt itself ships either piece, the corresponding section becomes
// dead and can be retired.

#pragma once

#include <QFuture>
#include <QFutureWatcher>
#include <QObject>
#include <QPromise>
#include <coroutine>
#include <exception>
#include <type_traits>
#include <utility>

namespace cute::async_detail {

// Common machinery shared between the T and void specializations.
// Derived adds `return_value(v)` (T case) or `return_void()` (void
// case) — they can't both live here because C++ requires exactly one
// of the two on a promise type, and which one depends on T.
template<class T>
struct PromiseBase {
    QPromise<T> promise_;

    QFuture<T> get_return_object() {
        promise_.start();
        return promise_.future();
    }
    std::suspend_never initial_suspend() noexcept { return {}; }
    std::suspend_never final_suspend() noexcept { return {}; }
    void unhandled_exception() {
        promise_.setException(std::current_exception());
        promise_.finish();
    }
};

template<class T>
struct Promise : PromiseBase<T> {
    template<class U>
    void return_value(U&& v) {
        this->promise_.addResult(std::forward<U>(v));
        this->promise_.finish();
    }
};

template<>
struct Promise<void> : PromiseBase<void> {
    void return_void() {
        this->promise_.finish();
    }
};

// Awaitable adapter for QFuture<T>. If the future is already finished
// we skip the watcher; otherwise a heap-allocated QFutureWatcher
// resumes the coroutine when the future completes, then queues
// itself for `deleteLater()`.
//
// Suspension-path constraint: `deleteLater()` and the watcher's
// queued cross-thread `finished` signal both require a running
// QEventLoop. qml_app / widget_app builds always have one
// (QApplication::exec). cli_app builds do not, so `co_await`-ing a
// future that hasn't finished yet inside a cli_app will hang and
// leak the watcher. The async_demo deliberately keeps every
// awaited future synchronously-ready (await_ready() == true) so the
// suspension path is never exercised. Lifting this constraint needs
// cli_app to enter an event loop when async fns are present.
template<class T>
struct QFutureAwaiter {
    QFuture<T> future_;

    bool await_ready() const noexcept { return future_.isFinished(); }

    void await_suspend(std::coroutine_handle<> h) {
        auto* watcher = new QFutureWatcher<T>;
        QObject::connect(watcher, &QFutureWatcher<T>::finished,
                         [h, watcher]() {
                             watcher->deleteLater();
                             h.resume();
                         });
        watcher->setFuture(future_);
    }

    decltype(auto) await_resume() {
        if constexpr (std::is_void_v<T>) {
            future_.waitForFinished();
            return;
        } else {
            return future_.result();
        }
    }
};

} // namespace cute::async_detail

template<class T>
auto operator co_await(QFuture<T> f) {
    return ::cute::async_detail::QFutureAwaiter<T>{std::move(f)};
}

// std::coroutine_traits is the customization point. Specializing it for
// QFuture<T> as the return type lets `QFuture<T> foo() { co_return v; }`
// pick up our promise. Args... swallows whatever parameter list the
// coroutine has; we don't need to vary by it.
namespace std {

template<class T, class... Args>
struct coroutine_traits<QFuture<T>, Args...> {
    using promise_type = ::cute::async_detail::Promise<T>;
};

template<class... Args>
struct coroutine_traits<QFuture<void>, Args...> {
    using promise_type = ::cute::async_detail::Promise<void>;
};

} // namespace std
