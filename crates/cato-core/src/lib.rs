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
        let mut ctx = ModCtx {
            canon: std::collections::HashMap::new(),
            ns: Vec::new(),
        };
        // Mark the root as seen (empty prefix) so a cycle back to the entry
        // file terminates instead of re-expanding it.
        ctx.canon.insert(file.to_string(), String::new());
        let located = expand_file(file, src, "", &mut ctx)?;
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
        // Ownership-driven destruction: after all passes (so none of them
        // rewrites the drop annotations away), plan scope-exit drops from
        // the ownership states and free still-owned move-type params.
        let facts = hir::compute_adt_facts(&program.adts);
        let sigs: std::collections::HashMap<String, Vec<hir::Borrow>> = program
            .defns
            .iter()
            .map(|d| (d.name.clone(), d.borrows.clone()))
            .collect();
        let rets: std::collections::HashMap<String, hir::Ty> = program
            .defns
            .iter()
            .map(|d| (d.name.clone(), d.ret))
            .collect();
        let ret_borrowed_names: std::collections::HashSet<String> = program
            .defns
            .iter()
            .filter(|d| d.ret_borrowed)
            .map(|d| d.name.clone())
            .collect();
        let alias_ret = hir::compute_alias_ret(&program.defns);
        for d in program.defns.iter_mut() {
            if d.is_extern {
                continue;
            }
            hir::plan_drops(
                &d.name,
                &d.params,
                &d.borrows,
                d.ret,
                d.ret_borrowed,
                &mut d.body,
                &sigs,
                &ret_borrowed_names,
                &rets,
                &facts,
                &program.adts,
                true,
                &alias_ret,
            )?;
        }
        for lam in program.lambdas.iter_mut() {
            // Lambda params are values inside the (interim, leaked) env;
            // plan their scope-exit drops but no param-free wrapper.
            let sig = format!("<lambda {}>", lam.mangled);
            hir::plan_drops(
                &sig,
                &lam.params,
                &[],
                lam.ret,
                false,
                &mut lam.body,
                &sigs,
                &ret_borrowed_names,
                &rets,
                &facts,
                &program.adts,
                false,
                &alias_ret,
            )?;
        }
        Ok(program)
    })
}

/// Builtin multimethods, prepended to every compilation. `(print x)`
/// dispatches on the static type of `x`; examples use `print` instead of
/// the per-type builtins (which remain as the method bodies' primitives).
const PRELUDE: &str = r#"
(deftype (Option a)
  (Some [v :- a])
  (None))
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

/// What a top-level form is, with respect to `require`.
enum RequireForm {
    /// Not a `(require ...)` form.
    NotRequire,
    /// Head is `require` but the spec is not `[path :as alias]`.
    Malformed,
    /// A well-formed `(require [path :as alias])`.
    Ok(String, String),
}

/// Classify a top-level form as `(require [path :as alias])`. `require` is a
/// reserved head, so a list starting with it is always a require form: a
/// wrong shape is `Malformed` (an error), never an ordinary form.
fn require_of(form: &Expr) -> RequireForm {
    let Expr::List(items) = form else {
        return RequireForm::NotRequire;
    };
    if !matches!(items.first(), Some(Expr::LitSym(h)) if h == "require") {
        return RequireForm::NotRequire;
    }
    let Some(Expr::Vector(spec)) = items.get(1) else {
        return RequireForm::Malformed;
    };
    if spec.len() != 3 {
        return RequireForm::Malformed;
    }
    let Some(Expr::LitStr(path)) = spec.first() else {
        return RequireForm::Malformed;
    };
    if !matches!(spec.get(1), Some(Expr::LitSym(s)) if s == ":as") {
        return RequireForm::Malformed;
    }
    let Some(Expr::LitSym(alias)) = spec.get(2) else {
        return RequireForm::Malformed;
    };
    RequireForm::Ok(path.clone(), alias.clone())
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

/// Defn names declared by these forms. Only `defn` — `extern` names are
/// global C symbols (the backend emits them verbatim, e.g. `system`,
/// `cato_str_byte`), so they are never prefixed and every module sees them.
fn defn_names(forms: &[(Expr, crate::ast::Span)]) -> Vec<String> {
    let mut out = Vec::new();
    for (f, _) in forms {
        if let Expr::List(items) = f {
            if matches!(items.first(), Some(Expr::LitSym(h)) if h == "defn") {
                if let Some(Expr::LitSym(n)) = items.get(1) {
                    out.push(n.clone());
                }
            }
        }
    }
    out
}

/// Module-graph state threaded through `expand_file`.
struct ModCtx {
    /// Resolved path -> canonical prefix (the alias the file was first
    /// required with; `""` for the root). Presence also marks a file seen,
    /// so cycles terminate.
    canon: std::collections::HashMap<String, String>,
    /// `(ns ...)` names already declared, for the duplicate check.
    ns: Vec<String>,
}

/// Lexically normalize a path: drop `.` and empty components and resolve
/// `..` against the preceding component, without touching the filesystem
/// (no symlink resolution). A leading `..` on a relative path is kept; on
/// an absolute path it is dropped at the root. This is what lets a module
/// reached from different directories (`../lib/ast.cato` from `hir/` and
/// `reader/`, `lib/ast.cato` from the root) dedup to one resolved string.
fn normalize_path(p: &str) -> String {
    let absolute = p.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for comp in p.split('/') {
        match comp {
            "" | "." => {}
            ".." => match out.last() {
                Some(&last) if last != ".." => {
                    out.pop();
                }
                _ if !absolute => out.push(".."),
                _ => {}
            },
            c => out.push(c),
        }
    }
    let joined = out.join("/");
    if absolute {
        format!("/{}", joined)
    } else {
        joined
    }
}

/// Resolve a `require` path relative to the requiring file's directory and
/// lexically normalize the result, so the same file reached by different
/// relative spellings dedups to one canonical path string.
fn parent_join(file: &str, rel: &str) -> String {
    normalize_path(
        &std::path::Path::new(file)
            .parent()
            .unwrap_or_else(|| std::path::Path::new(""))
            .join(rel)
            .to_string_lossy(),
    )
}

/// The canonical name of a module member: `prefix__name`, or `name` for the
/// root (empty prefix) and for every type name.
fn prefixed(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{}__{}", prefix, name)
    }
}

