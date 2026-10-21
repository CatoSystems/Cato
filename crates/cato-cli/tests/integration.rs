// tests/integration.rs
use std::path::Path;
use std::process::Command;

/// Link emitted IR plus the C runtime into `exe`. `extra` carries
/// test-specific flags (sanitizers, opt levels) ahead of the inputs.
/// glibc keeps math symbols in libm, so ELF links add `-lm`; libSystem
/// and the Windows CRT already carry them.
fn link_ir(ll: &Path, rt: &Path, exe: &Path, extra: &[&str]) {
    let mut cmd = Command::new("clang");
    cmd.args(extra).arg(ll).arg(rt).arg("-o").arg(exe);
    if cfg!(target_os = "linux") {
        cmd.arg("-lm");
    }
    let status = cmd.status().expect("clang link");
    assert!(status.success(), "clang link failed");
}

/// Child stdout as text, with Windows CRLF normalized so expectations
/// can be written with plain LF.
fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

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
    let stdout = stdout_of(&out);
    assert!(stdout.contains("cato "), "got: {}", stdout);
}

// `cato compile` compiles through to a native executable.
#[test]
fn compile_produces_running_binary() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("../../examples/slice.cato");
    let bin = manifest.join("../../target/cli_build_demo");

    let status = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg("compile")
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .status()
        .expect("cato compile");
    assert!(status.success(), "compile failed");

    let out = Command::new(&bin).output().expect("run built binary");
    assert!(out.status.success(), "built binary failed");
    assert_eq!(stdout_of(&out), "2.5\n20\n");

    // Default output name: the input stem next to the source is not
    // required — with -o given, nothing else is created.
    let _ = std::fs::remove_file(&bin);
}

// The interpreter driver bin agrees with the natively compiled binary.
#[test]
fn run_native_matches_interpreter() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.parent().unwrap().parent().unwrap();
    let src = manifest.join("../../examples/slice.cato");
    let cato = env!("CARGO_BIN_EXE_cato");

    let interp = Command::new(root.join("target/debug/cato-interp-run"))
        .arg(&src)
        .output()
        .expect("interp");
    assert!(interp.status.success(), "interp failed");

    let bin = manifest.join("../../target/cli_runparity_bin");
    let status = Command::new(cato)
        .arg("compile")
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .status()
        .expect("cato compile");
    assert!(status.success(), "compile failed");
    let native = Command::new(&bin).output().expect("native run");
    assert!(
        native.status.success(),
        "native run failed: {}",
        String::from_utf8_lossy(&native.stderr)
    );

    assert_eq!(stdout_of(&interp), stdout_of(&native),);
    let _ = std::fs::remove_file(&bin);
}

// `--emit-ll` keeps the intermediate IR next to the native build.
#[test]
fn compile_keeps_ir_with_emit_ll() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("../../examples/slice.cato");
    let bin = manifest.join("../../target/cli_emitll_demo");
    let ll = manifest.join("../../target/cli_emitll_demo.ll");

    let status = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg("compile")
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .arg("--emit-ll")
        .arg(&ll)
        .status()
        .expect("cato compile");
    assert!(status.success(), "compile failed");

    let ir = std::fs::read_to_string(&ll).expect("emit-ll file");
    assert!(ir.contains("define i32 @main"), "IR must contain main");

    let _ = std::fs::remove_file(&bin);
    let _ = std::fs::remove_file(&ll);
}

