//! Borrowed returns (`&T`) — seed parse and ownership behavior.

use cato_core::compile_to_program;

#[test]
fn borrowed_return_parses() {
    let src = r#"
(deftype N (Leaf) (Node [l :- N r :- N]))
(defn get-l [n :- &N] :- &N (match n (Node [l r] l) (_ n)))
(defn main [] :- i64 0)
"#;
    // Parsing must accept `&T` (no longer "unknown type name"). Whether the
    // ownership checker accepts the borrowed field is Task 2's contract.
    match compile_to_program(src) {
        Ok(_) => {}
        Err(e) => assert!(!e.contains("unknown type name"), "parser must accept &T: {e}"),
    }
}

#[test]
fn borrowed_return_list_form_rejected() {
    // `&(Tree i64)` reads as a standalone `&` + list -> reject with guidance.
    let src = r#"
(deftype (Tree a) (Node [v :- a l :- (Tree a) r :- (Tree a)]) (Leaf))
(defn leaf [t :- (Tree i64)] :- &(Tree i64) (Leaf))
(defn main [] :- i64 0)
"#;
    let e = compile_to_program(src).unwrap_err().to_string();
    assert!(e.contains("borrowed return"), "got: {e}");
}
