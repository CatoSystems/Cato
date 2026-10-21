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
        Err(e) => assert!(
            !e.contains("unknown type name"),
            "parser must accept &T: {e}"
        ),
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

#[test]
fn borrowed_return_body_accepts_borrowed_field() {
    // The positive case: an aliasing match binder (a field of a borrowed
    // scrutinee) is exactly what a `&T` return hands back. Must compile.
    let src = r#"
(deftype N (Leaf) (Node [l :- N r :- N]))
(defn get-l [n :- &N] :- &N (match n (Node [l r] l) (_ n)))
(defn main [] :- i64 0)
"#;
    compile_to_program(src).expect("borrowed field return must be accepted");
}

#[test]
fn borrowed_return_body_rejects_owned_tail() {
    let src = r#"
(deftype N (Leaf) (Node [l :- N r :- N]))
(defn bad [n :- &N] :- &N (Node n n))
(defn main [] :- i64 0)
"#;
    let e = compile_to_program(src).unwrap_err().to_string();
    assert!(e.contains("E304"), "got: {e}");
}

#[test]
fn borrowed_return_non_boxed_rejected() {
    let i64_case = "(defn f [x :- &i64] :- &i64 x)\n(defn main [] :- i64 0)\n";
    let adt_case =
        "(deftype Box (B [v :- i64]))\n(defn f [x :- &Box] :- &Box x)\n(defn main [] :- i64 0)\n";
    for src in [i64_case, adt_case] {
        let e = compile_to_program(src).unwrap_err().to_string();
        assert!(e.contains("borrowed return"), "got: {e}");
    }
}

#[test]
fn borrowed_return_must_derive_from_borrow_param() {
    // A `&T` call handed an *owned* parameter returns a borrow into a value
    // this function frees at exit — it must not escape (E304). The borrow's
    // origin has to be one of the function's own `&` parameters.
    let src = r#"
(deftype N (Leaf) (Node [l :- N r :- N]))
(defn get-l [n :- &N] :- &N (match n (Node [l r] l) (_ n)))
(defn g [n :- N] :- &N (get-l n))
(defn main [] :- i64 0)
"#;
    let e = compile_to_program(src).unwrap_err().to_string();
    assert!(e.contains("E304"), "got: {e}");
}

#[test]
fn borrowed_result_to_owned_param_is_e303() {
    // `take` keeps (stores) its parameter; one that only read it would be
    // inferred lent, and lending a borrowed result is fine.
    let src = r#"
(deftype N (Leaf) (Node [l :- N r :- N]))
(defn get-l [n :- &N] :- &N (match n (Node [l r] l) (_ n)))
(deftype Hd (Hd [n :- N]))
(defn take [n :- N] :- Hd (Hd n))
(defn main [] :- i64 (match (take (get-l (Node (Leaf) (Leaf)))) (Hd [x] 0)))
"#;
    let e = compile_to_program(src).unwrap_err().to_string();
    assert!(e.contains("E303"), "got: {e}");
}

#[test]
fn borrowed_return_survives_specialization() {
    // `f` is generic (type param `A`) but returns a concrete boxed-ADT
    // borrow. Instantiating `f` for `i64` must keep the borrowed-return
    // marking: otherwise `g`'s tail `(f x 3)` reads as an owned call and
    // `g` is wrongly rejected with E304.
    let src = r#"
(deftype H (H1 [v :- i64 next :- H]) (H2))
(defn f [x :- &H y :- A] :- &H x)
(defn g [x :- &H] :- &H (f x 3))
(defn main [] :- i64 0)
"#;
    compile_to_program(src).expect("borrowed return must survive monomorphization");
}