// `-O` levels and `--asan` flow through to the C compiler.
// Hermetic (static-llvm/in-process-lld) builds reject --asan outright, so this
// only applies to the legacy cc-based path.
#[cfg(not(feature = "static-llvm"))]
#[test]
fn compile_honors_opt_level_and_asan_flags() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("../../examples/slice.cato");
    let bin = manifest.join("../../target/cli_flags_demo");
    let cato = env!("CARGO_BIN_EXE_cato");

    // Only these cases need no sanitizer runtime: Windows ships none, so
    // the ASan link there would produce a binary that cannot start.
    let cases: &[&[&str]] = if cfg!(target_os = "windows") {
        &[&["-O0"][..], &["--opt-level", "3"][..]]
    } else {
        &[&["-O0"][..], &["--opt-level", "3"][..], &["--asan"][..]]
    };
    for extra in cases {
        let status = Command::new(cato)
            .arg("compile")
            .arg(&src)
            .arg("-o")
            .arg(&bin)
            .args(*extra)
            .status()
            .expect("cato compile");
        assert!(status.success(), "compile with {:?} failed", extra);
        let out = Command::new(&bin).output().expect("run built binary");
        assert_eq!(stdout_of(&out), "2.5\n20\n");
    }
    let _ = std::fs::remove_file(&bin);
}

// The interpreter driver bin never touches a C compiler.
#[test]
fn interp_driver_needs_no_c_compiler() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.parent().unwrap().parent().unwrap();
    let src = manifest.join("../../examples/slice.cato");
    // The interpreter needs no C compiler: point CC at a missing binary and
    // the driver bin must still succeed.
    let out = Command::new(root.join("target/debug/cato-interp-run"))
        .arg(&src)
        .env("CC", "/no/such/compiler")
        .output()
        .expect("interp run");
    assert!(
        out.status.success(),
        "interp run must not need a C compiler"
    );
    assert_eq!(stdout_of(&out), "2.5\n20\n");
}

#[test]
fn cli_rejects_dangling_o_flag() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("../../examples/slice.cato");
    let out = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg("compile")
        .arg(&src)
        .args(["-o"])
        .output()
        .expect("run binary");
    assert!(
        !out.status.success(),
        "expected -o without filename to fail"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("-o requires a filename"), "got: {}", stderr);
}

#[test]
fn cli_rejects_unknown_flag() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("../../examples/slice.cato");
    let out = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg("compile")
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
        .arg("compile")
        .arg(&src)
        .arg("--emit-ir")
        .arg("-o")
        .arg(&out_ll)
        .status()
        .expect("cato compile");
    assert!(status.success(), "cato compile failed");

    link_ir(&out_ll, &rt, &exe, &[]);

    let out = Command::new(&exe).output().expect("run binary");
    let stdout = stdout_of(&out);
    assert_eq!(stdout, "2.5\n20\n");
}

fn compile_and_run(src_name: &str, src: &str) -> String {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src_path = manifest.join("../../target").join(src_name);
    std::fs::write(&src_path, src).expect("write source");
    let out_ll = manifest
        .join("../../target")
        .join(format!("{}.ll", src_name));
    let rt = manifest.join("../../runtime/runtime.c");
    let exe = manifest
        .join("../../target")
        .join(format!("{}_bin", src_name));

    let status = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg("compile")
        .arg(&src_path)
        .arg("--emit-ir")
        .arg("-o")
        .arg(&out_ll)
        .status()
        .expect("cato compile");
    assert!(status.success(), "cato compile failed");

    link_ir(&out_ll, &rt, &exe, &[]);

    let out = Command::new(&exe).output().expect("run binary");
    assert!(
        out.status.success(),
        "binary failed ({:?}):\n{}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    stdout_of(&out).to_string()
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
    assert_eq!(compile_and_run("own", &src), "123\n7\n");
}

#[test]
fn compiles_trees_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(manifest.join("../../examples/trees.cato")).unwrap();
    assert_eq!(compile_and_run("trees", &src), "17\n25\n");
}

#[test]
fn compiles_host_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(manifest.join("../../examples/host.cato")).unwrap();
    assert_eq!(
        compile_and_run("host", &src),
        "123\n-1\n2.5\n42 3.5 18\n15\n0\nnone\n"
    );
}

#[test]
fn compiles_loops_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(manifest.join("../../examples/loops.cato")).unwrap();
    assert_eq!(compile_and_run("loops", &src), "55\n6765\nababab\n35\n7\n");
}

