use cato_core::ast::Expr;
use cato_core::reader::read_all;

fn expr_eq(input: &str, expected: Expr) {
    let mut v = read_all(input).unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!(v.pop(), Some(expected));
}

#[test]
fn reads_atoms() {
    expr_eq("42", Expr::LitI64(42));
    expr_eq("-7", Expr::LitI64(-7));
    expr_eq("3.5", Expr::LitF64(3.5));
    expr_eq("\"hi\"", Expr::LitStr("hi".to_string()));
    expr_eq("foo", Expr::LitSym("foo".to_string()));
}

#[test]
fn reads_sym_special() {
    expr_eq("even?", Expr::LitSym("even?".to_string()));
    expr_eq("+", Expr::LitSym("+".to_string()));
}

#[test]
fn reads_nested() {
    expr_eq("(defn f [x] (+ x 1))", Expr::List(vec![
        Expr::LitSym("defn".to_string()),
        Expr::LitSym("f".to_string()),
        Expr::Vector(vec![Expr::LitSym("x".to_string())]),
        Expr::List(vec![
            Expr::LitSym("+".to_string()),
            Expr::LitSym("x".to_string()),
            Expr::LitI64(1),
        ]),
    ]));
}

#[test]
fn skips_comments_and_whitespace() {
    let v = read_all("; a comment\n(+ 1 2) ; trailing\n").unwrap();
    assert_eq!(v.len(), 1);
}

#[test]
fn rejects_unbalanced() {
    assert!(read_all("(1 2").is_err());
    assert!(read_all(")").is_err());
}

#[test]
fn reads_multiple_top_level() {
    let v = read_all("(a) (b)").unwrap();
    assert_eq!(v.len(), 2);
}

#[test]
fn reads_set_literals() {
    let v = read_all("#{5 3 3 1}").unwrap();
    match &v[0] {
        Expr::SetLit(items) => {
            assert_eq!(items.len(), 4);
            assert!(matches!(&items[0], Expr::LitI64(5)));
        }
        other => panic!("expected set literal, got {:?}", other),
    }
    let v = read_all("(let [s #{1 2}] s)").unwrap();
    assert!(matches!(&v[0], Expr::List(_)));
}
