//! Cute HIR -> MIR lowering.
//!
//! Lowering responsibilities:
//! - ARC retain/release insertion for non-QObject reference types
//! - `?` operator desugaring to early-return on `cute::Result`
//! - block (`{ |x| body }`) lowering to explicit-capture closures
//! - string interpolation `"#{expr}"` to QString concatenation
//! - `async fn` / `await` to C++20 coroutine shape

pub fn placeholder() {}
