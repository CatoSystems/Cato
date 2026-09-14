//! The `cato repl` session: a form-at-a-time frontend over the interpreter.
//!
//! Definitional forms (`defn`, `deftype`, `extern`, `require`) accumulate
//! into the session program; any other form is evaluated immediately by
//! compiling the session plus a synthetic `__repl` wrapper and running it
//! in the interpreter. Everything recompiles per entry — slow at very
//! large session sizes, exact in semantics, and zero extra machinery.

use cato_core::ast::Expr;
use cato_core::compile_named;
use cato_interp::fmt_value;
use std::io::{BufRead, Write};

const HEADS: [&str; 5] = ["defn", "deftype", "extern", "ns", "require"];

pub fn run() {
    let mut session = String::new();
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    eprintln!("cato repl — forms evaluate via the interpreter; :quit exits");
    loop {
        print!("cato=> ");
        std::io::stdout().flush().unwrap();
        let Some(line) = lines.next() else { break };
        let line = line.unwrap();
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == ":quit" || trimmed == ":q" {
            break;
        }
        let forms = match cato_core::reader::read_all(trimmed) {
            Ok(f) if f.len() == 1 => f,
            Ok(_) => {
                eprintln!("error: exactly one form per entry");
                continue;
            }
            Err(e) => {
                eprintln!("error: {}", e);
                continue;
            }
        };
        let is_def = matches!(&forms[0], Expr::List(items) if matches!(
            items.first(), Some(Expr::LitSym(h)) if HEADS.contains(&h.as_str())
        ));
        if is_def {
            session.push_str(trimmed);
            session.push('\n');
            let name = match &forms[0] {
                Expr::List(items) => match items.get(1) {
                    Some(Expr::LitSym(n)) => n.clone(),
                    _ => String::new(),
                },
                _ => String::new(),
            };
            // Compile eagerly so definition errors surface here.
            let src = format!("{}", session);
            match compile_named("<repl>", &src) {
                Ok(_) => println!("{} => defined", name),
                Err(e) => {
                    eprintln!("error: {}", e);
                    // Roll back the failed definition.
                    session.truncate(session.len() - trimmed.len() - 1);
                }
            }
            continue;
        }
        let src = format!(
            "{}\n(defn __repl [] {})",
            session, trimmed
        );
        match compile_named("<repl>", &src) {
            Ok(program) => match cato_interp::Interpreter::new(&program).run_entry("__repl") {
                Ok((v, out)) => {
                    if !out.is_empty() {
                        print!("{}", out);
                    }
                    println!("=> {}", fmt_value(&v));
                }
                Err(e) => eprintln!("error: {}", e),
            },
            Err(e) => eprintln!("error: {}", e),
        }
    }
}
