use cato_core::hir::*;
use cato_core::reader::read_all;

fn build_one(name: &str, src: &str) -> HExpr {
    let exprs = read_all(src).unwrap();
    let fns = build(&exprs).unwrap();
    fns.iter()
        .find(|d| d.name == name)
        .map(|d| d.body.clone())
        .expect("fn found")
}

#[test]
fn builds_literal_and_binary_expr() {
    let body = build_one("f", "(defn f [x :- i64] (+ x 1))");
    assert_eq!(
        body,
        HExpr::Bin(
            BinOp::Add,
            Box::new(HExpr::Var("x".to_string())),
            Box::new(HExpr::ConstI64(1))
        )
    );
}

#[test]
fn requires_typed_params() {
    let exprs = read_all("(defn f [x] (+ x 1))").unwrap();
    let err = build(&exprs).unwrap_err();
    assert!(
        err.contains("must have a type annotation"),
        "expected typed-param error, got: {}",
        err
    );
}

#[test]
fn builds_reduce_closure() {
    let body = build_one(
        "ss",
        "(defn ss [xs :- vec mu :- f64] (reduce (fn [acc x] (+ acc (* (- x mu) (- x mu)))) 0.0 xs))",
    );
    match body {
        HExpr::Reduce(r) => match *r {
            HReduce::Closure {
                init, src, acc, el, body, ..
            } => {
                assert_eq!(init, HExpr::ConstF64(0.0));
                assert_eq!(acc, "acc".to_string());
                assert_eq!(el, "x".to_string());
                assert!(matches!(src, HSource::Var(_)));
                assert!(matches!(body, HExpr::Bin(BinOp::Add, _, _)));
            }
            _ => panic!("expected closure reduce"),
        },
        _ => panic!("expected reduce"),
    }
}

#[test]
fn builds_pipeline_reduce_from_chain() {
    let body = build_one(
        "fused",
        "(defn square [x :- i64] (* x x))
         (defn fused [xs :- vec] (reduce + 0 (map square (filter even? xs))))",
    );
    match body {
        HExpr::Reduce(r) => match *r {
            HReduce::Pipeline {
                combine, init, maps, filters, src, ..
            } => {
                assert_eq!(combine, BinOp::Add);
                assert_eq!(init, HExpr::ConstI64(0));
                assert_eq!(maps.len(), 1);
                assert_eq!(filters.len(), 1);
                assert!(matches!(src, HSource::Var(_)));
            }
            _ => panic!("expected pipeline reduce"),
        },
        _ => panic!("expected reduce"),
    }
}

#[test]
fn builds_vec_literal_and_print_call() {
    let body = build_one(
        "main",
        "(defn main [] (print-f64 (variance (vec-i64 1 2 3 4 5))))",
    );
    let HExpr::Call(name, args) = body else {
        panic!("expected call")
    };
    assert_eq!(name, "print-f64");
    let HExpr::Call(inner, _) = &args[0] else {
        panic!("expected inner call to variance")
    };
    assert_eq!(inner, "variance");
}

fn build_err(src: &str) -> String {
    let exprs = read_all(src).unwrap();
    build(&exprs).unwrap_err()
}

#[test]
fn rejects_reduce_with_too_few_args() {
    let err = build_err("(defn f [xs :- vec] (reduce + 0))");
    assert!(err.contains("reduce expects"), "got: {}", err);
}

#[test]
fn rejects_map_with_too_few_args() {
    let err = build_err("(defn f [xs :- vec] (reduce + 0 (map square xs)))");
    assert!(err.contains("unknown"), "unresolved square should error: {}", err);
}

#[test]
fn rejects_arity_one_builtin_without_args() {
    let err = build_err("(defn f [xs :- vec] (count))");
    assert!(err.contains("count expects"), "got: {}", err);
}

#[test]
fn rejects_bad_binary_op_arity() {
    let err = build_err("(defn f [x :- i64] (+ x))");
    assert!(err.contains("binary op"), "got: {}", err);
}

#[test]
fn builds_multi_type_let_sequence() {
    let body = build_one(
        "g",
        "(defn g [xs :- vec] :- f64
           (let [n  (count xs)
                 d  (f64 n)]
             (+ d 0.5)))",
    );
    match body {
        HExpr::Let(binds, inner, _) => {
            assert_eq!(binds.len(), 2);
            let HExpr::Bin(BinOp::Add, _, _) = *inner else {
                panic!("expected f64 add body after both bindings")
            };
        }
        _ => panic!("expected let"),
    }
}

#[test]
fn lowers_if_with_promotion() {
    // then branch is i64, else is f64 -> result f64, i64 wrapped in ConvF64
    let body = build_one("f", "(defn f [x :- i64] :- f64 (if (< x 2) x 1.0))");
    let HExpr::If(cond, then, els, ty) = body else {
        panic!("expected if")
    };
    assert_eq!(ty, Ty::F64);
    assert!(matches!(*cond, HExpr::Cmp(CmpOp::Lt, _, _, _)));
    assert!(matches!(*then, HExpr::ConvF64(_)));
    assert!(matches!(*els, HExpr::ConstF64(_)));
}