#[test]
fn compiles_options_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(manifest.join("../../examples/options.cato")).unwrap();
    assert_eq!(compile_and_run("options", &src), "41\n7\n99\n1.5\n10\n1\n");
}

#[test]
fn compiles_strings2_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(manifest.join("../../examples/strings2.cato")).unwrap();
    assert_eq!(
        compile_and_run("strings2", &src),
        "1\n0\n1\nno\n1\n1\n0\n1\n99\n111\n0\n0\nC\n1\n1\n1\n1\n1\n"
    );
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
    let stdout = stdout_of(&out);
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
        .arg("compile")
        .arg(&src)
        .arg("--emit-ir")
        .arg("-o")
        .arg(&out_ll)
        .status()
        .expect("cato compile");
    assert!(status.success(), "cato compile failed");

    link_ir(&out_ll, &rt, &exe, &[]);

    let out = Command::new(&exe).output().expect("run binary");
    let stdout = stdout_of(&out);
    assert_eq!(
        stdout, "3\n1\n0\n3\n0\n1\n3\nb\na-b-c\nb-\n",
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
      (print-string (get parts 1 ""))   ; (empty)
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
        .arg("compile")
        .arg(&src)
        .arg("--emit-ir")
        .arg("-o")
        .arg(&out_ll)
        .status()
        .expect("cato compile");
    assert!(status.success(), "cato compile failed");

    link_ir(&out_ll, &rt, &exe, &[]);

    let out = Command::new(&exe).output().expect("run binary");
    let stdout = stdout_of(&out);
    assert_eq!(
        stdout, "44\n99\n-56\n4294967295\n1\n0\n4464\n0.1\n7\n2.5\n",
        "widths2.cato output"
    );
}

// Phase 13: scope-exit frees — heap bindings are released, with no
// double-frees or use-after-frees (checked under AddressSanitizer).
// The Windows CI toolchain ships no ASan runtime, so the instrumentation
// would link but not run; the Linux job is the leak-detecting one.
#[cfg(not(target_os = "windows"))]
#[test]
fn scope_exit_frees_are_sanitizer_clean() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let rt = manifest.join("../../runtime/runtime.c");
    for name in [
        "heap",
        "maps",
        "sets",
        "slice",
        "narrow_vectors",
        "intops",
        "closures_escape",
        "lending",
        "mem/ffi_own",
    ] {
        // (mem/ cases are covered by their own sanitizer tests below)
        let src = manifest
            .join("../../examples")
            .join(format!("{}.cato", name));
        let stem = name.replace('/', "_");
        let out_ll = manifest
            .join("../../target")
            .join(format!("frees_{}.ll", stem));
        let exe = manifest
            .join("../../target")
            .join(format!("frees_{}_bin", stem));

        let status = Command::new(env!("CARGO_BIN_EXE_cato"))
            .arg("compile")
            .arg(&src)
            .args(["--emit-ir", "--asan", "-o"])
            .arg(&out_ll)
            .status()
            .expect("cato compile");
        assert!(status.success(), "{}: cato compile failed", name);

        link_ir(&out_ll, &rt, &exe, &["-fsanitize=address"]);
        let out = Command::new(&exe).output().expect("run binary");
        assert!(
            out.status.success(),
            "{}: binary failed:\n{}",
            name,
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("AddressSanitizer"),
            "{}: sanitizer error:\n{}",
            name,
            stderr
        );
    }
}

// Ownership-driven destruction: a moved parameter's buffer must NOT be
// freed by the callee when ownership transferred (here: into a `let` that
// is returned). Under `--no-opt` the shape-based pre-S2 planner freed the
// returned box and the caller's next callee freed it again — a double-free
// caught by ASAN. The state-driven planner suppresses the moved param and
// frees exactly once (in the final consumer).
#[test]
fn compiles_options2_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(manifest.join("../../examples/options2.cato")).unwrap();
    assert_eq!(
        compile_and_run("options2", &src),
        "120\n-1\n-1\n201\n-1\n77\n6\n106\n"
    );
}

