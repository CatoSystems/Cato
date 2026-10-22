use std::path::Path;

const COMMITTED: &[(&str, &[&str])] = &[
    (
        "x86_64-linux",
        &[
            "runtime.o",
            "crt1.o",
            "crti.o",
            "crtn.o",
            "libc.a",
            "libzigc.a",
            "libcompiler_rt.a",
        ],
    ),
    (
        "aarch64-linux",
        &[
            "runtime.o",
            "crt1.o",
            "crti.o",
            "crtn.o",
            "libc.a",
            "libzigc.a",
            "libcompiler_rt.a",
        ],
    ),
    (
        "x86_64-windows",
        &[
            "runtime.o",
            "crt2.o",
            "libmingw32.a",
            "libzigc.a",
            "libcompiler_rt.a",
            "advapi32.lib",
            "kernel32.lib",
            "ntdll.lib",
            "shell32.lib",
            "user32.lib",
            "api-ms-win-crt-conio-l1-1-0.lib",
            "api-ms-win-crt-convert-l1-1-0.lib",
            "api-ms-win-crt-environment-l1-1-0.lib",
            "api-ms-win-crt-filesystem-l1-1-0.lib",
            "api-ms-win-crt-heap-l1-1-0.lib",
            "api-ms-win-crt-locale-l1-1-0.lib",
            "api-ms-win-crt-math-l1-1-0.lib",
            "api-ms-win-crt-multibyte-l1-1-0.lib",
            "api-ms-win-crt-private-l1-1-0.lib",
            "api-ms-win-crt-process-l1-1-0.lib",
            "api-ms-win-crt-runtime-l1-1-0.lib",
            "api-ms-win-crt-stdio-l1-1-0.lib",
            "api-ms-win-crt-string-l1-1-0.lib",
            "api-ms-win-crt-time-l1-1-0.lib",
            "api-ms-win-crt-utility-l1-1-0.lib",
        ],
    ),
];

const GENERATED_MACOS: &[(&str, &[&str])] = &[
    ("x86_64-macos", &["runtime.o", "libSystem.tbd"]),
    ("aarch64-macos", &["runtime.o", "libSystem.tbd"]),
];

fn main() {
    println!("cargo:rerun-if-changed=assets");
    println!("cargo:rustc-check-cfg=cfg(cato_macos_assets)");

    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let assets = Path::new(&manifest).join("assets");

    for (target, files) in COMMITTED {
        for file in *files {
            let path = assets.join(target).join(file);
            if !path.is_file() {
                panic!(
                    "cato-assets: missing embedded asset {} — restore the committed assets (git checkout -- crates/cato-assets/assets) or rebuild them with tools/gen_assets.sh --regen-committed",
                    path.display()
                );
            }
        }
    }

    // macOS asset dirs are gitignored (Apple SDK license): present only after
    // tools/gen_assets.sh runs on a macOS host. Their absence must fail the
    // *requesting* compile, never fall back.
    let macos_ready = GENERATED_MACOS.iter().all(|(target, files)| {
        files
            .iter()
            .all(|file| assets.join(target).join(file).is_file())
    });
    if macos_ready {
        println!("cargo:rustc-cfg=cato_macos_assets");
    }
}
