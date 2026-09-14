// tests/integration.rs
use std::process::Command;

#[test]
fn cli_hello_runs() {
    let out = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg("--help")
        .output()
        .expect("run binary");
    assert!(out.status.success());
}

#[test]
fn cli_version_prints_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg("--version")
        .output()
        .expect("run binary");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("cato "), "got: {}", stdout);
}

// `cato build` compiles through to a native executable.
#[test]
fn build_produces_running_binary() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("../../examples/slice.cato");
    let bin = manifest.join("../../target/cli_build_demo");

    let status = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg("build").arg(&src).arg("-o").arg(&bin)
        .status().expect("cato build");
    assert!(status.success(), "build failed");

    let out = Command::new(&bin).output().expect("run built binary");
    assert!(out.status.success(), "built binary failed");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "2.5\n20\n");

    // Default output name: the input stem next to the source is not
    // required — with -o given, nothing else is created.
    let _ = std::fs::remove_file(&bin);
}

// `cato run --native` executes the compiled binary in one step and agrees
// with the interpreter.
#[test]
fn run_native_matches_interpreter() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("../../examples/slice.cato");
    let cato = env!("CARGO_BIN_EXE_cato");

    let interp = Command::new(cato).arg("run").arg(&src).output().expect("interp");
    assert!(interp.status.success(), "interp failed");
    let native = Command::new(cato).args(["run", "--native"]).arg(&src).output().expect("native");
    assert!(native.status.success(), "native run failed: {}",
        String::from_utf8_lossy(&native.stderr));

    assert_eq!(
        String::from_utf8_lossy(&interp.stdout),
        String::from_utf8_lossy(&native.stdout),
    );
}

// `--emit-ll` keeps the intermediate IR next to the native build.
#[test]
fn build_keeps_ir_with_emit_ll() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("../../examples/slice.cato");
    let bin = manifest.join("../../target/cli_emitll_demo");
    let ll = manifest.join("../../target/cli_emitll_demo.ll");

    let status = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg("build").arg(&src).arg("-o").arg(&bin)
        .arg("--emit-ll").arg(&ll)
        .status().expect("cato build");
    assert!(status.success(), "build failed");

    let ir = std::fs::read_to_string(&ll).expect("emit-ll file");
    assert!(ir.contains("define i32 @main"), "IR must contain main");

    let _ = std::fs::remove_file(&bin);
    let _ = std::fs::remove_file(&ll);
}

// `-O` levels and `--asan` flow through to the C compiler.
#[test]
fn build_honors_opt_level_and_asan_flags() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("../../examples/slice.cato");
    let bin = manifest.join("../../target/cli_flags_demo");
    let cato = env!("CARGO_BIN_EXE_cato");

    for extra in [&["-O0"][..], &["--opt-level", "3"][..], &["--asan"][..]] {
        let status = Command::new(cato)
            .arg("build").arg(&src).arg("-o").arg(&bin)
            .args(extra)
            .status().expect("cato build");
        assert!(status.success(), "build with {:?} failed", extra);
        let out = Command::new(&bin).output().expect("run built binary");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "2.5\n20\n");
    }
    let _ = std::fs::remove_file(&bin);
}

// The interpreter stays the default for `cato run` (used by parity suites).
#[test]
fn run_without_native_uses_interpreter() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("../../examples/slice.cato");
    // The interpreter needs no C compiler: point CC at a missing binary and
    // `cato run` must still succeed.
    let out = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg("run").arg(&src)
        .env("CC", "/no/such/compiler")
        .output()
        .expect("interp run");
    assert!(out.status.success(), "interp run must not need a C compiler");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "2.5\n20\n");
}


#[test]
fn cli_rejects_dangling_o_flag() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("../../examples/slice.cato");
    let out = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg(&src)
        .args(["-o"])
        .output()
        .expect("run binary");
    assert!(!out.status.success(), "expected -o without filename to fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("-o requires a filename"), "got: {}", stderr);
}

#[test]
fn cli_rejects_unknown_flag() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("../../examples/slice.cato");
    let out = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg(&src)
        .arg("--no-such-flag")
        .output()
        .expect("run binary");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown flag"), "got: {}", stderr);
}

