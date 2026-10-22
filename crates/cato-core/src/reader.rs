use crate::ast::{Expr, Span};

/// A reader error with the byte span of the offending token.
#[derive(Debug, Clone)]
pub struct ReadErr {
    pub msg: String,
    pub span: Span,
}

fn rerr(msg: impl Into<String>, span: Span) -> ReadErr {
    ReadErr {
        msg: msg.into(),
        span,
    }
}

/// The byte span of every node of a form, in the shape of its `Expr`:
/// `kids` follow the children in order (a map literal's pairs flattened
/// key, value, key, value). Read only on the error path (`read_span_trees`)
/// to point a diagnostic at a sub-form.
#[derive(Debug, Clone)]
pub struct SpanNode {
    pub span: Span,
    pub kids: Vec<SpanNode>,
}

/// Read a program into its forms and each form's span tree.
pub fn read_span_trees(input: &str) -> Result<Vec<(Expr, SpanNode)>, ReadErr> {
    let (forms, _, trees) = read_core(input, true)?;
    Ok(forms.into_iter().zip(trees).collect())
}

pub fn read_all(input: &str) -> Result<Vec<Expr>, String> {
    read_all_spanned(input)
        .map(|(forms, _)| forms)
        .map_err(|e| e.msg)
}

/// Read a program, returning the forms with the byte span of each top-level
/// form (used for diagnostic attribution). Reader errors carry an exact span.
pub fn read_all_spanned(input: &str) -> Result<(Vec<Expr>, Vec<Span>), ReadErr> {
    read_core(input, false).map(|(forms, spans, _)| (forms, spans))
}

/// The reader's output for one input: the top-level forms, their spans, and
/// (with `trees`) each form's span tree.
type ReadOutput = (Vec<Expr>, Vec<Span>, Vec<SpanNode>);

/// The reader. With `trees`, also builds each top-level form's span tree.
fn read_core(input: &str, trees: bool) -> Result<ReadOutput, ReadErr> {
    let bytes = input.as_bytes();
    let mut i = 0usize;
    let mut top = Vec::new();
    let mut spans: Vec<Span> = Vec::new();
    let mut st = Trees {
        on: trees,
        stack: Vec::new(),
        top: Vec::new(),
    };
    let mut stack: Vec<Vec<Expr>> = Vec::new();
    let mut kind_stack: Vec<char> = Vec::new();
    let mut start_stack: Vec<usize> = Vec::new();
    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            ' ' | '\n' | '\r' | '\t' => {
                i += 1;
            }
            ';' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            '(' | '[' | '{' => {
                kind_stack.push(c);
                start_stack.push(i);
                stack.push(Vec::new());
                st.open();
                i += 1;
            }
            '#' if bytes.get(i + 1) == Some(&b'{') => {
                kind_stack.push('#');
                start_stack.push(i);
                stack.push(Vec::new());
                st.open();
                i += 2;
            }
            ')' | ']' | '}' => {
                let expected = match c {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                let here = Span::new(i, i + 1);
                let kind = kind_stack.pop();
                // `#{...}` sets close with '}' but carry their own kind.
                let is_set = kind == Some('#');
                if kind != Some(expected) && !(is_set && c == '}') {
                    return Err(rerr(format!("unbalanced delimiter '{}'", c), here));
                }
                let open = start_stack.pop().unwrap();
                let items = stack.pop().unwrap();
                if is_set {
                    let node = Expr::SetLit(items);
                    push_node(
                        node,
                        Span::new(open, i + 1),
                        &mut stack,
                        &mut top,
                        &mut spans,
                        &mut st,
                        true,
                    );
                    i += 1;
                    continue;
                }
                if expected == '{' {
                    if !items.len().is_multiple_of(2) {
                        return Err(rerr(
                            "map literal must have key/value pairs",
                            Span::new(open, i + 1),
                        ));
                    }
                    let pairs: Vec<(Expr, Expr)> = items
                        .chunks(2)
                        .map(|p| (p[0].clone(), p[1].clone()))
                        .collect();
                    let node = Expr::MapLit(pairs);
                    push_node(
                        node,
                        Span::new(open, i + 1),
                        &mut stack,
                        &mut top,
                        &mut spans,
                        &mut st,
                        true,
                    );
                    i += 1;
                    continue;
                }
                let node = if c == ')' {
                    Expr::List(items)
                } else {
                    Expr::Vector(items)
                };
                push_node(
                    node,
                    Span::new(open, i + 1),
                    &mut stack,
                    &mut top,
                    &mut spans,
                    &mut st,
                    true,
                );
                i += 1;
            }
            '"' => {
                let open = i;
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() && bytes[j] != b'"' {
                    if bytes[j] == b'\\' {
                        j += 1;
                    }
                    j += 1;
                }
                if j >= bytes.len() {
                    return Err(rerr("unterminated string", Span::new(open, bytes.len())));
                }
                let raw = &input[start..j];
                let unescaped = unescape(raw);
                let span = Span::new(open, j + 1);
                push_node(
                    Expr::LitStr(unescaped),
                    span,
                    &mut stack,
                    &mut top,
                    &mut spans,
                    &mut st,
                    false,
                );
                i = j + 1;
            }
            _ => {
                let start = i;
                while i < bytes.len() && !" \t\n\r()[]{};\"".contains(bytes[i] as char) {
                    i += 1;
                }
                let token = &input[start..i];
                let atom = parse_atom(token).map_err(|e| rerr(e, Span::new(start, i)))?;
                let span = Span::new(start, i);
                push_node(atom, span, &mut stack, &mut top, &mut spans, &mut st, false);
            }
        }
    }
    if !stack.is_empty() {
        let open = start_stack.first().copied().unwrap_or(0);
        return Err(rerr("unclosed delimiter", Span::new(open, open + 1)));
    }
    Ok((top, spans, st.top))
}

