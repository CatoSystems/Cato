//! LLVM IR emission, split by concern: module assembly (this file), function
//! context, loop hoisting and emission, scalar emitters, ADTs, and heap
//! construction. All items are crate-internal; the public surface is
//! `LlvmBackend` plus the legacy free functions re-exported by the crate root.

mod adt;
mod analysis;
mod calls;
mod closures;
mod comprehension;
mod emit_loop;
mod exprs;
mod exprs_flow;
mod exprs_val;
mod fnctx;
mod funcs;
mod heap_map;
mod heap_set;
mod heap_vec;
mod intops;
mod loops_spec;
mod rewrite;
mod state;

pub(crate) use adt::*;
pub(crate) use analysis::*;
pub(crate) use calls::*;
pub(crate) use closures::*;
pub(crate) use comprehension::*;
pub(crate) use emit_loop::*;
pub(crate) use exprs::*;
pub(crate) use exprs_flow::*;
pub(crate) use exprs_val::*;
pub(crate) use fnctx::*;
pub(crate) use funcs::*;
pub(crate) use heap_map::*;
pub(crate) use heap_set::*;
pub(crate) use heap_vec::*;
pub(crate) use intops::*;
pub(crate) use loops_spec::*;
pub(crate) use rewrite::*;
pub(crate) use state::*;

use cato_core::hir::*;
use cato_core::Lambda;
use cato_core::{hir::AdtDef, hir::Defn, Backend};
use std::collections::HashMap;
use std::collections::HashSet;

pub fn emit_module(adts: &[AdtDef], fns: &[Defn]) -> String {
    emit_module_fast(adts, fns, false, &[], &[], 1)
}

pub fn emit_module_fast(
    adts: &[AdtDef],
    fns: &[Defn],
    fast: bool,
    fn_sigs: &[(Vec<Ty>, Ty)],
    lambdas: &[Lambda],
    threads: usize,
) -> String {
    // Signature table: callee name -> (ret, param types). Includes declared
    // defns and hoisted loop helpers.
    let mut sigs: HashMap<String, (Ty, Vec<Ty>)> = HashMap::new();
    for d in fns {
        sigs.insert(
            d.name.clone(),
            (d.ret, d.params.iter().map(|(_, t)| *t).collect()),
        );
    }

    let mut loops: Vec<LoopSpec> = Vec::new();
    let mut idmap: HashMap<String, usize> = HashMap::new();
    let mut plain: Vec<Defn> = Vec::new();
    let mut mdl = Mdl::new(
        adts.to_vec(),
        fast,
        fn_sigs.to_vec(),
        lambdas.to_vec(),
        threads,
    );
    for d in fns.iter().filter(|d| d.is_extern && d.ret == Ty::Str) {
        mdl.extern_str.insert(d.name.clone(), d.ret_owned);
    }

    for d in fns {
        // Every function keeps the ABI its signature advertises: a loop body
        // is hoisted into a helper and called with the helper's own
        // (data, len, captures) parameter order. Emitting the function *as*
        // the helper would hand callers a signature they never pass — they
        // use the declared parameter order — which corrupts the call on
        // targets that assign argument registers by position (Windows x64).
        let body = rewrite_body(
            &d.body,
            &mangle(&d.name),
            &Scope::new(d.params.clone(), adts),
            &mut idmap,
            &mut loops,
            &mut sigs,
        )
        .unwrap_or_else(|e| panic!("codegen: {}", e));
        let mut nd = d.clone();
        nd.body = body;
        plain.push(nd);
    }

    let mut output: Vec<String> = Vec::new();

    output.push("; cato: generated LLVM IR (vertical slice)".to_string());
    output.push("%VecI64 = type { ptr, i64, i64 }".to_string());
    output.push("%Closure = type { ptr, ptr, ptr }".to_string());
    let boxed = mdl.boxed.clone();
    for (i, adt) in adts.iter().enumerate() {
        let n = cato_core::adt_total_slots(adts, &boxed, i as u32);
        output.push(format!("%{} = type {{ i64, [{} x i64] }}", adt.name, n));
    }
    output.push(String::new());
    // Heap helpers used by clone/drop glue and allocating constructors.
    mdl.ensure_decl("declare ptr @malloc(i64)");
    mdl.ensure_decl("declare void @free(ptr)");
    mdl.ensure_decl("declare ptr @strdup(ptr)");
    mdl.ensure_decl("declare ptr @cato_str_vec_copy(ptr, i64, i64)");
    mdl.ensure_decl("declare void @cato_str_vec_free(ptr, i64)");
    mdl.ensure_decl("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1, i1)");
    // Deep-clone helpers for boxed (recursive) ADTs: clone must produce an
    // owning copy, so a boxed value's children are cloned recursively.
    for (i, _) in adts.iter().enumerate() {
        if boxed[i] {
            output.push(String::new());
            output.push(emit_adt_clone_fn(adts, &boxed, i as u32));
        }
    }
    // Drop glue for every owning ADT (frees transitively; boxed ADTs also
    // free their box). Free-list reuse at the call sites keeps this cheap.
    let owns = mdl.owns.clone();
    for (i, _) in adts.iter().enumerate() {
        if owns[i] {
            output.push(String::new());
            output.push(emit_adt_drop_fn(adts, &owns, &boxed, i as u32));
        }
    }

    output.push("declare void @cato_print_f64(double)".to_string());
    output.push("declare void @cato_print_i64(i64)".to_string());
    output.push("declare void @cato_print_string(ptr)".to_string());

    for spec in &loops {
        output.push(String::new());
        output.push(emit_loop(spec, &mut mdl, &sigs));
        if spec.parallel {
            output.push(String::new());
            output.push(emit_parallel_runners(spec, &mut mdl));
        }
    }
    for defn in &plain {
        output.push(String::new());
        if defn.is_extern {
            let decl = emit_extern_decl(defn, &mut mdl);
            mdl.declare(decl.trim_end().to_string());
        } else {
            output.push(emit_plain(defn, &mut mdl, &sigs));
        }
    }
    // Closure bodies get the same loop-hoisting rewrite as plain defns.
    for lambda in &mdl.lambdas.clone() {
        let mut idmap2: HashMap<String, usize> = HashMap::new();
        // A lambda body sees its parameters and its captures.
        let mut lvars = lambda.captures.clone();
        lvars.extend(lambda.params.iter().cloned());
        let body = rewrite_body(
            &lambda.body,
            &lambda.mangled,
            &Scope::new(lvars, adts),
            &mut idmap2,
            &mut loops,
            &mut sigs,
        )
        .unwrap_or_else(|e| panic!("codegen: {}", e));
        let mut l = lambda.clone();
        l.body = body;
        output.push(String::new());
        output.push(emit_lambda(&l, &mut mdl, &sigs));
        output.push(emit_lambda_drop(&l, &mut mdl, &sigs));
    }

    if fast {
        output.push(String::new());
        output.push("!0 = distinct !{!0, !1}".to_string());
        output.push("!1 = !{!\"llvm.loop.vectorize.enable\", i1 true}".to_string());
    }

    // Runtime-helper declares minted during emission belong at module top.
    for d in &mdl.decls {
        let i = output
            .iter()
            .position(|l| l.starts_with("declare void @cato_print_i64"))
            .unwrap_or(0);
        output.insert(i + 1, d.clone());
        output.insert(i + 1, String::new());
    }

    // Vec-literal globals minted during emission belong at module top.
    for g in mdl.globals() {
        let i = output
            .iter()
            .position(|l| l.starts_with("declare void @cato_print_i64"))
            .unwrap_or(0);
        output.insert(i + 1, g.clone());
        output.insert(i + 1, String::new());
    }

    output.join("\n")
}

