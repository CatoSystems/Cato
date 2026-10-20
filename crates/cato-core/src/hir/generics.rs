//! CLOS-style generic functions (multimethods) over closed ADTs.
//!
//! A source-level abstraction only: `defgeneric`/`defmethod` forms are
//! collected into a method table, and every call site is resolved through
//! a **structural specificity partial order** into ordinary statically
//! resolved control flow — a compiler-generated dispatch function holding
//! a nested `match`, calling hidden per-method functions. There is no
//! runtime method lookup, registration, vtable, or type metadata.
//!
//! Specificity is structural, never positional: `Variant(V of A) >
//! Adt(A)` for single dispatch, component-wise for multiple dispatch
//! (`Circle × Shape` and `Shape × Circle` are incomparable — two
//! applicable incomparable methods are a compile-time ambiguity).
//! Declaration order affects nothing.
//!
//! Dispatch domain: ADTs and their variants, builtin scalars, `str`,
//! `bool`, `map`, `set`, and concrete vectors — `(Vector i64)`,
//! `(Vector f64)`, `(Vector str)`. Collection specializers are exact (no
//! element-type wildcards), so hidden methods lower once per method with
//! concrete parameter types.

use super::*;
use crate::ast::{Expr, Span};

/// May a multimethod dispatch on this type? Concrete types only — a
/// `(Vector T)` element parameter inside a generic defn cannot select a
/// method, because methods lower once per concrete signature. Map and set
/// specializers are not offered yet: the LLVM backend cannot pass map/set
/// values as call arguments (vectors carry a `(ptr, len)` ABI; maps and
/// sets do not yet), so accepting them would break backend parity.
pub(crate) fn is_dispatchable_ty(t: Ty) -> bool {
    t.is_int()
        || t.is_float()
        || matches!(t, Ty::Str | Ty::Bool | Ty::Map | Ty::Set)
        || matches!(t, Ty::Vec(e) if !e.is_param())
}

pub(crate) fn is_defgeneric(form: &Expr) -> bool {
    head_is(form, "defgeneric")
}

pub(crate) fn is_defmethod(form: &Expr) -> bool {
    head_is(form, "defmethod")
}

fn head_is(form: &Expr, h: &str) -> bool {
    matches!(
        form,
        Expr::List(items) if matches!(items.first(), Some(Expr::LitSym(x)) if x == h)
    )
}

/// A specializer for one parameter position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Spec {
    /// A concrete builtin type (`i64`, `f64`, `str`, `bool`, widths,
    /// `(Vector i64)`, `map`, `set`).
    Builtin(Ty),
    /// Any concrete vector: `(Vector _)`. Instantiated per element type —
    /// each call site lowers a specialized hidden method for the
    /// argument's concrete element type.
    VecAny,
    /// The whole ADT: matches every variant.
    Adt(u32),
    /// Exactly this variant of this ADT.
    Variant(u32, u32),
}

impl Spec {
    /// Ground type of a value bound by this specializer (variants bind as
    /// their ADT — Cato has no variant subtypes). Wildcards have none:
    /// they must be instantiated first.
    pub(crate) fn ground(&self) -> Ty {
        match self {
            Spec::Builtin(t) => *t,
            Spec::Adt(a) | Spec::Variant(a, _) => Ty::Adt(*a),
            Spec::VecAny => panic!("internal: (Vector _) ground requested before instantiation"),
        }
    }

    fn matches_ty(&self, t: Ty) -> bool {
        match (self, t) {
            (Spec::Builtin(s), u) => *s == u,
            (Spec::VecAny, Ty::Vec(e)) => !e.is_param(),
            (Spec::Adt(a), Ty::Adt(x)) | (Spec::Variant(a, _), Ty::Adt(x)) => *a == x,
            _ => false,
        }
    }

    fn matches_var(&self, adt: u32, var: u32) -> bool {
        match self {
            Spec::Adt(a) => *a == adt,
            Spec::Variant(a, v) => *a == adt && *v == var,
            Spec::Builtin(_) | Spec::VecAny => false,
        }
    }

    /// `self` is strictly more specific than `other`: a variant over its
    /// own ADT, or a concrete vector over the `(Vector _)` wildcard. Equal
    /// specializers are not strictly more specific.
    fn strictly_more(&self, other: &Spec) -> bool {
        match (self, other) {
            (Spec::Variant(a1, _), Spec::Adt(a2)) if a1 == a2 => true,
            (Spec::Builtin(Ty::Vec(_)), Spec::VecAny) => true,
            _ => false,
        }
    }

    /// `self` is at least as specific as `other` (equal or strictly more).
    fn at_least(&self, other: &Spec) -> bool {
        self == other || self.strictly_more(other)
    }

