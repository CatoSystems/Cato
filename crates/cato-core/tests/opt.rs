//! Middle-end pass tests: each pass is asserted structurally on the HIR,
//! comparing the optimized program against the unoptimized one.
//!
//! Note: the `print` multimethod's hidden methods (`print__m0`, ...) have
//! small scalar-typed bodies, so they legitimately inline away; assertions
//! therefore look for folded values and call targets rather than exact
//! shapes. Semantic equivalence (opt-off vs opt-on) is covered end-to-end
//! by the interpreter differential suite in cato-backend-interp and by the
//! LLVM integration tests, which run with optimization on by default.

use cato_core::hir::{BinOp, HExpr};
use cato_core::{compile_named_with, Program};

fn compile_opt(src: &str, optimize: bool) -> Program {
    compile_named_with("opt_test.cato", src, optimize).expect("compile")
}

fn main_body(p: &Program) -> &HExpr {
    &p.defns.iter().find(|d| d.name == "main").unwrap().body
}

fn consts_in(e: &HExpr) -> Vec<i64> {
    let mut out = Vec::new();
    cato_core::opt::walk::visit_nodes(e, &mut |n| {
        if let HExpr::ConstI64(v) = n {
            out.push(*v);
        }
    });
    out
}

fn calls_in(e: &HExpr) -> Vec<String> {
    let mut out = Vec::new();
    cato_core::opt::walk::visit_nodes(e, &mut |n| {
        if let HExpr::Call(name, _) = n {
            out.push(name.clone());
        }
    });
    out
}

fn clones_in(e: &HExpr) -> usize {
    let mut count = 0;
    cato_core::opt::walk::visit_nodes(e, &mut |n| {
        if matches!(n, HExpr::Clone(_)) {
            count += 1;
        }
    });
    count
}

// ---------------------------------------------------------------------------
// M1: constant folding
// ---------------------------------------------------------------------------

#[test]
fn folds_arithmetic_to_constants() {
    let p = compile_opt("(defn main [] (print (+ 1 2)))", true);
    assert_eq!(consts_in(main_body(&p)), vec![3]);
    assert!(
        !calls_in(main_body(&p))
            .iter()
            .any(|c| c != "print-i64" && c != "print__m0"),
        "no unexpected calls left: {:?}",
        calls_in(main_body(&p))
    );
}

#[test]
fn folds_comparisons_and_if_branches() {
    let p = compile_opt("(defn main [] (print (if (< 1 2) 10 20)))", true);
    assert_eq!(consts_in(main_body(&p)), vec![10]);
}

#[test]
fn folds_not_condition_by_swapping_branches() {
    let p = compile_opt("(defn main [] (print (if (not (= 1 1)) 10 20)))", true);
    assert_eq!(consts_in(main_body(&p)), vec![20]);
}

#[test]
fn wraps_integer_overflow_like_the_backends() {
    let p = compile_opt("(defn main [] (print (+ 9223372036854775807 1)))", true);
    assert_eq!(consts_in(main_body(&p)), vec![i64::MIN]);
}

#[test]
fn does_not_fold_division_by_zero() {
    let p = compile_opt("(defn main [] (print-i64 (/ 1 0)))", true);
    let mut found_div = false;
    cato_core::opt::walk::visit_nodes(main_body(&p), &mut |n| {
        if matches!(n, HExpr::Bin(BinOp::Div, _, _)) {
            found_div = true;
        }
    });
    assert!(found_div, "(/ 1 0) must not be folded");
}

#[test]
fn folds_string_concatenation() {
    let p = compile_opt("(defn main [] (print (str \"a\" \"b\" \"c\")))", true);
    let mut lits: Vec<String> = Vec::new();
    cato_core::opt::walk::visit_nodes(main_body(&p), &mut |n| {
        if let HExpr::StrLit(s) = n {
            lits.push(s.clone());
        }
    });
    assert_eq!(lits, vec!["abc".to_string()]);
}

#[test]
fn folds_vector_element_access_and_count() {
    let p = compile_opt(
        "(defn main [] (do (print (get [10 20 30] 1)) (print (count [4 5]))))",
        true,
    );
    assert_eq!(consts_in(main_body(&p)), vec![20, 2]);
}

// ---------------------------------------------------------------------------
// M1: DCE + copy propagation
// ---------------------------------------------------------------------------

#[test]
fn drops_unused_pure_bindings_and_copies_single_uses() {
    // x is unused (dropped); y is used once (copied in, binder removed).
    let p = compile_opt("(defn main [] (let [x (+ 1 2) y 5] (print y)))", true);
    assert_eq!(consts_in(main_body(&p)), vec![5]);
    let mut lets = 0;
    cato_core::opt::walk::visit_nodes(main_body(&p), &mut |n| {
        if matches!(n, HExpr::Let(..)) {
            lets += 1;
        }
    });
    assert_eq!(lets, 0, "both bindings should be gone");
}

