use super::*;

pub(crate) fn is_extern(form: &Expr) -> bool {
    matches!(
        form,
        Expr::List(items) if matches!(items.first(), Some(Expr::LitSym(h)) if h == "extern")
    )
}

/// (extern name [param :- ty ...] :- ret) — a direct C FFI declaration.
pub(crate) fn parse_extern(form: &Expr, adt_by_name: &HashMap<String, u32>) -> Result<RawDefn, String> {
    let Expr::List(items) = form else {
        return Err("expected (extern ...)".into());
    };
    if items.len() < 3 {
        return Err("extern expects a name and a params vector".into());
    }
    let Expr::LitSym(name) = &items[1] else {
        return Err("extern name must be a symbol".into());
    };
    let Expr::Vector(params) = &items[2] else {
        return Err("extern params must be a vector".into());
    };
    let mut ty_params = Vec::new();
    let (params, _borrows) = parse_params(params, adt_by_name, &mut ty_params)?;
    if !ty_params.is_empty() {
        return Err("extern declarations must have concrete types".into());
    }
    let mut ret = Ty::I64;
    let mut body_start = 3;
    if items.len() > body_start + 1 {
        if let Expr::LitSym(sep) = &items[body_start] {
            if sep == ":-" {
                ret = parse_ty(&items[body_start + 1], adt_by_name, &mut ty_params)?;
                body_start += 2;
            }
        }
    }
    if items.len() > body_start {
        return Err("extern declarations take no body".into());
    }
    Ok(RawDefn {
        name: name.clone(),
        params,
        ret,
        borrows: vec![Borrow::Shared; 0],
        ty_params,
        is_extern: true,
        body: Expr::List(Vec::new()),
    })
}

pub(crate) fn is_deftype(form: &Expr) -> bool {
    matches!(
        form,
        Expr::List(items) if matches!(items.first(), Some(Expr::LitSym(h)) if h == "deftype")
    )
}

/// (deftype Name (Variant [field :- ty ...]) ...)
pub(crate) fn parse_deftype(form: &Expr) -> Result<AdtDef, String> {
    let Expr::List(items) = form else {
        return Err("expected (deftype ...)".into());
    };
    if items.len() < 2 {
        return Err("deftype expects a name and at least one variant".into());
    }
    let Expr::LitSym(name) = &items[1] else {
        return Err("deftype name must be a symbol".into());
    };
    let mut variants = Vec::new();
    for v in &items[2..] {
        let Expr::List(vitems) = v else {
            return Err(format!("deftype {} variant must be a list", name));
        };
        let Some(Expr::LitSym(vname)) = vitems.first() else {
            return Err(format!("deftype {} variant name must be a symbol", name));
        };
        let mut fields = Vec::new();
        if vitems.len() > 1 {
            let Expr::Vector(fitems) = &vitems[1] else {
                return Err(format!("variant {} fields must be a vector", vname));
            };
            if vitems.len() > 2 {
                return Err(format!("variant {} takes only a fields vector", vname));
            }
            let mut i = 0;
            while i < fitems.len() {
                let Expr::LitSym(fname) = &fitems[i] else {
                    return Err(format!("variant {} field name must be a symbol", vname));
                };
                let Some(Expr::LitSym(sep)) = fitems.get(i + 1) else {
                    return Err(format!("variant {} field {} expects (name :- ty)", vname, fname));
                };
                if sep != ":-" {
                    return Err(format!("variant {} field {} expects (name :- ty)", vname, fname));
                }
                let Some(Expr::LitSym(ty)) = fitems.get(i + 2) else {
                    return Err(format!("variant {} field {} expects a type", vname, fname));
                };
                fields.push(parse_scalar_ty(ty)?);
                i += 3;
            }
        }
        variants.push((vname.clone(), fields));
    }
    Ok(AdtDef {
        name: name.clone(),
        variants,
    })
}