#[test]
fn compiles_strvec_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(manifest.join("../../examples/strvec.cato")).unwrap();
    assert_eq!(compile_and_run("strvec", &src), "3\n4\n5\nd\n!\na\n");
}

#[cfg(not(target_os = "windows"))]
#[test]
fn strvec_is_sanitizer_clean() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let rt = manifest.join("../../runtime/runtime.c");
    let src = manifest.join("../../examples/strvec.cato");
    let out_ll = manifest.join("../../target/strvec.ll");
    let exe = manifest.join("../../target/strvec_bin");

    let status = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg("compile")
        .arg(&src)
        .args(["--emit-ir", "--asan", "-o"])
        .arg(&out_ll)
        .status()
        .expect("cato compile");
    assert!(status.success(), "cato compile failed");

    link_ir(&out_ll, &rt, &exe, &["-fsanitize=address"]);

    let out = Command::new(&exe).output().expect("run binary");
    assert!(
        out.status.success(),
        "binary failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("AddressSanitizer"),
        "sanitizer error:\n{}",
        stderr
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn vec_field_adt_is_sanitizer_clean() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let rt = manifest.join("../../runtime/runtime.c");
    let src = manifest.join("../../examples/mem/vec_field_adt.cato");
    let out_ll = manifest.join("../../target/vec_field_adt.ll");
    let exe = manifest.join("../../target/vec_field_adt_bin");

    let status = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg("compile")
        .arg(&src)
        .args(["--emit-ir", "--asan", "-o"])
        .arg(&out_ll)
        .status()
        .expect("cato compile");
    assert!(status.success(), "cato compile failed");

    link_ir(&out_ll, &rt, &exe, &["-fsanitize=address"]);

    let out = Command::new(&exe).output().expect("run binary");
    assert!(
        out.status.success(),
        "binary failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout_of(&out), "41\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("AddressSanitizer"),
        "sanitizer error:\n{}",
        stderr
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn moved_param_return_is_sanitizer_clean_without_opt() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let rt = manifest.join("../../runtime/runtime.c");
    let src = manifest.join("../../examples/mem/moved_param_return.cato");
    let out_ll = manifest.join("../../target/moved_param_return.ll");
    let exe = manifest.join("../../target/moved_param_return_bin");

    let status = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg("compile")
        .arg(&src)
        .args(["--emit-ir", "--asan", "--no-opt", "-o"])
        .arg(&out_ll)
        .status()
        .expect("cato compile");
    assert!(status.success(), "cato compile failed");

    link_ir(&out_ll, &rt, &exe, &["-fsanitize=address"]);

    let out = Command::new(&exe).output().expect("run binary");
    assert!(
        out.status.success(),
        "binary failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout_of(&out), "41\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("AddressSanitizer"),
        "sanitizer error:\n{}",
        stderr
    );
}

// A move transfers the drop duty: the source binding frees nothing, the
// heir frees exactly once. Verifiable in the IR: exactly one drop call.
#[test]
fn move_transfers_the_drop_duty_exactly_once() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("../../examples/mem/moved_param_return.cato");
    let out_ll = manifest.join("../../target/moved_param_transfer.ll");

    let status = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg("compile")
        .arg(&src)
        .args(["--emit-ir", "--no-opt", "-o"])
        .arg(&out_ll)
        .status()
        .expect("cato compile");
    assert!(status.success(), "cato compile failed");

    let ir = std::fs::read_to_string(&out_ll).unwrap();
    let count = ir.matches("call void @drop__").count();
    assert_eq!(
        count, 1,
        "the returned box must be dropped exactly once (in the final consumer), IR:\n{}",
        ir
    );
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
            HExpr::Let(_, body, frees, _) => {
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
    assert!(
        names.contains(&"m".to_string()),
        "map binding should be freed"
    );
    assert!(
        names.contains(&"s".to_string()),
        "set binding should be freed"
    );
}

// Phase 18: `(parallel (reduce ...))` — chunked across the runtime's
// thread shim, same result as the sequential loop at every thread count.
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
            .arg("compile")
            .arg(&src)
            .args(["--threads", t])
            .arg("--emit-ir")
            .arg("-o")
            .arg(&out_ll)
            .status()
            .expect("cato compile");
        assert!(status.success(), "cato compile failed");
        link_ir(&out_ll, &rt, &exe, &[]);
        let out = Command::new(&exe).output().expect("run binary");
        assert!(out.status.success(), "binary failed at {} threads", t);
        outputs.push(stdout_of(&out).to_string());
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
    std::fs::write(
        &bad,
        "(defn main [] :- i64 (parallel (reduce (fn [a x] (- a x)) 0 (vec-i64 1 2 3))))",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg("compile")
        .arg(&bad)
        .output()
        .expect("run binary");
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
    let stdout = stdout_of(&out);
    assert!(stdout.contains("#'user/sq"), "got: {}", stdout);
    assert!(
        stdout.contains("user=> "),
        "clojure-style prompt: {}",
        stdout
    );
    assert!(stdout.contains("\n144"), "got: {}", stdout);
    assert!(stdout.contains("\n7"), "got: {}", stdout);
}

// Build `src` natively (extra flags appended) and run it.
fn build_and_run_native(name: &str, src: &str, extra: &[&str]) -> std::process::Output {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src_path = manifest.join("../../target").join(format!("{name}.cato"));
    std::fs::write(&src_path, src).expect("write source");
    let exe = manifest.join("../../target").join(format!("{name}_bin"));
    let status = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg("compile")
        .arg(&src_path)
        .arg("-o")
        .arg(&exe)
        .args(extra)
        .status()
        .expect("cato compile");
    assert!(status.success(), "cato compile failed for {name}");
    let out = Command::new(&exe).output().expect("run binary");
    let _ = std::fs::remove_file(&exe);
    out
}

// `(do s… e)` in i64 position yields `e` natively, as in the interpreter
// (the backend used to lower every such `do` to 0, so `(exit (f))` with
// `(defn f [] (do (print-string ..) 2))` exited 0).
#[test]
fn do_block_yields_its_last_value_natively() {
    let out = build_and_run_native(
        "do_tail_value",
        "(defn d [c :- str] :- i64 (do (print-string \"x\") 2))\n\
         (defn main [] (let [a (args)] (exit (d (get a 0 \"\")))))\n",
        &[],
    );
    assert_eq!(stdout_of(&out), "x\n");
    assert_eq!(
        out.status.code(),
        Some(2),
        "the do block's value is the exit status"
    );
}

// Re-binding an owned (Vector str) in the same let (`[a (split ..) a
// (conj a ..)]`) used to free the last buffer twice at scope exit.
#[cfg(all(not(feature = "static-llvm"), not(target_os = "windows")))]
#[test]
fn let_rebinding_an_owned_str_vec_frees_once() {
    let out = build_and_run_native(
        "let_rebind_str_vec",
        "(defn f [cc :- str] :- i64 (let [a (split cc \",\") a (conj a \"x\") a (conj a \"y\")] (count a)))\n\
         (defn main [] (let [a (args)] (print (f \"p,q\"))))\n",
        &["--asan"],
    );
    assert!(
        out.status.success(),
        "ASAN run failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout_of(&out), "4\n");
}

// The reduce accumulator's type comes from the init's real type, not its
// syntax: an f64 *variable* init accumulates in f64.
#[test]
fn reduce_with_f64_variable_init() {
    let src = "(defn total [x0 :- f64 xs :- (Vector f64)] :- f64 (reduce + x0 xs))\n\
               (defn main [] (print-f64 (total 0.5 [1.25 2.0])))\n";
    assert_eq!(compile_and_run("reduce_f64_var", src), "3.25\n");
}

#[test]
fn compiles_intops_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(manifest.join("../../examples/intops.cato")).unwrap();
    assert_eq!(
        compile_and_run("intops", &src),
        "-2\n3\n5\n-17\n-22\n-136\n-5\n32\n16\n2\n-3\n17\n-5\n-22\n136\n4\n576460752303423488\n-18\n22\n22\n1\n1234631\n1234630\n9876536\n308641\n2\n-1234568\n-2\n5\n-32\n-50\n99\n56\n4\n4\n64\n100\n55\n55\n1.5\n"
    );
}

#[test]
fn compiles_narrow_vectors_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(manifest.join("../../examples/narrow_vectors.cato")).unwrap();
    assert_eq!(
        compile_and_run("narrow_vectors", &src),
        "3\n250\n253\n4\n1000\n224500\n10\n240\n50\n3.75\n99993\n-2\n"
    );
}