    fn display(&self, adts: &[AdtDef]) -> String {
        match self {
            Spec::Builtin(t) => ty_display(*t),
            Spec::VecAny => "(Vector _)".into(),
            Spec::Adt(a) => adts[*a as usize].name.clone(),
            Spec::Variant(a, v) => adts[*a as usize].variants[*v as usize].0.clone(),
        }
    }
}

fn ty_display(t: Ty) -> String {
    match t {
        Ty::I64 => "i64".into(),
        Ty::I32 => "i32".into(),
        Ty::I8 => "i8".into(),
        Ty::I16 => "i16".into(),
        Ty::U8 => "u8".into(),
        Ty::U16 => "u16".into(),
        Ty::U32 => "u32".into(),
        Ty::U64 => "u64".into(),
        Ty::Usize => "usize".into(),
        Ty::F64 => "f64".into(),
        Ty::F32 => "f32".into(),
        Ty::Bool => "bool".into(),
        Ty::Str => "str".into(),
        Ty::Vec(el) => format!("(Vector {})", elem_display(el)),
        Ty::Map => "map".into(),
        Ty::Set => "set".into(),
        other => format!("{:?}", other),
    }
}

fn elem_display(e: Elem) -> String {
    match e {
        Elem::I64 => "i64".into(),
        Elem::F64 => "f64".into(),
        Elem::Str => "str".into(),
        Elem::N(n) => n.name().into(),
        Elem::Param(p) => format!("T{}", p),
    }
}

pub(crate) struct MethodDef {
    pub span: Span,
    pub specs: Vec<Spec>,
    /// Whole-value binder names, one per parameter.
    pub names: Vec<String>,
    /// Destructuring form `(Variant binders...)`: (param index, variant,
    /// binder names).
    pub destructure: Vec<(usize, u32, Vec<String>)>,
    pub ret: Option<Ty>,
    pub body: Expr,
    pub hidden: String,
}

pub(crate) struct GenericDef {
    pub arity: usize,
    pub ret: Option<Ty>,
    pub methods: Vec<MethodDef>,
}

/// Per-program generic-function state.
#[derive(Default)]
pub(crate) struct GenState {
    pub generics: HashMap<String, GenericDef>,
    /// Hidden/dispatch defn name -> return type (for `ty_of` of calls).
    pub call_rets: HashMap<String, Ty>,
    /// Lowered hidden/dispatch defns, drained into the program before
    /// ownership checking.
    pub extra: Vec<Defn>,
    /// Method return types, once lowered (key: hidden name).
    pub lowered_rets: HashMap<String, Ty>,
    /// Dispatch defn cache: signature key -> defn name.
    pub dispatch_cache: HashMap<String, String>,
}

/// One slot of a resolution combo: a fixed builtin type, or a variant of
/// an ADT position.
#[derive(Clone, Copy, PartialEq)]
enum Slot {
    Fixed(Ty),
    Var(u32, u32),
}

// ---------------------------------------------------------------------------
// Collection (source forms -> method table)
// ---------------------------------------------------------------------------

