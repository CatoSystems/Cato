use super::*;

/// Shared type-resolution state for parsing: the declared ADT table, the
/// name map, and the monomorphized-instance table (filled eagerly as
/// annotations like `(Option i64)` resolve — S1a).
pub(crate) struct TypeCx<'a> {
    pub adts: &'a [AdtDef],
    pub adt_by_name: &'a HashMap<String, u32>,
    pub insts: &'a RefCell<AdtInsts>,
}

pub(crate) fn is_extern(form: &Expr) -> bool {
    matches!(
        form,
        Expr::List(items) if matches!(items.first(), Some(Expr::LitSym(h)) if h == "extern")
    )
}

/// (extern name [param :- ty ...] :- ret) — a direct C FFI declaration.
pub(crate) fn parse_extern(
    form: &Expr,
    tcx: &TypeCx,
    fnsigs: &mut Vec<(Vec<Ty>, Ty)>,
) -> Result<RawDefn, String> {
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
    let (params, _borrows) = parse_params(params, tcx, &mut ty_params, fnsigs)?;
    if !ty_params.is_empty() {
        return Err("extern declarations must have concrete types".into());
    }
    let mut ret = Ty::I64;
    let mut body_start = 3;
    if items.len() > body_start + 1 {
        if let Expr::LitSym(sep) = &items[body_start] {
            if sep == ":-" {
                ret = parse_ty(&items[body_start + 1], tcx, &mut ty_params, fnsigs)?;
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
        ret_borrowed: false,
    })
}

pub(crate) fn is_deftype(form: &Expr) -> bool {
    matches!(
        form,
        Expr::List(items) if matches!(items.first(), Some(Expr::LitSym(h)) if h == "deftype")
    )
}

/// (deftype Name (Variant [field :- ty ...]) ...)
/// (deftype (Name a b) (Variant [field :- ty ...]) ...) — generic template
/// Pass 0a: collect just the header (name + type parameters) so field types
/// may reference any declared template regardless of declaration order.
pub(crate) fn parse_deftype_header(form: &Expr) -> Result<(String, Vec<String>), String> {
    let Expr::List(items) = form else {
        return Err("expected (deftype ...)".into());
    };
    if items.len() < 2 {
        return Err("deftype expects a name and at least one variant".into());
    }
    match &items[1] {
        Expr::LitSym(s) => Ok((s.clone(), Vec::new())),
        Expr::List(hitems) => {
            let Some(Expr::LitSym(s)) = hitems.first() else {
                return Err("deftype name must be a symbol".into());
            };
            let mut ps = Vec::new();
            for p in &hitems[1..] {
                let Expr::LitSym(pn) = p else {
                    return Err(format!("deftype {} type parameter must be a symbol", s));
                };
                if ps.iter().any(|x| x == pn) {
                    return Err(format!("deftype {} repeats type parameter {}", s, pn));
                }
                ps.push(pn.clone());
            }
            Ok((s.clone(), ps))
        }
        _ => Err("deftype name must be a symbol".into()),
    }
}

/// Pass 0b: parse the variants now that every declared template is known.
///
/// A field type may be: a scalar, `str`, one of this deftype's own type
/// parameters, `(Vector scalar|str|param)`, a bare ADT name (concrete, or a
/// template applied to this deftype's parameters in order), or an applied
/// template `(Name a b ...)` with the same positional-parameter rule.
pub(crate) fn parse_deftype_variants(
    form: &Expr,
    name: &str,
    ty_params: &[String],
    decls: &HashMap<String, (u32, Vec<String>)>,
) -> Result<Vec<(String, Vec<Ty>)>, String> {
    let Expr::List(items) = form else {
        return Err("expected (deftype ...)".into());
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
                let Some(ty_expr) = fitems.get(i + 2) else {
                    return Err(format!("variant {} field {} expects a type", vname, fname));
                };
                fields.push(parse_field_ty(ty_expr, ty_params, decls)?);
                i += 3;
            }
        }
        variants.push((vname.clone(), fields));
    }
    Ok(variants)
}