// End-to-end: parse/build/fuse/emit to IR, compile with clang, run, assert output.
#[test]
fn compiles_slice_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("../../examples/slice.cato");
    let out_ll = manifest.join("../../target/slice.ll");
    let rt = manifest.join("../../runtime/runtime.c");
    let exe = manifest.join("../../target/slice_bin");

    let status = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg(&src).arg("--emit-ir").arg("-o").arg(&out_ll)
        .status().expect("cato run");
    assert!(status.success(), "cato compile failed");

    let cc = Command::new("clang")
        .arg(&out_ll).arg(&rt).arg("-o").arg(&exe)
        .status().expect("clang link");
    assert!(cc.success(), "clang link failed");

    let out = Command::new(&exe).output().expect("run binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout, "2.5\n20\n");
}

fn compile_and_run(src_name: &str, src: &str) -> String {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src_path = manifest.join("../../target").join(src_name);
    std::fs::write(&src_path, src).expect("write source");
    let out_ll = manifest.join("../../target").join(format!("{}.ll", src_name));
    let rt = manifest.join("../../runtime/runtime.c");
    let exe = manifest.join("../../target").join(format!("{}_bin", src_name));

    let status = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg(&src_path).arg("--emit-ir").arg("-o").arg(&out_ll)
        .status().expect("cato run");
    assert!(status.success(), "cato compile failed");

    let cc = Command::new("clang")
        .arg(&out_ll).arg(&rt).arg("-o").arg(&exe)
        .status().expect("clang link");
    assert!(cc.success(), "clang link failed");

    let out = Command::new(&exe).output().expect("run binary");
    assert!(out.status.success(), "binary failed");
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn compiles_normalize_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(manifest.join("../../examples/normalize.cato")).unwrap();
    assert_eq!(compile_and_run("normalize", &src), "0.75\n");
}

#[test]
fn compiles_count_even_below_to_expected_output() {
    let src = r#"
(defn count-even-below [xs :- vec n :- i64] :- i64
  (reduce (fn [acc x] (if (and (even? x) (< x n)) (+ acc 1) acc)) 0 xs))
(defn main []
  (print-i64 (count-even-below (vec-i64 1 2 3 4 5 6) 6)))
"#;
    assert_eq!(compile_and_run("count_even_below", &src), "2\n");
}

#[test]
fn compiles_shape_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(manifest.join("../../examples/shape.cato")).unwrap();
    assert_eq!(compile_and_run("shape", &src), "19.635\n12\n16\n4\n");
}

#[test]
fn compiles_collections_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(manifest.join("../../examples/collections.cato")).unwrap();
    assert_eq!(
        compile_and_run("collections", &src),
        "35\n9\n140\n64\n42\n3\n5\n10\n30\n"
    );
}

#[test]
fn compiles_generic_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(manifest.join("../../examples/generic.cato")).unwrap();
    assert_eq!(
        compile_and_run("generic", &src),
        "10\n2.5\n7.25\n9\n2.5\n50\n"
    );
}

#[test]
fn compiles_own_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(manifest.join("../../examples/own.cato")).unwrap();
    assert_eq!(compile_and_run("own", &src), "123\n3\n7\n");
}

#[test]
fn compiles_strings_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(manifest.join("../../examples/strings.cato")).unwrap();
    assert_eq!(
        compile_and_run("strings", &src),
        "hello, cato\n4\n1\n2\n0\n10\n"
    );
}

#[test]
fn compiles_heap_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(manifest.join("../../examples/heap.cato")).unwrap();
    assert_eq!(
        compile_and_run("heap", &src),
        "3\n8\n30\n4\n10\nheapalloc\n9\n3\n9\n"
    );
}

#[test]
fn compiles_maps_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(manifest.join("../../examples/maps.cato")).unwrap();
    assert_eq!(
        compile_and_run("maps", &src),
        "20\n3\n99\n99\n20\n3\n70\n4\n0\n-1\n"
    );
}

#[test]
fn compiles_widths_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(manifest.join("../../examples/widths.cato")).unwrap();
    assert_eq!(compile_and_run("widths", &src), "7\n30\n10\n42\n");
}

