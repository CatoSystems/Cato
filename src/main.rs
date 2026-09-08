use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (file, out) = parse_args(&args);
    let src = std::fs::read_to_string(&file)
        .unwrap_or_else(|e| { eprintln!("cato: cannot read {}: {}", file, e); std::process::exit(2); });
    let parsed = cato::reader::read_all(&src)
        .unwrap_or_else(|e| { eprintln!("cato: parse error: {}", e); std::process::exit(2); });
    let expanded = cato::expand::expand_all(&parsed)
        .unwrap_or_else(|e| { eprintln!("cato: macro error: {}", e); std::process::exit(2); });
    let fns = cato::hir::build(&expanded)
        .unwrap_or_else(|e| { eprintln!("cato: semantic error: {}", e); std::process::exit(2); });
    let fused: Vec<cato::hir::Defn> = fns
        .into_iter()
        .map(|mut d| { let b = cato::fuse::check(&d.body); d.body = b; d })
        .collect();
    let ir = cato::codegen::emit_module(&fused);
    match out {
        Some(p) => std::fs::write(&p, &ir)
            .unwrap_or_else(|e| { eprintln!("cato: cannot write {}: {}", p, e); std::process::exit(2); }),
        None => print!("{}", ir),
    }
}

fn parse_args(args: &[String]) -> (String, Option<String>) {
    let mut file = None;
    let mut out = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                match args.get(i + 1) {
                    Some(p) if !p.starts_with('-') => { out = Some(p.clone()); i += 2; }
                    _ => { eprintln!("cato: -o requires a filename argument"); std::process::exit(2); }
                }
            }
            "--help" | "-h" => { usage(); std::process::exit(0); }
            s if s.starts_with('-') => { eprintln!("cato: unknown flag {}", s); std::process::exit(2); }
            s => { file = Some(s.to_string()); i += 1; }
        }
    }
    match file {
        Some(f) if Path::new(&f).exists() => (f, out),
        Some(f) => { eprintln!("cato: no such file {}", f); std::process::exit(2); }
        None => { usage(); std::process::exit(2); }
    }
}

fn usage() {
    eprintln!("cato: compile a .cato program to LLVM IR\n\nUSAGE:\n  cato <file.cato> [-o out.ll]\n\nBy default the LLVM IR is written to stdout; use -o to write it to a file.");
}