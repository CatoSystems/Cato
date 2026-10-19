//! C++ shim over lld's in-process link API (feature `link`).
//!
//! Without the `link` feature this crate is an empty stub: callers are
//! `cfg`-gated on `in-process-lld`, which enables `link`.
