use cato::ast::Expr;
use cato::expand::expand_all;
use cato::reader::read_all;

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