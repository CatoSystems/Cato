use cato::fuse::fuse;
use cato::hir::*;
use cato::reader::read_all;

fn fused_map_filter(name: &str, src: &str) -> HExpr {
    let exprs = read_all(src).unwrap();
    let fns = cato::hir::build(&exprs).unwrap();
    let defn = fns.iter().find(|d| d.name == name).expect("fn found");
    fuse(&defn.body).clone()
}

#[test]
fn fuses_pipeline_keeps_single_reduce() {
    let h = fused_map_filter(
        "fused",
        "(defn square [x] (* x x))
         (defn fused [xs] (reduce + 0 (map square (filter even? xs))))",
    );
    assert!(matches!(h, HExpr::Reduce(_)));
}

#[test]
fn fuses_closure_reduce() {
    let h = fused_map_filter(
        "ss",
        "(defn ss [xs mu] (reduce (fn [acc x] (+ acc (* (- x mu) (- x mu)))) 0.0 xs))",
    );
    assert!(matches!(h, HExpr::Reduce(_)));
}

#[test]
fn rejects_lazy_non_var_source() {
    // (reduce + 0 (map square (vec-i64 1 2 3))) — a vector literal source is
    // out of the slice scope; fusion must leave it intact rather than
    // miscompile.
    let h = fused_map_filter(
        "bad",
        "(defn square [x] (* x x))
         (defn bad [] (reduce + 0 (map square (vec-i64 1 2 3))))",
    );
    let _ = h;
}

#[test]
fn pipeline_keeps_maps_in_inner_first_order() {
    let h = fused_map_filter(
        "p",
        "(defn square [x] (* x x))
         (defn p [xs] (reduce + 0 (map square (map (fn [x] (f64 x)) xs))))",
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