/// Lower a raw defn body (an implicit `do` when it holds multiple forms).
pub(crate) fn lower_body_expr(body: &Expr, env: &Env, ctx: &Ctx) -> Result<HExpr, String> {
    match body {
        Expr::List(items) if matches!(items.first(), Some(Expr::LitSym(h)) if h == "do") => {
            lower_body_forms(items, env, ctx)
        }
        _ => lower(body, env, ctx),
    }
}


// ---------------------------------------------------------------------------
// Ownership / borrow checking
// ---------------------------------------------------------------------------

/// Types that move: ADTs and vectors. Scalars and strings (immutable static
/// data) are Copy.
pub(crate) fn parse_raw(form: &Expr, adt_by_name: &HashMap<String, u32>) -> Result<RawDefn, String> {
    let Expr::List(items) = form else {
        return Err("expected (defn ...)".into());
    };
    let Some(Expr::LitSym(head)) = items.first() else {
        return Err("expected defn".into());
    };
    if head != "defn" {
        return Err(format!("unknown top-level form {}", head));
    }
    let Expr::LitSym(name) = &items[1] else {
        return Err("defn name must be a symbol".into());
    };
    let Expr::Vector(params) = &items[2] else {
        return Err("defn params must be a vector".into());
    };
    let mut ty_params: Vec<String> = Vec::new();
    let (params, borrows) = parse_params(params, adt_by_name, &mut ty_params)?;

    let mut body_start = 3;
    let mut ret = Ty::I64;
    if items.len() > body_start + 1 {
        if let Expr::LitSym(sep) = &items[body_start] {
            if sep == ":-" {
                ret = parse_ty(&items[body_start + 1], adt_by_name, &mut ty_params)?;
                body_start += 2;
            }
        }
    }

    let body = if items.len() > body_start {
        if items.len() - body_start == 1 {
            items[body_start].clone()
        } else {
            // Multiple body forms -> implicit sequence.
            Expr::List(std::iter::once(Expr::LitSym("do".to_string()))
                .chain(items[body_start..].iter().cloned())
                .collect())
        }
    } else {
        return Err("defn body required".into());
    };

    Ok(RawDefn {
        name: name.clone(),
        params,
        ret,
        borrows,
        ty_params,
        is_extern: false,
        body,
    })
}

pub(crate) fn parse_params(
    params: &[Expr],
    adt_by_name: &HashMap<String, u32>,
    ty_params: &mut Vec<String>,
) -> Result<(Vec<(String, Ty)>, Vec<Borrow>), String> {
    let mut out = Vec::new();
    let mut borrows = Vec::new();
    let mut i = 0;
    while i < params.len() {
        let Expr::LitSym(name) = &params[i] else {
            return Err("param must be a symbol".into());
        };
        if i + 2 < params.len() {
            if let Expr::LitSym(sep) = &params[i + 1] {
                if sep == ":-" {
                    // The borrow sigil may be written on the param name
                    // (`&x :- (Vector f64)`) or the annotation symbol
                    // (`x :- &vec`). List annotations take the sigil from
                    // the name position.
                    let (name_borrow, bare) = split_borrow(name);
                    let (ty_borrow, ty) = match &params[i + 2] {
                        Expr::LitSym(ty_sym) => {
                            let (b, rest) = split_borrow(ty_sym);
                            (b, parse_ty_sym(rest, adt_by_name, ty_params)?)
                        }
                        other => (
                            Borrow::Owned,
                            parse_ty(other, adt_by_name, ty_params)?,
                        ),
                    };
                    let borrow = if name_borrow != Borrow::Owned {
                        name_borrow
                    } else {
                        ty_borrow
                    };
                    out.push((bare.to_string(), ty));
                    borrows.push(borrow);
                    i += 3;
                    continue;
                }
            }
        }
        return Err(format!("function parameter '{}' must have a type annotation (param :- ty)", name));
    }
    Ok((out, borrows))
}