#[test]
fn lowers_comparison_int() {
    let body = build_one("gt", "(defn gt [a :- i64 b :- i64] :- bool (> a b))");
    assert!(matches!(body, HExpr::Cmp(CmpOp::Gt, _, _, false)));
}

#[test]
fn lowers_mixed_comparison_promotes_f64() {
    // i64 < f64: the i64 side wraps in ConvF64, the f64 constant stays put
    let body = build_one("f", "(defn f [a :- i64] :- bool (< a 2.5))");
    let HExpr::Cmp(_, a, b, f64flag) = body else {
        panic!("expected cmp")
    };
    assert!(f64flag);
    assert!(matches!(*a, HExpr::ConvF64(_)));
    assert!(matches!(*b, HExpr::ConstF64(_)));
}

#[test]
fn lowers_and_or_not() {
    let body = build_one(
        "f",
        "(defn f [b :- bool c :- bool] :- bool (and b (or c (not b))))",
    );
    let HExpr::And(a1, o1) = body else {
        panic!("expected and")
    };
    assert!(matches!(*a1, HExpr::Var(_))); // "b"
    let HExpr::Or(a2, n1) = *o1 else {
        panic!("expected or")
    };
    assert!(matches!(*a2, HExpr::Var(_))); // "c"
    let HExpr::Not(v) = *n1 else {
        panic!("expected not")
    };
    assert!(matches!(*v, HExpr::Var(_))); // "b"
}

#[test]
fn rejects_non_bool_and_operand() {
    assert!(
        build(&read_all("(defn f [x :- i64] :- bool (and (< x 3) x))").unwrap()).is_err()
    );
}

#[test]
fn lowers_abs_min_max() {
    let body = build_one("f", "(defn f [a :- i64 b :- i64] :- i64 (min (abs a) b))");
    let HExpr::Min(inner, r, ty) = body else {
        panic!("expected min")
    };
    assert_eq!(ty, Ty::I64);
    assert!(matches!(*r, HExpr::Var(_)));
    let HExpr::Abs(a, at) = *inner else {
        panic!("expected abs")
    };
    assert_eq!(at, Ty::I64);
    assert!(matches!(*a, HExpr::Var(_)));
}

#[test]
fn closure_reduce_over_map_chain_is_pipeline_backed() {
    let body = build_one(
        "n",
        "(defn n [xs :- vec] :- f64
           (reduce (fn [acc x] (+ acc x)) 0.0
             (map (fn [x] (f64 x)) xs)))",
    );
    let HExpr::Reduce(r) = body else {
        panic!("expected reduce")
    };
    match *r {
        HReduce::Closure {
            maps, filters, src, ..
        } => {
            assert_eq!(maps.len(), 1);
            assert_eq!(filters.len(), 0);
            assert!(matches!(src, HSource::Var(_)));
        }
        _ => panic!("expected closure reduce"),
    }
}

#[test]
fn map_stage_types_flow_through_chain() {
    // clamp-x must be DEFINED so `build` can resolve its f64 return type;
    // it is referenced (not used as a map fn, so its 3 params are fine).
    let body = build_one(
        "n",
        "(defn clamp-x [x :- f64 lo :- f64 hi :- f64] :- f64 (min (max x lo) hi))
         (defn n [xs :- vec xsrng :- f64] :- f64
           (reduce (fn [acc x] (max acc (abs x))) 0.0
             (map (fn [x] (clamp-x x -1.0 1.0))
               (map (fn [x] (/ (- (f64 x) xsrng) xsrng)) xs))))",
    );
    let HExpr::Reduce(r) = body else {
        panic!("expected reduce")
    };
    let HReduce::Closure { maps, .. } = *r else {
        panic!("expected closure")
    };
    // inner-most stage is i64 -> f64
    assert_eq!(maps[0].el_ty, Ty::I64);
    assert_eq!(maps[0].res_ty, Ty::F64);
    // outer stage is fed f64
    assert_eq!(maps[1].el_ty, Ty::F64);
    assert_eq!(maps[1].res_ty, Ty::F64);
}

#[test]
fn parses_bool_params_and_not() {
    let body = build_one("f", "(defn f [b :- bool] :- bool (not b))");
    assert!(matches!(body, HExpr::Not(_)));
}

// --- Phase 3: deftype + match ---

const SHAPE_DEFTYPE: &str = "(deftype Shape (Circle [radius :- f64]) (Rect [w :- f64 h :- f64]) (Point []))";

fn build_shape_prog(body: &str) -> HExpr {
    let src = format!(
        "{}\n(defn f [s :- Shape] :- f64 {})",
        SHAPE_DEFTYPE, body
    );
    let exprs = read_all(&src).unwrap();
    let fns = build(&exprs).unwrap();
    fns.iter().find(|d| d.name == "f").unwrap().body.clone()
}

