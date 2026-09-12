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
        .arg(&src).arg("-o").arg(&out_ll)
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
        .arg(&src_path).arg("-o").arg(&out_ll)
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