// A (Vector u8) stores one byte per element: the literal is an i8 array
// and the elements load at byte alignment (an `align 8` claim on a byte
// array would be undefined behaviour).
#[test]
fn narrow_vectors_store_at_element_width() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("../../examples/narrow_vectors.cato");
    let out_ll = manifest.join("../../target/narrow_vectors_width.ll");
    let status = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg("compile")
        .arg(&src)
        .args(["--emit-ir", "--no-opt", "-o"])
        .arg(&out_ll)
        .status()
        .expect("cato compile");
    assert!(status.success());
    let ir = std::fs::read_to_string(&out_ll).unwrap();
    assert!(
        ir.contains("private constant [3 x i8] [i8 1, i8 2, i8 250]"),
        "u8 literal not narrow:\n{}",
        ir
    );
    assert!(ir.contains("load i8, ptr") && !ir.contains("load i8, ptr %t0, align 8"));
    assert!(
        !ir.lines()
            .any(|l| l.contains("load i8") && l.contains("align 8")),
        "byte load claims 8-byte alignment"
    );
}

// `recur` is a scope exit: every loop shape in loop_drops.cato frees what
// each iteration owned. Sanitizer-clean at -O0 (no double free, no use
// after free) and the right answers (the vector swap prints 21).
#[cfg(not(target_os = "windows"))]
#[test]
fn loop_drops_are_sanitizer_clean() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let rt = manifest.join("../../runtime/runtime.c");
    let src = manifest.join("../../examples/mem/loop_drops.cato");
    let out_ll = manifest.join("../../target/loop_drops.ll");
    let exe = manifest.join("../../target/loop_drops_bin");
    let status = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg("compile")
        .arg(&src)
        .args(["--emit-ir", "--asan", "-o"])
        .arg(&out_ll)
        .status()
        .expect("cato compile");
    assert!(status.success(), "cato compile failed");
    link_ir(&out_ll, &rt, &exe, &["-O0", "-fsanitize=address", "-w"]);
    let out = Command::new(&exe).output().expect("run binary");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("AddressSanitizer"),
        "sanitizer error:\n{}",
        stderr
    );
    assert!(
        out.status.success(),
        "binary failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout_of(&out), "345800\n21\n8\n");
}

