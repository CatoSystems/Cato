use cato::hir::*;
use cato::reader::read_all;

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
                assert!(matches!(*src, HExpr::Var(_)));
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
                assert!(matches!(*src, HExpr::Var(_)));
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
        HExpr::Let(binds, inner) => {
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
            assert!(matches!(*src, HExpr::Var(_)));
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
