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
    static SOURCE: RefCell<Option<SourceMap>> = const { RefCell::new(None) };
}

/// Install the source map for the duration of `f`, so errors can render
/// locations. Nested calls replace the map (compile entry points only).
pub fn with_source<T>(file: &str, src: &str, f: impl FnOnce() -> T) -> T {
    SOURCE.with(|s| *s.borrow_mut() = Some(SourceMap::new(file, src)));
    let out = f();
    SOURCE.with(|s| *s.borrow_mut() = None);
    out
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
    SOURCE.with(|s| match s.borrow().as_ref() {
        Some(map) => render_at(map, span, &head),
        None => head,
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
