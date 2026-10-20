//! Coded diagnostics with source spans.
//!
//! Errors flow through the pipeline as plain strings (existing call sites and
//! substring assertions keep working). This module adds:
//!
//! - a thread-local source registry (`with_source`), installed by the compile
//!   entry points, so errors can be rendered against the original text,
//! - `locate`, which attaches a stable error code plus a `file:line:col`
//!   location and caret snippet to a message,
//! - `explain`, the long-form documentation behind `cato --explain CODE`.

use crate::ast::Span;
use std::cell::RefCell;

// ---------------------------------------------------------------------------
// Source registry
// ---------------------------------------------------------------------------

pub struct SourceMap {
    file: String,
    src: String,
    /// Byte offset where each line starts (line 1 starts at 0).
    line_starts: Vec<usize>,
}

impl SourceMap {
    pub fn new(file: &str, src: &str) -> SourceMap {
        let mut line_starts = vec![0usize];
        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        SourceMap {
            file: file.to_string(),
            src: src.to_string(),
            line_starts,
        }
    }

    /// 1-based (line, column) for a byte offset. Columns count bytes.
    pub fn line_col(&self, off: usize) -> (usize, usize) {
        let off = off.min(self.src.len());
        let line = match self.line_starts.binary_search(&off) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        (line + 1, off - self.line_starts[line] + 1)
    }

    /// The text of 1-based `line`, without its trailing newline.
    fn line_text(&self, line: usize) -> &str {
        let start = self.line_starts[line - 1];
        let end = self
            .line_starts
            .get(line)
            .copied()
            .unwrap_or(self.src.len());
        self.src[start..end].trim_end_matches('\n').trim_end_matches('\r')
    }
}

thread_local! {
    static SOURCES: RefCell<Vec<SourceMap>> = const { RefCell::new(Vec::new()) };
}

/// Install the entry-file source map and clear the registry for the duration
/// of `f`. Required files register themselves via [`register_source`]; their
/// spans carry the returned id.
pub fn with_source<T>(file: &str, src: &str, f: impl FnOnce() -> T) -> T {
    SOURCES.with(|s| {
        s.borrow_mut().clear();
        s.borrow_mut().push(SourceMap::new(file, src));
    });
    let out = f();
    SOURCES.with(|s| s.borrow_mut().clear());
    out
}

/// Register a source file (the entry file or a required module) and return
/// the 1-based id to stamp onto its spans (0 means "unknown file").
pub fn register_source(file: &str, src: &str) -> u32 {
    SOURCES.with(|s| {
        let mut s = s.borrow_mut();
        s.push(SourceMap::new(file, src));
        s.len() as u32
    })
}

// ---------------------------------------------------------------------------
// Error sites: pointing a defn-level error at the sub-form that raised it
// ---------------------------------------------------------------------------
//
// The AST carries no spans below top-level forms, so lowering records the
// chain of expressions an error passed through (innermost first): each
// `lower_expect` that fails pushes its expression, and one that succeeds
// drops whatever a recovered attempt beneath it pushed. On the error path,
// `refine` re-reads the form's source with a span tree and walks the chain
// outermost-first, narrowing to the first matching sub-form each step.
// Forms rewritten by macro expansion do not match their source and are
// skipped, so the result is the innermost unexpanded form on the chain.

thread_local! {
    static SITE: RefCell<Vec<crate::ast::Expr>> = const { RefCell::new(Vec::new()) };
}

/// Chain length to restore with [`site_reset`] when lowering succeeds.
pub fn site_mark() -> usize {
    SITE.with(|s| s.borrow().len())
}

pub fn site_reset(mark: usize) {
    SITE.with(|s| s.borrow_mut().truncate(mark));
}

pub fn site_push(e: &crate::ast::Expr) {
    SITE.with(|s| s.borrow_mut().push(e.clone()));
}

pub fn site_clear() {
    SITE.with(|s| s.borrow_mut().clear());
}

