//! `cute-qpi-gen` library half — the parts that are independent of
//! libclang and therefore safe to test under `cargo test --lib`
//! without `libclang.dylib` resolvable on the dyld path.
//!
//! - [`typesystem`] — toml schema for the per-Qt-module typesystem
//!   files under `stdlib/qt/typesystem/`.
//! - [`types`] — pure data types (`Method`, `CollectedClass`, …)
//!   produced by the scraper and consumed by the emitter.
//! - [`emit`] — `.qpi` text emission from the collected types.
//!
//! The libclang-driven scraper itself lives in `src/clang_walk.rs`
//! and is bound to the `cute-qpi-gen` binary target only — that
//! crate's `Cargo.toml` sets `test = false` on the bin so the test
//! runner never tries to dyld-load libclang.

pub mod emit;
pub mod types;
pub mod typesystem;
