//! Trivia-preserving CST contract: lossless flattening, in-position
//! trivia siblings, and reader-parity errors.

use cato_fmt::cst::{flatten, parse, Elem, NodeKind, TriviaKind};
use std::path::PathBuf;

fn fixtures() -> Vec<PathBuf> {
    let mut v = Vec::new();
    for dir in ["examples", "selfhost/cato/reader/fixtures"] {
        let d = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(dir);
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().map(|x| x == "cato").unwrap_or(false) {
                    let name = p.file_name().unwrap().to_string_lossy().to_string();
                    if name.starts_with("err_") {
                        continue;
                    }
                    v.push(p);
                }
            }
        }
    }
    v
}

#[test]
fn tree_text_is_lossless() {
    for path in fixtures() {
        let src = std::fs::read_to_string(&path).expect("read");
        let tops = parse(&src).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
        assert_eq!(
            flatten(&tops),
            src,
            "lossless failed for {}",
            path.display()
        );
    }
}

#[test]
fn comment_between_forms_is_trivia_sibling() {
    let tops = parse("(a)\n; note\n(b)").expect("parse");
    assert_eq!(tops.len(), 5, "tops: {tops:?}");
    // List, Ws, Comment, Ws, List
    assert!(matches!(&tops[0], cato_fmt::cst::Top::Node(n) if n.kind == NodeKind::List));
    match &tops[2] {
        cato_fmt::cst::Top::Trivia(t) => {
            assert_eq!(t.kind, TriviaKind::Comment);
            assert_eq!(t.text, "; note");
        }
        other => panic!("expected trivia, got {other:?}"),
    }
    assert!(matches!(&tops[4], cato_fmt::cst::Top::Node(n) if n.kind == NodeKind::List));
}

#[test]
fn comment_inside_list_trailing_close() {
    let tops = parse("(a b ; note\n)").expect("parse");
    let cato_fmt::cst::Top::Node(list) = &tops[0] else {
        panic!("expected list");
    };
    // kids: a, ws, b, ws, comment, ws — comment is last non-ws elem
    // before the close token.
    let comment_idx = list
        .kids
        .iter()
        .position(|e| matches!(e, Elem::Trivia(t) if t.kind == TriviaKind::Comment))
        .expect("comment kid");
    // Everything after the comment must be trivia (the newline).
    for e in &list.kids[comment_idx + 1..] {
        assert!(
            matches!(e, Elem::Trivia(_)),
            "only trivia after comment: {e:?}"
        );
    }
    assert!(list.close.is_some());
}

#[test]
fn mismatched_delimiter_is_error() {
    let err = parse("(]").expect_err("must error");
    assert!(err.msg.contains("unbalanced delimiter"), "msg: {}", err.msg);
}

#[test]
fn stray_close_is_error() {
    let err = parse(")").expect_err("must error");
    assert!(
        err.msg.contains("unbalanced delimiter") || err.msg.contains("stray"),
        "msg: {}",
        err.msg
    );
}

#[test]
fn unclosed_delimiter_is_error() {
    let err = parse("(a").expect_err("must error");
    assert!(err.msg.contains("unclosed"), "msg: {}", err.msg);
}

#[test]
fn map_with_odd_elements_is_error() {
    let err = parse("{1 2 3}").expect_err("must error");
    assert!(
        err.msg.contains("map literal must have key/value pairs"),
        "msg: {}",
        err.msg
    );
}

#[test]
fn comment_only_and_empty_sources_parse() {
    let tops = parse("; just a comment").expect("parse");
    assert!(flatten(&tops).contains("; just a comment"));
    assert!(parse("").is_ok());
    assert!(parse("\n\n").is_ok());
}
