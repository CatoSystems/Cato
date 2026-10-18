//! Backend parity: the interpreter and the LLVM backend must agree on the
//! whole examples/ suite (LLVM side is compiled with clang and executed).

use std::process::Command;

const EXAMPLES: &[&str] = &[
    "strvec",
    "options2",
    "closures",
    "collection_generics",
    "collections",
    "generic",
    "heap",
    "hamt",
    "host",
    "loops",
    "maps",
    "normalize",
    "options",
    "own",
    "parallel",
    "sets",
    "shape",
    "slice",
    "strings",
    "strings2",
    "trees",
    "widths",
    "widths2",
    "ffi",
    "generics",
];

#[test]
fn interpreter_agrees_with_llvm_on_all_examples() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.parent().unwrap().parent().unwrap();
    let cato = root.join("target/debug/cato");
    let rt = root.join("runtime/runtime.c");

    for name in EXAMPLES {
        let src = root.join("examples").join(format!("{}.cato", name));
        // Interpreter output.
        let out = Command::new(root.join("target/debug/cato-interp-run")).arg(&src).output().expect("interp run");
        assert!(out.status.success(), "{}: interp failed: {}", name, String::from_utf8_lossy(&out.stderr));
        let interp = String::from_utf8_lossy(&out.stdout).to_string();

        // LLVM output: emit IR, compile, run.
        let ll = root.join("target").join(format!("parity_{}.ll", name));
        let exe = root.join("target").join(format!("parity_{}_bin", name));
        let status = Command::new(&cato).arg("compile").arg(&src).arg("--emit-ir").arg("-o").arg(&ll).status().expect("cato emit");
        assert!(status.success(), "{}: emit failed", name);
        let cc = Command::new("clang").arg(&ll).arg(&rt).arg("-o").arg(&exe).status().expect("clang");
        assert!(cc.success(), "{}: clang failed", name);
        let out = Command::new(&exe).output().expect("llvm run");
        let llvm = String::from_utf8_lossy(&out.stdout).to_string();

        assert_eq!(interp, llvm, "{}: backend parity mismatch", name);
    }
}

#[test]
fn generic_instantiations_over_str_and_widths_emit_valid_ir() {
    // ty_mangle must give every instantiable type an LLVM-safe suffix:
    // `ident__??` was not even parseable IR.
    let src = r#"
(defn ident [x :- T] :- T x)
(defn pick [a :- T b :- T] :- T
  (if (< (count a) (count b)) a b))
(defn narrow [x :- T] :- T x)
(defn main []
  (let [s (ident "hello")
        w (narrow (i32 7))]
    (do
      (print (count s))
      (print-string (pick "ab" "cde"))
      (print (i64 w)))))
"#;
    let program = cato_core::compile_to_program(src).expect("compile");
    let out = cato_interp::Interpreter::new(&program).run().expect("run");
    assert_eq!(out, "5\nab\n7\n");

    // LLVM side: emit must succeed and clang must accept the symbols.
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.parent().unwrap().parent().unwrap();
    let cato = root.join("target/debug/cato");
    let scratch = root.join("target/parity_str_generic.cato");
    std::fs::write(&scratch, src).expect("scratch src");
    let ll = root.join("target/parity_str_generic.ll");
    let exe = root.join("target/parity_str_generic_bin");
    let status = Command::new(&cato)
        .arg("compile")
        .arg(&scratch)
        .arg("--emit-ir")
        .arg("-o")
        .arg(&ll)
        .status()
        .expect("cato emit");
    assert!(status.success(), "emit failed");
    let cc = Command::new("clang")
        .arg(&ll)
        .arg(root.join("runtime/runtime.c"))
        .arg("-o")
        .arg(&exe)
        .status()
        .expect("clang");
    assert!(cc.success(), "clang rejected the emitted IR");
    let out = Command::new(&exe).output().expect("llvm run");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "5\nab\n7\n");
}

