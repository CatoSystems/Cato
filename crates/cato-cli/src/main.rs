//! The `cato` compiler binary: wires the cato-core frontend pipeline to a
//! backend selected on the command line.

mod cli;
mod lsp;
mod native;
mod repl;
mod ui;

use cli::{Cli, Mode, Target};
use cato_core::Backend;
use cato_interp::InterpBackend;
use cato_llvm::{LlvmBackend, Target as LlvmTarget};
use std::time::Instant;

fn main() {
    let started = Instant::now();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|s| s.as_str()) == Some("repl") {
        repl::run();
        return;
    }
    if args.first().map(|s| s.as_str()) == Some("lsp") {
        lsp::run();
        return;
    }
    let cli = cli::parse_args(&args);
    let src = std::fs::read_to_string(&cli.file).unwrap_or_else(|e| {
        eprintln!("{} cannot read {}: {}", ui::error_lead(), cli.file, e);
        std::process::exit(1);
    });
    let program = cato_core::compile_named_with(&cli.file, &src, !cli.no_opt)
        .unwrap_or_else(|e| {
            eprintln!("{} {}", ui::error_lead(), e);
            std::process::exit(1);
        });
    if cli.dump_hir {
        eprintln!("{:#?}", program);
    }

    // `--backend interp` executes instead of compiling, exactly like `run`.
    let mode = match cli.mode {
        Mode::Compile if cli.backend == "interp" => Mode::RunInterp,
        other => other,
    };
    let result = match mode {
        // Compile check; IR text only on explicit `--emit-ir`.
        Mode::Compile => {
            if !cli.emit_ir {
                eprintln!(
                    "{} compiled {} in {}ms",
                    ui::check_mark(),
                    cli.file,
                    started.elapsed().as_millis()
                );
                return;
            }
            let backend = llvm_backend(&cli);
            let ir = backend
                .emit(&program)
                .unwrap_or_else(|e| {
                    eprintln!("{} backend error: {}", ui::error_lead(), e);
                    std::process::exit(1);
                });
            match &cli.out {
                Some(p) => std::fs::write(p, &ir).unwrap_or_else(|e| {
                    eprintln!("{} cannot write {}: {}", ui::error_lead(), p, e);
                    std::process::exit(1);
                }),
                None => print!("{}", ir),
            }
            return;
        }
        // Compile to a native executable and (for run) execute it.
        Mode::Build => {
            let default = match cli.target {
                Target::Wasm32 => format!("{}.wasm", default_stem(&cli.file)),
                Target::Native => default_stem(&cli.file),
            };
            let out = cli.out.clone().unwrap_or(default);
            match native::build_native(&program, &cli, std::path::Path::new(&out)) {
                Ok(bin) => {
                    eprintln!(
                        "{} built {} in {}ms",
                        ui::check_mark(),
                        bin.display(),
                        started.elapsed().as_millis()
                    );
                }
                Err(e) => {
                    eprintln!("{} {}", ui::error_lead(), e);
                    std::process::exit(1);
                }
            }
            return;
        }
        Mode::RunNative => {
            let bin = std::env::temp_dir().join(format!(
                "cato_run_{}{}",
                std::process::id(),
                std::env::consts::EXE_SUFFIX
            ));
            let bin = native::build_native(&program, &cli, &bin).unwrap_or_else(|e| {
                eprintln!("{} {}", ui::error_lead(), e);
                std::process::exit(1);
            });
            let code = native::run_binary(&bin).unwrap_or_else(|e| {
                eprintln!("{} {}", ui::error_lead(), e);
                std::process::exit(1);
            });
            let _ = std::fs::remove_file(&bin);
            std::process::exit(code);
        }
        Mode::RunInterp => {
            // The interpreter's artifact is the program output. Runtime
            // panics (malformed programs reaching interp invariants) are
            // reported as errors, not raw backtraces.
            //
            // The tree-walking interpreter recurses once per Cato call
            // frame, and debug-build eval frames are large — the 8MB main
            // stack dies around ~100 nested Cato calls. Run it on a thread
            // with a real stack (the same fix rustc applies for itself);
            // the reservation is virtual, so untouched pages cost nothing
            // and shallow programs behave exactly as before.
            let program_args = cli.interp_args.clone();
            let result = std::thread::Builder::new()
                .stack_size(512 * 1024 * 1024)
                .spawn(move || {
                    crate::ui::guard(|| InterpBackend { program_args }.emit(&program))
                })
                .expect("spawn interp thread")
                .join()
                .unwrap_or_else(|_| {
                    eprintln!("{} interp thread panicked", ui::error_lead());
                    std::process::exit(1);
                });
            result.unwrap_or_else(|e| {
                eprintln!("{} {}", ui::error_lead(), e);
                std::process::exit(1);
            })
        }
    };
    print!("{}", result);
}

/// The LLVM backend configured from CLI flags.
fn llvm_backend(cli: &Cli) -> LlvmBackend {
    LlvmBackend {
        fast: cli.fast,
        threads: cli.threads,
        target: match cli.target {
            Target::Native => LlvmTarget::Native,
            Target::Wasm32 => LlvmTarget::Wasm32,
        },
    }
}

/// `slice.cato` -> `slice`.
fn default_stem(file: &str) -> String {
    std::path::Path::new(file)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "a.out".to_string())
}
