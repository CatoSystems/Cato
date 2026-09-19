use cato_core::fuse::check;
use cato_core::hir::*;
use cato_core::reader::read_all;

fn fused_map_filter(name: &str, src: &str) -> HExpr {
    let exprs = read_all(src).unwrap();
    let fns = cato_core::hir::build(&exprs).unwrap();
    let defn = fns.iter().find(|d| d.name == name).expect("fn found");
    check(&defn.body).clone()
}

#[test]
fn fuses_pipeline_keeps_single_reduce() {
    let h = fused_map_filter(
        "fused",
        "(defn square [x :- i64] (* x x))
         (defn fused [xs :- vec] (reduce + 0 (map square (filter even? xs))))",
    );
    assert!(matches!(h, HExpr::Reduce(_)));
}

#[test]
fn fuses_closure_reduce() {
    let h = fused_map_filter(
        "ss",
        "(defn ss [xs :- vec mu :- f64] (reduce (fn [acc x] (+ acc (* (- (f64 x) mu) (- (f64 x) mu)))) 0.0 xs))",
    );
    assert!(matches!(h, HExpr::Reduce(_)));
}

#[test]
fn rejects_lazy_non_var_source() {
    // (reduce + 0 (map square 5)) — only vector variables, vector literals,
    // ranges, zips and take/drop wrappers are valid fused sources.
    let exprs = read_all(
        "(defn square [x :- i64] (* x x))
         (defn bad [] (reduce + 0 (map square 5)))",
    )
    .unwrap();
    let err = cato_core::hir::build(&exprs).unwrap_err();
    assert!(err.contains("vector variable"), "got: {}", err);
}

#[test]
fn pipeline_keeps_maps_in_inner_first_order() {
    let h = fused_map_filter(
        "p",
        "(defn square [x :- i64] (* x x))
         (defn p [xs :- vec] (reduce + 0 (map square (map (fn [x] (f64 x)) xs))))",
    );
    if let HExpr::Reduce(r) = h {
        if let HReduce::Pipeline { maps, .. } = *r {
            assert_eq!(maps.len(), 2);
        } else {
            panic!("expected pipeline");
        }
    } else {
        panic!("expected reduce");
    }
}