#[test]
fn string_builder_patterns_agree_between_backends() {
    // The S4 selfhost stdlib leans on vector-typed control flow: if/match
    // bodies producing (Vector str), conj in value position, and vector
    // clone — all through recursion (loop variables are Copy-only).
    let src = r#"
(defn sb-new [] :- (Vector str) [])
(defn sb-add [b :- (Vector str) s :- str] :- (Vector str)
  (conj b s))
(defn sb-build [b :- (Vector str) n :- i64] :- (Vector str)
  (if (< n 1)
    b
    (let [t (sb-add b "w")]
      (sb-build t (- n 1)))))
(defn pick-vec [n :- i64] :- (Vector str)
  (match (get (vec-i64 1) 0)
    (Some [_] (let [t (conj (sb-new) "one")] t))
    (None [] (sb-new))))
(defn main []
  (let [b1 (sb-new)
        b2 (sb-add b1 "a")
        b3 (sb-build (clone b2) 3)
        bv (pick-vec 5)]
    (do
      (print (count b2))
      (print (count b3))
      (print (join "" b3))
      (print (count bv))
      (print (get (clone b2) 0 "?")))))
"#;
    let program = cato_core::compile_to_program(src).expect("compile");
    let out = cato_interp::Interpreter::new(&program).run().expect("run");
    assert_eq!(out, "1\n4\nawww\n1\na\n");

    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.parent().unwrap().parent().unwrap();
    let cato = root.join("target/debug/cato");
    let scratch = root.join("target/parity_strbuilder.cato");
    std::fs::write(&scratch, src).expect("scratch src");
    let ll = root.join("target/parity_strbuilder.ll");
    let exe = root.join("target/parity_strbuilder_bin");
    let status = Command::new(&cato)
        .arg("compile")
        .arg(&scratch)
        .arg("--emit-ir")
        .arg("-o")
        .arg(&ll)
        .status()
        .expect("cato emit");
    assert!(status.success(), "emit failed");
    let cc = Command::new("clang")
        .arg(&ll)
        .arg(root.join("runtime/runtime.c"))
        .arg("-o")
        .arg(&exe)
        .status()
        .expect("clang");
    assert!(cc.success(), "clang rejected the emitted IR");
    let out = Command::new(&exe).output().expect("llvm run");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1\n4\nawww\n1\na\n");
}

#[test]
fn interp_evaluates_bool_literals() {
    let src = r#"
(defn main []
  (let [flag true
        off false]
    (do
      (print flag)
      (print off)
      (print (if true 10 20))
      (print (if off "yes" "no"))
      (print (and true false))
      (print (or true false))
      (print (not true)))))
"#;
    let program = cato_core::compile_to_program(src).expect("compile");
    let out = cato_interp::Interpreter::new(&program).run().expect("run");
    assert_eq!(out, "1\n0\n10\nno\n0\n1\n0\n");
}

#[test]
fn interp_orders_strings() {
    let src = r#"
(defn main []
  (let [a "apple"
        b "banana"]
    (do
      (print (< a b))
      (print (> a b))
      (print (<= "apple" "apple"))
      (print (>= a b))
      (print (< "b" "apple"))
      (print (if (< a b) "a-first" "b-first")))))
"#;
    let program = cato_core::compile_to_program(src).expect("compile");
    let out = cato_interp::Interpreter::new(&program).run().expect("run");
    assert_eq!(out, "1\n0\n1\n0\n0\na-first\n");
}

#[test]
fn string_ordering_rejects_mixed_operands() {
    let src = r#"
(defn main [] (print (< "a" 1)))
"#;
    assert!(cato_core::compile_to_program(src).is_err());
}

#[test]
fn interp_cmpw_orders_u64_top_bit_like_llvm() {
    // (u64 -1) is u64::MAX — its i64 bit pattern is -1. The backend emits
    // `ult`, so unsigned order says MAX < 1 is false; a signed interp
    // compare would say true. This corner is outside the examples suite.
    let src = r#"
(defn main [] :- i64
  (do
    (print (if (< (u64 -1) (u64 1)) 1 0))
    (print (if (> (u64 -1) (u64 1)) 1 0))
    (print (if (< (u64 1) (u64 -1)) 1 0))))
"#;
    let program = cato_core::compile_to_program(src).expect("compile");
    let out = cato_interp::Interpreter::new(&program).run().expect("run");
    assert_eq!(out, "0\n1\n1\n");
}

