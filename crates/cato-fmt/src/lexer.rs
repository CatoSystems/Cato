//! Lossless Cato lexer: every byte of input becomes exactly one token's
//! `text` (whitespace, comments, strings, atoms, delimiters). Classification
//! of atoms is deferred to the parser — this layer stays raw.

use crate::FmtError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokKind {
    /// Whitespace (space, `\n`, `\r`, `\t`).
    Ws,
    /// `;` line comment, excluding the terminating newline.
    Comment,
    /// `(`, `[`, `{`, or `#{`.
    Open,
    /// `)`, `]`, or `}`.
    Close,
    /// `"…"` including both quotes; backslash escapes skipped.
    Str,
    /// Anything else (raw, unclassified).
    Atom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokKind,
    /// The raw byte slice as String, byte-identical to the input.
    pub text: String,
}

const ATOM_STOP: &str = " \t\n\r()[]{};\"";

/// Lex `src` into a lossless token stream.
pub fn lex(src: &str) -> Result<Vec<Token>, FmtError> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b' ' | b'\n' | b'\r' | b'\t' => {
                let start = i;
                while i < bytes.len() && matches!(bytes[i], b' ' | b'\n' | b'\r' | b'\t') {
                    i += 1;
                }
                push(&mut out, TokKind::Ws, &src[start..i]);
            }
            b';' => {
                let start = i;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                push(&mut out, TokKind::Comment, &src[start..i]);
            }
            b'(' | b'[' | b'{' => {
                push(&mut out, TokKind::Open, &src[i..i + 1]);
                i += 1;
            }
            b'#' if bytes.get(i + 1) == Some(&b'{') => {
                push(&mut out, TokKind::Open, &src[i..i + 2]);
                i += 2;
            }
            b')' | b']' | b'}' => {
                push(&mut out, TokKind::Close, &src[i..i + 1]);
                i += 1;
            }
            b'"' => {
                let open = i;
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] != b'"' {
                    if bytes[j] == b'\\' {
                        j += 1;
                    }
                    j += 1;
                }
                if j >= bytes.len() {
                    return Err(FmtError::new("unterminated string", open));
                }
                push(&mut out, TokKind::Str, &src[open..j + 1]);
                i = j + 1;
            }
            _ => {
                let start = i;
                while i < bytes.len() && !ATOM_STOP.contains(bytes[i] as char) {
                    i += 1;
                }
                push(&mut out, TokKind::Atom, &src[start..i]);
            }
        }
    }
    Ok(out)
}

fn push(out: &mut Vec<Token>, kind: TokKind, text: &str) {
    out.push(Token { kind, text: text.to_string() });
}