/// The narrowest source span inside `span` (a top-level form) for the
/// recorded error chain; `span` itself when nothing narrower matches.
pub fn refine(span: Span) -> Span {
    let chain = SITE.with(|s| std::mem::take(&mut *s.borrow_mut()));
    if chain.is_empty() || span == Span::default() {
        return span;
    }
    let text = SOURCES.with(|s| {
        let sources = s.borrow();
        let map = span.file.checked_sub(1).and_then(|i| sources.get(i as usize))?;
        map.src.get(span.lo..span.hi).map(|t| t.to_string())
    });
    let Some(text) = text else { return span };
    let Ok(mut forms) = crate::reader::read_span_trees(&text) else { return span };
    if forms.len() != 1 {
        return span;
    }
    let (form, tree) = forms.remove(0);
    let (mut expr, mut node) = (&form, &tree);
    for want in chain.iter().rev() {
        if let Some((e, n)) = find_sub(expr, node, want) {
            expr = e;
            node = n;
        }
    }
    Span { lo: span.lo + node.span.lo, hi: span.lo + node.span.hi, file: span.file }
}

/// Preorder search for `want` under (`e`, `n`), which share one shape.
fn find_sub<'a>(
    e: &'a crate::ast::Expr,
    n: &'a crate::reader::SpanNode,
    want: &crate::ast::Expr,
) -> Option<(&'a crate::ast::Expr, &'a crate::reader::SpanNode)> {
    use crate::ast::Expr;
    if e == want {
        return Some((e, n));
    }
    let kids: Vec<&Expr> = match e {
        Expr::List(xs) | Expr::Vector(xs) | Expr::SetLit(xs) => xs.iter().collect(),
        Expr::MapLit(ps) => ps.iter().flat_map(|(k, v)| [k, v]).collect(),
        _ => return None,
    };
    kids.into_iter()
        .zip(n.kids.iter())
        .find_map(|(k, kn)| find_sub(k, kn, want))
}

// Ownership errors are found on HIR, which keeps no positions. The checker
// instead reports *which occurrence* of a symbol the error is at — the k-th
// value-position read of a variable, the k-th call of a function, the k-th
// `match` — in walk order, which follows source order per symbol. The
// source form is then walked with the same notion of occurrence: binders
// (defn params, let/loop/if-let/doseq binding names, match arm binders)
// and `:- type` annotations are skipped, and a `fn` form counts once for a
// variable it captures (the checker sees the capture, not the body).

thread_local! {
    static OCC: RefCell<Option<(String, usize)>> = const { RefCell::new(None) };
}

pub fn set_occurrence(sym: &str, k: usize) {
    OCC.with(|o| *o.borrow_mut() = Some((sym.to_string(), k)));
}

/// The recorded occurrence's span inside `span` (a top-level form); `span`
/// itself when there is none or it cannot be matched.
pub fn refine_occurrence(span: Span) -> Span {
    let Some((sym, k)) = OCC.with(|o| o.borrow_mut().take()) else { return span };
    if k == 0 || span == Span::default() {
        return span;
    }
    let text = SOURCES.with(|s| {
        let sources = s.borrow();
        let map = span.file.checked_sub(1).and_then(|i| sources.get(i as usize))?;
        map.src.get(span.lo..span.hi).map(|t| t.to_string())
    });
    let Some(text) = text else { return span };
    let Ok(forms) = crate::reader::read_span_trees(&text) else { return span };
    let Some((form, tree)) = forms.first() else { return span };
    let mut left = k;
    match occ_walk(form, tree, &sym, &mut left) {
        Some(at) => Span { lo: span.lo + at.lo, hi: span.lo + at.hi, file: span.file },
        None => span,
    }
}

fn head_of(e: &crate::ast::Expr) -> Option<&str> {
    match e {
        crate::ast::Expr::List(xs) => match xs.first() {
            Some(crate::ast::Expr::LitSym(h)) => Some(h.as_str()),
            _ => None,
        },
        _ => None,
    }
}

