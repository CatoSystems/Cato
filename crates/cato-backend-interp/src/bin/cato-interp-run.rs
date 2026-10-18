//! `cato-interp-run <file.cato> [args…]` — the tree-walking interpreter
//! behind a process boundary, for harnesses that used to spawn
//! `cato run`. The interpreter calls std::process::exit itself (`(exit n)`
//!), so programs like the kernel CLI must run in their own process.

use cato_core::Backend;
use cato_interp::InterpBackend;

fn main() {
    let mut args = std::env::args().skip(1);
    let file = match args.next() {
        Some(f) => f,
        None => {
            eprintln!("usage: cato-interp-run <file.cato> [args…]");
            std::process::exit(2);
        }
    };
    let program_args: Vec<String> = args.collect();
    let src = std::fs::read_to_string(&file).unwrap_or_else(|e| {
        eprintln!("cato: error: cannot read {}: {}", file, e);
        std::process::exit(1);
    });
    let program = cato_core::compile_named_with(&file, &src, true).unwrap_or_else(|e| {
        eprintln!("cato: error: {}", e);
        std::process::exit(1);
    });
    // The interpreter recurses once per Cato call frame; deep programs need
    // more than the 8MB main stack (same fix as the old `cato run`).
    let result = std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(move || guard(|| InterpBackend { program_args }.emit(&program)))
        .expect("spawn interp thread")
        .join()
        .unwrap_or_else(|_| {
            eprintln!("cato: error: interp thread panicked");
            std::process::exit(1);
        });
    match result {
        Ok(out) => print!("{}", out),
        Err(e) => {
            eprintln!("cato: error: {}", e);
            std::process::exit(1);
        }
    }
}

/// Catch interpreter panics (invariant violations on malformed input) as
/// errors instead of raw backtraces. Silences the panic hook for the
/// duration, like cato-cli's ui::guard.
fn guard<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::panic::set_hook(previous_hook);
    match result {
        Ok(r) => r,
        Err(payload) => {
            if let Some(s) = payload.downcast_ref::<String>() {
                Err(s.clone())
            } else if let Some(s) = payload.downcast_ref::<&str>() {
                Err((*s).to_string())
            } else {
                Err("internal error".to_string())
            }
        }
    }
}
