//! The `cato` compiler binary: wires the cato-core frontend pipeline to a
//! backend selected on the command line.

mod cli;
mod driver;
mod fmt;
#[cfg(feature = "static-llvm")]
mod hermetic;
mod lsp;
mod native;
#[cfg(feature = "static-llvm")]
mod objgen;
mod repl;
mod ui;

use cato_core::Backend;
use cato_llvm::LlvmBackend;
use cli::Cli;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|s| s.as_str()) == Some("repl") {
        no_args_subcommand(
            "repl",
            &args[1..],
            "cato repl — interactive session over the interpreter \
             (definitions accumulate; :quit or Ctrl-D exits)",
        );
        repl::run();
        return;
    }
    if args.first().map(|s| s.as_str()) == Some("lsp") {
        no_args_subcommand(
            "lsp",
            &args[1..],
            "cato lsp — language server over stdio (started by an editor, \
             not interactively)",
        );
        lsp::run();
        return;
    }
    if args.first().map(|s| s.as_str()) == Some("format") {
        fmt::run(&args[1..]);
        return;
    }
    // The front end — and the drop of the program it builds — recurses once
    // per nested form. Windows' main-thread stack is far smaller than Unix's
    // 8MB default, so a large program overflows it; do the work on a
    // big-stack thread instead. Errors still end the process from there.
    std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(move || compile(&args))
        .expect("spawn compiler thread")
        .join()
        .unwrap_or_else(|_| std::process::exit(1));
}

/// `cato compile <file> [-o out] [flags]` and `--emit-ir`: the front end and
/// the LLVM backend, run on the big-stack thread.
fn compile(args: &[String]) {
    let started = Instant::now();
    let cli = cli::parse_args(args);
    let src = std::fs::read_to_string(&cli.file).unwrap_or_else(|e| {
        eprintln!("{} cannot read {}: {}", ui::error_lead(), cli.file, e);
        std::process::exit(1);
    });
    let program = cato_core::compile_named_with(&cli.file, &src, !cli.no_opt).unwrap_or_else(|e| {
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
        // `--emit-ir --asan`: IR ready for `clang -fsanitize=address`.
        let ir = if cli.asan {
            cato_llvm::with_function_attribute(&ir, "sanitize_address")
        } else {
            ir
        };
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
            // Toolchain errors may carry their own `cato:` lead.
            let e = e.strip_prefix("cato: ").unwrap_or(&e);
            eprintln!("{} {}", ui::error_lead(), e);
            std::process::exit(1);
        }
    }
}

/// `repl` and `lsp` take no arguments: `--help`/`-h` describes the
/// subcommand, anything else is a usage error (exit 2) — never silently
/// ignored.
fn no_args_subcommand(name: &str, rest: &[String], help: &str) {
    match rest.first().map(|s| s.as_str()) {
        None => {}
        Some("--help" | "-h") => {
            println!("{help}");
            std::process::exit(0);
        }
        Some(other) => {
            eprintln!(
                "{} cato {name} takes no arguments (got '{other}')",
                ui::error_lead()
            );
            std::process::exit(2);
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