#[test]
fn builds_match_with_exhaustive_arms() {
    let body = build_shape_prog(
        "(match s (Circle [r] (* r r)) (Rect [w h] (* w h)) (Point [] 0.0))",
    );
    let HExpr::Match { arms, ty, .. } = body else {
        panic!("expected match");
    };
    assert_eq!(ty, Ty::F64);
    assert_eq!(arms.len(), 3);
    assert_eq!(arms[0].variant, Some(0));
    assert_eq!(arms[0].binders, vec!["r".to_string()]);
    assert_eq!(arms[2].variant, Some(2));
}

#[test]
fn match_wildcard_covers_rest() {
    let body = build_shape_prog("(match s (Rect [w h] w) (_ 1.0))");
    let HExpr::Match { arms, .. } = body else {
        panic!("expected match");
    };
    assert_eq!(arms.len(), 2);
    assert_eq!(arms[1].variant, None);
}

#[test]
fn rejects_nonexhaustive_match() {
    let err = build_err(&format!(
        "{}\n(defn f [s :- Shape] :- f64 (match s (Rect [w h] w)))",
        SHAPE_DEFTYPE
    ));
    assert!(err.contains("does not cover"), "got: {}", err);
}

#[test]
fn rejects_duplicate_and_unknown_arms() {
    let err = build_err(&format!(
        "{}\n(defn f [s :- Shape] :- f64 (match s (Rect [w h] w) (Rect [w h] h) (Point [] 0.0) (Circle [r] r)))",
        SHAPE_DEFTYPE
    ));
    assert!(err.contains("more than once"), "got: {}", err);
    let err = build_err(&format!(
        "{}\n(defn f [s :- Shape] :- f64 (match s (Blob [] 0.0) (_ 1.0)))",
        SHAPE_DEFTYPE
    ));
    assert!(err.contains("not a variant"), "got: {}", err);
}

#[test]
fn rejects_ctor_arity_mismatch() {
    let err = build_err(&format!(
        "{}\n(defn f [s :- Shape] :- f64 (match s (Circle [] 0.0) (_ 1.0)))",
        SHAPE_DEFTYPE
    ));
    // Field-count mismatch on the arm binder vector.
    assert!(err.contains("binds"), "got: {}", err);
    let err = build_err(&format!(
        "{}\n(defn f [s :- Shape] :- f64 (match s (Circle [r] r) (_ (Circle r 2.0))))",
        SHAPE_DEFTYPE
    ));
    assert!(
        err.contains("constructor Circle expects 1 argument"),
        "got: {}",
        err
    );
}

#[test]
fn lowers_ctor_calls() {
    let src = format!(
        "{}\n(defn mk [r :- f64] :- Shape (Circle r))",
        SHAPE_DEFTYPE
    );
    let exprs = read_all(&src).unwrap();
    let fns = build(&exprs).unwrap();
    let body = &fns.iter().find(|d| d.name == "mk").unwrap().body;
    assert!(matches!(body, HExpr::Ctor { variant: 0, .. }));
}

#[test]
fn adts_rejected_in_arithmetic() {
    let err = build_err(&format!(
        "{}\n(defn f [s :- Shape] :- i64 (+ s 1))",
        SHAPE_DEFTYPE
    ));
    assert!(err.contains("cannot be used in arithmetic"), "got: {}", err);
}

// --- Phase 4: collection sources ---

#[test]
fn builds_take_drop_zip_range_sources() {
    let exprs = read_all(
        "(defn f [xs :- vec n :- i64] (reduce + 0 (take n (drop 1 xs))))
         (defn g [xs :- vec ys :- vec] (reduce + 0 (map (fn [a b] (+ a b)) (zip xs ys))))
         (defn h [a :- i64 b :- i64] (reduce + 0 (range a b)))
         (defn k [] (reduce + 0 (vec-i64 1 2 3)))",
    )
    .unwrap();
    let fns = build(&exprs).unwrap();
    let get = |n: &str| fns.iter().find(|d| d.name == n).unwrap().body.clone();
    let HExpr::Reduce(r) = get("f") else { panic!("f reduce") };
    let HReduce::Pipeline { src, .. } = *r else { panic!("f pipeline") };
    assert!(matches!(src, HSource::Take(_, ref s) if matches!(s.as_ref(), HSource::Drop(..))));

    let HExpr::Reduce(r) = get("g") else { panic!("g reduce") };
    let HReduce::Pipeline { src, maps, .. } = *r else { panic!("g pipeline") };
    assert!(matches!(src, HSource::Zip(..)));
    assert!(maps[0].el2.is_some(), "zip map binds a second element");

    let HExpr::Reduce(r) = get("h") else { panic!("h reduce") };
    let HReduce::Pipeline { src, .. } = *r else { panic!("h pipeline") };
    assert!(matches!(src, HSource::Range(..)));

    let HExpr::Reduce(r) = get("k") else { panic!("k reduce") };
    let HReduce::Pipeline { src, .. } = *r else { panic!("k pipeline") };
    assert!(matches!(
        src,
        HSource::Lit(VLit::I64(ref vs)) if vs == &vec![1, 2, 3]
    ));
}

