//! Cato frontend — backend-agnostic pipeline:
//! reader → macro expansion → HIR build → fusion check.
//!
//! A `Program` is the hand-off artifact: backends (see `cato-backend-*`)
//! consume it and emit code.

pub mod ast;
pub mod diagnostics;
pub mod expand;
pub mod fuse;
pub mod hir;
pub mod reader;

pub use ast::Expr;
pub use hir::{AdtDef, Defn};

/// A closure body emitted out-of-line by code backends: the calling
/// convention is `@lambda_N(ptr env, params...) -> ret`, where `env` points
/// to a block holding the captures in order.
#[derive(Debug, Clone)]
pub struct Lambda {
    pub mangled: String,
    /// Captures (name, type) in env-slot order.
    pub captures: Vec<(String, hir::Ty)>,
    pub params: Vec<(String, hir::Ty)>,
    pub ret: hir::Ty,
    pub body: hir::HExpr,
}

/// A fully lowered, fusion-checked program ready for a backend.
#[derive(Debug, Clone)]
pub struct Program {
    pub adts: Vec<AdtDef>,
    pub defns: Vec<Defn>,
    /// Closure bodies (Phase 14).
    pub lambdas: Vec<Lambda>,
    /// Closure signature table: `(param tys, ret ty)` per `Ty::Fn` index.
    pub fn_sigs: Vec<(Vec<hir::Ty>, hir::Ty)>,
}

/// A code generation backend: consumes a `Program`, emits source text (LLVM
/// IR today; other IRs/targets can implement this without touching the
/// frontend).
pub trait Backend {
    fn name(&self) -> &'static str;
    fn emit(&self, program: &Program) -> Result<String, String>;
}

/// Run the full frontend pipeline. Errors carry codes and, where a span is
/// known, a `file:line:col` location with a caret snippet.
pub fn compile_to_program(src: &str) -> Result<Program, String> {
    compile_named("input.cato", src)
}

/// Like `compile_to_program`, with the file name used in diagnostics.
pub fn compile_named(file: &str, src: &str) -> Result<Program, String> {
    diagnostics::with_source(file, src, || {
        let (forms, spans) = reader::read_all_spanned(src)
            .map_err(|e| diagnostics::locate(e.span, &e.msg))?;
        let expanded = expand::expand_all_located(&forms, &spans)?;
        let located: Vec<_> = expanded.into_iter().zip(spans).collect();
        let (adts, defns, lambdas, fn_sigs) = hir::build_program_located(&located)?;
        let defns = defns
            .into_iter()
            .map(|mut d| {
                d.body = fuse::check(&d.body);
                d
            })
            .collect();
        let lambdas = lambdas
            .into_iter()
            .map(|mut l| {
                l.body = fuse::check(&l.body);
                l
            })
            .collect();
        Ok(Program {
            adts,
            defns,
            lambdas,
            fn_sigs,
        })
    })
}
