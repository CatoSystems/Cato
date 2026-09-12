use crate::ast::{Expr, Span};

/// Rewrite macro forms into core forms, bottom-up.
///
/// Threading:
/// `->`   threads the previous result as the first argument of each step.
/// `->>`  threads it as the last argument.
///
/// Branching / binding (all desugar into `if`/`let`/`do`/`reduce` — no
/// codegen changes):
/// `(when c b...)`            → `(if c (do b...) 0)`
/// `(when-not c b...)`        → `(if c 0 (do b...))`
/// `(cond c1 b1 ... [else])`  → nested `if`s; an odd trailing form is the
///                              default, otherwise no match yields 0
/// `(if-let [x e] t [f])`     → `(let [x e] (if x t f-or-0))`
/// `(when-let [x e] b...)`    → `(let [x e] (if x (do b...) 0))`
/// `(if-not c t f)`           → `(if c f t)`
/// `(doseq [x xs] b...)`      → `(reduce (fn [_acc x] (do b... 0)) 0 xs)`
///
/// Variadic booleans:
/// `(and a b c...)` / `(or a b c...)` → nested binary forms.
pub fn expand_all(prog: &[Expr]) -> Result<Vec<Expr>, String> {
    prog.iter().map(expand_form).collect()
}

/// Expand with per-top-level-form spans: an expansion error is attributed to
/// the form it occurred in.
pub fn expand_all_located(prog: &[Expr], spans: &[Span]) -> Result<Vec<Expr>, String> {
    prog.iter()
        .zip(spans)
        .map(|(e, s)| expand_form(e).map_err(|m| crate::diagnostics::locate(*s, &m)))
        .collect()
}

fn expand_form(e: &Expr) -> Result<Expr, String> {
    match e {
        Expr::List(items) => {
            let items: Vec<Expr> = items.iter().map(expand_form).collect::<Result<_, _>>()?;
            expand_list(items)
        }
        Expr::Vector(items) => {
            // Binding vectors can contain macro forms in value position,
            // e.g. (let [x (-> a b)] ...).
            let items: Vec<Expr> = items.iter().map(expand_form).collect::<Result<_, _>>()?;
            Ok(Expr::Vector(items))
        }
        Expr::MapLit(pairs) => {
            let mut pairs2: Vec<(Expr, Expr)> = Vec::new();
            for (k, v) in pairs {
                pairs2.push((expand_form(k)?, expand_form(v)?));
            }
            let pairs = pairs2;
            Ok(Expr::MapLit(pairs))
        }
        other => Ok(other.clone()),
    }
}

fn expand_list(items: Vec<Expr>) -> Result<Expr, String> {
    let thread_pos = match items.first() {
        Some(Expr::LitSym(h)) if h == "->" => Some(false),
        Some(Expr::LitSym(h)) if h == "->>" => Some(true),
        _ => None,
    };
    if let Some(last) = thread_pos {
        return expand_thread(&items[1..], last);
    }
    match items.first() {
        Some(Expr::LitSym(h)) => match h.as_str() {
            "when" => expand_when(items),
            "when-not" => expand_when_not(items),
            "cond" => expand_cond(items),
            "if-let" => expand_if_let(items),
            "when-let" => expand_when_let(items),
            "if-not" => expand_if_not(items),
            "doseq" => expand_doseq(items),
            "and" | "or" => expand_and_or(items),
            _ => Ok(Expr::List(items)),
        },
        _ => Ok(Expr::List(items)),
    }
}

fn sym(s: &str) -> Expr {
    Expr::LitSym(s.to_string())
}

fn list(items: Vec<Expr>) -> Expr {
    Expr::List(items)
}

fn do_form(body: &[Expr]) -> Expr {
    list(std::iter::once(sym("do")).chain(body.iter().cloned()).collect())
}

fn expand_when(items: Vec<Expr>) -> Result<Expr, String> {
    if items.len() < 3 {
        return Err("when expects (when <cond> <body>...)".into());
    }
    Ok(list(vec![
        sym("if"),
        items[1].clone(),
        do_form(&items[2..]),
        Expr::LitI64(0),
    ]))
}

