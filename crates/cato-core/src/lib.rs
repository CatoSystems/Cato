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
pub mod opt;
pub mod reader;

pub use ast::Expr;
pub use hir::{adt_total_slots, compute_adt_facts, compute_boxed, field_slot_count, AdtDef, AdtFacts, Defn};

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
    compile_named_with("input.cato", src, true)
}

/// Like `compile_to_program`, with the file name used in diagnostics.
pub fn compile_named(file: &str, src: &str) -> Result<Program, String> {
    compile_named_with(file, src, true)
}

/// Compile with the middle-end optimization pipeline on (`true`, default)
/// or off (`false` — the pre-optimization HIR, useful for debugging and
/// differential testing).
pub fn compile_named_with(file: &str, src: &str, optimize: bool) -> Result<Program, String> {
    diagnostics::with_source(file, src, || {
        let mut seen_files = std::collections::HashSet::new();
        seen_files.insert(file.to_string());
        let mut seen_ns: Vec<String> = Vec::new();
        let located = expand_file(file, src, &mut seen_files, &mut seen_ns)?;
        // The prelude defines `print` as a multimethod over the builtin
        // types; it participates in the same pipeline as user code.
        let (prelude_forms, _) = reader::read_all_spanned(PRELUDE)
            .map_err(|e| e.msg)?;
        let located: Vec<_> = prelude_forms
            .into_iter()
            .map(|f| (f, crate::ast::Span::default()))
            .chain(located)
            .collect();
        let (forms, spans): (Vec<_>, Vec<_>) = located.iter().cloned().unzip();
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
        let mut program = Program {
            adts,
            defns,
            lambdas,
            fn_sigs,
        };
        if optimize {
            opt::optimize(&mut program)?;
        }
        // S1c: owned move-type parameters free at body exit (after all
        // passes, so none of them rewrites the wrapper away).
        let facts = hir::compute_adt_facts(&program.adts);
        hir::annotate_param_frees(&mut program, &facts);
        Ok(program)
    })
}

/// Builtin multimethods, prepended to every compilation. `(print x)`
/// dispatches on the static type of `x`; examples use `print` instead of
/// the per-type builtins (which remain as the method bodies' primitives).
const PRELUDE: &str = r#"
(defgeneric print [x])
(defmethod print [x :- i64] (print-i64 x))
(defmethod print [x :- f64] (print-f64 x))
(defmethod print [x :- str] (print-string x))
(defmethod print [x :- bool] (print-i64 x))
(defmethod print [x :- i8] (print-i64 x))
(defmethod print [x :- i16] (print-i64 x))
(defmethod print [x :- i32] (print-i64 x))
(defmethod print [x :- u8] (print-i64 x))
(defmethod print [x :- u16] (print-i64 x))
(defmethod print [x :- u32] (print-i64 x))
(defmethod print [x :- u64] (print-i64 x))
(defmethod print [x :- usize] (print-i64 x))
"#;

// ---------------------------------------------------------------------------
// Phase 19: modules — (ns ...) and (require [path :as alias])
// ---------------------------------------------------------------------------

/// Is this form `(require [path :as alias])`?
fn require_of(form: &Expr) -> Option<(String, String)> {
    let Expr::List(items) = form else { return None };
    if !matches!(items.first(), Some(Expr::LitSym(h)) if h == "require") {
        return None;
    }
    let Some(Expr::Vector(spec)) = items.get(1) else {
        return None;
    };
    if spec.len() != 3 {
        return None;
    }
    let Some(Expr::LitStr(path)) = spec.first() else {
        return None;
    };
    if !matches!(spec.get(1), Some(Expr::LitSym(s)) if s == ":as") {
        return None;
    }
    let Some(Expr::LitSym(alias)) = spec.get(2) else {
        return None;
    };
    Some((path.clone(), alias.clone()))
}

/// Rewrite call heads in `forms`: `from` -> `to` (position-0 symbols of
/// list forms), plus defn headers named `from`.
fn rewrite_heads(forms: &mut [(Expr, crate::ast::Span)], from: &str, to: &str) {
    for (form, _) in forms.iter_mut() {
        rewrite_heads_expr(form, from, to);
    }
}

