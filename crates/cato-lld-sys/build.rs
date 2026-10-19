use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=csrc/lld_shim.cpp");
    println!("cargo:rerun-if-env-changed=LLVM_SYS_231_PREFIX");
    if !cfg!(feature = "link") {
        return;
    }
    let prefix = env::var("LLVM_SYS_231_PREFIX")
        .expect("LLVM_SYS_231_PREFIX must be set; run tools/build_toolchain.sh or set it");
    let prefix = PathBuf::from(prefix);
    let llvm_config = prefix.join("bin").join("llvm-config");
    if !llvm_config.exists() {
        panic!(
            "llvm-config not found at {:?}; run tools/build_toolchain.sh to build vendor/llvm-prefix",
            llvm_config
        );
    }
    let include = prefix.join("include");
    let libdir = prefix.join("lib");
    let mut cc = cc::Build::new();
    cc.cpp(true)
        .std("c++17")
        .flag("-fno-exceptions")
        .flag("-fno-rtti")
        .file("csrc/lld_shim.cpp")
        .include(&include)
        .include(include.join("lld"));
    cc.compile("lld_shim");
    println!("cargo:rustc-link-search=native={}", libdir.display());
    for lib in &["lldELF", "lldCOFF", "lldMachO", "lldMinGW", "lldCommon"] {
        println!("cargo:rustc-link-lib=static={lib}");
    }
    let out = Command::new(&llvm_config)
        .arg("--link-static")
        .arg("--libs")
        .arg("--system-libs")
        .output()
        .expect("failed to run llvm-config");
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout);
        for tok in s.split_whitespace() {
            if let Some(rest) = tok.strip_prefix("-l") {
                if rest.is_empty() {
                    continue;
                }
                if rest == "dl"
                    || rest == "rt"
                    || rest == "pthread"
                    || rest == "m"
                    || rest == "stdc++"
                    || rest == "c++"
                    || rest == "c++abi"
                    || rest == "xml2"
                    || rest == "z"
                    || rest == "icuuc"
                    || rest == "icui18n"
                    || rest == "icudata"
                    || rest == "ncurses"
                    || rest == "tinfo"
                    || rest == "termcap"
                    || rest == "panel"
                    || rest == "ffi"
                {
                    continue;
                }
                println!("cargo:rustc-link-lib=static={rest}");
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rustc-link-lib=c++");
        println!("cargo:rustc-link-lib=xml2");
    }
    #[cfg(target_os = "linux")]
    {
        println!("cargo:rustc-link-lib=stdc++");
    }
}