#[test]
fn explain_prints_diagnostic_docs() {
    let out = Command::new(env!("CARGO_BIN_EXE_cato"))
        .args(["--explain", "E301"])
        .output()
        .expect("run binary");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("E301"), "got: {}", stdout);
    assert!(stdout.contains("moved"), "got: {}", stdout);
}

#[test]
fn explain_rejects_unknown_code() {
    let out = Command::new(env!("CARGO_BIN_EXE_cato"))
        .args(["--explain", "E999"])
        .output()
        .expect("run binary");
    assert!(!out.status.success());
}

// Phase 11: sets & string building, end-to-end.
#[test]
fn compiles_sets_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("../../examples/sets.cato");
    let out_ll = manifest.join("../../target/sets.ll");
    let rt = manifest.join("../../runtime/runtime.c");
    let exe = manifest.join("../../target/sets_bin");

    let status = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg(&src).arg("--emit-ir").arg("-o").arg(&out_ll)
        .status().expect("cato run");
    assert!(status.success(), "cato compile failed");

    let cc = Command::new("clang")
        .arg(&out_ll).arg(&rt).arg("-o").arg(&exe)
        .status().expect("clang link");
    assert!(cc.success(), "clang link failed");

    let out = Command::new(&exe).output().expect("run binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout,
        "3\n1\n0\n3\n0\n1\n3\nb\na-b-c\nb-\n",
        "sets.cato output"
    );
}

#[test]
fn subs_split_join_edge_cases() {
    let out = compile_and_run(
        "strings2.cato",
        r#"(defn main []
  (let [parts (split "x,,y" ",")
        one (split "abc" ",")
        j (join "" one)]
    (do
      (print-i64 (count parts))      ; 3 (empty middle part kept)
      (print-string (get parts 1))   ; (empty)
      (print-i64 (count one))        ; 1
      (print-string j)               ; abc
      (print-string (subs "hello" 1 4))  ; ell
      (print-string (subs "hi" 1 1)) ; (empty)
      (print-string (get parts 9 "?"))))) ; ? (out of bounds default)
"#,
    );
    assert_eq!(out, "3\n\n1\nabc\nell\n\n?\n");
}

// Phase 12: remaining widths, end-to-end.
#[test]
fn compiles_widths2_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("../../examples/widths2.cato");
    let out_ll = manifest.join("../../target/widths2.ll");
    let rt = manifest.join("../../runtime/runtime.c");
    let exe = manifest.join("../../target/widths2_bin");

    let status = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg(&src).arg("--emit-ir").arg("-o").arg(&out_ll)
        .status().expect("cato run");
    assert!(status.success(), "cato compile failed");

    let cc = Command::new("clang")
        .arg(&out_ll).arg(&rt).arg("-o").arg(&exe)
        .status().expect("clang link");
    assert!(cc.success(), "clang link failed");

    let out = Command::new(&exe).output().expect("run binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout,
        "44\n99\n-56\n4294967295\n1\n0\n4464\n0.1\n7\n2.5\n",
        "widths2.cato output"
    );
}

// Phase 13: scope-exit frees — heap bindings are released, with no
// double-frees or use-after-frees (checked under AddressSanitizer).
#[test]
fn scope_exit_frees_are_sanitizer_clean() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let rt = manifest.join("../../runtime/runtime.c");
    for name in ["heap", "maps", "sets", "slice"] {
        let src = manifest.join("../../examples").join(format!("{}.cato", name));
        let out_ll = manifest.join("../../target").join(format!("frees_{}.ll", name));
        let exe = manifest.join("../../target").join(format!("frees_{}_bin", name));

        let status = Command::new(env!("CARGO_BIN_EXE_cato"))
            .arg(&src).arg("--emit-ir").arg("-o").arg(&out_ll)
            .status().expect("cato run");
        assert!(status.success(), "{}: cato compile failed", name);

        let cc = Command::new("clang")
            .args(["-fsanitize=address"])
            .arg(&out_ll).arg(&rt).arg("-o").arg(&exe)
            .status().expect("clang link");
        assert!(cc.success(), "{}: clang failed", name);

        let out = Command::new(&exe).output().expect("run binary");
        assert!(out.status.success(), "{}: binary failed", name);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!stderr.contains("AddressSanitizer"), "{}: sanitizer error:\n{}", name, stderr);
    }
}

