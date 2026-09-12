/// A byte-offset range into the source text (`lo` inclusive, `hi` exclusive).
/// The zero span means "unknown" — diagnostics render without a location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub lo: usize,
    pub hi: usize,
}

impl Span {
    pub fn join(self, other: Span) -> Span {
        Span {
            lo: self.lo.min(other.lo),
            hi: self.hi.max(other.hi),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    LitI64(i64),
    LitF64(f64),
    LitStr(String),
    LitSym(String),
    List(Vec<Expr>),
    Vector(Vec<Expr>),
    /// `#{1 2 3}` set literal.
    SetLit(Vec<Expr>),
    /// `{k1 v1 k2 v2 ...}` map literal with alternating key/value forms.
    MapLit(Vec<(Expr, Expr)>),
}
