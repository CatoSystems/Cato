//! The `cato` compiler binary: wires the cato-core frontend pipeline to a
//! backend selected on the command line.

mod cli;

use cato_core::{compile_named, Backend};
use cato_interp::InterpBackend;
use cato_llvm::LlvmBackend;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = cli::parse_args(&args);
    let src = std::fs::read_to_string(&cli.file).unwrap_or_else(|e| {
        eprintln!("cato: cannot read {}: {}", cli.file, e);
        std::process::exit(2);
    });
    let program =
        compile_named(&cli.file, &src).unwrap_or_else(|e| { eprintln!("cato: {}", e); std::process::exit(2); });
    let result = match cli.backend.as_str() {
        "llvm" => LlvmBackend { fast: cli.fast }.emit(&program),
        "interp" => InterpBackend.emit(&program),
        other => {
            eprintln!("cato: unknown backend '{}' (available: llvm, interp)", other);
            std::process::exit(2);
        }
    };
    let ir = result
        .unwrap_or_else(|e| { eprintln!("cato: backend error: {}", e); std::process::exit(2); });
    if cli.backend == "interp" {
        // The interpreter's artifact is the program output.
        print!("{}", ir);
        return;
    }
    match cli.out {
        Some(p) => std::fs::write(&p, &ir)
            .unwrap_or_else(|e| { eprintln!("cato: cannot write {}: {}", p, e); std::process::exit(2); }),
        None => print!("{}", ir),
    }
}
