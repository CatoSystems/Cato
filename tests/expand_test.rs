use cato::ast::Expr;
use cato::expand::expand_all;
use cato::reader::read_all;

fn pp(e: &Expr) -> String {
    match e {
        Expr::LitI64(v) => v.to_string(),
        Expr::LitF64(v) => format!("{}", v),
        Expr::LitStr(s) => format!("{:?}", s),
        Expr::LitSym(s) => s.clone(),
        Expr::List(items) | Expr::Vector(items) => {
            let inner: Vec<String> = items.iter().map(pp).collect();
            let (o, c) = if matches!(e, Expr::List(_)) { ("(", ")") } else { ("[", "]") };
            format!("{}{}{}", o, inner.join(" "), c)
        }
    }
}

fn expand_one(src: &str) -> Expr {
    let mut v = expand_all(&read_all(src).unwrap()).unwrap();
    assert_eq!(v.len(), 1);
    v.pop().unwrap()
}

fn list(items: Vec<Expr>) -> Expr {
    Expr::List(items)
}
fn sym(s: &str) -> Expr {
    Expr::LitSym(s.to_string())
}

#[test]
fn threads_first_position() {
    assert_eq!(
        expand_one("(-> x (f a))"),
        list(vec![sym("f"), sym("x"), sym("a")])
    );
}

#[test]
fn threads_bare_symbol_step() {
    assert_eq!(expand_one("(-> x f)"), list(vec![sym("f"), sym("x")]));
}

#[test]
fn threads_zero_arg_step() {
    assert_eq!(expand_one("(-> x (f))"), list(vec![sym("f"), sym("x")]));
}

#[test]
fn threads_last_position() {
    assert_eq!(
        expand_one("(->> xs (map f))"),
        list(vec![sym("map"), sym("f"), sym("xs")])
    );
}

#[test]
fn threads_last_position_with_args() {
    assert_eq!(
        expand_one("(->> xs (f a))"),
        list(vec![sym("f"), sym("a"), sym("xs")])
    );
}

#[test]
fn expands_normalize_chain() {
    // (->> xs (map g) (map h) (reduce r 0.0))
    //   -> (reduce r 0.0 (map h (map g xs)))
    assert_eq!(
        expand_one("(->> xs (map g) (map h) (reduce r 0.0))"),
        list(vec![
            sym("reduce"),
            sym("r"),
            Expr::LitF64(0.0),
            list(vec![
                sym("map"),
                sym("h"),
                list(vec![sym("map"), sym("g"), sym("xs")]),
            ]),
        ])
    );
}

#[test]
fn expands_inside_defn_body() {
    let v = expand_all(&read_all("(defn f [xs :- vec] (->> xs (map g)))").unwrap()).unwrap();
    assert_eq!(v.len(), 1);
}

#[test]
fn rejects_empty_thread() {
    assert!(expand_all(&read_all("(->)").unwrap()).is_err());
    assert!(expand_all(&read_all("(->>)").unwrap()).is_err());
}
// --- Phase 5: branching/binding macros ---

#[test]
fn expands_when_to_if_do() {
    let e = expand_one("(when c 1 2)");
    assert_eq!(pp(&e), "(if c (do 1 2) 0)");
}

#[test]
fn expands_cond_to_nested_ifs() {
    let e = expand_one("(cond a 1 b 2 9)");
    assert_eq!(pp(&e), "(if a 1 (if b 2 9))");
    let e = expand_one("(cond a 1)");
    assert_eq!(pp(&e), "(if a 1 0)");
}

#[test]
fn expands_if_let_around_condition_value() {
    let e = expand_one("(if-let [x e] 1 2)");
    assert_eq!(pp(&e), "(let [x e] (if x 1 2))");
    let e = expand_one("(when-let [x e] 1 2)");
    assert_eq!(pp(&e), "(let [x e] (if x (do 1 2) 0))");
}

#[test]
fn expands_doseq_to_reduce() {
    let e = expand_one("(doseq [x xs] (print-i64 x))");
    assert_eq!(
        pp(&e),
        "(reduce (fn [_acc x] (do (print-i64 x) 0)) 0 xs)"
    );
}

#[test]
fn expands_variadic_and_or() {
    let e = expand_one("(and a b c)");
    assert_eq!(pp(&e), "(and a (and b c))");
    let e = expand_one("(or a b)");
    assert_eq!(pp(&e), "(or a b)");
    let e = expand_one("(or x)");
    assert_eq!(pp(&e), "x");
}

#[test]
fn expands_macros_inside_binding_vectors() {
    let e = expand_one("(let [x (-> a f)] x)");
    assert_eq!(pp(&e), "(let [x (f a)] x)");
}

#[test]
fn expands_macros_inside_deftype_variants_are_untouched() {
    let out = expand_all(&read_all(
        "(deftype S (Mk [x :- i64]))\n(defn f [x :- i64] (when (> x 1) 1))",
    )
    .unwrap())
    .unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(pp(&out[0]), "(deftype S (Mk [x :- i64]))");
    assert_eq!(pp(&out[1]), "(defn f [x :- i64] (if (> x 1) (do 1) 0))");
}
