//! Command-line argument parsing and usage text for the `cato` binary.

use crate::ui;
use std::path::Path;

/// Known flags for did-you-mean suggestions.
const FLAGS: &[&str] = &[
    "--emit-ir",
    "--emit-ll",
    "--target",
    "--fast",
    "--no-opt",
    "--dump-hir",
    "--threads",
    "--cc",
    "--asan",
    "--opt-level",
    "--backend",
    "--explain",
    "--version",
    "--help",
];

/// Known commands for did-you-mean suggestions.
const COMMANDS: &[&str] = &["build", "run", "repl", "lsp", "help"];

/// What the binary should do after compiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Compile only: run the full pipeline and report errors. Nothing is
    /// written unless an emit flag is given.
    Compile,
    /// Compile to a native executable in the current toolchain.
    Build,
    /// Execute with the tree-walking interpreter (`cato run`).
    RunInterp,
    /// Build and execute a native binary (`cato run --native`).
    RunNative,
}

/// Emission targets (`--target`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The compiling host.
    Native,
    /// WebAssembly 32-bit freestanding (`wasm32-unknown-unknown`).
    Wasm32,
}

pub struct Cli {
    pub mode: Mode,
    pub file: String,
    pub out: Option<String>,
    /// Arguments after the file in `cato run file args...` (S3).
    pub interp_args: Vec<String>,
    /// Emit LLVM IR text in compile mode (`--emit-ir`).
    pub emit_ir: bool,
    pub target: Target,
    pub fast: bool,
    pub backend: String,
    /// Worker threads for `(parallel ...)` loops.
    pub threads: usize,
    /// Skip the middle-end optimization pipeline (pre-optimization HIR).
    pub no_opt: bool,
    /// Dump the HIR to stderr after compilation.
    pub dump_hir: bool,
    /// C compiler used to link native binaries (`--cc` > `CATO_CC` > `CC`
    /// > "clang").
    pub cc: Option<String>,
    /// Optimization level passed to the C compiler (0–3).
    pub opt_level: u8,
    /// Where to keep the intermediate LLVM IR when building natively.
    pub emit_ll: Option<String>,
    /// Link with AddressSanitizer.
    pub asan: bool,
}

impl Default for Cli {
    fn default() -> Self {
        Cli {
            mode: Mode::Compile,
            file: String::new(),
            out: None,
            interp_args: Vec::new(),
            emit_ir: false,
            target: Target::Native,
            fast: false,
            backend: "llvm".to_string(),
            threads: 1,
            no_opt: false,
            dump_hir: false,
            cc: None,
            opt_level: 2,
            emit_ll: None,
            asan: false,
        }
    }
}