/// Pull `defgeneric`/`defmethod` forms out of the form stream, validate
/// them, and return the remaining forms plus the method table.
pub(crate) fn collect_generics(
    prog: &[(Expr, Span)],
    tcx: &TypeCx,
) -> Result<(Vec<(Expr, Span)>, GenState), String> {
    use crate::diagnostics::locate;
    let mut rest = Vec::new();
    let mut order: Vec<String> = Vec::new();
    let mut gens = GenState::default();

    // Variant name -> (adt, variant); first declaration wins, like ctors.
    let mut variant_of: HashMap<&str, (u32, u32)> = HashMap::new();
    for (ai, adt) in tcx.adts.iter().enumerate() {
        for (vi, (vn, _)) in adt.variants.iter().enumerate() {
            variant_of.entry(vn.as_str()).or_insert((ai as u32, vi as u32));
        }
    }

    for (form, span) in prog {
        if is_defgeneric(form) {
            let Expr::List(items) = form else { unreachable!() };
            let bad = |m: &str| locate(*span, m);
            let Some(Expr::LitSym(name)) = items.get(1) else {
                return Err(bad("defgeneric expects a name"));
            };
            let Some(Expr::Vector(ps)) = items.get(2) else {
                return Err(bad("defgeneric params must be a vector"));
            };
            // Params may be annotated; the annotations are documentation —
            // dispatch domains come from the methods.
            let mut ret = None;
            let mut next = 3;
            if let Some(Expr::LitSym(sep)) = items.get(3) {
                if sep == ":-" {
                    match items.get(4) {
                        Some(Expr::LitSym(t)) => {
                            ret = Some(parse_ty_sym(t, tcx.adt_by_name, &mut Vec::new())?);
                            next = 5;
                        }
                        _ => return Err(bad("defgeneric :- expects a return type")),
                    }
                }
            }
            if items.len() > next {
                return Err(bad("unexpected trailing forms in defgeneric"));
            }
            if gens.generics.contains_key(name.as_str()) {
                return Err(bad(&format!("duplicate defgeneric {}", name)));
            }
            // Params are `x` or `x :- ty` triples; count parameters.
            let mut arity = 0;
            let mut k = 0;
            while k < ps.len() {
                arity += 1;
                k += match ps.get(k + 1) {
                    Some(Expr::LitSym(s)) if s == ":-" => 3,
                    _ => 1,
                };
            }
            gens.generics.insert(
                name.clone(),
                GenericDef {
                    arity,
                    ret,
                    methods: Vec::new(),
                },
            );
            order.push(name.clone());
            continue;
        }
        if is_defmethod(form) {
            let Expr::List(items) = form else { unreachable!() };
            let bad = |m: &str| locate(*span, m);
            let Some(Expr::LitSym(name)) = items.get(1) else {
                return Err(bad("defmethod expects a generic name"));
            };
            let Some(Expr::Vector(specs)) = items.get(2) else {
                return Err(bad("defmethod specializers must be a vector"));
            };
            let mut si = 3;
            let mut ret = None;
            if let Some(Expr::LitSym(sep)) = items.get(3) {
                if sep == ":-" {
                    match items.get(4) {
                        Some(Expr::LitSym(t)) => {
                            ret = Some(parse_ty_sym(t, tcx.adt_by_name, &mut Vec::new())?);
                            si = 5;
                        }
                        _ => return Err(bad("defmethod :- expects a return type")),
                    }
                }
            }
            let body = if items.len() - si == 1 {
                items[si].clone()
            } else {
                Expr::List(
                    std::iter::once(Expr::LitSym("do".into()))
                        .chain(items[si..].iter().cloned())
                        .collect(),
                )
            };

            let mut sp: Vec<Spec> = Vec::new();
            let mut names: Vec<String> = Vec::new();
            let mut destructure: Vec<(usize, u32, Vec<String>)> = Vec::new();
            let mut k = 0;
            while k < specs.len() {
                match &specs[k] {
                    // (Variant binders...) — destructuring specializer.
                    Expr::List(vi) => {
                        let Some(Expr::LitSym(vname)) = vi.first() else {
                            return Err(
                                bad("destructuring specializer must start with a variant")
                            );
                        };
                        let Some(&(a, v)) = variant_of.get(vname.as_str()) else {
                            return Err(bad(&format!("unknown variant {}", vname)));
                        };
                        let binders: Vec<String> = vi[1..]
                            .iter()
                            .map(|b| match b {
                                Expr::LitSym(n) => n.clone(),
                                _ => String::new(),
                            })
                            .collect();
                        let nfields = tcx.adts[a as usize].variants[v as usize].1.len();
                        if binders.len() != nfields {
                            return Err(bad(&format!(
                                "variant {} has {} field(s) but the specializer binds {}",
                                vname, nfields, binders.len()
                            )));
                        }
                        names.push(format!("__p{}", sp.len()));
                        destructure.push((sp.len(), v, binders));
                        sp.push(Spec::Variant(a, v));
                        k += 1;
                    }
                    // name :- Type
                    Expr::LitSym(pname) => {
                        let bad_spec = || {
                            bad("specializers expect (name :- Type) or (Variant binders...)")
                        };
                        let Some(Expr::LitSym(sep)) = specs.get(k + 1) else {
                            return Err(bad_spec());
                        };
                        if sep != ":-" {
                            return Err(bad_spec());
                        }
                        let Some(ty) = specs.get(k + 2) else {
                            return Err(bad("specializer expects a type"));
                        };
                        let spec = match ty {
                            // (Vector i64|f64|str) concrete, or (Vector _).
                            Expr::List(_) => match parse_specializer_spec(ty) {
                                Ok(spec) => spec,
                                Err(e) => return Err(bad(&e)),
                            },
                            Expr::LitSym(tyname) => {
                                if let Ok(Ty::Adt(a)) =
                                    parse_ty_sym(tyname, tcx.adt_by_name, &mut Vec::new())
                                {
                                    Spec::Adt(a)
                                } else if let Some(&(a, v)) = variant_of.get(tyname.as_str()) {
                                    Spec::Variant(a, v)
                                } else {
                                    match parse_ty_sym(tyname, tcx.adt_by_name, &mut Vec::new()) {
                                        Ok(t) if is_dispatchable_ty(t) => Spec::Builtin(t),
                                        _ => {
                                            return Err(bad(&format!(
                                                "unknown specializer type {} (builtin scalars, str, bool, map, set, concrete vectors, ADTs and their variants only)",
                                                tyname
                                            )))
                                        }
                                    }
                                }
                            }
                            _ => return Err(bad_spec()),
                        };
                        names.push(pname.clone());
                        sp.push(spec);
                        k += 3;
                    }
                    _ => return Err(bad("bad specializer form")),
                }
            }

            let Some(g) = gens.generics.get_mut(name.as_str()) else {
                return Err(bad(&format!(
                    "defmethod {} has no defgeneric (declare it first)",
                    name
                )));
            };
            if sp.len() != g.arity {
                return Err(bad(&format!(
                    "defmethod {} expects {} specializer(s), got {}",
                    name,
                    g.arity,
                    sp.len()
                )));
            }
            if let (Some(gr), Some(mr)) = (g.ret, ret) {
                if gr != mr {
                    return Err(bad(&format!(
                        "defmethod {} returns {:?} but the generic declares {:?}",
                        name, mr, gr
                    )));
                }
            }
            g.methods.push(MethodDef {
                span: *span,
                specs: sp,
                names,
                destructure,
                ret,
                body,
                hidden: format!("{}__m{}", name, g.methods.len()),
            });
            continue;
        }
        rest.push((form.clone(), *span));
    }

    // Finalize: duplicate methods and structurally unreachable methods.
    for name in &order {
        let g = gens.generics.get(name).unwrap();
        for (mi, m) in g.methods.iter().enumerate() {
            for (ni, n) in g.methods.iter().enumerate() {
                if ni <= mi {
                    continue;
                }
                if n.specs == m.specs {
                    return Err(locate(
                        n.span,
                        &format!(
                            "duplicate method for {}: ({})",
                            name,
                            display_specs(&m.specs, tcx.adts)
                        ),
                    ));
                }
            }
        }
        check_unreachable(name, g, tcx.adts)?;
    }

    Ok((rest, gens))
}

