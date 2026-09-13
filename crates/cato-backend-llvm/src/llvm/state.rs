use super::*;

pub(crate) struct Mdl {
    pub(crate) vec_globals: Vec<String>,
    pub(crate) adts: Vec<AdtDef>,
    pub(crate) counter: usize,
    /// Emit `llvm.loop.vectorize.enable` metadata on fused loops (--fast).
    pub(crate) fast: bool,
    /// Deduplicated `declare` lines emitted lazily (runtime helpers and
    /// user externs share the namespace).
    pub(crate) decls: Vec<String>,
}

impl Mdl {
    pub(crate) fn new(adts: Vec<AdtDef>, fast: bool) -> Self {
        Mdl {
            vec_globals: Vec::new(),
            adts,
            counter: 0,
            fast,
            decls: Vec::new(),
        }
    }
    /// Emit a `declare` once per symbol (deduplicated by line).
    pub(crate) fn declare(&mut self, line: String) {
        if !self.decls.contains(&line) {
            self.decls.push(line);
        }
    }
    pub(crate) fn adt_name(&self, idx: u32) -> String {
        format!("%{}", self.adts[idx as usize].name)
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
            Ty::Adt(_) | Ty::Str => "ptr".into(),
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
        }
        let elem = match lit {
            VLit::I64(_) => Elem::I64,
            VLit::F64(_) => Elem::F64,
        };
        let len = match lit {
            VLit::I64(vs) => vs.len(),
            VLit::F64(vs) => vs.len(),
        };
        (label, len.to_string(), elem)
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
            Elem::Param(_) => panic!("codegen: unresolved element type parameter"),
        }
    }
    pub(crate) fn globals(&self) -> &[String] {
        &self.vec_globals
    }
}

// ---------------------------------------------------------------------------
// Function emitter
// ---------------------------------------------------------------------------
