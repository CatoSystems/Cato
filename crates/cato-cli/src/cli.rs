//! Command-line argument parsing and usage text for the `cato` binary.

use std::path::Path;

pub struct Cli {
    pub file: String,
    pub out: Option<String>,
    pub fast: bool,
    pub backend: String,
}

pub fn parse_args(args: &[String]) -> Cli {
    let mut file = None;
    let mut out = None;
    let mut fast = false;
    let mut backend = "llvm".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => match args.get(i + 1) {
                Some(p) if !p.starts_with('-') => {
                    out = Some(p.clone());
                    i += 2;
                }
                _ => {
                    eprintln!("cato: -o requires a filename argument");
                    std::process::exit(2);
                }
            },
            "--fast" => {
                fast = true;
                i += 1;
            }
            "--backend" => match args.get(i + 1) {
                Some(b) => {
                    backend = b.clone();
                    i += 2;
                }
                None => {
                    eprintln!("cato: --backend requires a name");
                    std::process::exit(2);
                }
            },
            "--help" | "-h" => {
                usage();
                std::process::exit(0);
            }
            s if s.starts_with('-') => {
                eprintln!("cato: unknown flag {}", s);
                std::process::exit(2);
            }
            s => {
                file = Some(s.to_string());
                i += 1;
            }
        }
    }
    match file {
        Some(f) if Path::new(&f).exists() => Cli {
            file: f,
            out,
            fast,
            backend,
        },
        Some(f) => {
            eprintln!("cato: no such file {}", f);
            std::process::exit(2);
        }
        None => {
            usage();
            std::process::exit(2);
        }
    }
}

fn usage() {
    eprintln!(
        "cato: compile a .cato program to LLVM IR\n\nUSAGE:\n  cato <file.cato> [-o out.ll] [--fast] [--backend llvm]\n\nBy default the LLVM IR is written to stdout; use -o to write it to a file.\n--fast attaches vectorization metadata to fused loops (auto-SIMD).\n\nBackends: llvm (default)."
    );
}
