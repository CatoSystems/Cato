//! Cato frontend — backend-agnostic pipeline:
//! reader → macro expansion → HIR build → fusion check.
//!
//! A `Program` is the hand-off artifact: backends (see `cato-backend-*`)
//! consume it and emit code.

pub mod ast;
pub mod expand;
pub mod fuse;
pub mod hir;
pub mod reader;

pub use ast::Expr;
pub use hir::{AdtDef, Defn};

/// A fully lowered, fusion-checked program ready for a backend.
#[derive(Debug, Clone)]
pub struct Program {
    pub adts: Vec<AdtDef>,
    pub defns: Vec<Defn>,
}

/// A code generation backend: consumes a `Program`, emits source text (LLVM
/// IR today; other IRs/targets can implement this without touching the
/// frontend).
pub trait Backend {
    fn name(&self) -> &'static str;
    fn emit(&self, program: &Program) -> Result<String, String>;
}

/// Run the full frontend pipeline.
pub fn compile_to_program(src: &str) -> Result<Program, String> {
    let parsed = reader::read_all(src)?;
    let expanded = expand::expand_all(&parsed)?;
    let (adts, defns) = hir::build_program(&expanded)?;
    let defns = defns
        .into_iter()
        .map(|mut d| {
            d.body = fuse::check(&d.body);
            d
        })
        .collect();
    Ok(Program { adts, defns })
}
