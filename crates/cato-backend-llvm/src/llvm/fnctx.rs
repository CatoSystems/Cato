use super::*;

pub(crate) struct FnCtx {
    pub(crate) counter: usize,
    pub(crate) blk: usize,
    pub(crate) cur: String,
    pub(crate) lines: Vec<String>,
    /// Allocas hoisted to the function entry prelude (loop-safe ADT slots).
    pub(crate) prelude: Vec<String>,
    pub(crate) scalar: HashMap<String, (String, Ty)>,
    pub(crate) vecs: HashMap<String, (String, String, Elem)>,
    /// Growable heap bindings: name -> alloca slot holding
    /// `%VecI64 { ptr, len, cap }` (in-place conj mutates through it).
    pub(crate) grow: HashMap<String, (String, Elem)>,
    /// Growable map bindings: name -> header slot (entries are 16 bytes).
    pub(crate) growm: HashMap<String, (String, Ty)>,
}

impl FnCtx {
    pub(crate) fn new() -> Self {
        FnCtx {
            counter: 0,
            blk: 0,
            cur: String::new(),
            lines: Vec::new(),
            prelude: Vec::new(),
            scalar: HashMap::new(),
            vecs: HashMap::new(),
            grow: HashMap::new(),
            growm: HashMap::new(),
        }
    }
    pub(crate) fn temp(&mut self) -> String {
        let n = self.counter;
        self.counter += 1;
        format!("%t{}", n)
    }
    pub(crate) fn block_name(&mut self) -> String {
        let n = self.blk;
        self.blk += 1;
        format!("b{}", n)
    }
    pub(crate) fn open(&mut self, label: &str) -> String {
        let l = label.to_string();
        self.cur = l.clone();
        l
    }
    pub(crate) fn line(&mut self, s: &str) {
        self.lines.push(s.to_string());
    }
    pub(crate) fn clone_with_lines(&self, lines: Vec<String>) -> Self {
        FnCtx {
            counter: self.counter,
            blk: self.blk,
            cur: self.cur.clone(),
            lines,
            prelude: self.prelude.clone(),
            scalar: self.scalar.clone(),
            vecs: self.vecs.clone(),
            grow: self.grow.clone(),
            growm: self.growm.clone(),
        }
    }
    /// Splice hoisted entry allocas directly after the `entry:` label.
    pub(crate) fn with_prelude(mut lines: Vec<String>, prelude: &[String]) -> Vec<String> {
        if !prelude.is_empty() {
            let pos = lines
                .iter()
                .position(|l| l == "entry:")
                .expect("entry block");
            for (k, p) in prelude.iter().enumerate() {
                lines.insert(pos + 1 + k, p.clone());
            }
        }
        lines
    }
}

/// Scalar type of a concrete vector element.
pub(crate) fn elem_ty_of(e: Elem) -> Ty {
    match e {
        Elem::I64 => Ty::I64,
        Elem::F64 => Ty::F64,
        Elem::Param(_) => panic!("codegen: unresolved element type parameter"),
    }
}

pub(crate) fn mangle(name: &str) -> String {
    name.replace('-', "_")
}

pub(crate) fn llvm_f64(v: f64) -> String {
    // LLVM decimal FP constants need an explicit decimal point before 'e'
    // (Rust's {:e} renders "0e0", which LLVM reads as an integer constant).
    let s = format!("{:e}", v);
    if let Some(pos) = s.find('e') {
        if s[..pos].contains('.') {
            s
        } else {
            format!("{}.0{}", &s[..pos], &s[pos..])
        }
    } else if s.contains('.') {
        s
    } else {
        format!("{}.0", s)
    }
}

// ---------------------------------------------------------------------------
// Loop function emission
// ---------------------------------------------------------------------------

/// Peel take/drop wrappers off a loop source, returning them in application
/// order (innermost first) plus the base source. `is_drop` distinguishes the
/// two wrapper kinds.
pub(crate) fn pin_value(ll_ty: &str, r: &str, c: &mut FnCtx) -> String {
    if r.starts_with('%') {
        return r.to_string();
    }
    let t = c.temp();
    if ll_ty == "double" {
        c.line(&format!("  {} = fadd double {}, 0.000000e+00", t, r));
    } else if ll_ty == "i1" {
        c.line(&format!("  {} = xor i1 {}, false", t, r));
    } else {
        c.line(&format!("  {} = add i64 {}, 0", t, r));
    }
    t
}