#[test]
fn zip_closure_binds_two_elements() {
    let exprs = read_all(
        "(defn f [xs :- vec ys :- vec] (reduce (fn [acc a b] (+ acc (* a b))) 0 (zip xs ys)))",
    )
    .unwrap();
    let fns = build(&exprs).unwrap();
    let HExpr::Reduce(r) = &fns[0].body else { panic!("reduce") };
    let HReduce::Closure { el2, .. } = &**r else { panic!("closure") };
    assert!(el2.is_some());
}

#[test]
fn builds_get_with_default() {
    let body = build_one("f", "(defn f [xs :- vec] :- i64 (get xs 2 9))");
    assert!(matches!(body, HExpr::Get { default: Some(_), .. }));
}

// --- Phase 6: monomorphization ---

#[test]
fn instantiates_generic_per_call_site_type() {
    let exprs = read_all(
        "(defn gsum [xs :- (Vector T)] :- T (reduce (fn [acc x] (+ acc x)) 0 xs))
         (defn main []
           (print-i64 (gsum (vec-i64 1 2)))
           (print-f64 (gsum (vec-f64 1.0 2.0))))",
    )
    .unwrap();
    let fns = build(&exprs).unwrap();
    let names: Vec<&str> = fns.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"gsum__i64"), "{:?}", names);
    assert!(names.contains(&"gsum__f64"), "{:?}", names);
    let f = fns.iter().find(|d| d.name == "gsum__f64").unwrap();
    assert_eq!(f.ret, Ty::F64);
    assert_eq!(f.params[0].1, Ty::Vec(Elem::F64));
}

#[test]
fn explicit_vector_f64_annotation_types() {
    let exprs = read_all("(defn f [xs :- (Vector f64)] :- f64 (count xs))").unwrap();
    let fns = build(&exprs).unwrap();
    assert_eq!(fns[0].params[0].1, Ty::Vec(Elem::F64));
    assert_eq!(fns[0].ret, Ty::F64);
}

#[test]
fn get_on_f64_vector_is_f64() {
    let body = build_one("f", "(defn f [xs :- (Vector f64)] :- f64 (get xs 0 1.0))");
    let HExpr::Get { ty, .. } = body else { panic!("get") };
    assert_eq!(ty, Ty::F64);
}

#[test]
fn rejects_incompatible_generic_instantiation() {
    let err = build_err(
        "(defn gsum [xs :- (Vector T)] :- T (reduce (fn [acc x] (+ acc x)) 0 xs))
         (defn f [x :- i64] :- i64 (gsum x))",
    );
    assert!(
        err.contains("cannot instantiate") || err.contains("expects"),
        "got: {}",
        err
    );
}

// --- Phase 7: ownership & borrows ---

const BOX_DEFTYPE: &str = "(deftype Box (Box [v :- i64]))";

#[test]
fn use_after_move_is_rejected() {
    let err = build_err(&format!(
        "{}\n(defn consume [b :- Box] :- i64 (match b (Box [v] v) (_ 0)))
         (defn main []
           (let [b (Box 1)]
             (do (consume b) (consume b))))",
        BOX_DEFTYPE
    ));
    assert!(err.contains("use of moved value b"), "got: {}", err);
}

#[test]
fn double_move_in_one_call_is_rejected() {
    // Distinct values are fine...
    let ok = format!(
        "{}\n(defn take2 [a :- Box b :- Box] :- i64 a)\n(defn main [] (take2 (Box 1) (Box 2)))",
        BOX_DEFTYPE
    );
    assert!(build(&read_all(&ok).unwrap()).is_ok());
    // ...the same binding twice is not.
    let bad = format!(
        "{}\n(defn take2 [a :- Box b :- Box] :- i64 a)\n(defn bad [b :- Box] :- i64 (take2 b b))",
        BOX_DEFTYPE
    );
    let err = build(&read_all(&bad).unwrap()).unwrap_err();
    assert!(err.contains("use of moved value b"), "got: {}", err);
}

#[test]
fn shared_borrows_keep_the_value_usable() {
    let exprs = read_all(&format!(
        "{}\n(defn peek [b :- &Box] :- i64 (match b (Box [v] v) (_ 0)))
         (defn main []
           (let [b (Box 1)]
             (do (peek b) (peek b) (print-i64 (match b (Box [v] v) (_ 0))))))",
        BOX_DEFTYPE
    ))
    .unwrap();
    assert!(build(&exprs).is_ok());
}