fn expand_when_not(items: Vec<Expr>) -> Result<Expr, String> {
    if items.len() < 3 {
        return Err("when-not expects (when-not <cond> <body>...)".into());
    }
    Ok(list(vec![
        sym("if"),
        items[1].clone(),
        Expr::LitI64(0),
        do_form(&items[2..]),
    ]))
}

fn expand_cond(items: Vec<Expr>) -> Result<Expr, String> {
    // (cond c1 b1 c2 b2 ... [default]) — an odd trailing form is the
    // default; otherwise a failed match yields 0.
    let arms = &items[1..];
    if arms.is_empty() {
        return Err("cond expects at least one test/body pair".into());
    }
    let (pairs, default) = if arms.len() % 2 == 0 {
        (&arms[..], None)
    } else {
        (&arms[..arms.len() - 1], Some(arms[arms.len() - 1].clone()))
    };
    let mut result = default.unwrap_or_else(|| Expr::LitI64(0));
    for pair in pairs.rchunks(2) {
        result = list(vec![
            sym("if"),
            pair[0].clone(),
            pair[1].clone(),
            result,
        ]);
    }
    Ok(result)
}

fn expand_if_let(items: Vec<Expr>) -> Result<Expr, String> {
    if items.len() != 4 && items.len() != 5 {
        return Err("if-let expects (if-let [x e] <then> [<else>])".into());
    }
    let binding = match &items[1] {
        Expr::Vector(b) if b.len() == 2 => b.clone(),
        _ => return Err("if-let binding must be a 2-element vector [x e]".into()),
    };
    let else_form = if items.len() == 4 {
        items[3].clone()
    } else {
        Expr::LitI64(0)
    };
    // The condition is the bound value x (must be bool in Cato).
    let x = binding[0].clone();
    Ok(list(vec![
        sym("let"),
        Expr::Vector(binding),
        list(vec![sym("if"), x, items[2].clone(), else_form]),
    ]))
}

fn expand_when_let(items: Vec<Expr>) -> Result<Expr, String> {
    if items.len() < 3 {
        return Err("when-let expects (when-let [x e] <body>...)".into());
    }
    let binding = match &items[1] {
        Expr::Vector(b) if b.len() == 2 => b.clone(),
        _ => return Err("when-let binding must be a 2-element vector [x e]".into()),
    };
    let x = binding[0].clone();
    Ok(list(vec![
        sym("let"),
        Expr::Vector(binding),
        list(vec![
            sym("if"),
            x,
            do_form(&items[2..]),
            Expr::LitI64(0),
        ]),
    ]))
}

fn expand_if_not(items: Vec<Expr>) -> Result<Expr, String> {
    if items.len() != 4 {
        return Err("if-not expects (if-not <cond> <then> <else>)".into());
    }
    Ok(list(vec![
        sym("if"),
        items[1].clone(),
        items[3].clone(),
        items[2].clone(),
    ]))
}

fn expand_doseq(items: Vec<Expr>) -> Result<Expr, String> {
    if items.len() < 3 {
        return Err("doseq expects (doseq [x xs] <body>...)".into());
    }
    let binding = match &items[1] {
        Expr::Vector(b) if b.len() == 2 => b.clone(),
        _ => return Err("doseq binding must be a 2-element vector [x xs]".into()),
    };
    // Body results are discarded: the reduce fn always yields 0 (i64),
    // matching the accumulator type.
    let mut body: Vec<Expr> = items[2..].to_vec();
    body.push(Expr::LitI64(0));
    Ok(list(vec![
        sym("reduce"),
        list(vec![
            sym("fn"),
            Expr::Vector(vec![sym("_acc"), binding[0].clone()]),
            do_form(&body),
        ]),
        Expr::LitI64(0),
        binding[1].clone(),
    ]))
}

fn expand_and_or(items: Vec<Expr>) -> Result<Expr, String> {
    let op = match items[0] {
        Expr::LitSym(ref h) => h.as_str(),
        _ => unreachable!(),
    };
    let args = &items[1..];
    if args.is_empty() {
        return Err(format!("{} expects at least one operand", op));
    }
    if args.len() == 1 {
        return Ok(args[0].clone());
    }
    let last = args[args.len() - 1].clone();
    let mut result = last;
    for a in args[..args.len() - 1].iter().rev() {
        result = list(vec![sym(op), a.clone(), result]);
    }
    Ok(result)
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
