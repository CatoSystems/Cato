fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "--help" {
        eprintln!("cato: compile a .cato program to LLVM IR\n\nUSAGE:\n  cato <file.cato> [--emit-llvm | -o out.ll]");
        std::process::exit(0);
    }
    eprintln!("cato: not yet implemented");
    std::process::exit(2);
}