// Phase 13: frees are annotated in HIR for non-escaping bindings.
// (Compiled with the optimizer off: fold + copy-prop would collapse these
// counts into constants and eliminate the bindings under test.)
#[test]
fn non_escaping_bindings_get_free_annotations() {
    let program = cato_core::compile_named_with(
        "frees.cato",
        "(defn main []
           (let [m {1 2}
                 s #{1 2}
                 v (vec-i64 1 2)]
             (do (print-i64 (count m)) (print-i64 (count s)) (print-i64 (count v)))))",
        false,
    )
    .expect("compile");
    let main = program.defns.iter().find(|d| d.name == "main").unwrap();
    fn lets(e: &cato_core::hir::HExpr) -> Vec<&Vec<String>> {
        use cato_core::hir::HExpr;
        match e {
            HExpr::Let(_, body, frees) => {
                let mut all = vec![frees];
                all.extend(lets(body));
                all
            }
            other => {
                let _ = other;
                Vec::new()
            }
        }
    }
    let frees: Vec<&Vec<String>> = lets(&main.body);
    assert!(!frees.is_empty(), "expected annotated lets");
    let names: Vec<String> = frees.iter().flat_map(|f| f.iter().cloned()).collect();
    assert!(names.contains(&"m".to_string()), "map binding should be freed");
    assert!(names.contains(&"s".to_string()), "set binding should be freed");
}

// Phase 18: `(parallel (reduce ...))` — chunked across pthreads, same
// result as the sequential loop at every thread count.
#[test]
fn parallel_reduce_matches_sequential() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("../../examples/parallel.cato");
    let rt = manifest.join("../../runtime/runtime.c");
    let mut outputs = Vec::new();
    for t in ["1", "2", "4", "8"] {
        let out_ll = manifest.join("../../target/par_test.ll");
        let exe = manifest.join("../../target/par_test_bin");
        let status = Command::new(env!("CARGO_BIN_EXE_cato"))
            .arg(&src).args(["--threads", t]).arg("--emit-ir").arg("-o").arg(&out_ll)
            .status().expect("cato run");
        assert!(status.success(), "cato compile failed");
        let cc = Command::new("clang")
            .arg(&out_ll).arg(&rt).arg("-o").arg(&exe)
            .status().expect("clang link");
        assert!(cc.success(), "clang link failed");
        let out = Command::new(&exe).output().expect("run binary");
        assert!(out.status.success(), "binary failed at {} threads", t);
        outputs.push(String::from_utf8_lossy(&out.stdout).to_string());
    }
    // Sequential first line and every parallel line agree at all counts.
    for o in &outputs[1..] {
        assert_eq!(o, &outputs[0], "thread-count-dependent output");
    }
    assert_eq!(
        outputs[0], "1275\n1275\n-3258495067890909184\n42925\n",
        "parallel reduce values"
    );
}

#[test]
fn parallel_rejects_unqualified_reduces() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bad = manifest.join("../../target/par_bad.cato");
    std::fs::write(&bad, "(defn main [] :- i64 (parallel (reduce (fn [a x] (- a x)) 0 (vec-i64 1 2 3))))").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg(&bad).output().expect("run binary");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("identity init"), "got: {}", stderr);
}

// Phase 19: the REPL evaluates expressions and defines functions.
#[test]
fn repl_evaluates_forms() {
    use std::io::Write as _;
    let mut child = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg("repl")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn repl");
    child
        .stdin.as_mut().unwrap()
        .write_all(b"(defn sq [x :- i64] :- i64 (* x x))\n(sq 12)\n(let [[a b] (vec-i64 3 4)] (+ a b))\n:quit\n")
        .expect("write repl");
    let out = child.wait_with_output().expect("repl output");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("#'user/sq"), "got: {}", stdout);
    assert!(stdout.contains("user=> "), "clojure-style prompt: {}", stdout);
    assert!(stdout.contains("\n144"), "got: {}", stdout);
    assert!(stdout.contains("\n7"), "got: {}", stdout);
}