/// A deftype field type (see `parse_deftype_variants` for the grammar).
fn parse_field_ty(
    e: &Expr,
    ty_params: &[String],
    decls: &HashMap<String, (u32, Vec<String>)>,
) -> Result<Ty, String> {
    match e {
        Expr::LitSym(s) => match s.as_str() {
            "i64" => Ok(Ty::I64),
            "i32" => Ok(Ty::I32),
            "f64" => Ok(Ty::F64),
            "bool" => Ok(Ty::Bool),
            "str" => Ok(Ty::Str),
            other => {
                if let Some(k) = ty_params.iter().position(|p| p == other) {
                    return Ok(Ty::Param(k as u32));
                }
                match decls.get(other) {
                    Some((idx, params)) if params.is_empty() => Ok(Ty::Adt(*idx)),
                    Some((_, params)) => Err(format!(
                        "generic ADT {} needs type arguments in a field type — apply it to this deftype's parameters: ({} {})",
                        other,
                        other,
                        params
                            .iter()
                            .map(|_| "...")
                            .collect::<Vec<_>>()
                            .join(" ")
                    )),
                    None => Err(format!(
                        "ADT field type must be i64, i32, f64, bool, str, a type parameter of this deftype, or a declared ADT name, got '{}'",
                        other
                    )),
                }
            }
        },
        Expr::List(items) => {
            let Some(Expr::LitSym(h)) = items.first() else {
                return Err("field type must be a symbol or an applied ADT name".into());
            };
            if h == "Vector" && items.len() == 2 {
                let el = match &items[1] {
                    Expr::LitSym(s) if Elem::of_name(s).is_some() => Elem::of_name(s).unwrap(),
                    Expr::LitSym(p) => {
                        let k = ty_params
                            .iter()
                            .position(|x| x == p)
                            .ok_or_else(|| {
                                format!("(Vector {}) element must be i64, f64, str or a type parameter of this deftype", p)
                            })?;
                        Elem::Param(k as u32)
                    }
                    _ => {
                        return Err(
                            "(Vector ...) element must be i64, f64, str or a type parameter of this deftype"
                                .into(),
                        )
                    }
                };
                return Ok(Ty::Vec(el));
            }
            // Applied template: (Name arg ...) — the positional-parameter
            // rule: the arguments must be this deftype's parameters in
            // order, so instantiation can pass its own arguments through.
            let Some(&(idx, ref tpl_params)) = decls.get(h.as_str()) else {
                return Err(format!("unknown ADT name {} in field type", h));
            };
            let args: Vec<&Expr> = items[1..].iter().collect();
            if args.len() != tpl_params.len() {
                return Err(format!(
                    "{} expects {} type argument(s) in a field type, got {}",
                    h,
                    tpl_params.len(),
                    args.len()
                ));
            }
            for (a, p) in args.iter().zip(tpl_params.iter()) {
                let Expr::LitSym(sym) = a else {
                    return Err(format!(
                        "{} type argument must be a parameter name of this deftype",
                        h
                    ));
                };
                if sym != p {
                    return Err(format!(
                        "field type ({} ...) must apply {} to this deftype's parameters in order (expected {})",
                        h, h, p
                    ));
                }
            }
            Ok(Ty::Adt(idx))
        }
        _ => Err("field type must be a symbol or an applied ADT name".into()),
    }
}

/// Lower a raw defn body (an implicit `do` when it holds multiple forms).
pub(crate) fn lower_body_expr(
    body: &Expr,
    env: &Env,
    ctx: &Ctx,
    expected: Option<Ty>,
) -> Result<HExpr, String> {
    match body {
        Expr::List(items) if matches!(items.first(), Some(Expr::LitSym(h)) if h == "do") => {
            lower_body_forms(items, env, ctx, expected)
        }
        _ => lower_expect(body, env, ctx, expected),
    }
}


// ---------------------------------------------------------------------------
// Ownership / borrow checking
// ---------------------------------------------------------------------------

