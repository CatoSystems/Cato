use super::*;

/// Saved binding-map entries for the names one emission scope rebinds.
#[derive(Default)]
pub(crate) struct BindScope {
    scalar: Vec<(String, Option<(String, Ty)>)>,
    vecs: Vec<(String, Option<(String, String, Elem)>)>,
    grow: Vec<(String, Option<(String, Elem)>)>,
    growm: Vec<(String, Option<(String, Ty)>)>,
}

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
    /// Innermost `loop` (S2): variable slots `(name, alloca, ty)` and the
    /// head block label that `recur` branches back to.
    pub(crate) loop_frame: Option<(Vec<(String, String, Ty)>, String)>,
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
            loop_frame: None,
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
    /// Snapshot the binding maps for `names` before a scope rebinds them.
    /// `let` bodies, match arm binders and loop variables must not leak
    /// past their extent: the flat maps would hand a shadowed name's
    /// register to code outside the scope — a dominance failure across
    /// sibling match arms, or a silently wrong read after a shadowing let.
    pub(crate) fn save_binds(&self, names: &[String]) -> BindScope {
        let mut s = BindScope::default();
        for n in names {
            s.scalar.push((n.clone(), self.scalar.get(n).cloned()));
            s.vecs.push((n.clone(), self.vecs.get(n).cloned()));
            s.grow.push((n.clone(), self.grow.get(n).cloned()));
            s.growm.push((n.clone(), self.growm.get(n).cloned()));
        }
        s
    }
    /// Put back what `save_binds` captured, dropping entries the scope
    /// introduced fresh.
    pub(crate) fn restore_binds(&mut self, s: BindScope) {
        for (n, prev) in s.scalar {
            match prev {
                Some(b) => {
                    self.scalar.insert(n, b);
                }
                None => {
                    self.scalar.remove(&n);
                }
            }
        }
        for (n, prev) in s.vecs {
            match prev {
                Some(b) => {
                    self.vecs.insert(n, b);
                }
                None => {
                    self.vecs.remove(&n);
                }
            }
        }
        for (n, prev) in s.grow {
            match prev {
                Some(b) => {
                    self.grow.insert(n, b);
                }
                None => {
                    self.grow.remove(&n);
                }
            }
        }
        for (n, prev) in s.growm {
            match prev {
                Some(b) => {
                    self.growm.insert(n, b);
                }
                None => {
                    self.growm.remove(&n);
                }
            }
        }
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
            loop_frame: self.loop_frame.clone(),
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
        Elem::Str => Ty::Str,
        Elem::Param(_) => panic!("codegen: unresolved element type parameter"),
    }
}

pub(crate) fn mangle(name: &str) -> String {
    let m = name.replace('-', "_");
    if reserved_local_shape(&m) {
        // User identifiers and the emitter's synthesized locals share the
        // function's local-value namespace: temps are `%t<n>`, block labels
        // are `b<n>` (branch targets are `%b<n>`), the entry label is
        // `entry`. A name in one of those shapes takes another `_u`; the
        // escaped form matches the same shape, so the map stays injective
        // and no user local can shadow a synthesized one.
        format!("{}_u", m)
    } else {
        m
    }
}

/// Does `s` (already `-`→`_` folded) collide with an emitter-minted local?
fn reserved_local_shape(s: &str) -> bool {
    if s == "entry" {
        return true;
    }
    let rest = match s.strip_prefix('t').or_else(|| s.strip_prefix('b')) {
        Some(r) => r,
        None => return false,
    };
    let digits: usize = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return false;
    }
    // Prior escapes leave zero or more `_u` pairs after the digits.
    let mut tail = &rest[digits..];
    while let Some(t) = tail.strip_prefix("_u") {
        tail = t;
    }
    tail.is_empty()
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