fn mentions(e: &crate::ast::Expr, sym: &str) -> bool {
    use crate::ast::Expr;
    match e {
        Expr::LitSym(s) => s == sym,
        Expr::List(xs) | Expr::Vector(xs) | Expr::SetLit(xs) => xs.iter().any(|x| mentions(x, sym)),
        Expr::MapLit(ps) => ps.iter().any(|(k, v)| mentions(k, sym) || mentions(v, sym)),
        _ => false,
    }
}

/// Count down `left` over the occurrences of `sym` under `e`; the span of
/// the one that reaches zero.
fn occ_walk(
    e: &crate::ast::Expr,
    n: &crate::reader::SpanNode,
    sym: &str,
    left: &mut usize,
) -> Option<Span> {
    use crate::ast::Expr;
    let hit = |left: &mut usize, at: Span| {
        *left -= 1;
        (*left == 0).then_some(at)
    };
    match e {
        Expr::LitSym(s) if s == sym => hit(left, n.span),
        Expr::List(xs) => {
            let kids: Vec<(&Expr, &crate::reader::SpanNode)> = xs.iter().zip(n.kids.iter()).collect();
            match head_of(e) {
                Some("ns" | "require" | "deftype" | "defgeneric" | "extern") => None,
                Some("defn" | "defmethod") => {
                    // (defn name [params] [:- ret] body...)
                    let skip = if matches!(xs.get(2), Some(Expr::Vector(_))) { 3 } else { 2 };
                    occ_seq(&kids[skip.min(kids.len())..], sym, left)
                }
                Some("let" | "loop" | "if-let" | "when-let" | "doseq" | "for" | "dotimes") => {
                    if let (Some((Expr::Vector(bs), bn)), true) = (kids.get(1).copied(), kids.len() > 1) {
                        let pairs: Vec<(&Expr, &crate::reader::SpanNode)> =
                            bs.iter().zip(bn.kids.iter()).collect();
                        let mut i = 0;
                        while i < pairs.len() {
                            i += 1; // the binder
                            if matches!(pairs.get(i), Some((Expr::LitSym(t), _)) if t == ":-") {
                                i += 2; // `:- type`
                            }
                            if let Some((v, vn)) = pairs.get(i) {
                                if let Some(at) = occ_walk(v, vn, sym, left) {
                                    return Some(at);
                                }
                            }
                            i += 1;
                        }
                        occ_seq(&kids[2..], sym, left)
                    } else {
                        occ_seq(&kids, sym, left)
                    }
                }
                Some("fn") => {
                    let params_bind = matches!(xs.get(1), Some(p) if mentions(p, sym));
                    let body_uses = xs.iter().skip(2).any(|x| mentions(x, sym));
                    if !params_bind && body_uses {
                        hit(left, n.span)
                    } else {
                        None
                    }
                }
                Some("match") => {
                    if let Some(at) = occ_seq(&kids[..kids.len().min(2)], sym, left) {
                        return Some(at);
                    }
                    for (arm, an) in kids.iter().skip(2) {
                        let r = match arm {
                            Expr::List(ys) => {
                                let ak: Vec<(&Expr, &crate::reader::SpanNode)> =
                                    ys.iter().zip(an.kids.iter()).collect();
                                let skip = if matches!(ys.get(1), Some(Expr::Vector(_))) { 2 } else { 1 };
                                occ_seq(&ak[skip.min(ak.len())..], sym, left)
                            }
                            other => occ_walk(other, an, sym, left),
                        };
                        if r.is_some() {
                            return r;
                        }
                    }
                    None
                }
                _ => occ_seq(&kids, sym, left),
            }
        }
        Expr::Vector(xs) | Expr::SetLit(xs) => {
            let kids: Vec<(&Expr, &crate::reader::SpanNode)> = xs.iter().zip(n.kids.iter()).collect();
            occ_seq(&kids, sym, left)
        }
        Expr::MapLit(ps) => {
            let flat: Vec<&Expr> = ps.iter().flat_map(|(k, v)| [k, v]).collect();
            let kids: Vec<(&Expr, &crate::reader::SpanNode)> = flat.into_iter().zip(n.kids.iter()).collect();
            occ_seq(&kids, sym, left)
        }
        _ => None,
    }
}

