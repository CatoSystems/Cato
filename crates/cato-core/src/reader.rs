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

pub fn read_all(input: &str) -> Result<Vec<Expr>, String> {
    read_all_spanned(input).map(|(forms, _)| forms).map_err(|e| e.msg)
}

/// Read a program, returning the forms with the byte span of each top-level
/// form (used for diagnostic attribution). Reader errors carry an exact span.
pub fn read_all_spanned(input: &str) -> Result<(Vec<Expr>, Vec<Span>), ReadErr> {
    let bytes = input.as_bytes();
    let mut i = 0usize;
    let mut top = Vec::new();
    let mut spans: Vec<Span> = Vec::new();
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
                i += 1;
            }
            ')' | ']' | '}' => {
                let expected = match c {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                let here = Span { lo: i, hi: i + 1 };
                if kind_stack.pop() != Some(expected) {
                    return Err(rerr(format!("unbalanced delimiter '{}'", c), here));
                }
                let open = start_stack.pop().unwrap();
                let items = stack.pop().unwrap();
                if expected == '{' {
                    if items.len() % 2 != 0 {
                        return Err(rerr(
                            "map literal must have key/value pairs",
                            Span { lo: open, hi: i + 1 },
                        ));
                    }
                    let pairs: Vec<(Expr, Expr)> = items
                        .chunks(2)
                        .map(|p| (p[0].clone(), p[1].clone()))
                        .collect();
                    let node = Expr::MapLit(pairs);
                    push_node(node, Span { lo: open, hi: i + 1 }, &mut stack, &mut top, &mut spans);
                    i += 1;
                    continue;
                }
                let node = if c == ')' {
                    Expr::List(items)
                } else {
                    Expr::Vector(items)
                };
                push_node(node, Span { lo: open, hi: i + 1 }, &mut stack, &mut top, &mut spans);
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
                    return Err(rerr("unterminated string", Span { lo: open, hi: bytes.len() }));
                }
                let raw = &input[start..j];
                let unescaped = raw.replace("\\n", "\n").replace("\\\"", "\"");
                let span = Span { lo: open, hi: j + 1 };
                push_node(Expr::LitStr(unescaped), span, &mut stack, &mut top, &mut spans);
                i = j + 1;
            }
            _ => {
                let start = i;
                while i < bytes.len() && !" \t\n\r()[]{};\"".contains(bytes[i] as char) {
                    i += 1;
                }
                let token = &input[start..i];
                let atom = parse_atom(token).map_err(|e| rerr(e, Span { lo: start, hi: i }))?;
                let span = Span { lo: start, hi: i };
                push_node(atom, span, &mut stack, &mut top, &mut spans);
            }
        }
    }
    if !stack.is_empty() {
        let open = start_stack.first().copied().unwrap_or(0);
        return Err(rerr(
            "unclosed delimiter",
            Span { lo: open, hi: open + 1 },
        ));
    }
    Ok((top, spans))
}

/// Push a finished node; at top level also record its span.
fn push_node(
    e: Expr,
    span: Span,
    stack: &mut Vec<Vec<Expr>>,
    top: &mut Vec<Expr>,
    spans: &mut Vec<Span>,
) {
    match stack.last_mut() {
        Some(par) => par.push(e),
        None => {
            top.push(e);
            spans.push(span);
        }
    }
}

fn parse_atom(tok: &str) -> Result<Expr, String> {
    if let Ok(v) = tok.parse::<i64>() {
        return Ok(Expr::LitI64(v));
    }
    if tok.contains('.') {
        if let Ok(v) = tok.parse::<f64>() {
            return Ok(Expr::LitF64(v));
        }
    }
    Ok(Expr::LitSym(tok.to_string()))
}
