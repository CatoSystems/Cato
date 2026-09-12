#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    LitI64(i64),
    LitF64(f64),
    LitStr(String),
    LitSym(String),
    List(Vec<Expr>),
    Vector(Vec<Expr>),
    /// `{k1 v1 k2 v2 ...}` map literal with alternating key/value forms.
    MapLit(Vec<(Expr, Expr)>),
}
