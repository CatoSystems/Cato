use crate::ast::Expr;

pub fn read_all(input: &str) -> Result<Vec<Expr>, String> {
    let bytes = input.as_bytes();
    let mut i = 0usize;
    let mut top = Vec::new();
    let mut stack: Vec<Vec<Expr>> = Vec::new();
    let mut kind_stack: Vec<char> = Vec::new();
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
                stack.push(Vec::new());
                i += 1;
            }
            ')' | ']' | '}' => {
                let expected = match c {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                if kind_stack.pop() != Some(expected) {
                    return Err(format!("unbalanced delimiter '{}'", c));
                }
                let items = stack.pop().unwrap();
                if expected == '{' {
                    if items.len() % 2 != 0 {
                        return Err("map literal must have key/value pairs".into());
                    }
                    let pairs: Vec<(Expr, Expr)> = items
                        .chunks(2)
                        .map(|p| (p[0].clone(), p[1].clone()))
                        .collect();
                    let node = Expr::MapLit(pairs);
                    match stack.last_mut() {
                        Some(par) => par.push(node),
                        None => top.push(node),
                    }
                    i += 1;
                    continue;
                }
                let node = if c == ')' {
                    Expr::List(items)
                } else {
                    Expr::Vector(items)
                };
                match stack.last_mut() {
                    Some(par) => par.push(node),
                    None => top.push(node),
                }
                i += 1;
            }
            '"' => {
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() && bytes[j] != b'"' {
                    if bytes[j] == b'\\' {
                        j += 1;
                    }
                    j += 1;
                }
                if j >= bytes.len() {
                    return Err("unterminated string".to_string());
                }
                let raw = &input[start..j];
                let unescaped = raw.replace("\\n", "\n").replace("\\\"", "\"");
                push_atom(Expr::LitStr(unescaped), &mut stack, &mut top);
                i = j + 1;
            }
            _ => {
                let start = i;
                while i < bytes.len() && !" \t\n\r()[]{};\"".contains(bytes[i] as char) {
                    i += 1;
                }
                let token = &input[start..i];
                let atom = parse_atom(token)?;
                push_atom(atom, &mut stack, &mut top);
            }
        }
    }
    if !stack.is_empty() {
        return Err("unclosed delimiter".to_string());
    }
    Ok(top)
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

fn push_atom(e: Expr, stack: &mut Vec<Vec<Expr>>, top: &mut Vec<Expr>) {
    match stack.last_mut() {
        Some(par) => par.push(e),
        None => top.push(e),
    }
}
