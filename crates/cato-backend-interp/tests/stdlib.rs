use std::process::Command;

#[test]
fn seed_builds_the_cato_stdlib_and_both_backends_agree() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.parent().unwrap().parent().unwrap();
    let cato = root.join("target/debug/cato");
    let rt = root.join("runtime/runtime.c");

    let entry = root.join("selfhost/cato/std/std_ref.cato");

    let out = Command::new(root.join("target/debug/cato-interp-run"))
        .arg(&entry)
        .output()
        .expect("interp run");
    assert!(
        out.status.success(),
        "seed failed on stdlib: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let interp = String::from_utf8_lossy(&out.stdout).to_string();

    let ll = root.join("target/stdlib.ll");
    let exe = root.join("target/stdlib_bin");
    let status = Command::new(&cato)
        .arg("compile")
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
        "stdlib: backend parity mismatch"
    );
}