fn rewrite_heads_expr(e: &mut Expr, from: &str, to: &str) {
    match e {
        Expr::List(items) => {
            if let Some(Expr::LitSym(h)) = items.first_mut() {
                if h == from {
                    *h = to.to_string();
                }
            }
            if matches!(items.first(), Some(Expr::LitSym(h)) if h == "defn") {
                if let Some(Expr::LitSym(n)) = items.get_mut(1) {
                    if n == from {
                        *n = to.to_string();
                    }
                }
            }
            for it in items.iter_mut() {
                rewrite_heads_expr(it, from, to);
            }
        }
        Expr::Vector(items) => {
            for it in items.iter_mut() {
                rewrite_heads_expr(it, from, to);
            }
        }
        Expr::MapLit(pairs) => {
            for (k, v) in pairs.iter_mut() {
                rewrite_heads_expr(k, from, to);
                rewrite_heads_expr(v, from, to);
            }
        }
        _ => {}
    }
}

/// Defn/extern names declared by these forms.
fn defn_names(forms: &[(Expr, crate::ast::Span)]) -> Vec<String> {
    let mut out = Vec::new();
    for (f, _) in forms {
        if let Expr::List(items) = f {
            if matches!(items.first(), Some(Expr::LitSym(h)) if h == "defn" || h == "extern") {
                if let Some(Expr::LitSym(n)) = items.get(1) {
                    out.push(n.clone());
                }
            }
        }
    }
    out
}

/// Load `file`, pull in its `(require ...)`s recursively, and return the
/// flattened (form, span) list (require/ns forms dropped). Required defns
/// are renamed `alias__name` and the requiring file's `alias/name` call
/// heads are rewritten to match. `(ns name)` is validated (one per file,
/// names unique across the compilation) and dropped.
fn expand_file(
    file: &str,
    src: &str,
    seen_files: &mut std::collections::HashSet<String>,
    seen_ns: &mut Vec<String>,
) -> Result<Vec<(Expr, crate::ast::Span)>, String> {
    let (all_forms, all_spans) = reader::read_all_spanned(src)
        .map_err(|e| diagnostics::locate(e.span, &e.msg))?;

    let mut kept: Vec<(Expr, crate::ast::Span)> = Vec::new();
    for (f, s) in all_forms.into_iter().zip(all_spans) {
        if let Expr::List(items) = &f {
            if matches!(items.first(), Some(Expr::LitSym(h)) if h == "ns") {
                match items.get(1) {
                    Some(Expr::LitSym(name)) => {
                        if seen_ns.contains(name) {
                            return Err(diagnostics::locate(
                                s,
                                &format!("duplicate ns {}", name),
                            ));
                        }
                        seen_ns.push(name.clone());
                    }
                    _ => return Err(diagnostics::locate(s, "ns expects a name")),
                }
                continue;
            }
        }
        kept.push((f, s));
    }

    let mut merged: Vec<(Expr, crate::ast::Span)> = Vec::new();
    let mut own: Vec<(Expr, crate::ast::Span)> = Vec::new();
    let mut alias_rewrites: Vec<(String, String)> = Vec::new();
    for (f, s) in kept {
        if let Some((path, alias)) = require_of(&f) {
            let dir = std::path::Path::new(file)
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_default();
            let child_file = dir.join(&path).to_string_lossy().to_string();
            if !seen_files.insert(child_file.clone()) {
                continue; // already merged once
            }
            let child_src = std::fs::read_to_string(&child_file).map_err(|e| {
                diagnostics::locate(
                    s,
                    &format!("require: cannot read {}: {}", child_file, e),
                )
            })?;
            let child = expand_file(&child_file, &child_src, seen_files, seen_ns)?;
            // Rename the child's defns under the alias and rewrite its own
            // internal call heads to the renamed versions.
            let names = defn_names(&child);
            let mut renamed = child;
            for n in &names {
                rewrite_heads(&mut renamed, n, &format!("{}__{}", alias, n));
            }
            merged.extend(renamed);
            for n in &names {
                alias_rewrites.push((format!("{}/{}", alias, n), format!("{}__{}", alias, n)));
            }
            continue;
        }
        own.push((f, s));
    }
    // Alias rewrites apply to the whole file, so run them after every
    // form has been collected.
    for (from, to) in &alias_rewrites {
        rewrite_heads(&mut own, from, to);
    }
    merged.extend(own);
    Ok(merged)
}
