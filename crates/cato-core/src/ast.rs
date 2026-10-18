/// A byte-offset range into the source text (`lo` inclusive, `hi` exclusive).
/// `file` is a 1-based id into the diagnostics source registry (0 = unknown);
/// the zero span means "unknown" — diagnostics render without a location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub lo: usize,
    pub hi: usize,
    pub file: u32,
}

impl Span {
    /// A span in an unregistered file (stamped later by the module loader).
    pub fn new(lo: usize, hi: usize) -> Span {
        Span { lo, hi, file: 0 }
    }

    pub fn join(self, other: Span) -> Span {
        Span {
            lo: self.lo.min(other.lo),
            hi: self.hi.max(other.hi),
            // The joined range starts at the lower offset; attribute it to
            // the file that side came from.
            file: if self.lo <= other.lo { self.file } else { other.file },
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    LitI64(i64),
    LitF64(f64),
    /// `true` / `false`.
    LitBool(bool),
    LitStr(String),
    LitSym(String),
    List(Vec<Expr>),
    Vector(Vec<Expr>),
    /// `#{1 2 3}` set literal.
    SetLit(Vec<Expr>),
    /// `{k1 v1 k2 v2 ...}` map literal with alternating key/value forms.
    MapLit(Vec<(Expr, Expr)>),
}
