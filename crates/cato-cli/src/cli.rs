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
    "--runtime",
    "--rtlib",
    "--explain",
    "--version",
    "--help",
];

/// Known commands for did-you-mean suggestions (live commands only).
const COMMANDS: &[&str] = &["compile", "repl", "lsp", "help"];

pub struct Cli {
    pub file: String,
    pub out: Option<String>,
    /// Emit LLVM IR text (`--emit-ir`): stop before linking.
    pub emit_ir: bool,
    pub target: &'static crate::driver::TargetSpec,
    pub fast: bool,
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
    /// Where to keep the intermediate LLVM IR when linking.
    pub emit_ll: Option<String>,
    /// Link with AddressSanitizer.
    pub asan: bool,
    /// Debug info (`-g`).
    pub debug: bool,
    /// Runtime source override (`--runtime`).
    pub runtime: Option<String>,
    /// Runtime library (`--rtlib`, default compiler-rt).
    pub rtlib: String,
    /// Everything after `--`, forwarded verbatim to the toolchain.
    pub passthrough: Vec<String>,
}

impl Default for Cli {
    fn default() -> Self {
        Cli {
            file: String::new(),
            out: None,
            emit_ir: false,
            target: crate::driver::lookup("native").unwrap(),
            fast: false,
            threads: 1,
            no_opt: false,
            dump_hir: false,
            cc: None,
            opt_level: 2,
            emit_ll: None,
            asan: false,
            debug: false,
            runtime: None,
            rtlib: "compiler-rt".to_string(),
            passthrough: Vec::new(),
        }
    }
}

pub fn parse_args(args: &[String]) -> Cli {
    let mut cli = Cli::default();
    let mut file: Option<String> = None;
    let mut explain = None;
    let mut saw_command = false;
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
            "-g" => {
                cli.debug = true;
                i += 1;
            }
            "--fast" => {
                cli.fast = true;
                i += 1;
            }
            "--emit-ir" => {
                cli.emit_ir = true;
                i += 1;
            }
            "--target" => match args.get(i + 1).map(|s| s.as_str()) {
                Some(name) if crate::driver::lookup(name).is_some() => {
                    cli.target = crate::driver::lookup(name).unwrap();
                    i += 2;
                }
                other => {
                    let got = other.unwrap_or("<missing>");
                    let names = crate::driver::target_names().join(", ");
                    eprintln!("cato: unknown --target '{got}' (known: {names})");
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
            "--runtime" => match args.get(i + 1) {
                Some(p) => {
                    cli.runtime = Some(p.clone());
                    i += 2;
                }
                None => {
                    eprintln!("cato: --runtime requires a path");
                    std::process::exit(2);
                }
            },
            "--rtlib" => match args.get(i + 1) {
                Some(p) => {
                    cli.rtlib = p.clone();
                    i += 2;
                }
                None => {
                    eprintln!("cato: --rtlib requires a name");
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
            "--" => {
                cli.passthrough.extend(args[i + 1..].iter().cloned());
                i = args.len();
            }
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
                eprintln!("cato: 'build' was renamed to 'compile' (cato compile <file> [-o out])");
                std::process::exit(2);
            }
            "run" if file.is_none() => {
                eprintln!(
                    "cato: 'run' was removed — 'cato compile' produces a binary; \
                     the interpreter lives in 'cato repl'"
                );
                std::process::exit(2);
            }
            "compile" if file.is_none() => {
                saw_command = true;
                i += 1;
            }
            s => {
                if file.is_some() {
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
    if cli.emit_ir && cli.emit_ll.is_some() {
        eprintln!(
            "cato: --emit-ir stops before linking; --emit-ll only applies when linking"
        );
        std::process::exit(2);
    }
    match file {
        Some(_) if !saw_command => {
            eprintln!(
                "cato: subcommand required — '<file>' needs a command; try 'cato compile <file>'"
            );
            std::process::exit(2);
        }
        Some(f) if Path::new(&f).exists() => {
            cli.file = f;
            cli
        }
        Some(f) => {
            eprintln!("{} no such file {}", ui::error_lead(), f);
            std::process::exit(2);
        }
        None if saw_command => {
            eprintln!("cato: compile requires an input file (.cato)");
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
    eprintln!("  quick start:   cato compile your_program.cato");
    eprintln!("  full usage:    cato --help");
}

/// The complete usage text (`cato --help`, `cato help`).
fn usage_text() -> String {
    format!(
        "cato {} — a statically-typed Lisp that compiles to native code and WebAssembly

USAGE:
  cato compile <file.cato> [-o bin]     compile to a native executable
  cato compile <file.cato> --emit-ir [-o out.ll]   emit LLVM IR (stdout by default)
  cato compile <file.cato> --target wasm32 [-o out.wasm]   compile to WebAssembly
  cato compile <file.cato> --target x86_64-linux --cc <cross-cc>   cross-compile
  cato repl | cato lsp                 interactive tooling
  cato --explain E301                  long-form documentation for a code
  cato help | cato --version

OPTIONS:
  -o <path>        output path (artifact name; IR path with --emit-ir; the
                   driver-owned output is never overridable via --)
  --emit-ir        stop after IR emission (stdout by default, or -o)
  --emit-ll <path> keep the intermediate LLVM IR alongside the artifact
  --target <t>     native (default), wasm32, x86_64-linux, aarch64-linux,
                   x86_64-macos, aarch64-macos, x86_64-windows (cross targets
                   stamp their LLVM triple in the IR; pass a cross-capable
                   --cc to link)
  -O0..-O3         C-compiler optimization level (default: -O2)
  --opt-level <n>  same as -On
  -g               debug info
  --asan           link the native binary with AddressSanitizer
  --fast           attach vectorization metadata to fused loops (auto-SIMD)
  --no-opt         skip the middle-end HIR pipeline (fold, DCE, copy-prop,
                   devirtualization, inlining, clone elision)
  --dump-hir       print the HIR to stderr for debugging
  --threads <n>    worker threads for (parallel ...) loops
  --cc <path>      C compiler used for linking (default: $CATO_CC, $CC, clang)
  --runtime <path> runtime source (default: $CATO_RUNTIME, repo runtime/runtime.c)
  --rtlib <name>   runtime library (default: compiler-rt)
  --               everything after -- is forwarded verbatim to the compiler
                   driver (each option still appears exactly once in argv)

EXIT STATUS:
  0  success
  1  compile error, or a runtime error executing the program
  2  command-line usage error

The parity suites assert interpreter/native output agreement across every
example. wasm32 builds need a wasm-capable clang with lld (e.g. 'brew
install llvm lld', then pass --cc); `compile --emit-ir --target wasm32`
always works, no linker required.",
        env!("CARGO_PKG_VERSION")
    )
}

/// Full help on stdout (`--help`, `cato help`).
fn print_help() {
    println!("{}", usage_text());
}
