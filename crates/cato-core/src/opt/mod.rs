//! The middle end: HIR-to-HIR optimization over a built `Program`.
//!
//! Runs between `hir::build_program_located` and `Backend::emit`, so every
//! backend (LLVM, interpreter, ...) inherits the improvements. The passes
//! are semantics-preserving by construction and the whole examples/ parity
//! suite (exact output, both backends, interpreter-vs-LLVM differential)
//! is the regression oracle.
//!
//! Pipeline: fold → simplify (DCE + copy-prop) → devirtualize → inline,
//! repeated to a fixed point (bounded), then free-lists are recomputed and
//! clone elision runs as the final, ownership-sensitive pass.

pub mod clone_elide;
pub mod devirt;
pub mod fold;
pub mod inline;
pub mod simplify;
pub mod walk;

use crate::hir::*;
use crate::Program;
use std::collections::{HashMap, HashSet};

/// Maximum fixed-point rounds; converges well before this on real code.
const MAX_ROUNDS: usize = 4;

/// Optimize `program` in place. Never fails on valid input: the trailing
/// ownership verification only fires if a pass broke an invariant, which is
/// a compiler bug, not a user error.
pub fn optimize(program: &mut Program) -> Result<(), String> {
    for _ in 0..MAX_ROUNDS {
        let mut changed = false;
        changed |= fold::run(program);
        changed |= simplify::run(program);
        changed |= devirt::run(program);
        changed |= inline::run(program);
        recompute_frees(program);
        if !changed {
            break;
        }
    }
    // Clone elision reads the fresh free lists and must not be followed by
    // re-annotation: it transfers ownership from the elided source to the
    // clone's destination, and re-annotating would free both.
    clone_elide::run(program);
    verify_ownership(program)
}

/// Rebuild every `Let` free list from scratch: clear, then re-run the
/// frontend's conservative annotation (leaks are possible, double-frees
/// are not). Also drops dangling entries left behind by removed bindings.
fn recompute_frees(program: &mut Program) {
    let facts = crate::hir::compute_adt_facts(&program.adts);
    for d in program.defns.iter_mut() {
        if d.is_extern {
            continue;
        }
        clear_and_annotate(&mut d.body, &facts);
    }
    for lam in program.lambdas.iter_mut() {
        clear_and_annotate(&mut lam.body, &facts);
    }
}

fn clear_and_annotate(body: &mut HExpr, facts: &crate::hir::AdtFacts) {
    let taken = std::mem::replace(body, HExpr::ConstI64(0));
    *body = walk::rewrite(taken, &mut |e| match e {
        HExpr::Let(binds, body, _) => HExpr::Let(binds, body, Vec::new()),
        other => other,
    });
    crate::hir::annotate_frees(body, facts);
}

/// Safety net: the ownership checker ran on the pre-optimization program;
/// run it again so a pass bug surfaces as a loud compile error instead of
/// a miscompile.
fn verify_ownership(program: &Program) -> Result<(), String> {
    let sigs: HashMap<String, Vec<Borrow>> = program
        .defns
        .iter()
        .map(|d| (d.name.clone(), d.borrows.clone()))
        .collect();
    for d in &program.defns {
        if d.is_extern {
            continue;
        }
        crate::hir::check_ownership(d, &sigs)?;
    }
    Ok(())
}

/// Set of all binder names in the program (test helper, kept public for
/// the inline pass's fresh-name minting).
#[allow(dead_code)]
pub fn binder_universe(program: &Program) -> HashSet<String> {
    let mut out = HashSet::new();
    for d in &program.defns {
        walk::all_binders(&d.body, &mut out);
    }
    for lam in &program.lambdas {
        walk::all_binders(&lam.body, &mut out);
    }
    out
}