#[test]
fn interp_evaluates_nth_and_char_str() {
    // Two-argument nth returns (Option i64); the explicit-default form
    // returns i64 directly. Out-of-range reads yield None / the default.
    let src = r#"
(defn main []
  (let [s "hello"
        c (char->str 65)
        t (char->str 9)]
    (do
      (print (match (nth s 0) (Some [b] b) (None [] 0)))
      (print (match (nth s 4) (Some [b] b) (None [] 0)))
      (print (match (nth s 99) (Some [b] b) (None [] 0)))
      (print (nth s 0 0))
      (print (nth s 99 7))
      (print-string c)
      (print (count c))
      (print (= t "\t"))
      (print (match (nth (char->str 122) 0) (Some [b] b) (None [] 0)))
      (print (char->str 300)))))
"#;
    let program = cato_core::compile_to_program(src).expect("compile");
    let out = cato_interp::Interpreter::new(&program).run().expect("run");
    // Out-of-range Option nth is None; char->str 300 truncates to ','.
    assert_eq!(out, "104\n111\n0\n104\n7\nA\n1\n1\n122\n,\n");
}

#[test]
fn interp_evaluates_generic_adts() {
    let src = r#"
(defn get-or [o :- (Option i64) d :- i64] :- i64
  (match o
    (Some [v] v)
    (None [] d)))
(defn opt-or [o :- (Option T) d :- T] :- T
  (match o
    (Some [v] v)
    (None [] d)))
(defn main []
  (let [a (Some 41)
        n :- (Option i64) (None)
        f (Some 1.5)]
    (do
      (print (get-or a 0))
      (print (get-or (clone n) 7))
      (print (opt-or (clone n) 99))
      (print (opt-or f 0.0))
      (print (match (Some 5) (Some [v] (* v 2)) (None [] 0))))))
"#;
    let program = cato_core::compile_to_program(src).expect("compile");
    let out = cato_interp::Interpreter::new(&program).run().expect("run");
    assert_eq!(out, "41\n7\n99\n1.5\n10\n");
}

#[test]
fn interp_evaluates_recursive_trees() {
    let src = r#"
(deftype (Tree a)
  (Node [v :- a l :- (Tree a) r :- (Tree a)])
  (Leaf))
(defn sum [t :- (Tree i64)] :- i64
  (match t
    (Node [v l r] (+ v (+ (sum (clone l)) (sum (clone r)))))
    (Leaf [] 0)))
(defn main []
  (let [t :- (Tree i64)
        (Node 5 (Node 3 (Leaf) (Leaf)) (Node 8 (Leaf) (Leaf)))]
    (sum (clone t))))
"#;
    let program = cato_core::compile_to_program(src).expect("compile");
    let (v, _) = cato_interp::Interpreter::new(&program)
        .run_entry("main")
        .expect("run");
    assert!(matches!(v, cato_interp::Value::Int(16)), "got {:?}", v);
}

#[test]
fn interp_evaluates_sets_strings_and_closures() {
    let src = r#"
(defn main []
  (let [s #{5 3 1}
        parts (split "a,b" ",")]
    (do
      (print-i64 (contains? s 3))
      (print-string (join "-" parts))
      (print-f64 (reduce (fn [acc x] (+ acc x)) 0.0 (vec-f64 1.5 2.5))))))
"#;
    let program = cato_core::compile_to_program(src).expect("compile");
    let out = cato_interp::Interpreter::new(&program).run().expect("run");
    assert_eq!(out, "1\na-b\n4\n");
}

#[test]
fn modules_agree_between_backends() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.parent().unwrap().parent().unwrap();
    let cato = root.join("target/debug/cato");
    let rt = root.join("runtime/runtime.c");
    let src = root.join("examples/modules/main.cato");

    let out = Command::new(root.join("target/debug/cato-interp-run")).arg(&src).output().expect("interp run");
    assert!(out.status.success(), "interp: {}", String::from_utf8_lossy(&out.stderr));
    let interp = String::from_utf8_lossy(&out.stdout).to_string();

    let ll = root.join("target/modules_main.ll");
    let exe = root.join("target/modules_main_bin");
    let status = Command::new(&cato).arg("compile").arg(&src).arg("--emit-ir").arg("-o").arg(&ll).status().expect("emit");
    assert!(status.success());
    let cc = Command::new("clang").arg(&ll).arg(&rt).arg("-o").arg(&exe).status().expect("clang");
    assert!(cc.success());
    let out = Command::new(&exe).output().expect("llvm run");
    let llvm = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(interp, llvm, "modules parity");
    assert_eq!(interp, "49\n25\n16\n");
}