/// Parse a type annotation expression: a builtin/ADT symbol, a type
/// parameter name, or `(Vector i64|f64|T)`.
pub(crate) fn parse_ty(
    e: &Expr,
    adt_by_name: &HashMap<String, u32>,
    ty_params: &mut Vec<String>,
) -> Result<Ty, String> {
    match e {
        Expr::List(items) => {
            let Some(Expr::LitSym(h)) = items.first() else {
                return Err("type annotation must be a symbol or (Vector ty)".into());
            };
            if h != "Vector" || items.len() != 2 {
                return Err(format!(
                    "unsupported type annotation (expected (Vector ty), got {} elements)",
                    items.len() - 1
                ));
            }
            let el = match &items[1] {
                Expr::LitSym(s) if s == "i64" => Elem::I64,
                Expr::LitSym(s) if s == "f64" => Elem::F64,
                Expr::LitSym(s) if s == "str" => Elem::Str,
                Expr::LitSym(p) => Elem::Param(register_ty_param(p, ty_params)),
                _ => return Err("Vector element type must be i64, f64 or a type parameter".into()),
            };
            Ok(Ty::Vec(el))
        }
        Expr::LitSym(s) => parse_ty_sym(s, adt_by_name, ty_params),
        _ => Err("type annotation must be a symbol or (Vector ty)".into()),
    }
}

/// Split a leading borrow sigil off an annotation symbol: `&Shape`,
/// `&!vec`, `&(Vector f64)`.
pub(crate) fn split_borrow(s: &str) -> (Borrow, &str) {
    if let Some(rest) = s.strip_prefix("&!") {
        (Borrow::Exclusive, rest)
    } else if let Some(rest) = s.strip_prefix('&') {
        (Borrow::Shared, rest)
    } else {
        (Borrow::Owned, s)
    }
}

pub(crate) fn parse_ty_sym(
    s: &str,
    adt_by_name: &HashMap<String, u32>,
    ty_params: &mut Vec<String>,
) -> Result<Ty, String> {
    match s {
        "i64" => Ok(Ty::I64),
        "i32" => Ok(Ty::I32),
        "f64" => Ok(Ty::F64),
        "vec" => Ok(VEC_I64),
        "bool" => Ok(Ty::Bool),
        "str" | "string" => Ok(Ty::Str),
        "map" => Ok(Ty::Map),
        other => match adt_by_name.get(other) {
            Some(&i) => Ok(Ty::Adt(i)),
            None => Ok(Ty::Param(register_ty_param(other, ty_params))),
        },
    }
}

pub(crate) fn register_ty_param(name: &str, ty_params: &mut Vec<String>) -> u32 {
    if let Some(i) = ty_params.iter().position(|p| p == name) {
        return i as u32;
    }
    ty_params.push(name.to_string());
    (ty_params.len() - 1) as u32
}

/// Substitute type parameters with concrete types.
pub(crate) fn parse_scalar_ty(s: &str) -> Result<Ty, String> {
    match s {
        "i64" => Ok(Ty::I64),
        "i32" => Ok(Ty::I32),
        "f64" => Ok(Ty::F64),
        "bool" => Ok(Ty::Bool),
        other => Err(format!(
            "ADT field type must be i64, f64 or bool, got '{}'",
            other
        )),
    }
}

pub(crate) fn vec_lit_i64(items: &[Expr]) -> Vec<i64> {
    items
        .iter()
        .map(|e| if let Expr::LitI64(v) = e { *v } else { unreachable!() })
        .collect()
}

pub(crate) fn vec_lit_f64(items: &[Expr]) -> Vec<f64> {
    items
        .iter()
        .map(|e| match e {
            Expr::LitI64(v) => *v as f64,
            Expr::LitF64(v) => *v,
            _ => unreachable!(),
        })
        .collect()
}

pub(crate) fn param_name(e: &Expr) -> Result<String, String> {
    match e {
        Expr::LitSym(s) => Ok(s.clone()),
        _ => Err("lambda param must be a symbol".into()),
    }
}

// Peel source wrappers and map/filter stages, inner-most first, returning
// (source, maps, filters). Sources fuse into one loop: a vector variable,
// a `range`, a `zip` of two vector variables, or any of those wrapped in
// `take`/`drop`. Map/filter functions may be inline `(fn ...)` lambdas,
// references to defined functions, or the `even?` builtin.