#[test]
fn keeps_bindings_with_effects() {
    // print-i64 has effects: its result binding must survive.
    let p = compile_opt("(defn main [] (let [u (print-i64 7)] 3))", true);
    let mut lets = 0;
    cato_core::opt::walk::visit_nodes(main_body(&p), &mut |n| {
        if matches!(n, HExpr::Let(..)) {
            lets += 1;
        }
    });
    assert_eq!(lets, 1, "effectful binding must be kept");
}

// ---------------------------------------------------------------------------
// M2: inlining
// ---------------------------------------------------------------------------

#[test]
fn inlines_small_functions_and_folds_through() {
    let src = r#"
(defn sq [x :- i64] (* x x))
(defn main [] (print (sq 7)))
"#;
    let p = compile_opt(src, true);
    assert_eq!(consts_in(main_body(&p)), vec![49]);
}

#[test]
fn does_not_inline_recursion() {
    let src = r#"
(defn count-down [n :- i64] (if (< n 1) 0 (count-down (- n 1))))
(defn main [] (print-i64 (count-down 3)))
"#;
    let p = compile_opt(src, true);
    let loop_fn = p.defns.iter().find(|d| d.name == "count-down").unwrap();
    assert!(
        cato_core::opt::walk::calls_name(&loop_fn.body, "count-down"),
        "recursive callee must not be inlined away"
    );
}

// ---------------------------------------------------------------------------
// M3: devirtualization
// ---------------------------------------------------------------------------

#[test]
fn devirtualizes_dispatch_on_fresh_constructors() {
    let src = r#"
(deftype Shape
  (Circle [r :- f64])
  (Rect [w :- f64 h :- f64]))
(defgeneric area [s :- Shape] :- i64)
(defmethod area [(Circle r)] :- i64 (* 2 3))
(defmethod area [(Rect w h)] :- i64 (* 4 5))
(defn main [] (print (area (Circle 1.0))))
"#;
    let unopt = compile_opt(src, false);
    let opt = compile_opt(src, true);

    let unopt_calls = calls_in(main_body(&unopt));
    let opt_calls = calls_in(main_body(&opt));
    let dispatch: Vec<&String> = unopt_calls.iter().filter(|c| c.starts_with("area__Shape")).collect();
    assert_eq!(dispatch.len(), 1, "unoptimized main goes through the dispatcher");
    let disp_name = dispatch[0].clone();

    assert!(
        !opt_calls.contains(&disp_name),
        "optimized main must not call the dispatcher: {:?}",
        opt_calls
    );
    // The chosen method for the `circle` arm, per the generated tree.
    let disp = unopt
        .defns
        .iter()
        .find(|d| d.name == disp_name)
        .expect("dispatch defn exists");
    let circle_method = match &disp.body {
        HExpr::Match { arms, .. } => match &arms[0].body {
            HExpr::Call(n, _) => n.clone(),
            other => panic!("unexpected dispatch arm {:?}", other),
        },
        other => panic!("unexpected dispatch body {:?}", other),
    };
    assert!(
        opt_calls.contains(&circle_method),
        "optimized main calls the variant's method directly: {:?}",
        opt_calls
    );
    // The circle method body folds to its constant result.
    let method = opt.defns.iter().find(|d| d.name == circle_method).unwrap();
    assert_eq!(consts_in(&method.body), vec![6]);
}

// ---------------------------------------------------------------------------
// M3: clone elision
// ---------------------------------------------------------------------------

#[test]
fn elides_clone_of_dead_binding() {
    // v is consumed only by the clone; elision transfers the buffer to w.
    let src = r#"
(defn main []
  (let [v (conj (vec-i64 1) 2)
        w (clone v)]
    (print (get w 0))))
"#;
    let p = compile_opt(src, true);
    assert_eq!(clones_in(main_body(&p)), 0, "clone of a dead binding should be elided");
}

#[test]
fn keeps_clone_of_live_binding() {
    // v is used after the clone: the copy is required.
    let src = r#"
(defn main []
  (let [v (conj (vec-i64 1) 2)
        w (clone v)]
    (do (print (get w 0)) (print (get v 1)))))
"#;
    let p = compile_opt(src, true);
    assert_eq!(clones_in(main_body(&p)), 1, "clone of a live binding must stay");
}

// ---------------------------------------------------------------------------
// Whole-pipeline sanity
// ---------------------------------------------------------------------------

#[test]
fn recompute_keeps_free_lists_consistent() {
    // After optimization, every name in a free list is still a binding in
    // that let (no dangling frees).
    let src = r#"
(defn helper [n :- i64] (let [v (conj (vec-i64 1 2) (+ n n))] (get v 0)))
(defn main [] (print-i64 (helper 9)))
"#;
    let p = compile_opt(src, true);
    for d in &p.defns {
        check_no_dangling_frees(&d.body);
    }
}

fn check_no_dangling_frees(e: &HExpr) {
    use std::collections::HashSet;
    cato_core::opt::walk::visit_nodes(e, &mut |n| match n {
        HExpr::Let(binds, _, frees) => {
            let bound: HashSet<&String> = binds.iter().map(|(n, _, _)| n).collect();
            for f in frees {
                assert!(bound.contains(f), "dangling free entry {}", f);
            }
        }
        _ => {}
    });
}
