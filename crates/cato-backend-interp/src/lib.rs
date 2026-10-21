//! Tree-walking interpreter backend (`cato-backend-interp`).
//!
//! Implements `Backend::emit` as *direct execution*: the same `Program` the
//! LLVM backend lowers to IR is evaluated here, and the captured stdout is
//! returned as the emitted artifact. No LLVM dependency — fast, portable,
//! and a differential-testing oracle for the LLVM backend (`cato run`).
//!
//! Semantics mirror the LLVM emission exactly:
//! - integer widths are closed and wrap at their width (LLVM `add` is
//!   wrapping); values are stored as i64 in the *unsigned interpretation*
//!   range for unsigned types, so comparisons are plain i64 compares;
//! - casts follow the same trunc/sext/zext/sitofp/fptoui matrix;
//! - sets/maps are immutable snapshots, conj/assoc/disj copy.

use cato_core::{Backend, Program};
use std::collections::HashMap;
use std::rc::Rc;

/// A runtime value. Integers of every width are stored as i64; unsigned
/// values are kept normalized into `0..2^bits` (the unsigned reading),
/// signed values sign-extended — so i64 comparisons are width-correct.
#[derive(Clone, Debug)]
pub enum Value {
    Int(i64),
    F64(f64),
    F32(f32),
    Bool(bool),
    Str(Rc<String>),
    Vec(Rc<Vec<Value>>),
    /// Sorted (key, value) entries — an immutable snapshot.
    Map(Rc<Vec<(i64, i64)>>),
    /// Sorted unique entries — an immutable snapshot.
    Set(Rc<Vec<i64>>),
    Adt {
        variant: u32,
        fields: Rc<Vec<Value>>,
    },
    Closure {
        params: Vec<(String, cato_core::hir::Ty)>,
        captures: Vec<(String, Value)>,
        body: Box<cato_core::hir::HExpr>,
    },
}

/// Wrap an i64 into the value range of `ty` (LLVM wrapping semantics).
fn wrap(v: i64, ty: cato_core::hir::Ty) -> i64 {
    use cato_core::hir::Ty;
    match ty {
        Ty::I8 | Ty::U8 => {
            let m = v & 0xFF;
            if ty == Ty::I8 && m >= 0x80 {
                m - 0x100
            } else {
                m
            }
        }
        Ty::I16 | Ty::U16 => {
            let m = v & 0xFFFF;
            if ty == Ty::I16 && m >= 0x8000 {
                m - 0x1_0000
            } else {
                m
            }
        }
        Ty::I32 | Ty::U32 => {
            let m = v & 0xFFFF_FFFF;
            if ty == Ty::I32 && m >= 0x8000_0000 {
                m - 0x1_0000_0000
            } else {
                m
            }
        }
        _ => v,
    }
}

// The innermost defn being run (`call_fn` sets it) so a panicked `int_of`
// names the function whose value shapes disagree — kernel debugging aid.
thread_local! {
    static INTERP_FN: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// Public interpreter: executes a program and captures printed output.
pub struct Interpreter<'p> {
    defns: HashMap<String, &'p cato_core::Defn>,
    out: String,
    /// Stash for the innermost loop's `recur` arguments (S2). Validation
    /// guarantees the nearest loop is the target, so one slot suffices.
    recur_sink: Option<Vec<Value>>,
    /// The interpreted program's arguments (`(args)`), S3.
    program_args: Vec<String>,
    /// Shared append-only symbol tables (`cato-stab-*` externs). A handle is
    /// an index here; entries are never removed, mirroring the runtime.
    stabs: Vec<Vec<String>>,
}

type R<T> = Result<T, String>;

