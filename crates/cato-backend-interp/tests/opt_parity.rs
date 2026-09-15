//! Optimization differential: with the middle-end on, the interpreter must
//! produce byte-identical output to the interpreter with it off, on the
//! whole examples/ suite plus targeted edge-case programs.
//!
//! This is the semantics oracle for the pass pipeline: folding, DCE,
//! copy-propagation, inlining, devirtualization and clone elision must
//! never change observable behavior.

use cato_core::{compile_named_with, Backend};
use cato_interp::InterpBackend;

fn run_with(src: &str, optimize: bool) -> String {
    let program = compile_named_with("opt_parity.cato", src, optimize).expect("compile");
    InterpBackend::default()
        .emit(&program)
        .expect("interp emit")
}

fn assert_same(src: &str) {
    let before = run_with(src, false);
    let after = run_with(src, true);
    assert_eq!(before, after, "optimization changed program output");
}

const EXAMPLES: &[&str] = &[
    "closures",
    "collection_generics",
    "collections",
    "generic",
    "heap",
    "hamt",
    "maps",
    "normalize",
    "own",
    "parallel",
    "sets",
    "shape",
    "slice",
    "strings",
    "widths",
    "widths2",
    "generics",
];

#[test]
fn optimization_preserves_example_outputs() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.parent().unwrap().parent().unwrap();
    for name in EXAMPLES {
        let path = root.join("examples").join(format!("{}.cato", name));
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
        assert_same(&src);
    }
}

#[test]
fn folding_edge_cases_preserve_output() {
    // Wrapping arithmetic at the i64 boundary and width truncation.
    assert_same(
        r#"(defn main []
  (do (print (+ 9223372036854775807 1))
      (print (* (- 0 9223372036854775807) 3))
      (print (i8 (+ 127 1)))
      (print (u8 (+ 255 1)))
      (print (i32 (* 100000 100000)))))"#,
    );
}

#[test]
fn unfolded_division_still_traps() {
    // Division by zero must stay a runtime operation: the optimizer leaves
    // it alone, and both modes trap identically (the interpreter panics).
    let src = "(defn main [] (print-i64 (/ 1 0)))";
    let trap = |optimize: bool| {
        let program = compile_named_with("div0.cato", src, optimize).expect("compile");
        std::panic::catch_unwind(move || {
            let _ = InterpBackend::default().emit(&program);
        })
        .is_err()
    };
    assert!(trap(false), "interp traps on div-by-zero before opt");
    assert!(trap(true), "interp must still trap after opt");
}

#[test]
fn inlining_preserves_effect_order() {
    // Impure args to an inlinable function must keep their eager
    // left-to-right evaluation at the call site.
    assert_same(
        r#"
(defn add [a :- i64 b :- i64] (+ a b))
(defn main []
  (do (print (add (do (print-i64 1) 10) (do (print-i64 2) 20)))
      (print (add 3 (do (print-i64 4) 40)))))
"#,
    );
}

#[test]
fn devirtualization_preserves_dispatch() {
    assert_same(
        r#"
(deftype Shape
  (Circle [r :- f64])
  (Rect [w :- f64 h :- f64])
  (Tri [a :- f64 b :- f64 c :- f64]))
(defgeneric area [s :- Shape] :- f64)
(defmethod area [(Circle r)] :- f64 (* 3.14159265358979 (* r r)))
(defmethod area [(Rect w h)] :- f64 (* w h))
(defmethod area [s :- Shape] :- f64 0.0)
(defn main []
  (do (print (area (Circle 2.0)))
      (print (area (Rect 3.0 4.0)))
      (print (area (Tri 1.0 1.0 1.0)))))
"#,
    );
}

#[test]
fn clone_elision_preserves_heap_behavior() {
    // Elidable clone (source dead) and non-elidable clone (source live)
    // must both behave identically.
    assert_same(
        r#"
(defn main []
  (let [a (conj (vec-i64 1) 2)
        b (clone a)]
    (do (print (get b 0))
        (print (count a)))))
"#,
    );
    assert_same(
        r#"
(defn main []
  (let [a (conj (vec-i64 1) 2)
        b (clone a)]
    (do (print (get b 1))
        (print (get a 0)))))
"#,
    );
}

#[test]
fn copy_prop_respects_heap_bindings_and_shadowing() {
    // Heap bindings must not be copy-propagated; a shadowed name must not
    // be substituted under its shadow.
    assert_same(
        r#"
(defn main []
  (let [v (conj (vec-i64 1) 2)
        x (count v)
        x (+ x 100)
        y x]
    (do (print x) (print y) (print (count v)))))
"#,
    );
}