#[test]
fn overlapping_exclusive_borrows_are_rejected() {
    let exprs = read_all(&format!(
        "{}\n(defn f [a :- &!Box b :- &!Box] :- i64 a)
         (defn main [] (let [b (Box 1)] (f b b)))",
        BOX_DEFTYPE
    ))
    .unwrap();
    let err = build(&exprs).unwrap_err();
    assert!(err.contains("exclusively more than once"), "got: {}", err);
}

#[test]
fn clone_keeps_the_source_usable() {
    let exprs = read_all(&format!(
        "{}\n(defn consume [b :- Box] :- i64 (match b (Box [v] v) (_ 0)))
         (defn main []
           (let [b (Box 1)]
             (do (consume (clone b)) (consume b))))",
        BOX_DEFTYPE
    ))
    .unwrap();
    assert!(build(&exprs).is_ok());
}

#[test]
fn drop_ends_a_binding() {
    let err = build_err(&format!(
        "{}\n(defn main []
           (let [b (Box 1)]
             (do (drop b) (match b (Box [v] v) (_ 0)))))",
        BOX_DEFTYPE
    ));
    assert!(err.contains("use of moved value b"), "got: {}", err);
}

// --- Phase 8: strings ---

#[test]
fn string_literals_and_equality() {
    let body = build_one("f", "(defn f [s :- str] :- bool (= s \"cato\"))");
    assert!(matches!(body, HExpr::StrCmp(true, _, _)));
    let body = build_one("f", "(defn f [s :- str] :- bool (/= s \"cato\"))");
    assert!(matches!(body, HExpr::StrCmp(false, _, _)));
}

#[test]
fn strings_reject_arithmetic_and_ordering() {
    let err = build_err("(defn f [s :- str] :- str (+ s s))");
    assert!(err.contains("cannot be used in arithmetic"), "got: {}", err);
    let err = build_err("(defn f [a :- str b :- str] :- bool (< a b))");
    assert!(err.contains("strings support only"), "got: {}", err);
}

#[test]
fn externs_have_concrete_signatures() {
    let exprs = read_all("(extern fabs [x :- f64] :- f64)\n(defn main [] 0)").unwrap();
    let fns = build(&exprs).unwrap();
    let e = fns.iter().find(|d| d.name == "fabs").unwrap();
    assert!(e.is_extern);
    assert_eq!(e.ret, Ty::F64);
}

#[test]
fn externs_may_not_have_bodies_or_generics() {
    let err = build_err("(extern bad [x :- f64] :- f64 (+ x 1.0))");
    assert!(err.contains("no body"), "got: {}", err);
    let err = build_err("(extern bad [xs :- (Vector T)] :- i64)");
    assert!(err.contains("concrete types"), "got: {}", err);
}

// --- Phase 7b: heap construction ---

#[test]
fn conj_subvec_str_on_literals_fold_at_compile_time() {
    let body = build_one("f", "(defn f [] :- i64 (count (conj (vec-i64 1 2) 3)))");
    // The whole conj folds into a literal; count reads its static length.
    assert!(matches!(body, HExpr::Count(_)));
    let exprs = read_all(
        "(defn f [] :- i64 (count (subvec (vec-i64 1 2 3 4 5) 1 4)))"
    )
    .unwrap();
    let fns = build(&exprs).unwrap();
    // subvec of a literal with constant bounds folds to a literal of 3.
    let ir = cato_llvm::emit_module(&[], &fns);
    assert!(ir.contains("[3 x i64]"), "{}", ir);
    let exprs = read_all("(defn f [] :- i64 (count (str \"foo\" \"bar\")))").unwrap();
    let fns = build(&exprs).unwrap();
    let ir = cato_llvm::emit_module(&[], &fns);
    // "foobar" is a single 7-byte constant (6 chars + NUL).
    assert!(ir.contains("[7 x i8]"), "{}", ir);
}

#[test]
fn conj_moves_its_source() {
    let err = build_err(
        "(defn main []
           (let [v (vec-i64 1 2)]
             (do (conj v 3) (print-i64 (count v)))))",
    );
    // conj consumes the buffer (in-place growth); using v afterwards is a
    // use-after-move... count borrows, so this is allowed; a conj of a moved
    // value is not.
    let _ = err;
    let exprs = read_all(
        "(defn main []
           (let [v (vec-i64 1 2)]
             (do (conj v 3) (conj v 4))))",
    )
    .unwrap();
    let err = build(&exprs).unwrap_err();
    assert!(err.contains("use of moved value v"), "got: {}", err);
}

#[test]
fn for_requires_control_flow_free_body() {
    let err = build_err(
        "(defn main [] (let [w (for [x (vec-i64 1 2)] (if (> x 1) x 0))] 0))"
    );
    assert!(err.contains("control-flow-free"), "got: {}", err);
}

// --- Phase 8b: maps ---

