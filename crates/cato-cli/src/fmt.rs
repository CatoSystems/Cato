//! `cato format` — CLI glue for the CST formatter: argument parsing, file
//! IO, and messages. Contract: spec §1/§3 — rewrite in place (quiet),
//! `--check` (CI mode: list would-change paths, exit 1), `--width`
//! (default 100, 20..=1000), `-` reads stdin → stdout; per-file parse
//! failures print `cato: <file>: <msg>` and processing continues.

use crate::ui;
use std::io::{Read, Write};

const FORMAT_FLAGS: &[&str] = &["--check", "--width", "--help"];

/// Run `cato format` with the arguments after the subcommand. Always
/// exits the process (0 success, 1 file/parse failure, 2 usage error).
pub fn run(args: &[String]) {
    let mut check = false;
    let mut width = 100usize;
    let mut files: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--check" => {
                check = true;
                i += 1;
            }
            "--width" => match args.get(i + 1).and_then(|s| s.parse::<usize>().ok()) {
                Some(n @ 20..=1000) => {
                    width = n;
                    i += 2;
                }
                Some(n) => {
                    eprintln!("cato: --width {n} is out of range (expected 20..=1000)");
                    std::process::exit(2);
                }
                None => {
                    eprintln!("cato: --width expects a number (e.g. --width 100)");
                    std::process::exit(2);
                }
            },
            "--help" | "-h" => {
                println!(
                    "usage: cato format <file>... [--check] [--width n]\n\n\
                     Rewrites each file in place with the CST formatter.\n\
                     \x20 --check     list files that would change; exit 1 if any\n\
                     \x20 --width n   target line width (20..=1000, default 100)\n\
                     \x20 -           read stdin, write stdout"
                );
                std::process::exit(0);
            }
            "-" => {
                files.push("-".to_string());
                i += 1;
            }
            s if s.starts_with('-') => {
                eprintln!(
                    "{} unknown flag {}{}",
                    ui::error_lead(),
                    s,
                    ui::suggestion_suffix(s, FORMAT_FLAGS)
                );
                std::process::exit(2);
            }
            s => {
                files.push(s.to_string());
                i += 1;
            }
        }
    }
    if files.is_empty() {
        eprintln!("cato: format requires at least one file (or - for stdin)");
        std::process::exit(2);
    }

    let mut failed = false;
    let mut changed = false;
    for f in &files {
        if f == "-" {
            let mut src = String::new();
            if let Err(e) = std::io::stdin().read_to_string(&mut src) {
                eprintln!("cato: -: {}", e);
                failed = true;
                continue;
            }
            match cato_fmt::format_source(&src, width) {
                Ok(out) => {
                    if check {
                        if out != src {
                            println!("-");
                            changed = true;
                        }
                    } else {
                        let stdout = std::io::stdout();
                        let mut lock = stdout.lock();
                        if lock.write_all(out.as_bytes()).is_err() {
                            std::process::exit(1);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("cato: -: {}", e);
                    failed = true;
                }
            }
            continue;
        }
        let src = match std::fs::read_to_string(f) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("cato: {}: {}", f, e);
                failed = true;
                continue;
            }
        };
        match cato_fmt::format_source(&src, width) {
            Ok(out) => {
                if out == src {
                    continue;
                }
                if check {
                    println!("{}", f);
                    changed = true;
                } else if let Err(e) = write_atomic(f, &out) {
                    eprintln!("cato: {}: {}", f, e);
                    failed = true;
                }
            }
            Err(e) => {
                eprintln!("cato: {}: {}", f, e);
                failed = true;
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
    if check && changed {
        std::process::exit(1);
    }
}

/// Replace `path` atomically: write a sibling temp file, then rename it
/// over the original, so a failed write never truncates the user's file.
fn write_atomic(path: &str, contents: &str) -> std::io::Result<()> {
    let p = std::path::Path::new(path);
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = p.with_file_name(format!("{}.cato-fmt-{}.tmp", name, std::process::id()));
    std::fs::write(&tmp, contents)?;
    if let Err(e) = std::fs::rename(&tmp, p) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}