/// Walk a sequence of forms in order, skipping `:- type` annotations.
fn occ_seq(
    kids: &[(&crate::ast::Expr, &crate::reader::SpanNode)],
    sym: &str,
    left: &mut usize,
) -> Option<Span> {
    let mut i = 0;
    while i < kids.len() {
        let (k, kn) = kids[i];
        if matches!(k, crate::ast::Expr::LitSym(t) if t == ":-") {
            i += 2;
            continue;
        }
        if let Some(at) = occ_walk(k, kn, sym, left) {
            return Some(at);
        }
        i += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// Error codes
// ---------------------------------------------------------------------------

/// Map a pipeline error message to its stable diagnostic code. Rules match on
/// the stable substrings the pipeline emits, so error sites stay untouched.
pub fn classify(msg: &str) -> &'static str {
    let m = msg;
    if m.contains("use of moved value") {
        "E301"
    } else if m.contains("Borrowed return requires a borrowed value") || m.contains("borrowed return:") {
        "E304"
    } else if m.contains("borrowed parameter and still belongs to the caller")
        || m.contains("borrowed result of")
    {
        "E303"
    } else if m.contains("non-exhaustive") || m.contains("does not cover") {
        "E401"
    } else if m.contains("unbalanced delimiter") {
        "E001"
    } else if m.contains("unterminated string") {
        "E002"
    } else if m.contains("key/value pairs") {
        "E003"
    } else if m.contains("unclosed delimiter") {
        "E004"
    } else if m.contains("unsupported borrow sigil") || m.contains("mixed widths")
        || m.contains("must share a width") || m.contains("in arithmetic")
        || m.contains("does not fit")
    {
        // Width/sigil misuse: an explicit cast or syntax fix is required.
        "E202"
    } else if m.contains("unbound variable") {
        "E206"
    } else if m.contains("unknown function") {
        "E207"
    } else if m.contains("unknown type name") {
        "E208"
    } else if m.contains("expects exactly") || m.contains("expects 2 operands")
        || m.contains("expects 1 argument") || m.contains("expects 2 arguments")
        || m.contains("expects 3 arguments")
    {
        "E201"
    } else if m.contains("ambiguous call to") {
        "E203"
    } else if m.contains("no method") {
        "E204"
    } else if m.contains("can never be selected") || m.contains("duplicate method for") {
        "E205"
    } else if m.contains("expects") || m.contains("must be") || m.contains("must have") {
        "E202"
    } else {
        "E000"
    }
}

const EXPLAIN: &[(&str, &str)] = &[
    ("E000", "Uncategorized error.\n\nThe message did not match a known diagnostic class.\nPlease report this — it should have a code."),
    ("E001", "Unbalanced delimiter.\n\nA closing `)` / `]` / `}` did not match the innermost\nopening delimiter. Check nesting and delimiter kind."),
    ("E002", "Unterminated string.\n\nA string literal was opened with `\"` but the file (or line)\nended before the closing quote. Escape inner quotes as `\\\"`."),
    ("E003", "Map literal missing a value.\n\n`{k1 v1 k2 v2}` requires an even number of forms: each key\nmust be immediately followed by its value."),
    ("E004", "Unclosed delimiter.\n\nThe file ended while a `(`, `[` or `{` was still open."),
    ("E101", "Macro expansion error.\n\nA macro (`when`, `cond`, `if-let`, `->`, ...) received the\nwrong shape. See the macro's expected form in the message."),
    ("E201", "Arity error.\n\nA form received the wrong number of operands. Builtins fix\ntheir arity (`(+ a b)`, `(count xs)`, ...); check the call."),
    ("E202", "Invalid form or argument type.\n\nThe form is shaped right but an operand has the wrong type\n(e.g. `conj` on a scalar), or the form itself is malformed."),
    ("E301", "Use of moved value.\n\nNon-Copy values (vectors, ADTs, maps) move when passed to an\nowned parameter or bound again. After the move the binding is\ndead. Clone it first (`(clone x)`) or borrow it (`&x`)."),
    ("E401", "Non-exhaustive match.\n\nA `match` over an ADT did not cover every variant and had no\n`_` wildcard arm. Add the missing variants or a default arm."),
    ("E203", "Ambiguous generic-function dispatch.\n\nTwo applicable methods are incomparable in specificity\n(e.g. `Circle × Shape` and `Shape × Circle`): neither is more\nspecific, so the method to run is undecidable. Add a method for\nthe exact combination (e.g. `Circle × Circle`)."),
    ("E204", "Uncovered generic-function dispatch.\n\nNo method applies to some type combination at a call site —\na variant combination, or a builtin/collection type like\n`(Vector str)`. Add a method for the missing type or a default\nmethod specializing the whole ADT."),
    ("E205", "Unreachable or duplicate generic-function method.\n\nA method can never be selected because a more specific method\ncovers every input it applies to, or two methods share the same\nspecializers. Remove one."),
    ("E206", "Unbound variable.\n\nA symbol in value position has no binding in scope: a typo, or\nuse before the binding. Every name must be introduced by a\n`let`, a parameter, a loop binder, or a match binder before use."),
    ("E207", "Unknown function.\n\nThe called name is not a `defn`, `extern`, builtin, generic,\nconstructor, or a closure binding. Check the spelling, or add\nthe `defn`."),
    ("E208", "Unknown type name.\n\nAn annotation named a type that is not a builtin, a declared\n`deftype`, or a type parameter. Type parameters are introduced\nby uppercase spelling (e.g. `T`) in a `defn` annotation; a\nlowercase unknown name is treated as a typo."),
    ("E303", "Move of a borrowed value.\n\n`&` / `&!` parameters do not own their value — the caller does.\nPassing one to an owned parameter (or `conj`-ing, capturing, or\ndropping it) would let the callee free a buffer the caller still\nreads. Clone it first, or take the parameter by value."),
    ("E304", "Borrowed return requires a borrowed value.\n\nA `&T` function may only return a value derived from its `&`\nparameters, never an owned or freshly-cloned value."),
];

/// Long-form documentation for a diagnostic code.
pub fn explain(code: &str) -> Option<&'static str> {
    EXPLAIN.iter().find(|(c, _)| *c == code).map(|(_, d)| *d)
}

