//! S4: `selfhost/cato/lib/` is ordinary Cato built by the seed. From this
//! phase onward the seed's suite guards kernel source: the reference driver
//! (`lib/stdlib_ref.cato`, which `require`s the sibling lib modules) runs
//! byte-identically on the interpreter and the clang-compiled LLVM backend.

use std::process::Command;

#[test]
fn seed_builds_the_kernel_stdlib_and_both_backends_agree() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.parent().unwrap().parent().unwrap();
    let cato = root.join("target/debug/cato");
    let rt = root.join("runtime/runtime.c");

    // The reference driver just `require`s its siblings; the loader pulls
    // them in. `stdlib_ref.cato` defines the only `main`.
    let entry = root.join("selfhost/cato/lib/stdlib_ref.cato");

    // The seed must compile it outright (interpreter).
    let out = Command::new(&cato)
        .arg("run")
        .arg(&entry)
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
        .arg(&entry)
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
