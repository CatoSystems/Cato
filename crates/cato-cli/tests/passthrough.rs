//! Driver composition contract (spec §1/§5): parsed options, then `--`
//! passthrough, then driver-owned inputs — every argv assembled by
//! `driver::link_invocations`, captured through a fake `--cc` whose path
//! contains a SPACE (proves argv arrays, not shell strings).

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Install a fake cc at `target/fake cc/cc` that dumps its argv, one per
/// line, to $CATO_ARGCAP.
fn fake_cc() -> PathBuf {
    let dir = root().join("target/fake cc");
    std::fs::create_dir_all(&dir).expect("mkdir fake cc dir");
    let cc = dir.join("cc");
    std::fs::write(
        &cc,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$CATO_ARGCAP\"\nexit 0\n",
    )
    .expect("write fake cc");
    let mut perm = std::fs::metadata(&cc).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&cc, perm).expect("chmod fake cc");
    cc
}

/// Compile with the fake cc and return the captured argv lines.
fn compile_capture(src: &std::path::Path, extra: &[&str]) -> Vec<String> {
    let cc = fake_cc();
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let cap = root().join(format!(
        "target/fake_cc_argv_{}.txt",
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let out = root().join("target/fake_out");
    let status = Command::new(root().join("target/debug/cato"))
        .arg("compile")
        .arg(src)
        .arg("-o")
        .arg(&out)
        .arg("--cc")
        .arg(&cc)
        .args(extra)
        .env("CATO_ARGCAP", &cap)
        .status()
        .expect("cato compile");
    assert!(status.success(), "compile with fake cc failed");
    let text = std::fs::read_to_string(&cap).expect("read captured argv");
    text.lines().map(|l| l.to_string()).collect()
}

#[test]
fn parsed_options_then_passthrough_then_inputs() {
    let src = root().join("examples/slice.cato");
    let args = compile_capture(&src, &["-g", "--", "-DEXTRA"]);
    let ll_idx = args.iter().position(|a| a.ends_with(".ll")).expect("ll input");
    let rt_idx = args.iter().position(|a| a.ends_with("runtime.c")).expect("runtime input");
    let o_idx = args.iter().position(|a| a == "-o").expect("-o");
    // Parsed options in contract order (-O, -g, -rtlib), then
    // passthrough, then inputs, -o last.
    assert_eq!(&args[ll_idx - 4..ll_idx], ["-O2", "-g", "-rtlib=compiler-rt", "-DEXTRA"],
        "args: {args:?}");
    assert_eq!(rt_idx, ll_idx + 1, "runtime right after ll: {args:?}");
    assert_eq!(o_idx + 2, args.len(), "-o out last: {args:?}");
    assert_eq!(args[o_idx + 1], root().join("target/fake_out").display().to_string());
}

#[test]
fn passthrough_rtlib_replaces_parsed_default() {
    let src = root().join("examples/slice.cato");
    let args = compile_capture(&src, &["--", "--rtlib=libgcc"]);
    assert!(args.contains(&"--rtlib=libgcc".to_string()), "args: {args:?}");
    assert!(!args.contains(&"-rtlib=compiler-rt".to_string()), "args: {args:?}");
}

#[test]
fn parsed_rtlib_flag_reaches_argv() {
    let src = root().join("examples/slice.cato");
    let args = compile_capture(&src, &["--rtlib", "myrt"]);
    assert!(args.contains(&"-rtlib=myrt".to_string()), "args: {args:?}");
    assert!(!args.contains(&"-rtlib=compiler-rt".to_string()), "args: {args:?}");
}

#[test]
fn passthrough_opt_level_replaces_parsed() {
    let src = root().join("examples/slice.cato");
    let args = compile_capture(&src, &["--", "-O3"]);
    assert!(args.contains(&"-O3".to_string()), "args: {args:?}");
    assert!(!args.contains(&"-O2".to_string()), "args: {args:?}");
}

#[test]
fn runtime_flag_overrides_default_runtime_source() {
    let src = root().join("examples/slice.cato");
    let custom = root().join("target/fake_rt.c");
    std::fs::write(&custom, "/* custom runtime */\n").expect("write custom rt");
    let cc = fake_cc();
    let cap = root().join("target/fake_cc_argv_runtime.txt");
    let out = root().join("target/fake_out");
    let status = Command::new(root().join("target/debug/cato"))
        .arg("compile")
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .arg("--cc")
        .arg(&cc)
        .arg("--runtime")
        .arg(&custom)
        .env("CATO_ARGCAP", &cap)
        .status()
        .expect("cato compile");
    assert!(status.success(), "compile with fake cc failed");
    let text = std::fs::read_to_string(&cap).expect("read captured argv");
    let argv: Vec<&str> = text.lines().collect();
    assert!(
        argv.iter().any(|a| *a == custom.display().to_string()),
        "--runtime source must appear in argv:\n{text}"
    );
    assert!(
        !argv.iter().any(|a| a.ends_with("runtime/runtime.c")),
        "default runtime must not appear when --runtime given:\n{text}"
    );
}

// A native cross target probes the cc's triple support first; a cc that
// cannot compile for it fails with exit 1 before any link argv runs.
#[test]
fn cross_target_probe_rejects_unsupported_cc() {
    let dir = root().join("target/fake cc");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let bad = dir.join("no-cross-cc");
    // Fails the empty-file probe: rejects any --target=… -c invocation.
    std::fs::write(&bad, "#!/bin/sh\nexit 1\n").expect("write");
    let mut perm = std::fs::metadata(&bad).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&bad, perm).expect("chmod");

    let src = root().join("examples/slice.cato");
    let out = root().join("target/fake_out");
    let result = Command::new(root().join("target/debug/cato"))
        .arg("compile")
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .arg("--cc")
        .arg(&bad)
        .arg("--target")
        .arg("x86_64-linux")
        .output()
        .expect("cato compile");
    assert_eq!(result.status.code(), Some(1), "tool failure is exit 1");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("does not support target"), "stderr: {stderr}");
    assert!(stderr.contains("x86_64-unknown-linux-gnu"), "stderr: {stderr}");
}
