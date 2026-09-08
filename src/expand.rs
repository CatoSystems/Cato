use crate::ast::Expr;

/// Rewrite every `->` / `->>` form into nested application, bottom-up.
/// `->`   threads the previous result as the first argument of each step.
/// `->>`  threads it as the last argument.
pub fn expand_all(prog: &[Expr]) -> Result<Vec<Expr>, String> {
    prog.iter().map(expand_form).collect()
}

fn expand_form(e: &Expr) -> Result<Expr, String> {
    let Expr::List(items) = e else {
        return Ok(e.clone());
    };
    let items: Vec<Expr> = items.iter().map(expand_form).collect::<Result<_, _>>()?;
    let thread_pos = match items.first() {
        Some(Expr::LitSym(h)) if h == "->" => Some(false),
        Some(Expr::LitSym(h)) if h == "->>" => Some(true),
        _ => None,
    };
    match thread_pos {
        None => Ok(Expr::List(items)),
        Some(last) => expand_thread(&items[1..], last),
    }
}

fn expand_thread(steps: &[Expr], last: bool) -> Result<Expr, String> {
    if steps.is_empty() {
        return Err("threading macro expects at least one step".into());
    }
    let mut acc = steps[0].clone();
    for step in &steps[1..] {
        acc = apply_step(acc, step, last)?;
    }
    Ok(acc)
}

fn apply_step(prev: Expr, step: &Expr, last: bool) -> Result<Expr, String> {
    match step {
        Expr::List(items) => {
            let mut out = items.clone();
            if last {
                out.push(prev);
            } else {
                out.insert(1, prev);
            }
            Ok(Expr::List(out))
        }
        Expr::LitSym(_) => Ok(Expr::List(vec![step.clone(), prev])),
        _ => Err("threading step must be a list or symbol".into()),
    }
}