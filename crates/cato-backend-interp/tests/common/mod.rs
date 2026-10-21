//! Shared helpers for the interpreter/kernel integration tests.

use std::path::Path;
use std::process::Command;

/// Link emitted IR plus the C runtime into `exe` with clang.
///
/// Mirrors the seed driver's native link flags: glibc keeps math symbols in
/// libm, so ELF links must add `-lm`, while libSystem and the Windows CRT
/// already carry them. The flag sits with the inputs so `-o out` stays last.
pub fn link_ir(ll: &Path, rt: &Path, exe: &Path) {
    let mut cmd = Command::new("clang");
    cmd.arg(ll).arg(rt);
    if cfg!(target_os = "linux") {
        cmd.arg("-lm");
    }
    cmd.arg("-o").arg(exe);
    let status = cmd.status().expect("clang spawn");
    assert!(status.success(), "clang rejected the emitted IR");
}
