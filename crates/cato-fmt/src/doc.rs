//! Classic Wadler-style pretty printer: groups measure their flat width
//! (a hard `Line` makes it infinite) and print flat when they fit the
//! remaining columns, otherwise break. `Nest` adds to the indent of
//! every break inside it.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Doc {
    /// Literal text (never breaks, width = char count).
    Text(String),
    /// Hard newline (always breaks).
    Line,
    /// Space when flat, newline when the enclosing group breaks.
    Soft,
    /// Nothing (zero width).
    Nil,
    Group(Vec<Doc>),
    /// Add `n` to the indent for breaks inside.
    Nest(i32, Box<Doc>),
    Concat(Vec<Doc>),
}

pub fn text(s: impl Into<String>) -> Doc {
    Doc::Text(s.into())
}

pub fn group(d: Doc) -> Doc {
    Doc::Group(vec![d])
}

pub fn nest(n: i32, d: Doc) -> Doc {
    Doc::Nest(n, Box::new(d))
}

pub fn concat(xs: Vec<Doc>) -> Doc {
    Doc::Concat(xs)
}

/// Flat width of `d`: `Line` ⇒ infinity (the group can never be flat).
fn flat_width(d: &Doc) -> Option<usize> {
    match d {
        Doc::Text(s) => Some(s.chars().count()),
        Doc::Line => None,
        Doc::Soft => Some(1),
        Doc::Nil => Some(0),
        Doc::Group(xs) | Doc::Concat(xs) => {
            let mut total = 0usize;
            for x in xs {
                total += flat_width(x)?;
            }
            Some(total)
        }
        Doc::Nest(_, inner) => flat_width(inner),
    }
}

/// Does `d`, printed flat, fit in `width - col`? (lookahead stops at
/// the first hard Line only when the candidate itself contains one —
/// handled by flat_width returning None).
fn fits(d: &Doc, remaining: usize) -> bool {
    match flat_width(d) {
        Some(w) => w <= remaining,
        None => false,
    }
}

/// Render `doc` to a String at the given width. Trailing whitespace is
/// stripped (broken `Soft` before a hard `Line` leaves an indented blank
/// line); the policy layer owns the final newline.
pub fn print(doc: &Doc, width: usize) -> String {
    let mut out = String::new();
    render(doc, width, 0, 0, false, &mut out);
    // Per-line trim (no trailing spaces mid-document), then trailing
    // newlines/indent at the very end.
    let trimmed: Vec<&str> = out.lines().map(|l| l.trim_end()).collect();
    trimmed.join("\n").trim_end().to_string()
}

fn render(d: &Doc, width: usize, indent: usize, col: usize, flat: bool, out: &mut String) -> usize {
    match d {
        Doc::Text(s) => {
            out.push_str(s);
            col + s.chars().count()
        }
        Doc::Nil => col,
        Doc::Line => {
            out.push('\n');
            for _ in 0..indent {
                out.push(' ');
            }
            indent
        }
        Doc::Soft => {
            if flat {
                out.push(' ');
                col + 1
            } else {
                out.push('\n');
                for _ in 0..indent {
                    out.push(' ');
                }
                indent
            }
        }
        Doc::Concat(xs) => {
            let mut c = col;
            for x in xs {
                c = render(x, width, indent, c, flat, out);
            }
            c
        }
        Doc::Nest(n, inner) => {
            let inner_indent = (indent as i32 + n).max(0) as usize;
            render(inner, width, inner_indent, col, flat, out)
        }
        Doc::Group(xs) => {
            let candidate = Doc::Concat(xs.clone());
            let broken = !fits(&candidate, width.saturating_sub(col));
            let mut c = col;
            for x in xs {
                c = render(x, width, indent, c, !broken, out);
            }
            c
        }
    }
}
