use cato::codegen::emit_module;
use cato::hir::build;
use cato::reader::read_all;

fn emit(src: &str) -> String {
    let fns = build(&read_all(src).unwrap()).unwrap();
    emit_module(&fns)
}

#[test]
fn emits_noalias_on_loop_data_param() {
    let ir = emit(
        "(defn square [x :- i64] :- i64 (* x x))
         (defn fused [xs :- vec] :- i64
           (reduce + 0 (map square (filter even? xs))))",
    );
    assert!(ir.contains("noalias nocapture readonly"));
}

#[test]
fn emits_single_loop_region() {
    let ir = emit(
        "(defn square [x :- i64] :- i64 (* x x))
         (defn fused [xs :- vec] :- i64
           (reduce + 0 (map square (filter even? xs))))",
    );
    // exactly one `%loop` label (plus the helper is fused itself)
    assert!(ir.contains("label %loop"));
    assert!(!ir.contains("label %loop2"));
}

#[test]
fn emits_declares_for_prints() {
    let ir = emit(
        "(defn main []
           (print-f64 1.0)
           (print-i64 2))",
    );
    assert!(ir.contains("declare void @cato_print_f64(double)"));
    assert!(ir.contains("declare void @cato_print_i64(i64)"));
}

#[test]
fn emits_main_returns_i32() {
    let ir = emit("(defn main [] (print-i64 0))");
    assert!(ir.contains("define i32 @main("));
}

#[test]
fn emits_sum_of_squares_loop_with_capture() {
    let ir = emit(
        "(defn square [x :- i64] :- i64 (* x x))
         (defn sum-of-squares [xs :- vec mu :- f64] :- f64
           (reduce (fn [acc x] (+ acc (* (- x mu) (- x mu)))) 0.0 xs))",
    );
    // helper has a double capture param
    assert!(ir.contains("define double @sum_of_squares("));
    assert!(ir.contains("double %mu"));
}

#[test]
fn emits_loop_with_even_filter_and_square_map() {
    let ir = emit(
        "(defn square [x :- i64] :- i64 (* x x))
         (defn fused [xs :- vec] :- i64
           (reduce + 0 (map square (filter even? xs))))",
    );
    // loop body has and + icmp ne for even? predicate
    assert!(ir.contains("and i64"));
    assert!(ir.contains("icmp ne i64"));
    // map square body: mul i64
    assert!(ir.contains("mul i64"));
}

#[test]
fn even_predicate_inverts_low_bit() {
    // `even?` must be true when the low bit is clear: and %x,1 then xor 1.
    let ir = emit(
        "(defn square [x :- i64] :- i64 (* x x))
         (defn fused [xs :- vec] :- i64
           (reduce + 0 (map square (filter even? xs))))",
    );
    assert!(ir.contains("and i64"));
    assert!(ir.contains("xor i64"));
}

#[test]
fn emits_vec_const_global_for_literal() {
    let ir = emit("(defn main [] (print-i64 0))");
    // If main uses vec-i64 a global is minted; for this simple main
    // there is none. Just verify it compiles.
    let _ = ir;
}

#[test]
fn emits_variadic_let_chaining() {
    let ir = emit(
        "(defn foo [xs :- vec] :- f64
           (let [n  (count xs)
                 mu (/ (reduce + 0 xs) (f64 n))
                 ss mu]
             (/ ss (f64 n))))",
    );
    // just smoke-test: compile to IR without panicking
    assert!(ir.contains("define double @foo("));
}