/// Rewrite every `alias/name` symbol in `forms` to the canonical member name
/// of the module that `alias` was bound to.
fn rewrite_alias_refs(forms: &mut [(Expr, crate::ast::Span)], aliases: &[(String, String)]) {
    for (form, _) in forms.iter_mut() {
        rewrite_alias_expr(form, aliases);
    }
}

fn rewrite_alias_expr(e: &mut Expr, aliases: &[(String, String)]) {
    match e {
        Expr::LitSym(s) => {
            for (alias, canon) in aliases {
                if let Some(rest) = s.strip_prefix(&format!("{}/", alias)) {
                    *s = prefixed(canon, rest);
                    break;
                }
            }
        }
        Expr::List(items) => {
            for it in items.iter_mut() {
                rewrite_alias_expr(it, aliases);
            }
        }
        Expr::Vector(items) => {
            for it in items.iter_mut() {
                rewrite_alias_expr(it, aliases);
            }
        }
        Expr::MapLit(pairs) => {
            for (k, v) in pairs.iter_mut() {
                rewrite_alias_expr(k, aliases);
                rewrite_alias_expr(v, aliases);
            }
        }
        _ => {}
    }
}

/// Load `file`, pull in its `(require ...)`s recursively, and return the
/// flattened (form, span) list (`ns`/`require` forms dropped). Each required
/// file has one canonical prefix — the alias it was first required with:
/// its own `defn`/`extern` names are renamed `prefix__name` and every
/// requirer's `alias/name` refs are rewritten to that canonical name, so
/// diamonds resolve. `(ns name)` is validated (one per file, names unique
/// across the compilation) and dropped. Types are never renamed.
fn expand_file(
    file: &str,
    src: &str,
    canon_prefix: &str,
    ctx: &mut ModCtx,
) -> Result<Vec<(Expr, crate::ast::Span)>, String> {
    // Register this file's text so its spans render against it, not the
    // entry file, and stamp the id onto every span the reader produced.
    let fid = diagnostics::register_source(file, src);
    let (all_forms, all_spans) = reader::read_all_spanned(src)
        .map_err(|e| {
            let span = crate::ast::Span { file: fid, ..e.span };
            diagnostics::locate(span, &e.msg)
        })?;
    let all_spans: Vec<crate::ast::Span> = all_spans
        .into_iter()
        .map(|s| crate::ast::Span { file: fid, ..s })
        .collect();

    let mut kept: Vec<(Expr, crate::ast::Span)> = Vec::new();
    for (f, s) in all_forms.into_iter().zip(all_spans) {
        if let Expr::List(items) = &f {
            if matches!(items.first(), Some(Expr::LitSym(h)) if h == "ns") {
                match items.get(1) {
                    Some(Expr::LitSym(name)) => {
                        if ctx.ns.contains(name) {
                            return Err(diagnostics::locate(
                                s,
                                &format!("duplicate ns {}", name),
                            ));
                        }
                        ctx.ns.push(name.clone());
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
    let mut this_aliases: Vec<(String, String)> = Vec::new();
    for (f, s) in kept {
        match require_of(&f) {
            RequireForm::Malformed => {
                return Err(diagnostics::locate(s, "require expects [path :as alias]"));
            }
            RequireForm::Ok(path, alias) => {
                let child = parent_join(file, &path);
                let canon = match ctx.canon.get(&child) {
                    Some(c) => c.clone(),
                    None => {
                        // Record before recursing so a cycle terminates.
                        ctx.canon.insert(child.clone(), alias.clone());
                        let child_src = std::fs::read_to_string(&child).map_err(|e| {
                            diagnostics::locate(
                                s,
                                &format!("require: cannot read {}: {}", child, e),
                            )
                        })?;
                        let child_forms = expand_file(&child, &child_src, &alias, ctx)?;
                        merged.extend(child_forms);
                        alias.clone()
                    }
                };
                this_aliases.push((alias, canon));
            }
            RequireForm::NotRequire => own.push((f, s)),
        }
    }

    // Rename this file's own members under its canonical prefix, then point
    // its `alias/name` refs at the canonical members of each required module.
    let names = defn_names(&own);
    for n in &names {
        rewrite_heads(&mut own, n, &prefixed(canon_prefix, n));
    }
    rewrite_alias_refs(&mut own, &this_aliases);
    merged.extend(own);
    Ok(merged)
}
