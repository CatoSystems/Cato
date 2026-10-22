//! Native link setup for the hermetic (in-process lld) build.
//!
//! Three groups of static libraries end up in the final binary: the shim
//! compiled from `csrc/lld_shim.cpp`, lld's own component archives, and
//! LLVM's component archives. Their order matters: linkers are one-pass, so
//! an archive that references symbols defined by a later archive only
//! resolves when that later archive is scanned afterwards. lld's drivers
//! reference LLVM, therefore lld comes first and LLVM second.
//!
//! The platform differences are handled here rather than in the callers:
//!
//!   * the llvm-config binary is `llvm-config.exe` on Windows;
//!   * `llvm-config --libs` prints `-lfoo` on Unix but absolute paths on
//!     MSVC, so the library names come from `--libnames` (bare file names on
//!     both platforms) and are normalised to the names rustc's `static=`
//!     expects;
//!   * LLVM is built without exceptions and without RTTI, and the shim must
//!     be compiled the same way or the two disagree about class layout. On
//!     MSVC that means `/EHs-c-` and `/GR-`; cl.exe does not understand the
//!     `-fno-exceptions` / `-fno-rtti` spellings used on Unix.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// lld's component archives. Each target flavour has its own driver
/// archive; the helpers they share live in lldCommon.
const LLD_LIBS: &[&str] = &["lldELF", "lldCOFF", "lldMachO", "lldMinGW", "lldCommon"];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=csrc/lld_shim.cpp");
    println!("cargo:rerun-if-env-changed=LLVM_SYS_231_PREFIX");
    if !cfg!(feature = "link") {
        return;
    }
    let prefix = PathBuf::from(
        env::var("LLVM_SYS_231_PREFIX")
            .expect("LLVM_SYS_231_PREFIX must be set; run tools/build_toolchain.sh or set it"),
    );
    let llvm_config = find_llvm_config(&prefix);
    let include = prefix.join("include");
    let libdir = prefix.join("lib");

    compile_shim(&include);
    println!("cargo:rustc-link-search=native={}", libdir.display());

    // lld first, then LLVM: the linkers scan archives once, in order.
    for lib in LLD_LIBS {
        println!("cargo:rustc-link-lib=static={lib}");
    }
    for lib in llvm_component_libs(&llvm_config) {
        println!("cargo:rustc-link-lib=static={lib}");
    }
    // The system libraries come last: they are the ones LLVM's archives
    // reference, and a shared library may also be referenced by the shim.
    for lib in system_libs(&llvm_config) {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
    for lib in cpp_runtime_libs() {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
}

/// Compile the C++ shim that exposes lld's drivers to Rust.
///
/// The shim is compiled with the same exception and RTTI settings as the
/// LLVM it links against: LLVM defaults to both being off, and mixing the
/// two settings produces mismatched vtables.
fn compile_shim(include: &Path) {
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .file("csrc/lld_shim.cpp")
        .include(include)
        .include(include.join("lld"));
    if build.get_compiler().is_like_msvc() {
        build
            .flag("/EHs-c-")
            .flag("/GR-")
            .define("_HAS_EXCEPTIONS", "0")
            .define("_CRT_SECURE_NO_WARNINGS", None);
    } else {
        build.flag("-fno-exceptions").flag("-fno-rtti");
    }
    build.compile("lld_shim");
}

/// Find the llvm-config of the toolchain prefix.
///
/// Windows installs it as `llvm-config.exe`; Unix keeps the extensionless
/// name.
fn find_llvm_config(prefix: &Path) -> PathBuf {
    let bin = prefix.join("bin");
    for name in ["llvm-config", "llvm-config.exe"] {
        let candidate = bin.join(name);
        if candidate.is_file() {
            return candidate;
        }
    }
    panic!(
        "llvm-config not found in {}; run tools/build_toolchain.sh to build vendor/llvm-prefix",
        bin.display()
    );
}

/// Run llvm-config and return its standard output.
fn llvm_config_output(llvm_config: &Path, args: &[&str]) -> String {
    let out = Command::new(llvm_config)
        .args(args)
        .output()
        .expect("failed to run llvm-config");
    assert!(
        out.status.success(),
        "llvm-config {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The names of LLVM's static component libraries.
///
/// `--libnames` prints the file names without a directory and without a
/// leading `-l`, on every platform; `--libs` would print `-lfoo` on Unix and
/// absolute paths on MSVC.
fn llvm_component_libs(llvm_config: &Path) -> Vec<String> {
    let out = llvm_config_output(llvm_config, &["--libnames", "--link-static"]);
    out.split_whitespace()
        .map(component_lib_name)
        .filter(|name| !name.is_empty())
        .collect()
}

/// Turn a library file name into the name rustc's `static=` expects:
/// `libLLVMCore.a` and `LLVMCore.lib` both become `LLVMCore`.
fn component_lib_name(file_name: &str) -> String {
    let trimmed = file_name.strip_prefix("lib").unwrap_or(file_name);
    trimmed
        .strip_suffix(".a")
        .or_else(|| trimmed.strip_suffix(".lib"))
        .unwrap_or(trimmed)
        .to_string()
}

/// The system libraries LLVM needs: the platform's C runtime, the XML
/// parser, and whatever else LLVM was configured against.
///
/// llvm-config reports them as `-lfoo` on Unix and as `foo.lib` on MSVC.
fn system_libs(llvm_config: &Path) -> Vec<String> {
    let out = llvm_config_output(llvm_config, &["--system-libs", "--link-static"]);
    out.split_whitespace().filter_map(system_lib_name).collect()
}

/// Turn a system library reference into a bare name rustc can link.
///
/// `-lfoo` (Unix), `foo.lib` (MSVC) and `foo` all name the same library.
/// A reference that carries a directory is reported by llvm-config when a
/// library was configured by absolute path; the directory becomes a link
/// search path instead.
fn system_lib_name(token: &str) -> Option<String> {
    let token = token.strip_prefix("-l").unwrap_or(token);
    if token.is_empty() {
        return None;
    }
    if token.contains('/') || token.contains('\\') {
        let path = Path::new(token);
        let dir = path.parent()?;
        println!("cargo:rustc-link-search=native={}", dir.display());
        let file_name = path.file_name()?.to_string_lossy().into_owned();
        return Some(component_lib_name(&file_name));
    }
    Some(token.strip_suffix(".lib").unwrap_or(token).to_string())
}

/// The C++ runtime LLVM was built against.
///
/// MSVC has no separate C++ runtime library: the C++ standard library
/// ships inside the CRT that the compiler links in by default.
fn cpp_runtime_libs() -> &'static [&'static str] {
    #[cfg(target_os = "macos")]
    {
        &["c++"]
    }
    #[cfg(target_os = "linux")]
    {
        &["stdc++"]
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        &[]
    }
}