/// Span-tree building alongside the reader's Expr stack (inert unless on).
struct Trees {
    on: bool,
    stack: Vec<Vec<SpanNode>>,
    top: Vec<SpanNode>,
}

impl Trees {
    fn open(&mut self) {
        if self.on {
            self.stack.push(Vec::new());
        }
    }

    /// A finished node: a collection closes its open frame.
    fn close(&mut self, span: Span, collection: bool) {
        if !self.on {
            return;
        }
        let kids = if collection {
            self.stack.pop().unwrap_or_default()
        } else {
            Vec::new()
        };
        let node = SpanNode { span, kids };
        match self.stack.last_mut() {
            Some(par) => par.push(node),
            None => self.top.push(node),
        }
    }
}

/// Push a finished node; at top level also record its span.
fn push_node(
    e: Expr,
    span: Span,
    stack: &mut [Vec<Expr>],
    top: &mut Vec<Expr>,
    spans: &mut Vec<Span>,
    trees: &mut Trees,
    collection: bool,
) {
    trees.close(span, collection);
    match stack.last_mut() {
        Some(par) => par.push(e),
        None => {
            top.push(e);
            spans.push(span);
        }
    }
}

/// Single left-to-right unescape pass: `\n` `\t` `\\` `\"`; any other
/// `\x` keeps both characters verbatim (lenient, as before).
fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut it = raw.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn parse_atom(tok: &str) -> Result<Expr, String> {
    if let Ok(v) = tok.parse::<i64>() {
        return Ok(Expr::LitI64(v));
    }
    if tok == "true" {
        return Ok(Expr::LitBool(true));
    }
    if tok == "false" {
        return Ok(Expr::LitBool(false));
    }
    if tok.contains('.') {
        if let Ok(v) = tok.parse::<f64>() {
            return Ok(Expr::LitF64(v));
        }
    }
    Ok(Expr::LitSym(tok.to_string()))
}
