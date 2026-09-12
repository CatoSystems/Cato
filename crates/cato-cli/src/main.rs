use cato_core::{compile_to_program, Backend};
use cato_llvm::LlvmBackend;
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (file, out, fast, backend) = parse_args(&args);
    let src = std::fs::read_to_string(&file)
        .unwrap_or_else(|e| { eprintln!("cato: cannot read {}: {}", file, e); std::process::exit(2); });
    let program = compile_to_program(&src)
        .unwrap_or_else(|e| { eprintln!("cato: {}", e); std::process::exit(2); });
    let result = match backend.as_str() {
        "llvm" => LlvmBackend { fast }.emit(&program),
        other => {
            eprintln!("cato: unknown backend '{}' (available: llvm)", other);
            std::process::exit(2);
        }
    };
    let ir = result
        .unwrap_or_else(|e| { eprintln!("cato: backend error: {}", e); std::process::exit(2); });
    match out {
        Some(p) => std::fs::write(&p, &ir)
            .unwrap_or_else(|e| { eprintln!("cato: cannot write {}: {}", p, e); std::process::exit(2); }),
        None => print!("{}", ir),
    }
}

fn parse_args(args: &[String]) -> (String, Option<String>, bool, String) {
    let mut file = None;
    let mut out = None;
    let mut fast = false;
    let mut backend = "llvm".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                match args.get(i + 1) {
                    Some(p) if !p.starts_with('-') => { out = Some(p.clone()); i += 2; }
                    _ => { eprintln!("cato: -o requires a filename argument"); std::process::exit(2); }
                }
            }
            "--fast" => { fast = true; i += 1; }
            "--backend" => {
                match args.get(i + 1) {
                    Some(b) => { backend = b.clone(); i += 2; }
                    None => { eprintln!("cato: --backend requires a name"); std::process::exit(2); }
                }
            }
            "--help" | "-h" => { usage(); std::process::exit(0); }
            s if s.starts_with('-') => { eprintln!("cato: unknown flag {}", s); std::process::exit(2); }
            s => { file = Some(s.to_string()); i += 1; }
        }
    }
    match file {
        Some(f) if Path::new(&f).exists() => (f, out, fast, backend),
        Some(f) => { eprintln!("cato: no such file {}", f); std::process::exit(2); }
        None => { usage(); std::process::exit(2); }
    }
}

fn usage() {
    eprintln!("cato: compile a .cato program to LLVM IR\n\nUSAGE:\n  cato <file.cato> [-o out.ll] [--fast] [--backend llvm]\n\nBy default the LLVM IR is written to stdout; use -o to write it to a file.\n--fast attaches vectorization metadata to fused loops (auto-SIMD).\n\nBackends: llvm (default).");
}
