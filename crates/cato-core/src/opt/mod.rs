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

/// Rebuild every `Let` free list from scratch: clear, then re-plan drops
/// from the ownership states (a binding frees iff it still owns its heap
/// buffer at scope exit). Errors are tolerated here — `verify_ownership`
/// reports them loudly once at the end.
fn recompute_frees(program: &mut Program) {
    let facts = crate::hir::compute_adt_facts(&program.adts);
    let sigs: HashMap<String, Vec<Borrow>> = program
        .defns
        .iter()
        .map(|d| (d.name.clone(), d.borrows.clone()))
        .collect();
    let rets: HashMap<String, Ty> = program
        .defns
        .iter()
        .map(|d| (d.name.clone(), d.ret))
        .collect();
    let ret_borrowed_names: HashSet<String> = program
        .defns
        .iter()
        .filter(|d| d.ret_borrowed)
        .map(|d| d.name.clone())
        .collect();
    for d in program.defns.iter_mut() {
        if d.is_extern {
            continue;
        }
        clear_frees(&mut d.body);
        let _ = crate::hir::plan_drops(
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
            false,
        );
    }
    for lam in program.lambdas.iter_mut() {
        clear_frees(&mut lam.body);
        let name = format!("<lambda {}>", lam.mangled);
        let _ = crate::hir::plan_drops(
            &name,
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
        );
    }
}

fn clear_frees(body: &mut HExpr) {
    let taken = std::mem::replace(body, HExpr::ConstI64(0));
    *body = walk::rewrite(taken, &mut |e| match e {
        HExpr::Let(binds, body, _) => HExpr::Let(binds, body, Vec::new()),
        other => other,
    });
}

/// Safety net: the ownership checker ran on the pre-optimization program;
/// run it again so a pass bug surfaces as a loud compile error instead of
/// a miscompile.
fn verify_ownership(program: &Program) -> Result<(), String> {
    // plan_drops re-checks move semantics while planning; the final
    // post-optimization verification runs it for the errors alone.
    let mut program = Program {
        adts: program.adts.clone(),
        defns: program.defns.clone(),
        lambdas: program.lambdas.clone(),
        fn_sigs: program.fn_sigs.clone(),
    };
    let facts = crate::hir::compute_adt_facts(&program.adts);
    let sigs: HashMap<String, Vec<Borrow>> = program
        .defns
        .iter()
        .map(|d| (d.name.clone(), d.borrows.clone()))
        .collect();
    let rets: HashMap<String, Ty> = program
        .defns
        .iter()
        .map(|d| (d.name.clone(), d.ret))
        .collect();
    let ret_borrowed_names: HashSet<String> = program
        .defns
        .iter()
        .filter(|d| d.ret_borrowed)
        .map(|d| d.name.clone())
        .collect();
    for d in program.defns.iter_mut() {
        if d.is_extern {
            continue;
        }
        crate::hir::plan_drops(
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
            false,
        )?;
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
