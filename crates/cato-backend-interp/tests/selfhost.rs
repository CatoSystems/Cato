//! S4: `selfhost/cato/lib/` is ordinary Cato built by the seed. From this
//! phase onward the seed's suite guards kernel source: every lib file
//! compiles, and the concatenated reference driver runs byte-identically
//! on the interpreter and the clang-compiled LLVM backend.

use std::process::Command;

/// Canonical concatenation order (one namespace; no `require` in the
/// kernel subset until after the fixpoint). `stdlib_ref.cato` defines the
/// only `main` and must come last.
const LIB_FILES: &[&str] = &[
    "sym",
    "strbuf",
    "span",
    "ast",
    "alist",
    "result",
    "stdlib_ref",
];

fn lib_path(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    root.join("selfhost/cato/lib").join(format!("{}.cato", name))
}

#[test]
fn seed_builds_the_kernel_stdlib_and_both_backends_agree() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.parent().unwrap().parent().unwrap();
    let cato = root.join("target/debug/cato");
    let rt = root.join("runtime/runtime.c");

    // Every lib file must exist (typos in the canonical order are errors).
    for name in LIB_FILES {
        assert!(lib_path(root, name).exists(), "missing {}", name);
    }

    // One namespace, concatenated in canonical order.
    let mut src = String::new();
    for name in LIB_FILES {
        src.push_str(
            &std::fs::read_to_string(lib_path(root, name)).expect("read lib"),
        );
    }
    let scratch = root.join("target/selfhost_stdlib.cato");
    std::fs::write(&scratch, &src).expect("write concat");

    // The seed must compile it outright.
    let out = Command::new(&cato)
        .arg("run")
        .arg(&scratch)
        .output()
        .expect("interp run");
    assert!(
        out.status.success(),
        "seed failed on selfhost stdlib: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let interp = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(!interp.is_empty(), "driver printed nothing");

    // And the LLVM side must emit, compile, and agree byte for byte.
    let ll = root.join("target/selfhost_stdlib.ll");
    let exe = root.join("target/selfhost_stdlib_bin");
    let status = Command::new(&cato)
        .arg(&scratch)
        .arg("--emit-ir")
        .arg("-o")
        .arg(&ll)
        .status()
        .expect("cato emit");
    assert!(status.success(), "emit failed");
    let cc = Command::new("clang")
        .arg(&ll)
        .arg(&rt)
        .arg("-o")
        .arg(&exe)
        .status()
        .expect("clang");
    assert!(cc.success(), "clang rejected the emitted IR");
    let out = Command::new(&exe).output().expect("llvm run");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        interp,
        "selfhost stdlib: backend parity mismatch"
    );
}