// ...and leak-free: 200 rounds of every shape leave nothing live at exit
// beyond the C runtime's own startup allocations (stdio buffers, ~80 KB).
#[cfg(target_os = "macos")]
#[test]
fn mem_examples_leave_nothing_live() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../..");
    let dylib = root.join("target/alloc_live_test.dylib");
    let cc = Command::new("clang")
        .args(["-dynamiclib", "-O2", "-o"])
        .arg(&dylib)
        .arg(root.join("tools/alloc_live.c"))
        .status()
        .expect("build alloc_live");
    assert!(cc.success());
    for name in ["loop_drops", "ffi_own"] {
        let exe = root.join(format!("target/{}_live_bin", name));
        let status = Command::new(env!("CARGO_BIN_EXE_cato"))
            .arg("compile")
            .arg(root.join(format!("examples/mem/{}.cato", name)))
            .arg("-o")
            .arg(&exe)
            .status()
            .expect("cato compile");
        assert!(status.success());
        let out = Command::new(&exe)
            .env("DYLD_INSERT_LIBRARIES", &dylib)
            .output()
            .expect("run");
        let stderr = String::from_utf8_lossy(&out.stderr);
        let live: i64 = stderr
            .lines()
            .find_map(|l| {
                l.strip_prefix("LIVE_EXIT ")
                    .map(|v| v.trim().parse().unwrap())
            })
            .unwrap_or_else(|| panic!("{}: no LIVE_EXIT line:\n{}", name, stderr));
        assert!(
            live < 200_000,
            "{}: {} bytes still live at exit",
            name,
            live
        );
    }
}

