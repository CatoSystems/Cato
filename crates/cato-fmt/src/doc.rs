//! Classic Wadler-style pretty printer: groups measure their flat width
//! (a hard `Line` makes it infinite) and print flat when they fit the
//! remaining columns, otherwise break. `Nest` adds to the indent of
//! every break inside it. Indentation is flushed lazily before the next
//! content, so blank lines never carry trailing spaces and string
//! literals spanning lines keep their bytes intact.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Doc {
    /// Literal text (never breaks, width = char count).
    Text(String),
    /// Hard newline (always breaks).
    Line,
    /// Space when flat, newline when the enclosing group breaks.
    Soft,
    /// Nothing when flat, newline when the enclosing group breaks
    /// (delimiter-hugging break: no space inside parens when flat).
    Softline,
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
        Doc::Softline => Some(0),
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

/// Does `d`, printed flat, fit in the remaining columns?
fn fits(d: &Doc, remaining: usize) -> bool {
    match flat_width(d) {
        Some(w) => w <= remaining,
        None => false,
    }
}

struct Out {
    buf: String,
    col: usize,
    /// Indent spaces owed for the current line; flushed before content.
    pending: usize,
}

impl Out {
    fn flush_indent(&mut self) {
        for _ in 0..self.pending {
            self.buf.push(' ');
        }
        self.col += self.pending;
        self.pending = 0;
    }
    fn push_str(&mut self, s: &str) {
        self.flush_indent();
        self.buf.push_str(s);
        self.col += s.chars().count();
    }
    fn newline(&mut self, indent: usize) {
        self.buf.push('\n');
        self.col = 0;
        self.pending = indent;
    }
}

/// Render `doc` to a String at the given width. Trailing whitespace is
/// never emitted (indent is lazy); a trailing newline from a final break
/// is dropped — the policy layer owns the final newline.
pub fn print(doc: &Doc, width: usize) -> String {
    let mut out = Out { buf: String::new(), col: 0, pending: 0 };
    render(doc, width, 0, false, &mut out);
    out.buf.trim_end().to_string()
}

fn render(d: &Doc, width: usize, indent: usize, flat: bool, out: &mut Out) {
    match d {
        Doc::Text(s) => out.push_str(s),
        Doc::Nil => {}
        Doc::Line => out.newline(indent),
        Doc::Soft => {
            if flat {
                out.push_str(" ");
            } else {
                out.newline(indent);
            }
        }
        Doc::Softline => {
            if !flat {
                out.newline(indent);
            }
        }
        Doc::Concat(xs) => {
            for x in xs {
                render(x, width, indent, flat, out);
            }
        }
        Doc::Nest(n, inner) => {
            let inner_indent = (indent as i32 + n).max(0) as usize;
            render(inner, width, inner_indent, flat, out);
        }
        Doc::Group(xs) => {
            let candidate = Doc::Concat(xs.clone());
            let col = out.col + out.pending; // pending indent counts toward the line
            let broken = !fits(&candidate, width.saturating_sub(col));
            for x in xs {
                render(x, width, indent, !broken, out);
            }
        }
    }
}
