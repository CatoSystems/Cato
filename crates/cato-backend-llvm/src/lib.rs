//! Cato LLVM IR backend.
//!
//! Emits textual LLVM IR from a `cato_core::Program`: tagged-union ADTs,
//! fused single-loop pipelines, unboxed scalars, `noalias`/`inbounds`/
//! `align 8` loop data, `--fast` vectorization metadata.

mod llvm;
pub use llvm::{emit_module, emit_module_fast, LlvmBackend, Target};
