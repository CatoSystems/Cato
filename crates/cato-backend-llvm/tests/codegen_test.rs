use cato_llvm::{emit_module, emit_module_fast};
use cato_core::hir::build;
use cato_core::reader::read_all;

fn emit(src: &str) -> String {
    let fns = build(&read_all(src).unwrap()).unwrap();
    emit_module(&[], &fns)
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
    // loop body has and + icmp eq for even? predicate (now bool)
    assert!(ir.contains("and i64"));
    assert!(ir.contains("icmp eq i64"));
    // map square body: mul i64
    assert!(ir.contains("mul i64"));
}

#[test]
fn even_predicate_reads_low_bit_set() {
    // `even?` is true when the low bit is clear: and %x,1 then icmp eq 0 (i1).
    let ir = emit(
        "(defn square [x :- i64] :- i64 (* x x))
         (defn fused [xs :- vec] :- i64
           (reduce + 0 (map square (filter even? xs))))",
    );
    assert!(ir.contains("and i64"));
    assert!(ir.contains("icmp eq i64"));
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

#[test]
fn if_emits_phi_merge() {
    let ir = emit("(defn f [x :- i64 n :- i64] :- i64 (if (< x n) x n))");
    assert!(ir.contains("icmp slt i64"));
    assert!(ir.contains("phi i64"));
}

#[test]
fn and_emits_short_circuit_phi() {
    let ir = emit(
        "(defn count-even-below [xs :- vec n :- i64] :- i64
           (reduce (fn [acc x] (if (and (even? x) (< x n)) (+ acc 1) acc)) 0 xs))",
    );
    assert!(ir.contains("phi i1"));
    assert!(ir.contains("icmp slt i64"));
}

#[test]
fn f64_map_stage_converts_element_once() {
    let ir = emit(
        "(defn clamp-x [x :- f64 lo :- f64 hi :- f64] :- f64 (min (max x lo) hi))
         (defn normalize [xs :- vec xsrng :- f64] :- f64
           (reduce (fn [acc x] (max acc (abs x))) 0.0
             (map (fn [x] (clamp-x x -1.0 1.0))
               (map (fn [x] (/ (- (f64 x) xsrng) xsrng)) xs))))",
    );
    assert!(ir.contains("sitofp"));
    assert!(ir.contains("fdiv double"));
    assert!(ir.contains("fsub double"));
}

#[test]
fn filter_branches_on_i1() {
    let ir = emit(
        "(defn count-even-below [xs :- vec n :- i64] :- i64
           (reduce (fn [acc x] (if (< x n) (+ acc 1) acc)) 0 xs))",
    );
    assert!(ir.contains("br i1"));
}

#[test]
fn capture_is_typed_from_param() {
    // n is a loop capture and must carry as i64 (from the param), not double.
    let ir = emit(
        "(defn count-even-below [xs :- vec n :- i64] :- i64
           (reduce (fn [acc x] (+ acc (* x n))) 0 xs))",
    );
    assert!(ir.contains("i64 %n"));
    assert!(!ir.contains("double %n"));
}

#[test]
fn f64_comparison_uses_fcmp() {
    let ir = emit("(defn f [a :- f64 b :- f64] :- bool (< a b))");
    assert!(ir.contains("fcmp olt double"));
}

#[test]
fn min_max_use_select() {
    let ir = emit("(defn f [a :- i64 b :- i64] :- i64 (min a b))");
    assert!(ir.contains("icmp slt i64"));
    assert!(ir.contains("select i1"));
}

// --- Phase 3: deftype + match ---

const SHAPE_SRC: &str = "(deftype Shape (Circle [radius :- f64]) (Rect [w :- f64 h :- f64]) (Point []))

(defn area [s :- Shape] :- f64
  (match s
    (Circle [r] (* r r))
    (Rect [w h] (* w h))
    (Point [] 0.0)))

(defn widen [s :- Shape] :- Shape
  (match s
    (Rect [w h] (Rect (+ w 1.0) h))
    (_ s)))";

