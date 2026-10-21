use std::env;
use std::path::PathBuf;

fn host_target_name() -> Option<&'static str> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("x86_64-linux")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("aarch64-linux")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("x86_64-macos")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("aarch64-macos")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("x86_64-windows")
    } else {
        None
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-check-cfg=cfg(cato_bundled_lld)");
    let has_static = env::var_os("CARGO_FEATURE_STATIC_LLVM").is_some();
    let has_inproc = env::var_os("CARGO_FEATURE_IN_PROCESS_LLD").is_some();
    if has_static && !has_inproc {
        let host = host_target_name().expect("hermetic build: unsupported host target");
        let mut path = PathBuf::from("../cato-assets/assets/lld")
            .join(host)
            .join("lld");
        if cfg!(target_os = "windows") {
            path.set_extension("exe");
        }
        if !path.exists() {
            panic!(
                "hermetic build needs the bundled lld — run tools/build_toolchain.sh && tools/gen_assets.sh (missing {:?})",
                path
            );
        }
        println!("cargo:rustc-cfg=cato_bundled_lld");
    }
}