#[test]
fn compiles_closures_escape_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src =
        std::fs::read_to_string(manifest.join("../../examples/closures_escape.cato")).unwrap();
    assert_eq!(compile_and_run("closures_escape", &src), "8\n111\n5\n");
}

// Vector values in value position: a slice returned directly, the
// argument vector counted in place, an out-of-range read of an empty
// vector (which must not load element 0).
#[test]
fn vector_values_in_value_position() {
    let src = "(defn cut [v :- &(Vector i64) n :- i64] :- (Vector i64) (subvec v 0 n))\n\
               (defn first-or [v :- &(Vector i64) d :- i64] :- i64 (get v 5 d))\n\
               (defn main [] (let [src (conj [7] 8) e (cut src 0) f (cut src 2)]\n\
                 (do (print-i64 (count (args))) (print-i64 (first-or e 42)) (print-i64 (count f)))))\n";
    assert_eq!(compile_and_run("vec_values", src), "0\n42\n2\n");
}

// Integer division by zero traps natively, as in the interpreter.
#[test]
fn integer_division_by_zero_traps() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("../../target/div0_trap.cato");
    std::fs::write(
        &src,
        "(defn d [a :- i64 b :- i64] :- i64 (/ a b))\n(defn main [] (print-i64 (d 7 0)))\n",
    )
    .unwrap();
    let exe = manifest.join("../../target/div0_trap_bin");
    let status = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg("compile")
        .arg(&src)
        .arg("-o")
        .arg(&exe)
        .status()
        .expect("cato compile");
    assert!(status.success());
    let out = Command::new(&exe).output().expect("run");
    assert!(!out.status.success(), "division by zero must not exit 0");
    assert!(String::from_utf8_lossy(&out.stderr).contains("integer division by zero"));
}

// `--asan` instruments the generated code itself: clang only instruments
// `.ll` functions that carry `sanitize_address`.
#[test]
fn asan_ir_carries_sanitize_address() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out_ll = manifest.join("../../target/asan_attr.ll");
    let status = Command::new(env!("CARGO_BIN_EXE_cato"))
        .arg("compile")
        .arg(manifest.join("../../examples/heap.cato"))
        .args(["--emit-ir", "--asan", "-o"])
        .arg(&out_ll)
        .status()
        .expect("cato compile");
    assert!(status.success());
    let ir = std::fs::read_to_string(&out_ll).unwrap();
    assert!(ir.contains("sanitize_address"));
    assert!(ir
        .lines()
        .filter(|l| l.starts_with("define "))
        .all(|l| l.contains("#9001")));
}

#[test]
fn compiles_lending_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(manifest.join("../../examples/lending.cato")).unwrap();
    assert_eq!(compile_and_run("lending", &src), "10\n10\n4\n4\n");
}

// Extern string results: borrowed by default, owned with `(own str)`, and a
// NULL `char*` reads as "" (getenv of an unset variable).
#[test]
fn compiles_ffi_own_to_expected_output() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = std::fs::read_to_string(manifest.join("../../examples/mem/ffi_own.cato")).unwrap();
    assert_eq!(compile_and_run("ffi_own", &src), "3100\n0\n");
}