/// The code generation target of the emitted module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Target {
    /// The compiling host (no explicit triple in the emitted IR; the
    /// consuming compiler picks its default).
    #[default]
    Native,
    /// WebAssembly 32-bit freestanding (`wasm32-unknown-unknown`). The
    /// emitted IR carries the wasm32 triple and data layout; runtime C
    /// helpers (print, malloc) become host imports.
    Wasm32,
    /// An explicit cross-compilation triple (e.g. `x86_64-unknown-linux-gnu`).
    /// The triple is stamped in the header; clang supplies the matching
    /// data layout from it.
    Triple(&'static str),
}

impl Target {
    /// `(triple, data layout)` for the module header, or `None` for the
    /// native default.
    fn header(self) -> Option<(&'static str, Option<&'static str>)> {
        match self {
            Target::Native => None,
            Target::Wasm32 => Some((
                "wasm32-unknown-unknown",
                Some("e-m:e-p:32:32-i64:64-a:0:32-n32:64-S128"),
            )),
            Target::Triple(t) => Some((t, None)),
        }
    }
}

/// The LLVM IR backend. `fast` attaches vectorization metadata to fused
/// loops (`--fast`).
pub struct LlvmBackend {
    pub fast: bool,
    /// Thread count for `(parallel ...)` loops (`--threads`, default 1).
    pub threads: usize,
    /// Emission target (`--target`, default native).
    pub target: Target,
}

impl Default for LlvmBackend {
    fn default() -> Self {
        LlvmBackend {
            fast: false,
            threads: 1,
            target: Target::Native,
        }
    }
}

impl Backend for LlvmBackend {
    fn name(&self) -> &'static str {
        "llvm"
    }

    fn emit(&self, program: &cato_core::Program) -> Result<String, String> {
        let ir = emit_module_fast(
            &program.adts,
            &program.defns,
            self.fast,
            &program.fn_sigs,
            &program.lambdas,
            self.threads,
        );
        Ok(match self.target.header() {
            None => ir,
            Some((triple, layout)) => {
                let layout = layout
                    .map(|l| format!("target datalayout = \"{l}\"\n"))
                    .unwrap_or_default();
                format!("{layout}target triple = \"{triple}\"\n\n{ir}")
            }
        })
    }
}