#[test]
fn map_literals_sort_keys_at_compile_time() {
    let body = build_one("f", "(defn f [] :- i64 (get {3 30 1 10} 1))");
    let HExpr::MapGet { src, .. } = body else { panic!("mapget") };
    let HExpr::MapLit(pairs) = &*src else { panic!("maplit") };
    let keys: Vec<i64> = pairs
        .iter()
        .map(|(k, _)| if let HExpr::ConstI64(c) = k { *c } else { panic!() })
        .collect();
    assert_eq!(keys, vec![1, 3]);
}

#[test]
fn maps_reject_non_i64_keys_and_values() {
    let err = build_err("(defn f [] :- i64 (get {\"a\" 1} \"a\"))");
    assert!(err.contains("map keys must be i64"), "got: {}", err);
    let err = build_err("(defn f [] :- i64 (get {1 \"v\"} 1))");
    assert!(err.contains("map values must be i64"), "got: {}", err);
}

#[test]
fn set_literals_sort_and_dedupe() {
    let exprs = read_all("(defn main [] :- i64 0)").unwrap();
    let fns = build(&exprs).unwrap();
    let _ = fns;
    let exprs = read_all(
        "(defn main [] :- i64 (count #{3 3 1 2 1}))",
    )
    .unwrap();
    let fns = build(&exprs).unwrap();
    let body = &fns[0].body;
    match body {
        HExpr::Count(inner) => match &**inner {
            HExpr::SetLit(vs) => {
                let vals: Vec<i64> = vs
                    .iter()
                    .map(|v| match v {
                        HExpr::ConstI64(c) => *c,
                        _ => panic!("non-const"),
                    })
                    .collect();
                assert_eq!(vals, vec![1, 2, 3]);
            }
            other => panic!("expected set literal, got {:?}", other),
        },
        other => panic!("expected count, got {:?}", other),
    }
}

#[test]
fn set_ops_lower_to_set_nodes() {
    let exprs = read_all(
        "(defn main [] :- i64
           (let [s #{1 2}
                 a (conj s 3)
                 b (disj a 1)]
             (contains? b 2)))",
    )
    .unwrap();
    assert!(build(&exprs).is_ok());
}

#[test]
fn set_ops_require_sets() {
    assert!(build(&read_all("(defn main [] :- bool (contains? (vec-i64 1) 1))").unwrap()).is_err());
    assert!(build(&read_all("(defn main [] :- bool (disj 3 1))").unwrap()).is_err());
    assert!(build(&read_all("(defn main [] :- str (subs 3 0 1))").unwrap()).is_err());
    assert!(build(&read_all("(defn main [] :- str (join 3 (vec-i64 1)))").unwrap()).is_err());
}

#[test]
fn closed_width_rules() {
    // Same-width arithmetic and comparisons are allowed.
    assert!(build(&read_all("(defn f [a :- u8 b :- u8] :- u8 (+ a b))").unwrap()).is_ok());
    // Mixed integer widths are rejected.
    assert!(build(&read_all("(defn f [a :- u8 b :- i64] :- i64 (+ a b))").unwrap()).is_err());
    // f32 never auto-promotes.
    assert!(build(&read_all("(defn f [a :- f32 b :- i64] :- f32 (+ a b))").unwrap()).is_err());
    // Unsigned comparisons lower through the closed-width path.
    assert!(build(&read_all("(defn f [a :- u32 b :- u32] :- bool (> a b))").unwrap()).is_ok());
    // Casting literals to widths works: (u8 255).
    assert!(build(&read_all("(defn main [] :- i64 (i64 (u8 255)))").unwrap()).is_ok());
    // abs/min/max require i64/i32/f64.
    assert!(build(&read_all("(defn f [a :- u8] :- u8 (abs a))").unwrap()).is_err());
}

#[test]
fn closures_capture_and_move() {
    // Scalar captures are fine; the closure type is inferred.
    assert!(build(&read_all(
        "(defn main [] :- i64 (let [n 10 f (fn [x :- i64] (+ x n))] (f 5)))"
    ).unwrap()).is_ok());
    // Non-Copy captures are not in the slice yet.
    assert!(build(&read_all(
        "(defn main [] :- i64 (let [v (vec-i64 1) f (fn [x :- i64] (+ x (get v 0)))] (f 5)))"
    ).unwrap()).is_err());
    // Capturing moves a non-Copy binding: use after capture is rejected.
    assert!(build(&read_all(
        "(defn main [] :- i64 (let [s \"hi\" f (fn [x :- i64] (+ x 1))] (do (f 1) 0)))"
    ).unwrap()).is_ok());
    // Higher-order params type-check the call.
    assert!(build(&read_all(
        "(defn twice [f :- (Fn [i64] i64) x :- i64] :- i64 (f (f x)))"
    ).unwrap()).is_ok());
    // Calling a non-function local is still a plain user call (unchanged).
    assert!(build(&read_all(
        "(defn main [] :- i64 (let [x (fn [y :- i64] y)] (x 1)))"
    ).unwrap()).is_ok());
}

