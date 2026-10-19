//! The `cato` compiler binary: wires the cato-core frontend pipeline to a
//! backend selected on the command line.

mod cli;
mod driver;
mod fmt;
mod lsp;
mod native;
#[cfg(feature = "static-llvm")]
mod objgen;
#[cfg(feature = "static-llvm")]
mod hermetic;
mod repl;
mod ui;

use cli::Cli;
use cato_core::Backend;
use cato_llvm::LlvmBackend;
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
    if args.first().map(|s| s.as_str()) == Some("format") {
        fmt::run(&args[1..]);
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

    // `--emit-ir` stops after IR emission (stdout or `-o`).
    if cli.emit_ir {
        let backend = llvm_backend(&cli);
        let ir = backend.emit(&program).unwrap_or_else(|e| {
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

    // Full compile: link a native/wasm artifact.
    let default = driver::default_out(cli.target, &default_stem(&cli.file));
    let out = cli.out.clone().unwrap_or(default);
    match native::build_native(&program, &cli, std::path::Path::new(&out)) {
        Ok(bin) => {
            eprintln!(
                "{} compiled {} in {}ms",
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
}

/// The LLVM backend configured from CLI flags.
fn llvm_backend(cli: &Cli) -> LlvmBackend {
    LlvmBackend {
        fast: cli.fast,
        threads: cli.threads,
        target: match (cli.target.flavor, cli.target.triple) {
            (driver::Flavor::Wasm, _) => cato_llvm::Target::Wasm32,
            (driver::Flavor::Native, Some(t)) => cato_llvm::Target::Triple(t),
            (driver::Flavor::Native, None) => cato_llvm::Target::Native,
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
