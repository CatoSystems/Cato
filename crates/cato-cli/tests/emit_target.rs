//! Emission opt-in and target tests: IR is written only when asked for
//! (`--emit-ir`), and the wasm32 target produces modules that real LLVM
//! wasm codegen accepts.

use std::path::PathBuf;
use std::process::Command;

fn cato() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cato"))
}

fn manifest_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn demo_source() -> PathBuf {
    manifest_root().join("target/optin_demo.cato")
}

fn write_demo() -> PathBuf {
    let path = demo_source();
    std::fs::write(
        &path,
        "(defn sq [x :- i64] (* x x))\n(defn main [] (print (sq 7)))\n",
    )
    .expect("write demo");
    path
}

// `cato compile -o` keeps stdout clean; the timing summary goes to stderr.
#[test]
fn compile_check_mode_is_silent() {
    let src = write_demo();
    let bin = manifest_root().join("target/optin_bin_demo");
    let out = cato().arg("compile").arg(&src).arg("-o").arg(&bin).output().expect("run cato");
    assert!(out.status.success(), "compile failed");
    assert!(out.stdout.is_empty(), "compile must print nothing to stdout, got {:?}",
        String::from_utf8_lossy(&out.stdout));
    let _ = std::fs::remove_file(&bin);
}

// Errors still surface in compile mode.
#[test]
fn compile_check_mode_reports_errors() {
    let src = manifest_root().join("target/optin_bad.cato");
    std::fs::write(&src, "(defn main [] (print (+ 1 \"nope\")))").expect("write bad");
    let out = cato().arg("compile").arg(&src).output().expect("run cato");
    assert!(!out.status.success(), "type error must fail the compile");
}

// IR only on `--emit-ir`; `-o` without it names the artifact instead.
#[test]
fn ir_is_opt_in_via_emit_ir() {
    let src = write_demo();
    let out = cato().arg("compile").arg(&src).arg("--emit-ir").output().expect("run cato");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("define i32 @main"), "IR must contain main");
}

