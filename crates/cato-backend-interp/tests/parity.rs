//! Backend parity: the interpreter and the LLVM backend must agree on the
//! whole examples/ suite (LLVM side is compiled with clang and executed).

use std::process::Command;

const EXAMPLES: &[&str] = &[
    "closures",
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
    "ffi",
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
        let out = Command::new(&cato).arg("run").arg(&src).output().expect("interp run");
        assert!(out.status.success(), "{}: interp failed: {}", name, String::from_utf8_lossy(&out.stderr));
        let interp = String::from_utf8_lossy(&out.stdout).to_string();

        // LLVM output: emit IR, compile, run.
        let ll = root.join("target").join(format!("parity_{}.ll", name));
        let exe = root.join("target").join(format!("parity_{}_bin", name));
        let status = Command::new(&cato).arg(&src).arg("-o").arg(&ll).status().expect("cato emit");
        assert!(status.success(), "{}: emit failed", name);
        let cc = Command::new("clang").arg(&ll).arg(&rt).arg("-o").arg(&exe).status().expect("clang");
        assert!(cc.success(), "{}: clang failed", name);
        let out = Command::new(&exe).output().expect("llvm run");
        let llvm = String::from_utf8_lossy(&out.stdout).to_string();

        assert_eq!(interp, llvm, "{}: backend parity mismatch", name);
    }
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
