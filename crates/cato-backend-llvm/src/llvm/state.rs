use super::*;
use cato_core::Lambda;

pub(crate) struct Mdl {
    pub(crate) vec_globals: Vec<String>,
    pub(crate) adts: Vec<AdtDef>,
    /// Per-ADT: heap-boxed (recursive or owning) or inline. Boxed ADT
    /// values are heap pointers; inline ADT values are pointers to stack
    /// slots of the named struct. Emission paths consult this to pick the
    /// convention.
    pub(crate) boxed: Vec<bool>,
    /// Per-ADT: owns heap memory (needs drop glue).
    pub(crate) owns: Vec<bool>,
    pub(crate) counter: usize,
    /// Emit `llvm.loop.vectorize.enable` metadata on fused loops (--fast).
    pub(crate) fast: bool,
    /// Deduplicated `declare` lines emitted lazily (runtime helpers and
    /// user externs share the namespace).
    pub(crate) decls: Vec<String>,
    /// Closure signature table (`Ty::Fn(i)` indexes it).
    pub(crate) fn_sigs: Vec<(Ty, Vec<Ty>)>,
    /// Out-of-line closure bodies.
    pub(crate) lambdas: Vec<Lambda>,
    /// Thread count for `(parallel ...)` loops (`--threads`).
    pub(crate) threads: usize,
    /// Externs returning `str`: name -> is the result owned (`(own str)`)?
    /// A NULL `char*` from C reads as the empty string (see `call_user`).
    pub(crate) extern_str: HashMap<String, bool>,
}