impl<'p> Interpreter<'p> {
    pub fn new(program: &'p Program) -> Interpreter<'p> {
        Interpreter {
            defns: program.defns.iter().map(|d| (d.name.clone(), d)).collect(),
            out: String::new(),
            recur_sink: None,
            program_args: Vec::new(),
            stabs: Vec::new(),
        }
    }

    /// Provide `(args)` for the interpreted program (CLI passthrough).
    pub fn set_program_args(&mut self, args: Vec<String>) {
        self.program_args = args;
    }

    /// Run `main` and return the captured stdout.
    pub fn run(self) -> R<String> {
        self.run_entry("main").map(|(_, out)| out)
    }

    /// Run an arbitrary zero-arg defn; returns (result value, stdout).
    pub fn run_entry(mut self, name: &str) -> R<(Value, String)> {
        let defn = self
            .defns
            .get(name)
            .ok_or_else(|| format!("interp: no {} defn", name))?;
        let args: Vec<Value> = defn.params.iter().map(|(_, t)| default_value(*t)).collect();
        let v = self.call(name, &args)?;
        Ok((v, std::mem::take(&mut self.out)))
    }

    fn call(&mut self, name: &str, args: &[Value]) -> R<Value> {
        // Builtins first.
        match name {
            "print-i64" => {
                self.out.push_str(&format!("{}\n", int_of(&args[0])));
                return Ok(Value::Int(0));
            }
            "print-f64" => {
                let v = match &args[0] {
                    Value::F64(v) => *v,
                    Value::F32(v) => *v as f64,
                    other => panic!("interp: print-f64 on {:?}", other),
                };
                self.out.push_str(&format!("{}\n", fmt_f64(v)));
                return Ok(Value::Int(0));
            }
            "print-string" => {
                match &args[0] {
                    Value::Str(s) => self.out.push_str(s),
                    other => panic!("interp: print-string on {:?}", other),
                }
                self.out.push('\n');
                return Ok(Value::Int(0));
            }
            // S3: raw print without the trailing newline.
            "print-str" => {
                match &args[0] {
                    Value::Str(s) => self.out.push_str(s),
                    other => panic!("interp: print-str on {:?}", other),
                }
                return Ok(Value::Int(0));
            }
            "even?" => {
                return Ok(Value::Bool(int_of(&args[0]) % 2 == 0));
            }
            _ => {}
        }
        // FFI externs the interpreter models natively.
        match name {
            "fabs" => {
                return Ok(Value::F64(f64_of(&args[0]).abs()));
            }
            "strlen" => {
                if let Value::Str(s) = &args[0] {
                    return Ok(Value::Int(s.len() as i64));
                }
            }
            // Unchecked byte read `s[i]`. One past the end is the string's
            // NUL, matching `(nth s i 0)`'s 0 default; negative indices and
            // non-strings read as 0.
            "cato-str-byte" => {
                if let Value::Str(s) = &args[0] {
                    let i = int_of(&args[1]);
                    let b = if i < 0 {
                        0
                    } else {
                        s.as_bytes().get(i as usize).copied().unwrap_or(0)
                    };
                    return Ok(Value::Int(b as i64));
                }
                return Ok(Value::Int(0));
            }
            "system" => {
                // libc semantics: the wait status (exit code << 8), or -1
                // when the shell could not run. The kernel CLI decodes the
                // code with an arithmetic shift; native links the real
                // libc symbol.
                if let Value::Str(cmd) = &args[0] {
                    let status = std::process::Command::new("sh")
                        .arg("-c")
                        .arg(cmd.as_str())
                        .status();
                    let code = match status {
                        Ok(s) => s.code().unwrap_or(-1),
                        Err(_) => -1,
                    };
                    return Ok(Value::Int((code as i64) << 8));
                }
            }
            "cato-spawn" => {
                if let Value::Vec(v) = &args[0] {
                    let argv: Vec<String> = v
                        .iter()
                        .map(|x| match x {
                            Value::Str(s) => s.to_string(),
                            _ => String::new(),
                        })
                        .collect();
                    if argv.is_empty() {
                        return Ok(Value::Int(-1));
                    }
                    // Same statuses as runtime.c's cato_spawn: the exit
                    // code, 128+signal when killed, 127 when the program
                    // cannot be executed.
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                    let status = std::process::Command::new(&argv[0])
                        .args(&argv[1..])
                        .status();
                    let code = match status {
                        Ok(s) => match s.code() {
                            Some(c) => c as i64,
                            None => {
                                #[cfg(unix)]
                                {
                                    use std::os::unix::process::ExitStatusExt;
                                    s.signal().map(|sig| 128 + sig as i64).unwrap_or(-1)
                                }
                                #[cfg(not(unix))]
                                {
                                    -1
                                }
                            }
                        },
                        Err(_) => 127,
                    };
                    return Ok(Value::Int(code));
                }
            }
            "cato-getenv" => {
                if let Value::Str(name) = &args[0] {
                    let v = std::env::var(name.as_str()).unwrap_or_default();
                    return Ok(Value::Str(Rc::new(v)));
                }
            }
            "cato-remove-file" => {
                if let Value::Str(p) = &args[0] {
                    let ok = std::fs::remove_file(p.as_str()).is_ok();
                    return Ok(Value::Int(if ok { 0 } else { -1 }));
                }
            }
            "cato-file-exists" => {
                if let Value::Str(p) = &args[0] {
                    return Ok(Value::Bool(std::path::Path::new(p.as_str()).exists()));
                }
            }
            "cato-host-linux" => {
                return Ok(Value::Bool(cfg!(target_os = "linux")));
            }
            "stdin-getline" => {
                let mut line = String::new();
                let n = std::io::stdin().read_line(&mut line).unwrap_or(0);
                if n == 0 && line.is_empty() {
                    return Ok(Value::Str(Rc::new(String::new())));
                }
                if line.ends_with('\n') {
                    line.pop();
                }
                if line.ends_with('\r') {
                    line.pop();
                }
                return Ok(Value::Str(Rc::new(line)));
            }
            "stdin-read-exact" => {
                if let Value::Int(n) = &args[0] {
                    let n = if *n < 0 { 0usize } else { *n as usize };
                    let mut buf = vec![0u8; n];
                    let mut got = 0usize;
                    use std::io::Read;
                    while got < n {
                        match std::io::stdin().read(&mut buf[got..]) {
                            Ok(0) => break,
                            Ok(k) => got += k,
                            Err(_) => break,
                        }
                    }
                    let s = String::from_utf8_lossy(&buf[..got]).to_string();
                    return Ok(Value::Str(Rc::new(s)));
                }
            }
            "stdout-flush" => {
                use std::io::Write;
                let _ = std::io::stdout().flush();
                return Ok(Value::Int(0));
            }
            "cato-stderr" => {
                if let Value::Str(s) = &args[0] {
                    use std::io::Write;
                    let _ = std::io::stderr().write_all(s.as_bytes());
                    let _ = std::io::stderr().flush();
                    return Ok(Value::Int(0));
                }
            }
            // Shared append-only symbol table (`lib/sym.cato`). A handle is
            // an index into `self.stabs`; entries are never removed.
            "cato-stab-new" => {
                self.stabs.push(Vec::new());
                return Ok(Value::Int((self.stabs.len() - 1) as i64));
            }
            "cato-stab-count" => {
                let h = int_of(&args[0]) as usize;
                return Ok(Value::Int(self.stabs[h].len() as i64));
            }
            "cato-stab-get" => {
                let h = int_of(&args[0]) as usize;
                let sid = int_of(&args[1]) as usize;
                let s = self.stabs[h][sid].clone();
                return Ok(Value::Str(Rc::new(s)));
            }
            "cato-stab-find" => {
                let h = int_of(&args[0]) as usize;
                if let Value::Str(s) = &args[1] {
                    for i in (0..self.stabs[h].len()).rev() {
                        if self.stabs[h][i] == **s {
                            return Ok(Value::Int(i as i64));
                        }
                    }
                }
                return Ok(Value::Int(-1));
            }
            "cato-stab-conj" => {
                let h = int_of(&args[0]) as usize;
                if let Value::Str(s) = &args[1] {
                    let sid = self.stabs[h].len() as i64;
                    self.stabs[h].push((**s).clone());
                    return Ok(Value::Int(sid));
                }
                return Ok(Value::Int(-1));
            }
            "cato-stab-add" => {
                let h = int_of(&args[0]) as usize;
                if let Value::Str(s) = &args[1] {
                    self.stabs[h].push((**s).clone());
                }
                return Ok(Value::Int(h as i64));
            }
            // Bit operations (num.cato externs). Mirror runtime.c.
            "cato-bit-and" => {
                return Ok(Value::Int(int_of(&args[0]) & int_of(&args[1])));
            }
            "cato-bit-or" => {
                return Ok(Value::Int(int_of(&args[0]) | int_of(&args[1])));
            }
            "cato-bit-xor" => {
                return Ok(Value::Int(int_of(&args[0]) ^ int_of(&args[1])));
            }
            "cato-bit-not" => {
                return Ok(Value::Int(!int_of(&args[0])));
            }
            "cato-shl" => {
                let a = int_of(&args[0]);
                let n = int_of(&args[1]);
                let v = if n < 0 || n >= 64 { 0 } else { a << n };
                return Ok(Value::Int(v));
            }
            "cato-shr" => {
                let a = int_of(&args[0]);
                let n = int_of(&args[1]);
                let v = if n < 0 || n >= 64 {
                    if a < 0 {
                        -1
                    } else {
                        0
                    }
                } else {
                    a >> n
                };
                return Ok(Value::Int(v));
            }
            "cato-ushr" => {
                let a = int_of(&args[0]) as u64;
                let n = int_of(&args[1]);
                let v = if n < 0 || n >= 64 { 0 } else { (a >> n) as i64 };
                return Ok(Value::Int(v));
            }
            "cato-popcount" => {
                return Ok(Value::Int((int_of(&args[0]) as u64).count_ones() as i64));
            }
            "cato-clz" => {
                let a = int_of(&args[0]) as u64;
                return Ok(Value::Int(a.leading_zeros() as i64));
            }
            "cato-ctz" => {
                let a = int_of(&args[0]) as u64;
                return Ok(Value::Int(a.trailing_zeros() as i64));
            }
            "cato-rotl" => {
                let a = int_of(&args[0]) as u64;
                let n = (int_of(&args[1]) as u32) & 63;
                return Ok(Value::Int(a.rotate_left(n) as i64));
            }
            "cato-rotr" => {
                let a = int_of(&args[0]) as u64;
                let n = (int_of(&args[1]) as u32) & 63;
                return Ok(Value::Int(a.rotate_right(n) as i64));
            }
            "cato-bswap" => {
                let a = int_of(&args[0]) as u64;
                return Ok(Value::Int(a.swap_bytes() as i64));
            }
            "cato-sqrt" => {
                return Ok(Value::F64(f64_of(&args[0]).sqrt()));
            }
            "cato-pow" => {
                return Ok(Value::F64(f64_of(&args[0]).powf(f64_of(&args[1]))));
            }
            "cato-floor" => {
                return Ok(Value::F64(f64_of(&args[0]).floor()));
            }
            "cato-ceil" => {
                return Ok(Value::F64(f64_of(&args[0]).ceil()));
            }
            "cato-round" => {
                return Ok(Value::F64(f64_of(&args[0]).round()));
            }
            _ => {}
        }
        let defn: &'p cato_core::Defn = *self
            .defns
            .get(name)
            .ok_or_else(|| format!("interp: unknown function {}", name))?;
        if defn.is_extern {
            return Err(format!("interp: unsupported extern {}", name));
        }
        let mut env: HashMap<String, Value> = defn
            .params
            .iter()
            .zip(args.iter())
            .map(|((p, _), v)| (p.clone(), v.clone()))
            .collect();
        INTERP_FN.with(|f| *f.borrow_mut() = name.to_string());
        self.eval(&defn.body, &mut env)
    }

    fn eval(&mut self, e: &cato_core::hir::HExpr, env: &mut HashMap<String, Value>) -> R<Value> {
        use cato_core::hir::{BinOp, HExpr, Ty, VLit};
        Ok(match e {
            HExpr::ConstI64(v) => Value::Int(*v),
            HExpr::ConstB(v) => Value::Bool(*v),
            HExpr::ConstF64(v) => Value::F64(*v),
            HExpr::StrLit(s) => Value::Str(Rc::new(s.clone())),
            HExpr::Var(v) => env.get(v).cloned().unwrap_or_else(|| {
                panic!(
                    "interp: unbound var {} (fn {})",
                    v,
                    INTERP_FN.with(|f| f.borrow().clone())
                )
            }),
            HExpr::BinF(op, a, b) => {
                let x = f32_of(&self.eval(a, env)?);
                let y = f32_of(&self.eval(b, env)?);
                Value::F32(apply_f32bin(*op, x, y))
            }
            HExpr::BinW(op, a, b, ty) => {
                if ty.is_float() {
                    // f64 arithmetic: the node's width is the semantics.
                    let x = f64_of(&self.eval(a, env)?);
                    let y = f64_of(&self.eval(b, env)?);
                    return Ok(Value::F64(apply_fbin(*op, x, y)));
                }
                let x = int_of(&self.eval(a, env)?);
                let y = int_of(&self.eval(b, env)?);
                // One semantics table with the constant folder.
                // Division/remainder by zero traps, as natively (the
                // contract `unfolded_division_still_traps` pins).
                match cato_core::hir::eval_int_binop(*op, x, y, *ty) {
                    Some(v) => Value::Int(v),
                    None => panic!("interp: integer {:?} by zero", op),
                }
            }
            HExpr::ICast(a, from, target) => {
                let v = self.eval(a, env)?;
                match target {
                    Ty::F64 => Value::F64(match v {
                        Value::Int(i) => {
                            if from.is_uint() {
                                i as u64 as f64
                            } else {
                                i as f64
                            }
                        }
                        Value::F32(f) => f as f64,
                        Value::F64(f) => f,
                        o => panic!("interp: cast to f64 on {:?}", o),
                    }),
                    Ty::F32 => Value::F32(match v {
                        Value::Int(i) => {
                            if from.is_uint() {
                                i as u64 as f32
                            } else {
                                i as f32
                            }
                        }
                        Value::F32(f) => f,
                        Value::F64(f) => f as f32,
                        o => panic!("interp: cast to f32 on {:?}", o),
                    }),
                    t if t.is_int() => {
                        let i = match v {
                            Value::Int(i) => i,
                            Value::F64(f) => {
                                if t.is_uint() {
                                    (f as i64 as u64) as i64
                                } else {
                                    f as i64
                                }
                            }
                            Value::F32(f) => {
                                if t.is_uint() {
                                    (f as i64 as u64) as i64
                                } else {
                                    f as i64
                                }
                            }
                            o => panic!("interp: cast to int on {:?}", o),
                        };
                        Value::Int(wrap(i, *t))
                    }
                    o => panic!("interp: bad cast target {:?}", o),
                }
            }
            HExpr::CmpW(op, a, b, ty) => {
                if ty.is_float() {
                    let (x, y) = (f64_of(&self.eval(a, env)?), f64_of(&self.eval(b, env)?));
                    Value::Bool(apply_fcmp(*op, x, y))
                } else {
                    let (x, y) = (int_of(&self.eval(a, env)?), int_of(&self.eval(b, env)?));
                    if ty.is_uint() {
                        // Top-bit values must order like the backend's `ult`
                        // family: compare the bit patterns as unsigned.
                        let (xu, yu) = (x as u64, y as u64);
                        Value::Bool(match op {
                            cato_core::hir::CmpOp::Eq => xu == yu,
                            cato_core::hir::CmpOp::Ne => xu != yu,
                            cato_core::hir::CmpOp::Lt => xu < yu,
                            cato_core::hir::CmpOp::Gt => xu > yu,
                            cato_core::hir::CmpOp::Le => xu <= yu,
                            cato_core::hir::CmpOp::Ge => xu >= yu,
                        })
                    } else {
                        // Values are already width-normalized; plain i64 compare.
                        Value::Bool(apply_icmp(*op, x, y))
                    }
                }
            }
            HExpr::And(a, b) => {
                let x = bool_of(&self.eval(a, env)?);
                let r = if !x {
                    false
                } else {
                    bool_of(&self.eval(b, env)?)
                };
                Value::Bool(r)
            }
            HExpr::Or(a, b) => {
                let x = bool_of(&self.eval(a, env)?);
                let r = if x {
                    true
                } else {
                    bool_of(&self.eval(b, env)?)
                };
                Value::Bool(r)
            }
            HExpr::Not(a) => Value::Bool(!bool_of(&self.eval(a, env)?)),
            HExpr::Abs(a, t) => {
                let v = self.eval(a, env)?;
                match t {
                    Ty::F64 => Value::F64(f64_of(&v).abs()),
                    _ => Value::Int(int_of(&v).abs()),
                }
            }
            HExpr::Min(a, b, t) => {
                let (x, y) = (self.eval(a, env)?, self.eval(b, env)?);
                if *t == Ty::F64 {
                    if f64_of(&x) < f64_of(&y) {
                        x
                    } else {
                        y
                    }
                } else if int_of(&x) < int_of(&y) {
                    x
                } else {
                    y
                }
            }
            HExpr::Max(a, b, t) => {
                let (x, y) = (self.eval(a, env)?, self.eval(b, env)?);
                if *t == Ty::F64 {
                    if f64_of(&x) > f64_of(&y) {
                        x
                    } else {
                        y
                    }
                } else if int_of(&x) > int_of(&y) {
                    x
                } else {
                    y
                }
            }
            HExpr::Count(inner) => {
                let v = self.eval(inner, env)?;
                let n = match &v {
                    Value::Str(s) => s.len() as i64,
                    Value::Vec(xs) => xs.len() as i64,
                    Value::Map(m) => m.len() as i64,
                    Value::Set(s) => s.len() as i64,
                    o => panic!("interp: count on {:?}", o),
                };
                Value::Int(n)
            }
            HExpr::VecLit(VLit::I64(vs)) => {
                Value::Vec(Rc::new(vs.iter().map(|v| Value::Int(*v)).collect()))
            }
            HExpr::VecLit(VLit::F64(vs)) => {
                Value::Vec(Rc::new(vs.iter().map(|v| Value::F64(*v)).collect()))
            }
            HExpr::VecLit(VLit::N(_, vs)) => {
                Value::Vec(Rc::new(vs.iter().map(|v| Value::Int(*v)).collect()))
            }
            HExpr::VecLit(VLit::F32(vs)) => {
                Value::Vec(Rc::new(vs.iter().map(|v| Value::F32(*v as f32)).collect()))
            }
            HExpr::SetLit(vs) => {
                let mut items: Vec<i64> = vs
                    .iter()
                    .map(|v| int_of(&self.eval(v, env).ok().unwrap_or(Value::Int(0))))
                    .collect();
                items.sort_unstable();
                items.dedup();
                Value::Set(Rc::new(items))
            }
            HExpr::MapLit(pairs) => {
                let mut entries: Vec<(i64, i64)> = Vec::new();
                for (k, v) in pairs.iter() {
                    let kv = int_of(&self.eval(k, env)?);
                    let vv = int_of(&self.eval(v, env)?);
                    entries.push((kv, vv));
                }
                entries.sort_unstable();
                Value::Map(Rc::new(entries))
            }
            HExpr::MapAssoc { m, k, v } => {
                let mv = self.eval(m, env)?;
                let kv = int_of(&self.eval(k, env)?);
                let vv = int_of(&self.eval(v, env)?);
                if let Value::Map(entries) = mv {
                    let mut out: Vec<(i64, i64)> = entries
                        .iter()
                        .copied()
                        .filter(|(ek, _)| *ek != kv)
                        .collect();
                    out.push((kv, vv));
                    out.sort_unstable();
                    Value::Map(Rc::new(out))
                } else {
                    panic!("interp: assoc on non-map")
                }
            }
            HExpr::MapGet {
                src,
                key,
                default,
                res,
            } => {
                let mv = self.eval(src, env)?;
                let kv = int_of(&self.eval(key, env)?);
                if let Some(opt_ty) = res {
                    // Two-argument form: (Option i64), Some on a hit.
                    let found = match &mv {
                        Value::Map(entries) => entries.iter().any(|(ek, _)| *ek == kv),
                        o => panic!("interp: map get on non-map {:?}", o),
                    };
                    let v = if found {
                        match &mv {
                            Value::Map(entries) => entries
                                .iter()
                                .find(|(ek, _)| *ek == kv)
                                .map(|(_, ev)| *ev)
                                .unwrap_or(0),
                            _ => 0,
                        }
                    } else {
                        0
                    };
                    let _ = opt_ty;
                    some_none(found, v)
                } else {
                    let mut res = match default {
                        Some(d) => int_of(&self.eval(d, env)?),
                        None => 0,
                    };
                    if let Value::Map(entries) = mv {
                        // Last match wins (assoc replaces).
                        for (ek, ev) in entries.iter() {
                            if *ek == kv {
                                res = *ev;
                            }
                        }
                    } else {
                        panic!("interp: map get on non-map")
                    }
                    Value::Int(res)
                }
            }
            HExpr::SetConj { s, x } => {
                let sv = self.eval(s, env)?;
                let xv = int_of(&self.eval(x, env)?);
                if let Value::Set(items) = sv {
                    let mut out = items.as_ref().clone();
                    if !out.contains(&xv) {
                        out.push(xv);
                        out.sort_unstable();
                    }
                    Value::Set(Rc::new(out))
                } else {
                    panic!("interp: conj on non-set")
                }
            }
            HExpr::SetDisj { s, x } => {
                let sv = self.eval(s, env)?;
                let xv = int_of(&self.eval(x, env)?);
                if let Value::Set(items) = sv {
                    let out: Vec<i64> = items.iter().copied().filter(|e| *e != xv).collect();
                    Value::Set(Rc::new(out))
                } else {
                    panic!("interp: disj on non-set")
                }
            }
            HExpr::SetContains { s, x } => {
                let sv = self.eval(s, env)?;
                let xv = int_of(&self.eval(x, env)?);
                match sv {
                    Value::Set(items) => Value::Bool(items.binary_search(&xv).is_ok()),
                    o => panic!("interp: contains? on {:?}", o),
                }
            }
            HExpr::Subs { s, start, end } => {
                let sv = self.eval(s, env)?;
                let st = int_of(&self.eval(start, env)?).max(0) as usize;
                let en = int_of(&self.eval(end, env)?).max(0) as usize;
                if let Value::Str(s) = sv {
                    let st = st.min(s.len());
                    let en = en.min(s.len()).max(st);
                    Value::Str(Rc::new(s[st..en].to_string()))
                } else {
                    panic!("interp: subs on non-str")
                }
            }
            HExpr::Split { s, sep } => {
                let sv = self.eval(s, env)?;
                let pv = self.eval(sep, env)?;
                match (sv, pv) {
                    (Value::Str(s), Value::Str(sep)) => {
                        let parts: Vec<Value> = if sep.is_empty() {
                            vec![Value::Str(s.clone())]
                        } else {
                            s.split(sep.as_str())
                                .map(|p| Value::Str(Rc::new(p.to_string())))
                                .collect()
                        };
                        Value::Vec(Rc::new(parts))
                    }
                    _ => panic!("interp: split on non-strings"),
                }
            }
            HExpr::Join { sep, xs } => {
                let pv = self.eval(sep, env)?;
                let xv = self.eval(xs, env)?;
                match (pv, xv) {
                    (Value::Str(sep), Value::Vec(parts)) => {
                        let strs: Vec<String> = parts
                            .iter()
                            .map(|p| match p {
                                Value::Str(s) => s.as_ref().clone(),
                                o => panic!("interp: join on {:?}", o),
                            })
                            .collect();
                        Value::Str(Rc::new(strs.join(&sep)))
                    }
                    _ => panic!("interp: join on non-strings"),
                }
            }
            HExpr::StrCat(args) => {
                let mut out = String::new();
                for a in args {
                    match &self.eval(a, env)? {
                        Value::Str(s) => out.push_str(s),
                        o => panic!("interp: str on {:?}", o),
                    }
                }
                Value::Str(Rc::new(out))
            }
            HExpr::StrCmp(eq, a, b) => {
                let x = self.eval(a, env)?;
                let y = self.eval(b, env)?;
                match (x, y) {
                    (Value::Str(a), Value::Str(b)) => {
                        let same = a == b;
                        Value::Bool(if *eq { same } else { !same })
                    }
                    _ => panic!("interp: string compare on non-strings"),
                }
            }
            HExpr::StrOrd(op, a, b) => {
                let x = self.eval(a, env)?;
                let y = self.eval(b, env)?;
                match (x, y) {
                    (Value::Str(a), Value::Str(b)) => {
                        // Byte-wise order; matches strcmp's sign test.
                        let ord = a.as_bytes().cmp(b.as_bytes());
                        use std::cmp::Ordering::*;
                        let r = match op {
                            cato_core::hir::CmpOp::Lt => ord == Less,
                            cato_core::hir::CmpOp::Gt => ord == Greater,
                            cato_core::hir::CmpOp::Le => ord != Greater,
                            cato_core::hir::CmpOp::Ge => ord != Less,
                            cato_core::hir::CmpOp::Eq | cato_core::hir::CmpOp::Ne => {
                                panic!("interp: StrOrd carries an ordering op")
                            }
                        };
                        Value::Bool(r)
                    }
                    _ => panic!("interp: string compare on non-strings"),
                }
            }
            HExpr::StrNth { s, idx, res } => {
                let sv = self.eval(s, env)?;
                let iv = int_of(&self.eval(idx, env)?);
                match sv {
                    Value::Str(s) => {
                        let found = iv >= 0 && (iv as usize) < s.len();
                        let b = if found {
                            s.as_bytes()[iv as usize] as i64
                        } else {
                            0
                        };
                        match res {
                            Some(_) => some_none(found, b),
                            None => Value::Int(b),
                        }
                    }
                    o => panic!("interp: nth on {:?}", o),
                }
            }
            HExpr::CharStr(c) => {
                let cv = int_of(&self.eval(c, env)?);
                // Low byte only, mirroring LLVM's trunc to i8.
                Value::Str(Rc::new(((cv as u8) as char).to_string()))
            }
            HExpr::Loop { binds, body, .. } => {
                // Initial bindings, then iterate: recur re-binds and jumps
                // to the head; a completing body value ends the loop.
                let names: Vec<String> = binds.iter().map(|(n, _, _)| n.clone()).collect();
                for (n, _, init) in binds {
                    let v = self.eval(init, env)?;
                    env.insert(n.clone(), v);
                }
                loop {
                    self.recur_sink = None;
                    let v = self.eval(body, env)?;
                    match self.recur_sink.take() {
                        Some(args) => {
                            for (n, a) in names.iter().zip(args) {
                                env.insert(n.clone(), a);
                            }
                        }
                        None => return Ok(v),
                    }
                }
            }
            HExpr::Recur(args, _) => {
                let mut out = Vec::new();
                for a in args {
                    out.push(self.eval(a, env)?);
                }
                self.recur_sink = Some(out);
                // Sentinel: validation guarantees the enclosing loop reads
                // the sink before anything else observes this value.
                return Ok(Value::Int(0));
            }
            HExpr::ToStr(e, t) => {
                let v = self.eval(e, env)?;
                let text = match (&v, t) {
                    (Value::Int(i), _) => format!("{}", i),
                    (Value::Bool(b), _) => format!("{}", *b as i64),
                    (Value::F64(f), _) => fmt_f64(*f),
                    (Value::F32(f), _) => fmt_f64(*f as f64),
                    o => panic!("interp: str on {:?}", o),
                };
                Value::Str(Rc::new(text))
            }
            HExpr::ParseNum { s, int, .. } => {
                let sv = self.eval(s, env)?;
                let text = match sv {
                    Value::Str(t) => t.as_ref().clone(),
                    o => panic!("interp: parse on {:?}", o),
                };
                if *int {
                    match text.trim().parse::<i64>() {
                        // Careful: Rust parse rejects "+5"? It accepts it.
                        Ok(v) => Value::Adt {
                            variant: 0,
                            fields: Rc::new(vec![Value::Int(v)]),
                        },
                        Err(_) => Value::Adt {
                            variant: 1,
                            fields: Rc::new(vec![]),
                        },
                    }
                } else {
                    match text.trim().parse::<f64>() {
                        Ok(v) => Value::Adt {
                            variant: 0,
                            fields: Rc::new(vec![Value::F64(v)]),
                        },
                        Err(_) => Value::Adt {
                            variant: 1,
                            fields: Rc::new(vec![]),
                        },
                    }
                }
            }
            HExpr::Slurp(p) => {
                let pv = self.eval(p, env)?;
                let path = match pv {
                    Value::Str(t) => t.as_ref().clone(),
                    o => panic!("interp: slurp on {:?}", o),
                };
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                Value::Str(Rc::new(text))
            }
            HExpr::Spit { path, data } => {
                let pv = self.eval(path, env)?;
                let dv = self.eval(data, env)?;
                let path = match pv {
                    Value::Str(t) => t.as_ref().clone(),
                    o => panic!("interp: spit path on {:?}", o),
                };
                let data = match dv {
                    Value::Str(t) => t.as_ref().clone(),
                    o => panic!("interp: spit data on {:?}", o),
                };
                let _ = std::fs::write(&path, data.as_bytes());
                Value::Int(0)
            }
            HExpr::Args => Value::Vec(Rc::new(
                self.program_args
                    .iter()
                    .map(|a| Value::Str(Rc::new(a.clone())))
                    .collect(),
            )),
            HExpr::Exit(c) => {
                let code = int_of(&self.eval(c, env)?);
                // Flush the captured output first: the CLI writes it to
                // stdout at the end of a normal run.
                use std::io::Write;
                let mut out = std::io::stdout();
                let _ = out.write_all(self.out.as_bytes());
                let _ = out.flush();
                std::process::exit(code as i32);
            }
            HExpr::Get {
                src,
                idx,
                default,
                ty,
                res,
            } => {
                let _ = ty;
                let sv = self.eval(src, env)?;
                let iv = int_of(&self.eval(idx, env)?);
                match sv {
                    Value::Vec(xs) => {
                        let found = iv >= 0 && (iv as usize) < xs.len();
                        match res {
                            Some(_) => {
                                let v = if found {
                                    xs[iv as usize].clone()
                                } else {
                                    Value::Int(0)
                                };
                                some_none(found, int_of(&v))
                            }
                            None => {
                                if found {
                                    xs[iv as usize].clone()
                                } else {
                                    match default {
                                        Some(d) => self.eval(d, env)?,
                                        None => Value::Int(0),
                                    }
                                }
                            }
                        }
                    }
                    o => panic!("interp: get on {:?}", o),
                }
            }
            HExpr::Conj { v, x, .. } => {
                let vv = self.eval(v, env)?;
                let xv = self.eval(x, env)?;
                match vv {
                    Value::Vec(xs) => {
                        let mut out = xs.as_ref().clone();
                        out.push(xv);
                        Value::Vec(Rc::new(out))
                    }
                    o => panic!("interp: conj on {:?}", o),
                }
            }
            HExpr::Subvec { v, start, end, .. } => {
                let vv = self.eval(v, env)?;
                let st = int_of(&self.eval(start, env)?).max(0) as usize;
                let en = int_of(&self.eval(end, env)?).max(0) as usize;
                match vv {
                    Value::Vec(xs) => {
                        let en = en.min(xs.len());
                        let st = st.min(en);
                        Value::Vec(Rc::new(xs[st..en].to_vec()))
                    }
                    o => panic!("interp: subvec on {:?}", o),
                }
            }
            HExpr::For { el, src, body, .. } => {
                let sv = self.eval(src, env)?;
                let mut out = Vec::new();
                if let Value::Vec(xs) = sv {
                    for x in xs.iter() {
                        env.insert(el.clone(), x.clone());
                        out.push(self.eval(body, env)?);
                    }
                } else {
                    panic!("interp: for on non-vector");
                }
                Value::Vec(Rc::new(out))
            }
            HExpr::Ctor { variant, args, .. } => {
                let fields: Vec<Value> = args
                    .iter()
                    .map(|a| self.eval(a, env))
                    .collect::<R<Vec<_>>>()?;
                Value::Adt {
                    variant: *variant,
                    fields: Rc::new(fields),
                }
            }
            HExpr::Match { scrut, arms, .. } => {
                let sv = self.eval(scrut, env)?;
                let (variant, fields) = match &sv {
                    Value::Adt { variant, fields } => (*variant, fields.clone()),
                    o => panic!(
                        "interp: match on {:?} (fn {})",
                        o,
                        INTERP_FN.with(|f| f.borrow().clone())
                    ),
                };
                for arm in arms {
                    if arm.variant == Some(variant) || arm.variant.is_none() {
                        let mut child = env.clone();
                        for (b, f) in arm.binders.iter().zip(fields.iter()) {
                            child.insert(b.clone(), f.clone());
                        }
                        return self.eval(&arm.body, &mut child);
                    }
                }
                return Err(format!(
                    "interp: match fell through (fn {})",
                    INTERP_FN.with(|f| f.borrow().clone())
                ));
            }
            HExpr::If(c, t, f, _) => {
                if bool_of(&self.eval(c, env)?) {
                    self.eval(t, env)?
                } else {
                    self.eval(f, env)?
                }
            }
            HExpr::Closure {
                params,
                captures,
                body,
                ..
            } => {
                let caps = captures
                    .iter()
                    .map(|(n, _)| {
                        let v = env
                            .get(n)
                            .cloned()
                            .unwrap_or_else(|| panic!("interp: capture {} unbound", n));
                        (n.clone(), v)
                    })
                    .collect();
                Value::Closure {
                    params: params.clone(),
                    captures: caps,
                    body: Box::new((**body).clone()),
                }
            }
            HExpr::ClosureCall { f, args, .. } => {
                let fv = self.eval(f, env)?;
                let (params, caps, body) = match fv {
                    Value::Closure {
                        params,
                        captures,
                        body,
                    } => (params, captures, body),
                    o => panic!("interp: calling non-closure {:?}", o),
                };
                let mut child = env.clone();
                for (n, v) in caps {
                    child.insert(n, v);
                }
                let vals: Vec<Value> = args
                    .iter()
                    .map(|a| self.eval(a, env))
                    .collect::<R<Vec<_>>>()?;
                for ((n, _), v) in params.iter().zip(vals) {
                    child.insert(n.clone(), v);
                }
                self.eval(&body, &mut child)?
            }
            HExpr::Let(binds, body, _frees, _rebind) => {
                let mut child = env.clone();
                for (name, value, _ty) in binds {
                    let v = self.eval(value, &mut child)?;
                    child.insert(name.clone(), v);
                }
                self.eval(body, &mut child)?
            }
            HExpr::Seq(vs) => {
                let mut last = Value::Int(0);
                for v in vs {
                    last = self.eval(v, env)?;
                }
                last
            }
            HExpr::Call(name, args) => {
                let vals: Vec<Value> = args
                    .iter()
                    .map(|a| self.eval(a, env))
                    .collect::<R<Vec<_>>>()?;
                self.call(name, &vals)?
            }
            HExpr::Reduce(r) => self.eval_reduce(r, env)?,
            HExpr::Clone(inner) => self.eval(inner, env)?,
            HExpr::DropStmt(_) => Value::Int(0),
        })
    }

    fn eval_reduce(
        &mut self,
        r: &cato_core::hir::HReduce,
        env: &mut HashMap<String, Value>,
    ) -> R<Value> {
        use cato_core::hir::{BinOp, HReduce};
        let (init, src, maps, filters, closure): (
            _,
            _,
            _,
            _,
            Option<(&String, &String, &cato_core::hir::HExpr)>,
        ) = match r {
            HReduce::Pipeline {
                init,
                src,
                maps,
                filters,
                ..
            } => (init, src, maps, filters, None),
            HReduce::Closure {
                init,
                src,
                maps,
                filters,
                acc,
                el,
                body,
                ..
            } => (init, src, maps, filters, Some((acc, el, body))),
        };
        let mut acc = self.eval(init, env)?;
        let items = self.source_items(src, env)?;
        for (el, el2) in items {
            let mut child = env.clone();
            let mut cur = el.clone();
            let mut skip = false;
            for f in filters {
                child.insert(f.el.clone(), cur.clone());
                if let Some(e2) = &f.el2 {
                    if let Some(v2) = &el2 {
                        child.insert(e2.clone(), v2.clone());
                    }
                }
                if !bool_of(&self.eval(&f.body, &mut child)?) {
                    skip = true;
                    break;
                }
            }
            if skip {
                continue;
            }
            for m in maps {
                child.insert(m.el.clone(), cur.clone());
                if let Some(e2) = &m.el2 {
                    if let Some(v2) = &el2 {
                        child.insert(e2.clone(), v2.clone());
                    }
                }
                cur = self.eval(&m.body, &mut child)?;
            }
            match closure {
                Some((accname, elname, body)) => {
                    child.insert(accname.clone(), acc.clone());
                    child.insert(elname.clone(), cur);
                    acc = self.eval(body, &mut child)?;
                }
                None => {
                    // Pipeline: combine is a BinOp over (acc, el).
                    let combine = match r {
                        HReduce::Pipeline { combine, .. } => *combine,
                        _ => unreachable!(),
                    };
                    if matches!(acc, Value::F64(_)) || matches!(cur, Value::F64(_)) {
                        acc = Value::F64(apply_fbin(combine, f64_of(&acc), f64_of(&cur)));
                    } else {
                        let (x, y) = (int_of(&acc), int_of(&cur));
                        let v =
                            cato_core::hir::eval_int_binop(combine, x, y, cato_core::hir::Ty::I64)
                                .unwrap_or_else(|| panic!("interp: integer {:?} by zero", combine));
                        acc = Value::Int(v);
                    };
                }
            }
        }
        Ok(acc)
    }

    /// Materialize a fused-loop source into (element, optional zip partner)
    /// pairs.
    fn source_items(
        &mut self,
        src: &cato_core::hir::HSource,
        env: &mut HashMap<String, Value>,
    ) -> R<Vec<(Value, Option<Value>)>> {
        use cato_core::hir::HSource;
        Ok(match src {
            HSource::Var(v) => {
                let v = env
                    .get(v)
                    .cloned()
                    .unwrap_or_else(|| panic!("interp: source var {} unbound", v));
                match v {
                    Value::Vec(xs) => xs.iter().map(|x| (x.clone(), None)).collect(),
                    o => panic!("interp: source on {:?}", o),
                }
            }
            HSource::Lit(vlit) => match vlit {
                cato_core::hir::VLit::I64(vs) => {
                    vs.iter().map(|v| (Value::Int(*v), None)).collect()
                }
                cato_core::hir::VLit::N(_, vs) => {
                    vs.iter().map(|v| (Value::Int(*v), None)).collect()
                }
                cato_core::hir::VLit::F32(vs) => {
                    vs.iter().map(|v| (Value::F32(*v as f32), None)).collect()
                }
                cato_core::hir::VLit::F64(vs) => {
                    vs.iter().map(|v| (Value::F64(*v), None)).collect()
                }
            },
            HSource::Range(a, b) => {
                let x = int_of(&self.eval(a, env)?);
                let y = int_of(&self.eval(b, env)?);
                (x..y).map(|i| (Value::Int(i), None)).collect()
            }
            HSource::Zip(a, b) => {
                let mut lhs = self.source_items(a, env)?;
                let rhs = self.source_items(b, env)?;
                for (i, (_, r)) in lhs.iter_mut().enumerate() {
                    *r = rhs.get(i).map(|(v, _)| v.clone());
                }
                lhs
            }
            HSource::Take(n, inner) => {
                let n = int_of(&self.eval(n, env)?) as usize;
                let mut items = self.source_items(inner, env)?;
                items.truncate(n);
                items
            }
            HSource::Drop(n, inner) => {
                let n = int_of(&self.eval(n, env)?) as usize;
                let mut items = self.source_items(inner, env)?;
                items.drain(..n.min(items.len()));
                items
            }
        })
    }
}

fn default_value(t: cato_core::hir::Ty) -> Value {
    use cato_core::hir::Ty;
    match t {
        Ty::F64 => Value::F64(0.0),
        Ty::F32 => Value::F32(0.0),
        Ty::Bool => Value::Bool(false),
        Ty::Str => Value::Str(Rc::new(String::new())),
        Ty::Vec(_) => Value::Vec(Rc::new(Vec::new())),
        Ty::Map => Value::Map(Rc::new(Vec::new())),
        Ty::Set => Value::Set(Rc::new(Vec::new())),
        _ => Value::Int(0),
    }
}

fn int_of(v: &Value) -> i64 {
    match v {
        Value::Int(i) => *i,
        Value::Bool(b) => *b as i64,
        o => panic!(
            "interp: expected int, got {:?} (fn {})",
            o,
            INTERP_FN.with(|f| f.borrow().clone())
        ),
    }
}

fn f64_of(v: &Value) -> f64 {
    match v {
        Value::F64(f) => *f,
        Value::F32(f) => *f as f64,
        Value::Int(i) => *i as f64,
        o => panic!("interp: expected float, got {:?}", o),
    }
}

fn f32_of(v: &Value) -> f32 {
    match v {
        Value::F32(f) => *f,
        Value::F64(f) => *f as f32,
        o => panic!("interp: expected f32, got {:?}", o),
    }
}

fn bool_of(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        o => panic!("interp: expected bool, got {:?}", o),
    }
}