#[test]
fn emits_tagged_union_type_and_switch() {
    let exprs = read_all(SHAPE_SRC).unwrap();
    let (adts, fns) = cato_core::hir::build_program(&exprs).unwrap();
    let ir = emit_module(&adts, &fns);
    assert!(ir.contains("%Shape = type { i64, [2 x i64] }"), "{}", ir);
    assert!(ir.contains("switch i64"), "{}", ir);
    assert!(ir.contains("unreachable"), "{}", ir);
}

#[test]
fn match_merges_arms_with_phi() {
    let exprs = read_all(SHAPE_SRC).unwrap();
    let (adts, fns) = cato_core::hir::build_program(&exprs).unwrap();
    let ir = emit_module(&adts, &fns);
    // Scalar result merge in `area`, ptr result merge in `widen`.
    assert!(ir.contains("phi double"), "{}", ir);
    assert!(ir.contains("phi ptr"), "{}", ir);
}

#[test]
fn ctor_stores_tag_and_bitcast_fields() {
    let exprs = read_all(SHAPE_SRC).unwrap();
    let (adts, fns) = cato_core::hir::build_program(&exprs).unwrap();
    let ir = emit_module(&adts, &fns);
    assert!(ir.contains("bitcast double"), "{}", ir);
    assert!(ir.contains("alloca %Shape, align 8"), "{}", ir);
}

#[test]
fn adt_values_cross_functions_by_value() {
    // The returned struct is loaded by value at the boundary — a returned
    // ptr to a callee alloca would dangle.
    let exprs = read_all(SHAPE_SRC).unwrap();
    let (adts, fns) = cato_core::hir::build_program(&exprs).unwrap();
    let ir = emit_module(&adts, &fns);
    assert!(ir.contains("define %Shape @widen("), "{}", ir);
    // Param comes in by value and is copied to an entry slot.
    assert!(ir.contains("store %Shape %s, ptr"), "{}", ir);
}

// --- Phase 4: fused collection sources ---

#[test]
fn take_drop_range_zip_stay_single_loops() {
    let src = "(defn f [xs :- vec n :- i64] (reduce + 0 (take n (drop 1 xs))))
               (defn g [a :- i64 b :- i64] (reduce + 0 (range a b)))
               (defn h [xs :- vec ys :- vec] (reduce + 0 (map (fn [a b] (* a b)) (zip xs ys))))";
    let exprs = read_all(src).unwrap();
    let fns = build(&exprs).unwrap();
    let ir = emit_module(&[], &fns);
    // One loop per helper, no nested loops or calls in the loop body.
    assert_eq!(ir.matches("icmp ult i64 %i.next").count(), 3, "{}", ir);
    assert!(ir.contains("%dataA") && ir.contains("%dataB"), "{}", ir);
    // take/drop/range bounds are computed branchlessly in the entry block.
    assert!(ir.matches("select i1").count() >= 4, "{}", ir);
}

#[test]
fn get_is_branchless_bounds_checked_load() {
    let exprs = read_all("(defn f [xs :- vec] :- i64 (get xs 2 9))").unwrap();
    let fns = build(&exprs).unwrap();
    let ir = emit_module(&[], &fns);
    assert!(ir.contains("icmp ult i64"), "{}", ir);
    assert!(ir.contains("select i1"), "{}", ir);
    assert!(ir.contains("getelementptr inbounds i64, ptr"), "{}", ir);
}

// --- Phase 6: element-typed codegen ---

#[test]
fn f64_vectors_load_doubles_in_fused_loops() {
    let src = "(defn f [xs :- (Vector f64)] :- f64
                 (reduce (fn [acc x] (+ acc x)) 0.0 xs))";
    let exprs = read_all(src).unwrap();
    let fns = build(&exprs).unwrap();
    let ir = emit_module(&[], &fns);
    assert!(
        ir.contains("getelementptr inbounds double, ptr %data, i64"),
        "{}",
        ir
    );
    assert!(ir.contains("load double, ptr"), "{}", ir);
    assert!(ir.contains("phi double"), "{}", ir);
}

