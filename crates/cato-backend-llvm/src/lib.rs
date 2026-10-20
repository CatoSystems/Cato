//! Cato LLVM IR backend.
//!
//! Emits textual LLVM IR from a `cato_core::Program`: tagged-union ADTs,
//! fused single-loop pipelines, unboxed scalars, `noalias`/`inbounds`/
//! `align 8` loop data, `--fast` vectorization metadata.

mod llvm;
pub use llvm::{emit_module, emit_module_fast, LlvmBackend, Target};

/// Attach a function attribute to every definition in emitted IR (e.g.
/// `sanitize_address`). Clang instruments only the functions that carry
/// `sanitize_address` when its input is `.ll`: without it,
/// `-fsanitize=address` checked nothing but the allocator and the C
/// runtime — out-of-bounds and use-after-free accesses in generated code
/// went unseen.
pub fn with_function_attribute(ir: &str, attr: &str) -> String {
    let mut out = String::with_capacity(ir.len() + 64);
    let mut tagged = false;
    for line in ir.lines() {
        if line.starts_with("define ") && line.trim_end().ends_with('{') {
            let head = line.trim_end().trim_end_matches('{').trim_end();
            out.push_str(head);
            out.push_str(" #9001 {\n");
            tagged = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if tagged {
        out.push_str(&format!("attributes #9001 = {{ {} }}\n", attr));
    }
    out
}