/// Build the interpreter's `(Option i64)` value: variant 0 = Some, 1 = None
/// (declaration order in the prelude).
fn some_none(found: bool, v: i64) -> Value {
    Value::Adt {
        variant: if found { 0 } else { 1 },
        fields: Rc::new(vec![Value::Int(v)]),
    }
}

fn apply_fbin(op: cato_core::hir::BinOp, x: f64, y: f64) -> f64 {
    match op {
        cato_core::hir::BinOp::Add => x + y,
        cato_core::hir::BinOp::Sub => x - y,
        cato_core::hir::BinOp::Mul => x * y,
        cato_core::hir::BinOp::Div => x / y,
        cato_core::hir::BinOp::Rem => x % y,
        o => unreachable!(
            "integer-only operator {:?} on floats (lowering rejects it)",
            o
        ),
    }
}

fn apply_f32bin(op: cato_core::hir::BinOp, x: f32, y: f32) -> f32 {
    match op {
        cato_core::hir::BinOp::Add => x + y,
        cato_core::hir::BinOp::Sub => x - y,
        cato_core::hir::BinOp::Mul => x * y,
        cato_core::hir::BinOp::Div => x / y,
        cato_core::hir::BinOp::Rem => x % y,
        o => unreachable!(
            "integer-only operator {:?} on floats (lowering rejects it)",
            o
        ),
    }
}