impl Mdl {
    pub(crate) fn new(
        adts: Vec<AdtDef>,
        fast: bool,
        fn_sigs: Vec<(Vec<Ty>, Ty)>,
        lambdas: Vec<Lambda>,
        threads: usize,
    ) -> Self {
        let facts = cato_core::compute_adt_facts(&adts);
        let owns = facts.owns;
        let boxed = facts.boxed;
        Mdl {
            vec_globals: Vec::new(),
            adts,
            boxed,
            owns,
            counter: 0,
            fast,
            decls: Vec::new(),
            fn_sigs: fn_sigs.into_iter().map(|(p, r)| (r, p)).collect(),
            lambdas,
            threads,
            extern_str: HashMap::new(),
        }
    }
    /// Emit a `declare` once per symbol (deduplicated by line).
    /// Add a module-level declaration once. Function declarations are
    /// keyed by symbol: a user `extern` and a runtime helper the backend
    /// itself calls (`strdup`, `strlen`, …) share one `declare`.
    pub(crate) fn declare(&mut self, line: String) {
        let sym = |l: &str| -> Option<String> {
            if !l.starts_with("declare ") {
                return None;
            }
            let at = l.find('@')?;
            let end = l[at..].find('(')? + at;
            Some(l[at..end].to_string())
        };
        let dup = match sym(&line) {
            Some(s) => self.decls.iter().any(|d| sym(d).as_deref() == Some(s.as_str())),
            None => self.decls.contains(&line),
        };
        if !dup {
            self.decls.push(line);
        }
    }
    pub(crate) fn adt_name(&self, idx: u32) -> String {
        format!("%{}", self.adts[idx as usize].name)
    }
    /// Is this ADT heap-boxed (recursive or owning)?
    pub(crate) fn adt_boxed(&self, idx: u32) -> bool {
        self.boxed.get(idx as usize).copied().unwrap_or(false)
    }
    /// Does this ADT own heap memory (needs drop glue)?
    pub(crate) fn adt_owns(&self, idx: u32) -> bool {
        self.owns.get(idx as usize).copied().unwrap_or(false)
    }
    /// Payload slots of ADT `idx` (the [N x i64] of its struct).
    pub(crate) fn adt_slots(&self, idx: u32) -> usize {
        cato_core::adt_total_slots(&self.adts, &self.boxed, idx)
    }
    /// Per-variant, per-field slot offsets of ADT `idx`.
    pub(crate) fn adt_field_offsets(&self, idx: u32) -> Vec<Vec<usize>> {
        self.adts[idx as usize].field_slots(&self.boxed, &self.adts)
    }
    /// LLVM scalar/pointer type for a Ty in value position. ADT values are
    /// always `ptr` to a stack slot (the named struct is only used for
    /// alloca/GEP), so signatures stay pointer-based — no boxing, no heap.
    pub(crate) fn ll_ty(&self, t: Ty) -> String {
        match t {
            Ty::I64 | Ty::U64 | Ty::Usize => "i64".into(),
            Ty::I32 | Ty::U32 => "i32".into(),
            Ty::I16 | Ty::U16 => "i16".into(),
            Ty::I8 | Ty::U8 => "i8".into(),
            Ty::F64 => "double".into(),
            Ty::F32 => "float".into(),
            Ty::Bool => "i1".into(),
            Ty::Vec(_) | Ty::Map | Ty::Set => {
                panic!("codegen: aggregate type in scalar position")
            }
            Ty::Adt(_) | Ty::Str | Ty::Fn(_) => "ptr".into(),
            Ty::Param(_) => panic!("codegen: unresolved type parameter"),
        }
    }
    pub(crate) fn mint_vec(&mut self, lit: &VLit) -> (String, String, Elem) {
        let id = self.counter;
        self.counter += 1;
        let label = format!("@g_vec_{}", id);
        match lit {
            VLit::I64(vs) => {
                let items: Vec<String> = vs.iter().map(|v| format!("i64 {}", v)).collect();
                self.vec_globals.push(format!(
                    "{} = private constant [{} x i64] [{}]",
                    label,
                    vs.len(),
                    items.join(", ")
                ));
            }
            VLit::F64(vs) => {
                let items: Vec<String> = vs
                    .iter()
                    .map(|v| format!("double {}", llvm_f64(*v)))
                    .collect();
                self.vec_globals.push(format!(
                    "{} = private constant [{} x double] [{}]",
                    label,
                    vs.len(),
                    items.join(", ")
                ));
            }
            VLit::N(_, vs) => {
                let ll = self.elem_ll(lit.elem());
                let items: Vec<String> = vs.iter().map(|v| format!("{} {}", ll, v)).collect();
                self.vec_globals.push(format!(
                    "{} = private constant [{} x {}] [{}]",
                    label,
                    vs.len(),
                    ll,
                    items.join(", ")
                ));
            }
            VLit::F32(vs) => {
                // LLVM spells a float constant as the double holding the
                // nearest f32 value.
                let items: Vec<String> = vs
                    .iter()
                    .map(|v| format!("float {}", llvm_f64((*v as f32) as f64)))
                    .collect();
                self.vec_globals.push(format!(
                    "{} = private constant [{} x float] [{}]",
                    label,
                    vs.len(),
                    items.join(", ")
                ));
            }
        }
        (label, lit.len().to_string(), lit.elem())
    }
    /// Mint a private NUL-terminated `[N x i8]` constant for a string
    /// literal; returns the global label.
    pub(crate) fn mint_str(&mut self, s: &str) -> String {
        let id = self.counter;
        self.counter += 1;
        let label = format!("@g_str_{}", id);
        let mut items: Vec<String> = s
            .bytes()
            .map(|b| format!("i8 {}", b))
            .collect();
        items.push("i8 0".to_string()); // NUL terminator
        self.vec_globals.push(format!(
            "{} = private unnamed_addr constant [{} x i8] [{}]",
            label,
            items.len(),
            items.join(", ")
        ));
        label
    }
    /// Lazy declare of a libc helper used by heap construction.
    pub(crate) fn ensure_decl(&mut self, line: &str) {
        self.declare(line.to_string());
    }
    /// LLVM type of a vector element.
    /// LLVM type of a scalar value in register/param position.
    pub(crate) fn ll_scalar(t: Ty) -> &'static str {
        match t {
            Ty::I64 | Ty::U64 | Ty::Usize => "i64",
            Ty::I32 | Ty::U32 => "i32",
            Ty::I16 | Ty::U16 => "i16",
            Ty::I8 | Ty::U8 => "i8",
            Ty::F64 => "double",
            Ty::F32 => "float",
            _ => panic!("ll_scalar on non-scalar {:?}", t),
        }
    }

    pub(crate) fn elem_ll(&self, e: Elem) -> &'static str {
        match e {
            Elem::I64 => "i64",
            Elem::F64 => "double",
            Elem::Str => "ptr",
            Elem::N(n) => match n {
                Narrow::I8 | Narrow::U8 => "i8",
                Narrow::I16 | Narrow::U16 => "i16",
                Narrow::I32 | Narrow::U32 => "i32",
                Narrow::U64 => "i64",
                Narrow::F32 => "float",
            },
            Elem::Param(_) => panic!("codegen: unresolved element type parameter"),
        }
    }
    /// One element's store: its LLVM type at its own alignment (a narrow
    /// element is not 8-byte aligned — claiming `align 8` would be UB).
    pub(crate) fn elem_store(&self, e: Elem, v: &str, p: &str) -> String {
        format!("  store {} {}, ptr {}, align {}", self.elem_ll(e), v, p, e.bytes())
    }
    pub(crate) fn elem_load(&self, e: Elem, dst: &str, p: &str) -> String {
        format!("  {} = load {}, ptr {}, align {}", dst, self.elem_ll(e), p, e.bytes())
    }
    pub(crate) fn globals(&self) -> &[String] {
        &self.vec_globals
    }
}

// ---------------------------------------------------------------------------
// Function emitter
// ---------------------------------------------------------------------------