pub fn all_codes() -> impl Iterator<Item = &'static str> {
    EXPLAIN.iter().map(|(c, _)| *c)
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Attach the code + location + caret snippet to an error message. Without a
/// registered source the message renders as `[CODE] msg`; with a zero span
/// the location line is omitted.
pub fn locate(span: Span, msg: &str) -> String {
    let code = classify(msg);
    let head = format!("[{}] {}", code, msg);
    if span == Span::default() {
        return head;
    }
    SOURCES.with(|s| {
        let sources = s.borrow();
        match span.file.checked_sub(1).and_then(|i| sources.get(i as usize)) {
            Some(map) => render_at(map, span, &head),
            None => head,
        }
    })
}

fn render_at(map: &SourceMap, span: Span, head: &str) -> String {
    let (line, col) = map.line_col(span.lo);
    let text = map.line_text(line);
    // Caret covers the span on its first line (multi-line forms are capped;
    // defn-granularity spans can be long, so clamp the underline width).
    let width = span.hi.saturating_sub(span.lo).clamp(1, text.len().saturating_sub(col - 1).max(1));
    let gutter = line.to_string();
    let pad = " ".repeat(gutter.len());
    format!(
        "{}\n  --> {}:{}:{}\n {} |\n {} | {}\n {} | {}{}",
        head,
        map.file,
        line,
        col,
        pad,
        gutter,
        text,
        pad,
        " ".repeat(col - 1),
        "^".repeat(width)
    )
}
