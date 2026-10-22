//! Multimethod devirtualization.
//!
//! `defgeneric` dispatch compiles to compiler-generated dispatch defns
//! holding nested matches over ADT components (see `hir::generics`). When a
//! call to such a defn passes a freshly constructed variant, the chosen
//! method is statically known: the call is rewritten to the method directly.
//! Only the single-ADT-position dispatch shape is handled — nested dispatch
//! trees are left to the generated dispatcher.

use super::walk::rewrite;
use crate::Program;
use crate::hir::*;
use std::cell::Cell;
use std::collections::HashMap;

/// variant tag -> hidden method name, for a dispatch defn.
type DispatchTable = HashMap<u32, String>;

pub fn run(program: &mut Program) -> bool {
    let mut tables: HashMap<String, DispatchTable> = HashMap::new();
    for d in &program.defns {
        if let Some(t) = dispatch_shape(d) {
            tables.insert(d.name.clone(), t);
        }
    }
    if tables.is_empty() {
        return false;
    }

    let changed = Cell::new(false);
    for d in program.defns.iter_mut() {
        if d.is_extern {
            continue;
        }
        let taken = std::mem::replace(&mut d.body, HExpr::ConstI64(0));
        d.body = rewrite(taken, &mut |e| devirt_one(e, &tables, &changed));
    }
    for lam in program.lambdas.iter_mut() {
        let taken = std::mem::replace(&mut lam.body, HExpr::ConstI64(0));
        lam.body = rewrite(taken, &mut |e| devirt_one(e, &tables, &changed));
    }
    changed.get()
}

fn devirt_one(e: HExpr, tables: &HashMap<String, DispatchTable>, changed: &Cell<bool>) -> HExpr {
    let HExpr::Call(name, args) = e else {
        return e;
    };
    let Some(table) = tables.get(&name) else {
        return HExpr::Call(name, args);
    };
    // Exactly one freshly constructed ADT argument: single-level dispatch.
    let ctor_positions: Vec<usize> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| matches!(a, HExpr::Ctor { .. }))
        .map(|(i, _)| i)
        .collect();
    if ctor_positions.len() != 1 {
        return HExpr::Call(name, args);
    }
    let HExpr::Ctor { variant, .. } = &args[ctor_positions[0]] else {
        unreachable!()
    };
    match table.get(variant) {
        Some(method) => {
            changed.set(true);
            // Args pass through unchanged: the dispatcher's leaf calls the
            // method with the same argument registers.
            HExpr::Call(method.clone(), args)
        }
        None => HExpr::Call(name, args),
    }
}

/// Recognize the generated single-level dispatch body: a `Match` over one
/// of the defn's params whose arms all call their method with the dispatch
/// params, in order (the shape `generics::build_tree` emits).
fn dispatch_shape(d: &Defn) -> Option<DispatchTable> {
    let HExpr::Match { scrut, arms, .. } = &d.body else {
        return None;
    };
    let HExpr::Var(p) = &**scrut else {
        return None;
    };
    if !d.params.iter().any(|(n, _)| n == p) {
        return None;
    }
    if arms.is_empty() {
        return None;
    }
    let param_exprs: Vec<HExpr> = d
        .params
        .iter()
        .map(|(n, _)| HExpr::Var(n.clone()))
        .collect();
    let mut table = DispatchTable::new();
    for arm in arms {
        arm.variant?;
        let HExpr::Call(method, cargs) = &arm.body else {
            return None;
        };
        if cargs != &param_exprs {
            return None;
        }
        table.insert(arm.variant.unwrap(), method.clone());
    }
    Some(table)
}