pub fn parse_args(args: &[String]) -> Cli {
    let mut cli = Cli::default();
    let mut file: Option<String> = None;
    let mut explain = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => match args.get(i + 1) {
                Some(p) if !p.starts_with('-') => {
                    cli.out = Some(p.clone());
                    i += 2;
                }
                _ => {
                    eprintln!("cato: -o requires a filename argument");
                    std::process::exit(2);
                }
            },
            "--fast" => {
                cli.fast = true;
                i += 1;
            }
            "--emit-ir" => {
                cli.emit_ir = true;
                i += 1;
            }
            "--target" => match args.get(i + 1).map(|s| s.as_str()) {
                Some("native") => {
                    cli.target = Target::Native;
                    i += 2;
                }
                Some("wasm32") => {
                    cli.target = Target::Wasm32;
                    i += 2;
                }
                _ => {
                    eprintln!("cato: --target expects 'native' or 'wasm32'");
                    std::process::exit(2);
                }
            },
            "--no-opt" => {
                cli.no_opt = true;
                i += 1;
            }
            "--dump-hir" => {
                cli.dump_hir = true;
                i += 1;
            }
            "--asan" => {
                cli.asan = true;
                i += 1;
            }
            "--native" => {
                cli.mode = Mode::RunNative;
                i += 1;
            }
            "--cc" => match args.get(i + 1) {
                Some(p) => {
                    cli.cc = Some(p.clone());
                    i += 2;
                }
                None => {
                    eprintln!("cato: --cc requires a compiler path");
                    std::process::exit(2);
                }
            },
            "-O0" | "-O1" | "-O2" | "-O3" => {
                cli.opt_level = args[i][2..].parse().unwrap();
                i += 1;
            }
            "--opt-level" => match args.get(i + 1).and_then(|s| s.parse::<u8>().ok()) {
                Some(n @ 0..=3) => {
                    cli.opt_level = n;
                    i += 2;
                }
                _ => {
                    eprintln!("cato: --opt-level expects 0, 1, 2 or 3");
                    std::process::exit(2);
                }
            },
            "--emit-ll" => match args.get(i + 1) {
                Some(p) => {
                    cli.emit_ll = Some(p.clone());
                    i += 2;
                }
                None => {
                    eprintln!("cato: --emit-ll requires a filename");
                    std::process::exit(2);
                }
            },
            "--threads" => match args.get(i + 1) {
                Some(n) => match n.parse::<usize>() {
                    Ok(v) if (1..=1024).contains(&v) => {
                        cli.threads = v;
                        i += 2;
                    }
                    _ => {
                        eprintln!("cato: --threads expects a number between 1 and 1024");
                        std::process::exit(2);
                    }
                },
                None => {
                    eprintln!("cato: --threads requires a number");
                    std::process::exit(2);
                }
            },
            "--explain" => match args.get(i + 1) {
                Some(c) => {
                    explain = Some(c.clone());
                    i += 2;
                }
                None => {
                    eprintln!("cato: --explain requires a code (e.g. --explain E301)");
                    std::process::exit(2);
                }
            },
            "--backend" => match args.get(i + 1) {
                Some(b) => {
                    cli.backend = b.clone();
                    i += 2;
                }
                None => {
                    eprintln!("cato: --backend requires a name");
                    std::process::exit(2);
                }
            },
            "--version" | "-V" => {
                println!("cato {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            s if s.starts_with('-') => {
                eprintln!(
                    "{} unknown flag {}{}",
                    ui::error_lead(),
                    s,
                    ui::suggestion_suffix(s, FLAGS)
                );
                std::process::exit(2);
            }
            "help" if file.is_none() => {
                print_help();
                std::process::exit(0);
            }
            "build" if file.is_none() => {
                cli.mode = Mode::Build;
                i += 1;
            }
            "run" if file.is_none() => {
                cli.mode = Mode::RunInterp;
                i += 1;
            }
            s => {
                if file.is_some() {
                    // `cato run file args...`: trailing positionals are the
                    // interpreted program's arguments (S3).
                    if cli.mode == Mode::RunInterp {
                        cli.interp_args.push(s.to_string());
                        i += 1;
                        continue;
                    }
                    // Two positionals: if the first one looked like a
                    // mistyped command, say so instead of "unexpected arg".
                    let first = file.as_deref().unwrap_or(s);
                    let suggestion = ui::suggestion_suffix(first, COMMANDS);
                    if !suggestion.is_empty() {
                        eprintln!(
                            "{} unknown command '{}'{}",
                            ui::error_lead(),
                            first,
                            suggestion
                        );
                    } else {
                        eprintln!(
                            "{} unexpected argument '{}' (only one input file)",
                            ui::error_lead(),
                            s
                        );
                    }
                    std::process::exit(2);
                }
                file = Some(s.to_string());
                i += 1;
            }
        }
    }
    if let Some(code) = explain {
        match cato_core::diagnostics::explain(&code) {
            Some(doc) => {
                println!("{}: {}", code, doc);
                std::process::exit(0);
            }
            None => {
                eprintln!(
                    "cato: unknown diagnostic code {} (known: {})",
                    code,
                    cato_core::diagnostics::all_codes().collect::<Vec<_>>().join(" ")
                );
                std::process::exit(2);
            }
        }
    }
    // Flag-combination validation before touching any files.
    if cli.emit_ir && cli.mode != Mode::Compile {
        eprintln!("cato: --emit-ir is only valid in compile mode (use --emit-ll with build/run)");
        std::process::exit(2);
    }
    if cli.emit_ll.is_some() && cli.mode == Mode::Compile {
        eprintln!("cato: --emit-ll is only valid with build/run --native (use --emit-ir in compile mode)");
        std::process::exit(2);
    }
    if cli.mode == Mode::Compile && cli.out.is_some() && !cli.emit_ir {
        eprintln!(
            "cato: -o with nothing to emit — pass --emit-ir to write LLVM IR, \
             or use 'cato build -o <bin>' for a native binary"
        );
        std::process::exit(2);
    }
    if cli.target == Target::Wasm32 && cli.mode == Mode::RunNative {
        eprintln!(
            "cato: wasm binaries cannot be executed directly — build them with \
             'cato build --target wasm32' and run them in a wasm runtime"
        );
        std::process::exit(2);
    }
    match file {
        Some(f) if Path::new(&f).exists() => {
            cli.file = f;
            cli
        }
        Some(f) => {
            eprintln!(
                "{} no such file {}{}",
                ui::error_lead(),
                f,
                ui::suggestion_suffix(&f, COMMANDS)
            );
            std::process::exit(2);
        }
        None => {
            banner();
            std::process::exit(2);
        }
    }
}