// The wasm32 target stamps the module with the wasm triple and layout.
#[test]
fn wasm_target_emits_wasm_module_header() {
    let src = write_demo();
    let out = cato().arg("compile").arg(&src).args(["--emit-ir", "--target", "wasm32"])
        .output().expect("run cato");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(r#"target triple = "wasm32-unknown-unknown""#));
    assert!(stdout.contains(r#"target datalayout = "e-m:e-p:32:32"#));

    // Native emission must NOT claim a wasm triple.
    let native = cato().arg("compile").arg(&src).arg("--emit-ir").output().expect("run cato");
    let stdout = String::from_utf8_lossy(&native.stdout);
    assert!(!stdout.contains("wasm32"), "native IR must not claim wasm32");
}

// The emitted wasm32 IR is real: LLVM's wasm codegen accepts it and emits
// a WebAssembly binary. Skipped where no LLVM toolchain with wasm support
// is installed (codegen is exercised via llc, no linker needed).
#[test]
fn wasm_ir_passes_through_llvm_wasm_codegen() {
    let llvm_bin = wasm_llvm_bin();
    let Some(llc) = llvm_bin.map(|p| p.join("llc")) else { return };
    if !llc.exists() {
        return;
    }
    let as_ = llc.with_file_name("llvm-as");
    if !as_.exists() {
        return;
    }

    let src = write_demo();
    let ir = manifest_root().join("target/optin_wasm.ll");
    let status = cato().arg("compile").arg(&src).args(["--emit-ir", "--target", "wasm32"])
        .arg("-o").arg(&ir)
        .status().expect("emit wasm ir");
    assert!(status.success(), "wasm IR emission failed");

    let bc = manifest_root().join("target/optin_wasm.bc");
    let obj = manifest_root().join("target/optin_wasm.o");
    let asm = Command::new(&as_).arg(&ir).arg("-o").arg(&bc).output().expect("llvm-as");
    assert!(asm.status.success(), "llvm-as failed: {}",
        String::from_utf8_lossy(&asm.stderr));
    let gen = Command::new(&llc).args(["-march=wasm32", "-filetype=obj"])
        .arg(&bc).arg("-o").arg(&obj).output().expect("llc");
    assert!(gen.status.success(), "llc wasm codegen failed: {}",
        String::from_utf8_lossy(&gen.stderr));

    let bytes = std::fs::read(&obj).expect("read wasm object");
    assert_eq!(&bytes[..4], b"\0asm", "output must be a WebAssembly binary");
}

// `cato compile --target wasm32` either links a real .wasm (toolchain with
// lld present) or fails with guidance. Never succeeds silently.
#[test]
fn wasm_build_links_or_explains() {
    if wasm_link_toolchain_present() {
        let src = write_demo();
        let out = manifest_root().join("target/optin_demo.wasm");
        let status = cato().arg("compile").arg("--target").arg("wasm32")
            .arg(&src).arg("-o").arg(&out)
            .status().expect("cato compile wasm");
        assert!(status.success(), "wasm build failed on a capable toolchain");
        let bytes = std::fs::read(&out).expect("read wasm");
        assert_eq!(&bytes[..4], b"\0asm");
    } else {
        let src = write_demo();
        let out = cato().arg("compile").arg("--target").arg("wasm32")
            .arg(&src).arg("-o").arg("/tmp/optin_demo.wasm")
            .output().expect("cato compile wasm");
        assert!(!out.status.success(), "wasm build must not silently no-op");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("wasm"), "error must mention wasm: {}", stderr);
    }
}

// An unknown --target names itself and lists the known targets.
#[test]
fn unknown_target_lists_known_names() {
    let src = write_demo();
    let out = cato().arg("compile").arg(&src).args(["--target", "powerpc"])
        .arg("--emit-ir").output().expect("run cato");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("powerpc"), "got: {}", stderr);
    assert!(stderr.contains("x86_64-linux"), "got: {}", stderr);
    assert!(stderr.contains("aarch64-macos"), "got: {}", stderr);
}

// Cross targets stamp the triple in the IR but let clang supply the
// data layout from it (no hardcoded layout for foreign triples).
#[test]
fn cross_target_emits_triple_in_ir() {
    let src = write_demo();
    let out = cato().arg("compile").arg(&src)
        .args(["--target", "aarch64-linux", "--emit-ir"])
        .output().expect("run cato");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(r#"target triple = "aarch64-unknown-linux-gnu""#),
        "got: {}", stdout
    );
    assert!(
        !stdout.contains("target datalayout"),
        "cross triple must not carry a datalayout: {}", stdout
    );
}

/// Homebrew LLVM, if installed (the only wasm32-capable toolchain on a
/// typical macOS dev box without wasi-sdk).
fn wasm_llvm_bin() -> Option<PathBuf> {
    for p in ["/opt/homebrew/opt/llvm/bin", "/usr/local/opt/llvm/bin"] {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn wasm_link_toolchain_present() -> bool {
    let ccs = [
        std::env::var("CATO_CC").unwrap_or_default(),
        "/opt/homebrew/opt/llvm/bin/clang".to_string(),
        "/usr/local/opt/llvm/bin/clang".to_string(),
        "clang".to_string(),
    ];
    for cc in ccs {
        if cc.is_empty() {
            continue;
        }
        let targets = Command::new(&cc).arg("--print-targets").output();
        let has_wasm = targets.map(|o| {
            String::from_utf8_lossy(&o.stdout).contains("wasm32")
        }).unwrap_or(false);
        if !has_wasm {
            continue;
        }
        let ld = Command::new(&cc).arg("-print-file-name=wasm-ld").output();
        let has_ld = ld.map(|o| {
            let s = String::from_utf8_lossy(&o.stdout);
            let s = s.trim();
            s != "wasm-ld" && std::path::Path::new(s).exists()
        }).unwrap_or(false);
        if has_ld {
            return true;
        }
    }
    false
}