/// Types that move: ADTs and vectors. Scalars and strings (immutable static
/// data) are Copy.
pub(crate) fn parse_raw(
    form: &Expr,
    tcx: &TypeCx,
    fnsigs: &mut Vec<(Vec<Ty>, Ty)>,
) -> Result<RawDefn, String> {
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
    let (params, borrows) = parse_params(params, tcx, &mut ty_params, fnsigs)?;

    let mut body_start = 3;
    let mut ret = Ty::I64;
    let mut ret_borrowed = false;
    if items.len() > body_start + 1 {
        if let Expr::LitSym(sep) = &items[body_start] {
            if sep == ":-" {
                match &items[body_start + 1] {
                    Expr::LitSym(ty_sym) => {
                        let (b, rest) = split_borrow(ty_sym)?;
                        if b == Borrow::Shared {
                            if rest.is_empty() {
                                return Err(
                                    "borrowed return must name a boxed ADT (write `:- &Name`, not `&(...)`) — borrowed return"
                                        .into(),
                                );
                            }
                            ret_borrowed = true;
                            ret = parse_ty_sym(rest, tcx.adt_by_name, &mut ty_params)?;
                        } else {
                            ret = parse_ty(
                                &items[body_start + 1],
                                tcx,
                                &mut ty_params,
                                fnsigs,
                            )?;
                        }
                    }
                    _ => {
                        ret =
                            parse_ty(&items[body_start + 1], tcx, &mut ty_params, fnsigs)?;
                    }
                }
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
        ret_borrowed,
    })
}

pub(crate) fn parse_params(
    params: &[Expr],
    tcx: &TypeCx,
    ty_params: &mut Vec<String>,
    fnsigs: &mut Vec<(Vec<Ty>, Ty)>,
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
                    let (name_borrow, bare) = split_borrow(name)?;
                    // `x :- &(Vector u8)`: the reader splits the sigil off
                    // a list annotation into its own `&` symbol.
                    if matches!(&params[i + 2], Expr::LitSym(amp) if amp == "&") {
                        let Some(list) = params.get(i + 3) else {
                            return Err(format!("parameter '{}': `&` needs a type", name));
                        };
                        out.push((bare.to_string(), parse_ty(list, tcx, ty_params, fnsigs)?));
                        borrows.push(Borrow::Shared);
                        i += 4;
                        continue;
                    }
                    let (ty_borrow, ty) = match &params[i + 2] {
                        Expr::LitSym(ty_sym) => {
                            let (b, rest) = split_borrow(ty_sym)?;
                            (b, parse_ty_sym(rest, tcx.adt_by_name, ty_params)?)
                        }
                        other => (
                            Borrow::Owned,
                            parse_ty(other, tcx, ty_params, fnsigs)?,
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
/// parameter name, `(Vector i64|f64|T)`, or `(Fn [t1 t2] ret)`.
pub(crate) fn parse_ty(
    e: &Expr,
    tcx: &TypeCx,
    ty_params: &mut Vec<String>,
    fnsigs: &mut Vec<(Vec<Ty>, Ty)>,
) -> Result<Ty, String> {
    match e {
        Expr::List(items) => {
            let Some(Expr::LitSym(h)) = items.first() else {
                return Err("type annotation must be a symbol or (Vector ty)".into());
            };
            if h == "Fn" {
                // (Fn [t1 ...] ret) — structural identity is resolved later
                // by deduplicating against the program's signature table.
                let Expr::Vector(pitems) = &items[1] else {
                    return Err("Fn params must be a vector".into());
                };
                let mut ptys = Vec::new();
                for p in pitems {
                    ptys.push(parse_scalar_or_vec(p, tcx, ty_params, fnsigs)?);
                }
                let ret = parse_scalar_or_vec(&items[2], tcx, ty_params, fnsigs)?;
                let sig = match fnsigs
                    .iter()
                    .position(|(p, r)| *p == ptys && *r == ret)
                {
                    Some(i) => i as u32,
                    None => {
                        fnsigs.push((ptys.clone(), ret));
                        (fnsigs.len() - 1) as u32
                    }
                };
                return Ok(Ty::Fn(sig));
            }
            if h != "Vector" && tcx.adt_by_name.contains_key(h.as_str()) {
                // Applied ADT type: `(Name arg ...)` instantiates a generic
                // ADT eagerly (S1a). Arguments are type symbols — scalars or
                // this defn's own type parameters.
                let args: Vec<Ty> = items[1..]
                    .iter()
                    .map(|a| parse_ty(a, tcx, ty_params, fnsigs))
                    .collect::<Result<_, _>>()?;
                let tpl = *tcx
                    .adt_by_name
                    .get(h.as_str())
                    .filter(|&i| !tcx.adts[*i as usize].ty_params.is_empty())
                    .ok_or_else(|| format!("{} takes no type parameters", h))?;
                return Ok(Ty::Adt(instantiate_adt(tcx.adts, tcx.insts, tpl, &args)?));
            }
            if h != "Vector" || items.len() != 2 {
                return Err(format!(
                    "unsupported type annotation (expected (Vector ty), got {} elements)",
                    items.len() - 1
                ));
            }
            let el = match &items[1] {
                Expr::LitSym(s) if Elem::of_name(s).is_some() => Elem::of_name(s).unwrap(),
                Expr::LitSym(p) => {
                    // A type parameter must be spelled like one (uppercase,
                    // e.g. T); anything else is a typo'd type name.
                    if !is_ty_param_name(p) {
                        return Err(format!("unknown type name {}", p));
                    }
                    Elem::Param(register_ty_param(p, ty_params))
                }
                _ => return Err("Vector element type must be i64, f64 or a type parameter".into()),
            };
            Ok(Ty::Vec(el))
        }
        Expr::LitSym(s) => parse_ty_sym(s, tcx.adt_by_name, ty_params),
        _ => Err("type annotation must be a symbol or (Vector ty)".into()),
    }
}

/// Split a leading borrow sigil off an annotation symbol: `&Shape`,
/// `&(Vector f64)`. (`&!` exclusive borrows were removed pre-1.0: the
/// uniqueness-based in-place paths need no user-facing exclusive lend.)
pub(crate) fn split_borrow(s: &str) -> Result<(Borrow, &str), String> {
    if s.starts_with("&!") {
        Err("unsupported borrow sigil `&!` — exclusive borrows were removed; use `&` or pass by value".to_string())
    } else if let Some(rest) = s.strip_prefix('&') {
        Ok((Borrow::Shared, rest))
    } else {
        Ok((Borrow::Owned, s))
    }
}

/// The built-in type names — one table for every annotation position
/// (parameters, returns, `let`/`loop` bindings, ADT fields).
pub(crate) fn builtin_ty_name(s: &str) -> Option<Ty> {
    Some(match s {
        "i64" => Ty::I64,
        "i32" => Ty::I32,
        "i8" => Ty::I8,
        "i16" => Ty::I16,
        "u8" => Ty::U8,
        "u16" => Ty::U16,
        "u32" => Ty::U32,
        "u64" => Ty::U64,
        "usize" => Ty::Usize,
        "f64" => Ty::F64,
        "f32" => Ty::F32,
        "vec" => VEC_I64,
        "bool" => Ty::Bool,
        "str" | "string" => Ty::Str,
        "map" => Ty::Map,
        "set" => Ty::Set,
        _ => return None,
    })
}

pub(crate) fn parse_ty_sym(
    s: &str,
    adt_by_name: &HashMap<String, u32>,
    ty_params: &mut Vec<String>,
) -> Result<Ty, String> {
    if let Some(t) = builtin_ty_name(s) {
        return Ok(t);
    }
    match s {
        other => match adt_by_name.get(other) {
            Some(&i) => Ok(Ty::Adt(i)),
            None if is_ty_param_name(other) => {
                Ok(Ty::Param(register_ty_param(other, ty_params)))
            }
            // Lowercase unknowns are typos, not type variables: reject them
            // instead of silently genericizing the defn.
            None => Err(format!("unknown type name {}", other)),
        },
    }
}

/// Type parameters are introduced implicitly by annotation spelling: they
/// must start with an uppercase letter (`T`), mirroring the ADT-name
/// convention. Lowercase unknown symbols are treated as the typos they are.
fn is_ty_param_name(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

/// Parse a Fn-annotation element type (scalar, str, or vector forms only —
/// closures of closure params are not in the slice).
fn parse_scalar_or_vec(
    e: &Expr,
    tcx: &TypeCx,
    ty_params: &mut Vec<String>,
    fnsigs: &mut Vec<(Vec<Ty>, Ty)>,
) -> Result<Ty, String> {
    match e {
        Expr::LitSym(s) => parse_ty_sym(s, tcx.adt_by_name, ty_params),
        Expr::List(items) if matches!(items.first(), Some(Expr::LitSym(h)) if h == "Vector") => {
            parse_ty(e, tcx, ty_params, fnsigs)
        }
        _ => Err("Fn param/ret types must be scalars, str, or (Vector ty)".into()),
    }
}

pub(crate) fn register_ty_param(name: &str, ty_params: &mut Vec<String>) -> u32 {
    if let Some(i) = ty_params.iter().position(|p| p == name) {
        return i as u32;
    }
    ty_params.push(name.to_string());
    (ty_params.len() - 1) as u32
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