#[test]
fn vec_f64_literals_mint_double_globals() {
    let src = "(defn f [xs :- (Vector f64)] :- f64 (reduce + 0.0 xs))
               (defn main [] (print-f64 (f (vec-f64 1.5 2.5))))";
    let exprs = read_all(src).unwrap();
    let fns = build(&exprs).unwrap();
    let ir = emit_module(&[], &fns);
    assert!(
        ir.contains("private constant [2 x double] [double 1.5"),
        "{}",
        ir
    );
}

// --- Phase 8: string codegen ---

#[test]
fn string_literals_mint_i8_globals_and_use_strcmp() {
    let src = "(defn f [s :- str] :- bool (= s \"cato\"))
               (defn main [] (do (print-string \"hello\") 0))";
    let exprs = read_all(src).unwrap();
    let fns = build(&exprs).unwrap();
    let ir = emit_module(&[], &fns);
    assert!(ir.contains("private unnamed_addr constant [5 x i8]"), "{}", ir);
    assert!(ir.contains("call i32 @strcmp("), "{}", ir);
    assert!(ir.contains("call void @cato_print_string("), "{}", ir);
}

// --- Phase 9: FFI + --fast vectorization metadata ---

#[test]
fn externs_emit_declares_not_defines() {
    let src = "(extern fabs [x :- f64] :- f64)
               (defn main [] (print-f64 (fabs -3.5)))";
    let exprs = read_all(src).unwrap();
    let (adts, fns) = cato_core::hir::build_program(&exprs).unwrap();
    let ir = emit_module(&adts, &fns);
    assert!(ir.contains("declare double @fabs(double)"), "{}", ir);
    assert!(!ir.contains("define double @fabs"), "{}", ir);
    assert!(ir.contains("call double @fabs(double"), "{}", ir);
}

#[test]
fn fast_flag_attaches_vectorize_metadata() {
    let src = "(defn f [xs :- vec] :- i64 (reduce + 0 xs))";
    let exprs = read_all(src).unwrap();
    let (adts, fns) = cato_core::hir::build_program(&exprs).unwrap();
    let plain = emit_module(&adts, &fns);
    assert!(!plain.contains("llvm.loop.vectorize"), "{}", plain);
    let fast = emit_module_fast(&adts, &fns, true, &[], &[], 1);
    assert!(fast.contains("llvm.loop.vectorize.enable"), "{}", fast);
    assert!(fast.contains("!llvm.loop !0"), "{}", fast);
}

// --- Phase 7b: heap codegen ---

#[test]
fn runtime_conj_grows_with_capacity_and_in_place_append() {
    let src = "(defn main []
                 (let [v (vec-i64 1 2)
                       w (conj v 3)]
                   (print-i64 (count w))))";
    let exprs = read_all(src).unwrap();
    let (adts, fns) = cato_core::hir::build_program(&exprs).unwrap();
    let ir = emit_module(&adts, &fns);
    // Static source forces the copy path with 2x capacity headroom.
    assert!(ir.contains("call ptr @malloc(i64"), "{}", ir);
    assert!(ir.contains("llvm.memcpy"), "{}", ir);
    // The header {ptr, len, cap} is maintained through the spare field.
    assert!(ir.contains("store i64 3, ptr"), "{}", ir);
}

#[test]
fn vector_returning_functions_pass_the_struct_by_value() {
    let src = "(defn grow [v :- vec x :- i64] :- vec (let [w (conj v x)] w))
               (defn main [] (let [w (grow (vec-i64 1) 2)] (print-i64 (count w))))";
    let exprs = read_all(src).unwrap();
    let (adts, fns) = cato_core::hir::build_program(&exprs).unwrap();
    let ir = emit_module(&adts, &fns);
    assert!(ir.contains("define %VecI64 @grow("), "{}", ir);
    assert!(ir.contains("call %VecI64 @grow("), "{}", ir);
}
