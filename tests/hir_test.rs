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
