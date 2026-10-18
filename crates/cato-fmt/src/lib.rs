//! Full-fidelity formatting front end for Cato: a lossless lexer, a
//! trivia-preserving CST, a Wadler-style Doc engine, and the style
//! policy. The compiler's reader (`cato-core`) is untouched — this crate
//! must never regress it for the sake of a formatter (spec §3).

pub mod cst;
pub mod doc;
pub mod lexer;
pub mod policy;

/// A formatting failure with the byte offset of the offending spot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FmtError {
    pub msg: String,
    pub offset: usize,
}

impl FmtError {
    pub fn new(msg: impl Into<String>, offset: usize) -> Self {
        FmtError { msg: msg.into(), offset }
    }
}

impl std::fmt::Display for FmtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (at byte {})", self.msg, self.offset)
    }
}

/// Format Cato source at `width` columns. The output always ends with
/// exactly one newline; `format_source(format_source(x))` is guaranteed
/// to equal `format_source(x)` (spec §3).
pub fn format_source(src: &str, width: usize) -> Result<String, FmtError> {
    let tops = cst::parse(src)?;
    let doc = policy::tops_to_doc(&tops);
    let mut out = doc::print(&doc, width);
    out.push('\n');
    Ok(out)
}