/// Short pointer shown when `cato` is invoked with no arguments.
fn banner() {
    eprintln!(
        "cato {} — a statically-typed Lisp that compiles to native code and WebAssembly",
        env!("CARGO_PKG_VERSION")
    );
    eprintln!();
    eprintln!("  quick start:   cato run your_program.cato");
    eprintln!("  full usage:    cato --help");
}

/// The complete usage text (`cato --help`, `cato help`).
fn usage_text() -> String {
    format!(
        "cato {} — a statically-typed Lisp that compiles to native code and WebAssembly

USAGE:
  cato <file.cato>                     compile (type/mid-end check); no output
  cato <file.cato> --emit-ir [-o out.ll]   emit LLVM IR (stdout by default)
  cato build <file.cato> [-o bin]      compile to a native executable
  cato build --target wasm32 <file>    compile to a WebAssembly binary
  cato run <file.cato>                 execute with the built-in interpreter
  cato run --native <file.cato>        compile and execute a native binary
  cato repl | cato lsp                 interactive tooling
  cato --explain E301                  long-form documentation for a code
  cato help | cato --version

OPTIONS:
  --emit-ir        emit the LLVM IR in compile mode (opt-in)
  -o <path>        output path (IR in compile mode, binary for build)
  --target <t>     'native' (default) or 'wasm32' (wasm32-unknown-unknown)
  --emit-ll <path> keep the intermediate LLVM IR when building natively
  -O0..-O3         C-compiler optimization level (default: -O2 for native builds)
  --fast           attach vectorization metadata to fused loops (auto-SIMD)
  --no-opt         skip the middle-end HIR pipeline (fold, DCE, copy-prop,
                   devirtualization, inlining, clone elision)
  --dump-hir       print the HIR to stderr for debugging
  --threads <n>    worker threads for (parallel ...) loops
  --cc <path>      C compiler used for linking (default: $CATO_CC, $CC, clang)
  --asan           link the native binary with AddressSanitizer
  --backend <b>    compile-mode execution backend: llvm (default) or interp

EXIT STATUS:
  0  success
  1  compile error, or a runtime error executing the program
  2  command-line usage error

The interpreter and native execution produce identical output; the parity
suites in tests assert this across every example. wasm32 builds need a
wasm-capable clang with lld (e.g. 'brew install llvm lld', then pass --cc);
`--emit-ir --target wasm32` always works, no linker required.",
        env!("CARGO_PKG_VERSION")
    )
}

/// Full help on stdout (`--help`, `cato help`).
fn print_help() {
    println!("{}", usage_text());
}