fn display_specs(specs: &[Spec], adts: &[AdtDef]) -> String {
    specs
        .iter()
        .map(|s| s.display(adts))
        .collect::<Vec<_>>()
        .join(" × ")
}

/// Parse a collection type annotation in specializer position:
/// `(Vector i64|f64|str)` for a concrete vector, `(Vector _)` for the
/// wildcard. Element type parameters are rejected — except for the
/// wildcard, methods lower once per concrete signature.
fn parse_specializer_spec(e: &Expr) -> Result<Spec, String> {
    let Expr::List(items) = e else {
        return Err("type annotation must be a symbol or (Vector ...)".into());
    };
    let Some(Expr::LitSym(h)) = items.first() else {
        return Err("type annotation must be a symbol or (Vector ...)".into());
    };
    if h != "Vector" || items.len() != 2 {
        return Err(format!(
            "unsupported specializer (expected (Vector i64|f64|str|_), got {} form(s))",
            items.len().saturating_sub(1)
        ));
    }
    match &items[1] {
        Expr::LitSym(s) if s == "i64" => Ok(Spec::Builtin(Ty::Vec(Elem::I64))),
        Expr::LitSym(s) if s == "f64" => Ok(Spec::Builtin(Ty::Vec(Elem::F64))),
        Expr::LitSym(s) if s == "str" => Ok(Spec::Builtin(Ty::Vec(Elem::Str))),
        Expr::LitSym(u) if u == "_" => Ok(Spec::VecAny),
        Expr::LitSym(p) => Err(format!(
            "specializer element type `{p}` is a type parameter — use (Vector _) for any vector"
        )),
        _ => Err("Vector element type must be i64, f64, str or _".into()),
    }
}

