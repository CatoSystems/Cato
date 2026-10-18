//! S7: the kernel LLVM emitter — the full pipeline through native code.
//!
//! For each fixture: the kernel driver (read → expand → build → moveck →
//! emit) produces LLVM IR text; `clang` compiles it with runtime.c; the
//! binary runs and its stdout must byte-match `cato run` of the same
//! fixture through the seed interpreter. `host.cato`'s stdout is exempt
//! from the comparison (the interp's `(args)` excludes argv[0] by S3
//! design; the native binary sees the full argv — kernel-CLI argv
//! parity is S8's driver work) but must still succeed. Error fixtures
//! are excluded: out-of-contract input never reaches emission.

use std::process::Command;

fn setup() -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.parent().unwrap().parent().unwrap().to_path_buf();
    let cato = root.join("target/debug/cato");

    let entry = root.join("selfhost/cato/emit/emit_ref.cato");

    (root, entry, cato)
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else if p.extension().map(|x| x == "cato").unwrap_or(false) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn kernel_emitted_ir_runs_and_matches_the_seed_interpreter() {
    let (root, entry, cato) = setup();
    let rt = root.join("runtime/runtime.c");
    let fixtures = walk(&root.join("selfhost/cato/hir/fixtures"));
    let mut ran = 0;

    for file in &fixtures {
        let name = file.file_stem().unwrap().to_string_lossy().to_string();
        if name.starts_with("err_") {
            continue;
        }

        // 1. The kernel emits IR for the fixture.
        let out = Command::new(root.join("target/debug/cato-interp-run"))
            .arg(&entry)
            .arg(file)
            .arg("emit")
            .output()
            .expect("kernel emit run");
        assert!(
            out.status.success(),
            "kernel driver failed on {}: {}",
            file.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        let ir = String::from_utf8_lossy(&out.stdout).to_string();
        if ir.starts_with("err ") {
            // Out-of-contract fixture (build error) — no IR contract.
            continue;
        }
        assert!(ir.contains("define"), "no IR for {}", name);

        // 2. clang compiles it with the shared runtime. Dump-mode
        // fixtures (no `main`) get a compile-only check: valid IR, but
        // there is no entry point to link for either the binary or the
        // interp.
        let has_main = ir.contains("define i32 @main");
        let ll = root.join(format!("target/kernel_emit_{}.ll", name));
        let exe = root.join(format!("target/kernel_emit_{}_bin", name));
        std::fs::write(&ll, &ir).expect("write ll");
        let mut cc = Command::new("clang");
        if has_main {
            cc.arg(&ll).arg(&rt).arg("-o").arg(&exe);
        } else {
            cc.arg("-c").arg(&ll).arg("-o")
              .arg(root.join(format!("target/kernel_emit_{}.o", name)));
        }
        let cc = cc.output().expect("clang spawn");
        assert!(
            cc.status.success(),
            "clang rejected the kernel's IR for {}:\n{}",
            name,
            String::from_utf8_lossy(&cc.stderr)
        );

        // Dump-mode fixtures (no `main`) can be clang-checked but not run:
        // there is no entry point for either the binary or the interp.
        if !has_main {
            continue;
        }

        // 3. The binary runs.
        let bin = Command::new(&exe).output().expect("native run");
        assert!(
            bin.status.success(),
            "native binary failed for {}: {}",
            name,
            String::from_utf8_lossy(&bin.stderr)
        );

        // 4. Output parity with the seed interpreter over the fixture.
        let interp = Command::new(root.join("target/debug/cato-interp-run"))
            .arg(file)
            .output()
            .expect("interp run");
        assert!(interp.status.success(), "interp failed on {}", name);
        let interp_out = String::from_utf8_lossy(&interp.stdout).to_string();
        let native_out = String::from_utf8_lossy(&bin.stdout).to_string();
        if name == "host" {
            // argv[0] differences only (S3 interp excludes it); success is
            // the contract here.
        } else {
            assert_eq!(
                native_out, interp_out,
                "kernel-emitted binary disagrees with the seed interpreter on {}",
                name
            );
        }
        ran += 1;
    }
    assert!(ran > 5, "the emit differential covered only {} fixtures", ran);
}