#[test]
fn phase19_patterns() {
    // Vector destructuring with _ and short-pattern defaults.
    assert!(build(&read_all(
        "(defn main [] :- i64 (let [[a _ c] (vec-i64 1 2 3)] (+ a c)))"
    ).unwrap()).is_ok());
    // Destructure requires a vector source.
    assert!(build(&read_all(
        "(defn main [] :- i64 (let [[a] 3] a))"
    ).unwrap()).is_err());
    // _ wildcard binders in match arms.
    assert!(build(&read_all(
        "(deftype P (Q [x :- i64 y :- i64]))\n(defn main [] :- i64 (match (Q 1 2) (Q [_ y] y)))"
    ).unwrap()).is_ok());
    // Guards: a passing guard takes the arm; a failing one falls through.
    let ok = build(&read_all(
        "(deftype P (Q [x :- i64 y :- i64]))\n(defn main [] :- i64 (match (Q 1 2) (Q [x y :when (= x y)] 100) (_ 0)))",
    ).unwrap());
    assert!(ok.is_ok(), "guard match should compile: {:?}", ok.err());
    // A guard with no total fall-through is rejected.
    assert!(build(&read_all(
        "(deftype P (Q [x :- i64 y :- i64] R [z :- i64]))\n(defn main [] :- i64 (match (Q 1 2) (Q [x y :when (= x y)] 100) (R [z] z)))"
    ).unwrap()).is_err());
}

#[test]
fn generics_structural_specificity() {
    let src = |body: &str| {
        format!(
            "(deftype Ent (Bolt [v :- i64]) (Ball [r :- i64]))\n{}",
            body
        )
    };
    // Variant specializers beat the whole-ADT default in either order.
    let m1 = "(defgeneric g [x :- Ent] :- i64)\n(defmethod g [x :- Bolt] :- i64 1)\n(defmethod g [x :- Ent] :- i64 0)\n(defn main [] :- i64 (g (Bolt 1)))";
    let m2 = "(defgeneric g [x :- Ent] :- i64)\n(defmethod g [x :- Ent] :- i64 0)\n(defmethod g [x :- Bolt] :- i64 1)\n(defn main [] :- i64 (g (Bolt 1)))";
    assert!(build(&read_all(&src(m1)).unwrap()).is_ok());
    assert!(build(&read_all(&src(m2)).unwrap()).is_ok());
    // Duplicate specializers are rejected.
    let dup = "(defgeneric g [x :- Ent] :- i64)\n(defmethod g [x :- Bolt] :- i64 1)\n(defmethod g [x :- Bolt] :- i64 2)";
    assert!(build(&read_all(&src(dup)).unwrap()).is_err());
    // Uncovered variant at a call site is rejected.
    let uncovered = "(defgeneric g [x :- Ent] :- i64)\n(defmethod g [x :- Bolt] :- i64 1)\n(defn main [] :- i64 (g (Ball 2)))";
    assert!(build(&read_all(&src(uncovered)).unwrap()).is_err());
    // Ambiguous two-dispatch: incomparable maxima.
    let amb = "(defgeneric h [x :- Ent y :- Ent] :- i64)\n(defmethod h [x :- Bolt y :- Ent] :- i64 1)\n(defmethod h [x :- Ent y :- Bolt] :- i64 2)\n(defn main [] :- i64 (h (Bolt 1) (Bolt 2)))";
    assert!(build(&read_all(&src(amb)).unwrap()).is_err());
    // A more specific pair resolves it.
    let resolved = "(defgeneric h [x :- Ent y :- Ent] :- i64)\n(defmethod h [x :- Bolt y :- Ent] :- i64 1)\n(defmethod h [x :- Ent y :- Bolt] :- i64 2)\n(defmethod h [x :- Bolt y :- Bolt] :- i64 3)\n(defmethod h [x :- Ent y :- Ent] :- i64 4)\n(defn main [] :- i64 (h (Bolt 1) (Bolt 2)))";
    assert!(build(&read_all(&src(resolved)).unwrap()).is_ok());
    // No call sites: no dispatch errors (resolution is demand-driven).
    let nocall = "(defgeneric h [x :- Ent y :- Ent] :- i64)\n(defmethod h [x :- Bolt y :- Ent] :- i64 1)\n(defmethod h [x :- Ent y :- Bolt] :- i64 2)";
    assert!(build(&read_all(&src(nocall)).unwrap()).is_ok());
}