/// The combos a method applies to: builtin positions are fixed, ADT
/// positions range over all their variants, variant positions are fixed.
fn method_combos(m: &MethodDef, adts: &[AdtDef]) -> Vec<Vec<Slot>> {
    let mut out: Vec<Vec<Slot>> = vec![vec![]];
    for s in &m.specs {
        let options: Vec<Slot> = match s {
            Spec::Builtin(t) => vec![Slot::Fixed(*t)],
            // Dead: check_unreachable skips wildcard generics entirely.
            Spec::VecAny => unreachable!("(Vector _) has no fixed slot"),
            Spec::Adt(a) => (0..adts[*a as usize].variants.len())
                .map(|v| Slot::Var(*a, v as u32))
                .collect(),
            Spec::Variant(a, v) => vec![Slot::Var(*a, *v)],
        };
        out = out
            .into_iter()
            .flat_map(|prefix| {
                options
                    .iter()
                    .map(move |o| {
                        let mut c = prefix.clone();
                        c.push(*o);
                        c
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
    }
    out
}

fn applies_at(specs: &[Spec], combo: &[Slot]) -> bool {
    specs.iter().zip(combo.iter()).all(|(s, c)| match c {
        Slot::Fixed(t) => s.matches_ty(*t),
        Slot::Var(a, v) => s.matches_var(*a, *v),
    })
}

/// A method is **unreachable** when for every variant combination it
/// applies to, another applicable method is component-wise at least as
/// specific (so it can never be the unique maximum).
fn check_unreachable(name: &str, g: &GenericDef, adts: &[AdtDef]) -> Result<(), String> {
    // Wildcard generics skip the dominance analysis: `(Vector _)` is
    // never at_least an exact spec, so a wildcard method can only be
    // dominated by an identical one — which the duplicate check above
    // already rejects.
    if g.methods
        .iter()
        .any(|m| m.specs.iter().any(|s| matches!(s, Spec::VecAny)))
    {
        return Ok(());
    }
    use crate::diagnostics::locate;
    for m in &g.methods {
        let combos = method_combos(m, adts);
        if combos.len() > 4096 {
            continue; // conservative: skip huge dispatch spaces
        }
        let all_dominated = combos.iter().all(|combo| {
            g.methods.iter().any(|n| {
                n.hidden != m.hidden
                    && applies_at(&n.specs, combo)
                    && n.specs
                        .iter()
                        .zip(m.specs.iter())
                        .all(|(s2, s1)| s2.at_least(s1))
                    && n.specs
                        .iter()
                        .zip(m.specs.iter())
                        .any(|(s2, s1)| s2.strictly_more(s1))
            })
        });
        if all_dominated {
            return Err(locate(
                m.span,
                &format!(
                    "method {} of {} can never be selected: a more specific method covers every input it applies to",
                    display_specs(&m.specs, adts),
                    name
                ),
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Resolution (call site -> dispatch decision tree)
// ---------------------------------------------------------------------------

/// Resolve `(name args...)` with the given static argument types. Returns
/// a call to a hidden method (all-builtin signatures) or to a
/// compiler-generated dispatch defn (nested matches over ADT components).
pub(crate) fn generic_call(
    name: &str,
    args: Vec<HExpr>,
    arg_tys: Vec<Ty>,
    ctx: &Ctx,
) -> Result<HExpr, String> {
    let arity = arg_tys.len();
    let (declared_ret, method_names, specs_all) = {
        let gens = ctx.gens.borrow();
        let g = gens
            .generics
            .get(name)
            .ok_or_else(|| format!("unknown generic {}", name))?;
        if g.arity != arity {
            return Err(format!(
                "generic {} expects {} argument(s), got {}",
                name, g.arity, arity
            ));
        }
        for t in &arg_tys {
            match t {
                Ty::Adt(a) => {
                    if ctx.is_adt_inst(*a) {
                        return Err(format!(
                            "generic {} cannot dispatch on a monomorphized ADT instance yet (declare a concrete deftype for the specializer)",
                            name
                        ));
                    }
                }
                Ty::Vec(Elem::Param(_)) => {
                    return Err(format!(
                        "generic {} cannot dispatch on a (Vector T) — methods take concrete vector types",
                        name
                    ))
                }
                t if is_dispatchable_ty(*t) => {}
                other => {
                    return Err(format!(
                        "generic {} cannot dispatch on {:?} (builtin scalars, str, bool, map, set, vectors and ADTs only)",
                        name, other
                    ))
                }
            }
        }
        (
            g.ret,
            g.methods.iter().map(|m| m.hidden.clone()).collect::<Vec<_>>(),
            g.methods.iter().map(|m| m.specs.clone()).collect::<Vec<_>>(),
        )
    };

    // Applicability of method `mi` at `combo` (None slot = builtin
    // position, fixed by the call's static type).
    let applicable_at = |mi: usize, combo: &[Slot]| -> bool {
        specs_all[mi]
            .iter()
            .zip(combo.iter())
            .all(|(s, c)| match c {
                Slot::Fixed(t) => s.matches_ty(*t),
                Slot::Var(a, v) => s.matches_var(*a, *v),
            })
    };

    let adt_positions: Vec<(usize, u32, usize)> = arg_tys
        .iter()
        .enumerate()
        .filter_map(|(i, t)| match t {
            Ty::Adt(a) => Some((i, *a, ctx.adts[*a as usize].variants.len())),
            _ => None,
        })
        .collect();

    if adt_positions.is_empty() {
        // Pure builtin/collection signature: applicability through
        // matches_ty (wildcards match any concrete vector), resolved to
        // the unique maximal method under the partial order.
        let applicable: Vec<usize> = (0..method_names.len())
            .filter(|mi| {
                specs_all[*mi]
                    .iter()
                    .zip(arg_tys.iter())
                    .all(|(s, t)| s.matches_ty(*t))
            })
            .collect();
        if applicable.is_empty() {
            return Err(format!(
                "no method {} for ({})",
                name,
                arg_tys
                    .iter()
                    .map(|t| ty_display(*t))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let maximal: Vec<usize> = applicable
            .iter()
            .copied()
            .filter(|mi| {
                !applicable.iter().any(|ni| {
                    ni != mi
                        && specs_all[*ni].iter().zip(specs_all[*mi].iter()).all(|(s2, s1)| s2.at_least(s1))
                        && specs_all[*ni]
                            .iter()
                            .zip(specs_all[*mi].iter())
                            .any(|(s2, s1)| s2.strictly_more(s1))
                })
            })
            .collect();
        if maximal.len() > 1 {
            let comps: Vec<String> = maximal
                .iter()
                .map(|mi| display_specs(&specs_all[*mi], ctx.adts))
                .collect();
            return Err(format!(
                "ambiguous call to {} at ({}): {} are both applicable and neither is more specific — add a more specific method",
                name,
                arg_tys.iter().map(|t| ty_display(*t)).collect::<Vec<_>>().join(", "),
                comps.join(" and ")
            ));
        }
        // Wildcard positions instantiate with the call's concrete types;
        // methods without wildcards keep their classic hidden name.
        let mi = maximal[0];
        let (hidden, r) = resolve_method_instance(name, mi, Some(arg_tys.clone()), ctx)?;
        ctx.gens
            .borrow_mut()
            .call_rets
            .insert(hidden.clone(), r);
        return Ok(HExpr::Call(hidden, args));
    }

    // Cross product over the ADT components; every combo must resolve to
    // a unique maximal method under the structural partial order.
    let mut total = 1usize;
    for (_, _, nv) in &adt_positions {
        total *= nv;
    }
    if total > 65536 {
        return Err(format!("generic {} dispatch space too large", name));
    }
    let mut chosen: Vec<(Vec<(usize, u32)>, usize)> = Vec::new();
    for vi in 0..total {
        // Build one combo (variant index per ADT position).
        let mut rest = vi;
        let mut combo: Vec<(usize, u32)> = Vec::new();
        for (pos, _adt, nv) in &adt_positions {
            combo.push((*pos, (rest % nv) as u32));
            rest /= nv;
        }
        let slots: Vec<Slot> = (0..arg_tys.len())
            .map(|ci| match combo.iter().find(|(p, _)| *p == ci) {
                Some((_, v)) => {
                    let Ty::Adt(a) = arg_tys[ci] else {
                        unreachable!("combo slot on non-ADT")
                    };
                    Slot::Var(a, *v)
                }
                None => Slot::Fixed(arg_tys[ci]),
            })
            .collect();

        let applicable: Vec<usize> = (0..method_names.len())
            .filter(|mi| applicable_at(*mi, &slots))
            .collect();
        if applicable.is_empty() {
            return Err(format!(
                "no method {} for this variant combination ({}) — add a method or a default",
                name,
                display_combo(&combo, &arg_tys, ctx)
            ));
        }
        // Maximal set under the structural partial order.
        let maximal: Vec<usize> = applicable
            .iter()
            .copied()
            .filter(|mi| {
                !applicable.iter().any(|ni| {
                    ni != mi
                        && specs_all[*ni]
                            .iter()
                            .zip(specs_all[*mi].iter())
                            .all(|(s2, s1)| s2.at_least(s1))
                        && specs_all[*ni]
                            .iter()
                            .zip(specs_all[*mi].iter())
                            .any(|(s2, s1)| s2.strictly_more(s1))
                })
            })
            .collect();
        if maximal.len() > 1 {
            let comps: Vec<String> = maximal
                .iter()
                .map(|mi| display_specs(&specs_all[*mi], ctx.adts))
                .collect();
            return Err(format!(
                "ambiguous call to {} at ({}): {} are both applicable and neither is more specific — add a more specific method",
                name,
                display_combo(&combo, &arg_tys, ctx),
                comps.join(" and ")
            ));
        }
        chosen.push((combo, maximal[0]));
    }

    // Instantiate the selected methods: wildcard positions bind to this
    // call's concrete collection types (identical across combos — those
    // slots are fixed by the call's static types). Every method's
    // callable name lands in `call_names`; classic methods keep theirs.
    let mut call_names = method_names.clone();
    let mut instance_rets: Vec<Option<Ty>> = vec![None; method_names.len()];
    for (_combo, mi) in &chosen {
        if instance_rets[*mi].is_some() {
            continue;
        }
        let specs = &specs_all[*mi];
        let grounds: Vec<Ty> = specs
            .iter()
            .enumerate()
            .map(|(pi, s)| match s {
                // The wildcard is a builtin (non-ADT) position, fixed by
                // the call's static argument type at the same position.
                Spec::VecAny => arg_tys[pi],
                other => other.ground(),
            })
            .collect();
        let (h, r) = resolve_method_instance(name, *mi, Some(grounds), ctx)?;
        call_names[*mi] = h;
        instance_rets[*mi] = Some(r);
    }

    if chosen.is_empty() {
        return Err(format!(
            "no method {} applies — the dispatch space is empty",
            name
        ));
    }
    let mut ret: Option<Ty> = None;
    for (_, mi) in &chosen {
        let r = instance_rets[*mi]
            .expect("selected method instantiated above");
        match ret {
            None => ret = Some(r),
            Some(prev) if prev != r => {
                return Err(format!(
                    "methods of {} disagree on return type: {:?} vs {:?}",
                    name, prev, r
                ))
            }
            _ => {}
        }
    }
    let ret = ret.expect("chosen methods have return types");
    if let Some(gr) = declared_ret {
        if gr != ret {
            return Err(format!(
                "methods of {} return {:?} but the generic declares {:?}",
                name, ret, gr
            ));
        }
    }

    // Synthesize (or reuse) the dispatch defn for this signature.
    let key = format!(
        "{}__{}",
        name,
        arg_tys
            .iter()
            .map(|t| ty_key(*t, ctx))
            .collect::<Vec<_>>()
            .join("_")
    );
    if ctx.gens.borrow().dispatch_cache.contains_key(&key) {
        return Ok(HExpr::Call(key, args));
    }
    let params: Vec<String> = (0..arity).map(|i| format!("__a{}", i)).collect();
    let body = build_tree(&chosen, &adt_positions, 0, &params, &call_names, ret)?;
    ctx.gens.borrow_mut().extra.push(Defn {
        name: key.clone(),
        params: params.iter().cloned().zip(arg_tys.clone()).collect(),
        ret,
        body,
        borrows: vec![Borrow::Owned; arity],
        is_extern: false,
        ret_borrowed: false,
        ret_owned: false,
    });
    ctx.gens.borrow_mut().dispatch_cache.insert(key.clone(), key.clone());
    ctx.gens.borrow_mut().call_rets.insert(key.clone(), ret);
    Ok(HExpr::Call(key, args))
}

fn display_combo(combo: &[(usize, u32)], arg_tys: &[Ty], ctx: &Ctx) -> String {
    combo
        .iter()
        .map(|(pos, v)| {
            let Ty::Adt(a) = arg_tys[*pos] else {
                unreachable!()
            };
            ctx.adt_def(a).variants[*v as usize].0.clone()
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Symbol-safe type key for generated dispatch defn names (LLVM
/// identifiers — no spaces or brackets).
fn ty_key(t: Ty, ctx: &Ctx) -> String {
    match t {
        Ty::Adt(a) => ctx.adt_def(a).name.clone(),
        Ty::Vec(el) => format!("vec_{}", elem_key(el)),
        Ty::Map => "map".into(),
        Ty::Set => "set".into(),
        other => ty_display(other),
    }
}

fn elem_key(e: Elem) -> String {
    match e {
        Elem::I64 => "i64".into(),
        Elem::F64 => "f64".into(),
        Elem::Str => "str".into(),
        Elem::N(n) => n.name().into(),
        Elem::Param(p) => format!("t{}", p),
    }
}

/// Build the dispatch decision tree: nested matches over the ADT
/// components (in position order), leaves calling the resolved method.
fn build_tree(
    chosen: &[(Vec<(usize, u32)>, usize)],
    adt_positions: &[(usize, u32, usize)],
    depth: usize,
    params: &[String],
    method_names: &[String],
    ret: Ty,
) -> Result<HExpr, String> {
    let (pos, _adt, nvars) = adt_positions[depth];
    let mut arms = Vec::new();
    for v in 0..nvars as u32 {
        let sub: Vec<(Vec<(usize, u32)>, usize)> = chosen
            .iter()
            .filter(|(combo, _)| combo.iter().any(|(p, vv)| *p == pos && *vv == v))
            .cloned()
            .collect();
        let body = if depth + 1 == adt_positions.len() {
            let (_, mi) = &sub[0];
            HExpr::Call(
                method_names[*mi].clone(),
                params.iter().map(|p| HExpr::Var(p.clone())).collect(),
            )
        } else {
            build_tree(&sub, adt_positions, depth + 1, params, method_names, ret)?
        };
        arms.push(HArm {
            variant: Some(v),
            binders: vec![format!("__b{}_{}", depth, v)],
            body,
            exit_free: None,
        });
    }
    Ok(HExpr::Match {
        scrut: Box::new(HExpr::Var(params[pos].clone())),
        arms,
        ty: ret,
    })
}

/// Lower a method body once into its hidden defn; returns the ret type.
/// The hidden defn's params are the specializers' ground types, so the
/// dispatch tree (and plain builtin call sites) can call it with the
/// call's argument registers.
/// Lower method `mi` and return its callable hidden name plus return type.
/// When the method has `(Vector _)` positions, `grounds` (the call site's
/// concrete types at those and all other positions) selects a
/// **specialized instance**: the body lowers once per distinct ground
/// signature, cached under `name__m{mi}__{ground keys}`. Methods without
/// wildcards ignore the override and lower once under their plain hidden
/// name, exactly as before.
fn resolve_method_instance(
    name: &str,
    mi: usize,
    grounds_override: Option<Vec<Ty>>,
    ctx: &Ctx,
) -> Result<(String, Ty), String> {
    let (hidden, grounds, names, destructure, body_expr, declared) = {
        let gens = ctx.gens.borrow();
        let g = gens.generics.get(name).unwrap();
        let m = &g.methods[mi];
        let has_wildcard = m.specs.iter().any(|s| matches!(s, Spec::VecAny));
        match grounds_override {
            // Specialized instance: wildcard positions bind to the call's
            // concrete types.
            Some(gv) if has_wildcard => {
                let hidden = format!("{}__{}", m.hidden, grounds_key(&gv, ctx));
                (hidden, gv, m.names.clone(), m.destructure.clone(), m.body.clone(), m.ret)
            }
            // Classic: lower once under the method's own hidden name.
            _ => {
                let classic: Vec<Ty> = m.specs.iter().map(|s| s.ground()).collect();
                (
                    m.hidden.clone(),
                    classic,
                    m.names.clone(),
                    m.destructure.clone(),
                    m.body.clone(),
                    m.ret,
                )
            }
        }
    };
    {
        let gens = ctx.gens.borrow();
        if let Some(r) = gens.lowered_rets.get(&hidden) {
            return Ok((hidden, *r));
        }
    }
    let mut env = Env::new();
    for (n, t) in names.iter().zip(grounds.iter()) {
        env.insert(n.clone(), *t);
    }
    // Destructuring specializers put their binders in scope for the body.
    let mut child = env.clone();
    for (pi, variant, binders) in &destructure {
        let Ty::Adt(a) = grounds[*pi] else {
            unreachable!("destructure on non-ADT")
        };
        for (b, t) in binders
            .iter()
            .zip(ctx.adt_def(a).variants[*variant as usize].1.iter())
        {
            child.insert(b.clone(), *t);
        }
    }
    let mut body = lower(&body_expr, &child, ctx)?;
    let ret = ty_of(&body, &child, ctx);
    // Wrap destructured params in single-arm matches (the dispatch tree
    // only calls this method when the tag matches, so the arm is exact).
    for (pi, variant, binders) in destructure.iter().rev() {
        body = HExpr::Match {
            scrut: Box::new(HExpr::Var(names[*pi].clone())),
            arms: vec![HArm {
                variant: Some(*variant),
                binders: binders.clone(),
                body: body.clone(),
                exit_free: None,
            }],
            ty: ret,
        };
    }
    if let Some(d) = declared {
        if d != ret {
            return Err(format!(
                "method {} of {} returns {:?} but declares {:?}",
                hidden, name, ret, d
            ));
        }
    }
    ctx.gens.borrow_mut().extra.push(Defn {
        name: hidden.clone(),
        params: names.iter().cloned().zip(grounds).collect(),
        ret,
        body,
        borrows: vec![Borrow::Owned; names.len()],
        is_extern: false,
        ret_borrowed: false,
        ret_owned: false,
    });
    ctx.gens
        .borrow_mut()
        .lowered_rets
        .insert(hidden.clone(), ret);
    Ok((hidden, ret))
}

/// Symbol-safe cache key for a ground signature.
fn grounds_key(grounds: &[Ty], ctx: &Ctx) -> String {
    grounds.iter().map(|t| ty_key(*t, ctx)).collect::<Vec<_>>().join("_")
}