fn apply_icmp(op: cato_core::hir::CmpOp, x: i64, y: i64) -> bool {
    use cato_core::hir::CmpOp;
    match op {
        CmpOp::Eq => x == y,
        CmpOp::Ne => x != y,
        CmpOp::Lt => x < y,
        CmpOp::Gt => x > y,
        CmpOp::Le => x <= y,
        CmpOp::Ge => x >= y,
    }
}

fn apply_fcmp(op: cato_core::hir::CmpOp, x: f64, y: f64) -> bool {
    use cato_core::hir::CmpOp;
    match op {
        CmpOp::Eq => x == y,
        CmpOp::Ne => x != y,
        CmpOp::Lt => x < y,
        CmpOp::Gt => x > y,
        CmpOp::Le => x <= y,
        CmpOp::Ge => x >= y,
    }
}

/// `%g` formatting to match the C runtime's printf("%g\n"): 6 significant
/// digits, trailing zeros trimmed, exponent form outside 1e-4..1e7.
/// Public: the S5 differential oracle renders kernel-reader f64 atoms
/// through this same function, so both sides share one float contract.
pub fn fmt_f64(v: f64) -> String {
    if v == 0.0 {
        return "0".to_string();
    }
    let a = v.abs();
    if !(1e-4..1e6).contains(&a) {
        // Exponent form: C prints 6 significant digits and a 2+ digit exp.
        let s = format!("{:.5e}", v); // e.g. "1.04858e6"
        if let Some(pos) = s.find('e') {
            let (mant, exp) = s.split_at(pos);
            let exp = &exp[1..];
            let (sign, digits) = match exp.strip_prefix('-') {
                Some(d) => ('-', d),
                None => ('+', exp),
            };
            return format!("{}e{}{:0>2}", mant, sign, digits);
        }
        s
    } else {
        let prec = (5 - a.log10().floor() as i32).max(0) as usize;
        format!("{:.*}", prec, v)
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

/// The interpreter backend: `emit` executes the program and returns the
/// captured stdout as the artifact.
#[derive(Default)]
pub struct InterpBackend {
    /// Arguments visible to `(args)` (everything after the source file).
    pub program_args: Vec<String>,
}

impl Backend for InterpBackend {
    fn name(&self) -> &'static str {
        "interp"
    }

    fn emit(&self, program: &Program) -> Result<String, String> {
        let mut interp = Interpreter::new(program);
        interp.set_program_args(self.program_args.clone());
        interp.run()
    }
}

/// REPL-friendly rendering of a value.
pub fn fmt_value(v: &Value) -> String {
    match v {
        Value::Int(i) => i.to_string(),
        Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        Value::F64(f) => {
            let s = fmt_f64(*f);
            if s.contains('.') || s.contains('e') {
                s
            } else {
                format!("{}.0", s)
            }
        }
        Value::F32(f) => format!("{}f", fmt_f64(*f as f64)),
        Value::Str(s) => format!("{:?}", s),
        Value::Vec(xs) => {
            let inner: Vec<String> = xs.iter().map(fmt_value).collect();
            format!("[{}]", inner.join(" "))
        }
        Value::Set(xs) => {
            let inner: Vec<String> = xs.iter().map(|v| v.to_string()).collect();
            format!("#{{{}}}", inner.join(" "))
        }
        Value::Map(entries) => {
            let inner: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("{} {}", k, v))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        Value::Adt { variant, fields } => {
            let inner: Vec<String> = fields.iter().map(fmt_value).collect();
            format!("#<variant {} ({})>", variant, inner.join(" "))
        }
        Value::Closure { .. } => "#<closure>".to_string(),
    }
}