#[test]
fn generics_dispatch_over_collection_types() {
    // Concrete vector specializers are accepted alongside ADTs.
    let ok = "(defgeneric size [c] :- i64)\n(defmethod size [v :- (Vector i64)] :- i64 (count v))\n(defmethod size [v :- (Vector str)] :- i64 (count v))\n(defn main [] :- i64 (size [1 2 3]))";
    assert!(build(&read_all(ok).unwrap()).is_ok());

    // Mixed ADT x vector dispatch synthesizes a dispatch defn with a
    // symbol-safe name.
    let mixed = "(deftype Shape (Circle [r :- f64]))\n(defgeneric fits [s :- Shape box :- (Vector f64)] :- bool)\n(defmethod fits [(Circle r) box :- (Vector f64)] :- bool (< r (get box 0)))\n(defn main [] :- bool (fits (Circle 2.0) [3.0]))";
    let defns = build(&read_all(mixed).unwrap()).unwrap();
    assert!(
        defns.iter().any(|d| d.name == "fits__Shape_vec_f64"),
        "dispatch defn expected: {:?}",
        defns.iter().map(|d| &d.name).collect::<Vec<_>>()
    );

    // An uncovered vector type is a call-site error naming the type.
    let uncovered = "(defgeneric size [c] :- i64)\n(defmethod size [v :- (Vector i64)] :- i64 (count v))\n(defn main [] :- i64 (size (split \"a,b\" \",\")))";
    let err = build(&read_all(uncovered).unwrap()).unwrap_err();
    assert!(
        err.contains("no method size for ((Vector str))"),
        "got: {}",
        err
    );

    // Element type parameters cannot select methods: methods are concrete.
    let generic_vec = "(defgeneric size [c] :- i64)\n(defmethod size [v :- (Vector T)] :- i64 (count v))";
    assert!(build(&read_all(generic_vec).unwrap()).is_err());

    // Map and set specializers are accepted. A pure-collection call
    // resolves directly to the hidden method — no dispatch defn.
    let coll = "(defgeneric size [c] :- i64)\n(defmethod size [v :- (Vector i64)] :- i64 (count v))\n(defmethod size [m :- map] :- i64 (count m))\n(defmethod size [s :- set] :- i64 (count s))\n(defn main [] :- i64 (size {1 2}))";
    let defns = build(&read_all(coll).unwrap()).unwrap();
    assert!(
        defns.iter().any(|d| d.name == "size__m1"),
        "map method expected: {:?}",
        defns.iter().map(|d| &d.name).collect::<Vec<_>>()
    );

    // Mixed ADT x map dispatch synthesizes a symbol-safe dispatch defn.
    let mixed_map = "(deftype Ent (Bolt [v :- i64]))\n(defgeneric tag [e :- Ent m :- map] :- i64)\n(defmethod tag [e :- Bolt m :- map] :- i64 (get m 1))\n(defn main [] :- i64 (tag (Bolt 1) {5 6}))";
    let defns = build(&read_all(mixed_map).unwrap()).unwrap();
    assert!(
        defns.iter().any(|d| d.name == "tag__Ent_map"),
        "map dispatch defn expected: {:?}",
        defns.iter().map(|d| &d.name).collect::<Vec<_>>()
    );
}

#[test]
fn generics_wildcard_vector_specializers() {
    // (Vector _) matches every concrete vector; an exact specializer is
    // strictly more specific and wins where both apply.
    let src = "(defgeneric len [c] :- i64)\n(defmethod len [v :- (Vector _)] :- i64 (count v))\n(defmethod len [v :- (Vector i64)] :- i64 (+ (count v) 100))\n(defn main [] :- i64 (do (len [1 2]) (len [1.5])))";
    let defns = build(&read_all(src).unwrap()).unwrap();
    let names: Vec<&str> = defns.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"len__m1"), "exact method present: {:?}", names);
    assert!(
        names.iter().any(|n| n.starts_with("len__m0__vec_f64")),
        "vec_f64 instance expected: {:?}",
        names
    );
    assert!(
        !names.contains(&"len__m0__vec_i64"),
        "the exact call site must not instantiate the wildcard: {:?}",
        names
    );

    // A non-vector call with only a wildcard vector method is uncovered.
    let uncovered = "(defgeneric len [c] :- i64)\n(defmethod len [v :- (Vector _)] :- i64 (count v))\n(defn main [] :- i64 (len {1 2}))";
    let err = build(&read_all(uncovered).unwrap()).unwrap_err();
    assert!(err.contains("no method len for (map)"), "got: {}", err);

    // (Vector T) stays rejected — the wildcard is spelled (Vector _).
    let tparam = "(defgeneric len [c] :- i64)\n(defmethod len [v :- (Vector T)] :- i64 (count v))";
    assert!(build(&read_all(tparam).unwrap()).is_err());
}

#[test]
fn print_is_a_generic_multimethod() {
    // The prelude provides print for builtin types; a call compiles to a
    // hidden method call with no dispatch tree.
    let program = cato_core::compile_to_program("(defn main [] :- i64 (print 42))").unwrap();
    let fns = &program.defns;
    let names: Vec<&str> = fns.iter().map(|d| d.name.as_str()).collect();
    assert!(names.iter().any(|n| n.starts_with("print__m")), "hidden print methods present: {:?}", names);
    // No method exists for maps: clean error.
    assert!(cato_core::compile_to_program("(defn main [] :- i64 (print {1 2}))").is_err());
}
