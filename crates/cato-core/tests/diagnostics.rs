//! The errors/ suite: every negative program must produce the expected
//! diagnostic code, message, and `file:line:col` location with a caret.

use cato_core::compile_named;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/errors");

/// (file, expected code, expected 1-based line of the offending form,
///  message substring)
const CASES: &[(&str, &str, usize, &str)] = &[
    ("use_moved.cato", "E301", 3, "use of moved value xs"),
    (
        "exclusive_sigil.cato",
        "E202",
        3,
        "unsupported borrow sigil `&!`",
    ),
    ("nonexhaustive_match.cato", "E401", 5, "does not cover: Circle"),
    ("bad_arity.cato", "E201", 2, "expects 2 operands"),
    ("bad_type.cato", "E202", 2, "conj source must be a vector"),
    (
        "generics_ambiguous.cato",
        "E203",
        8,
        "neither is more specific",
    ),
    (
        "generics_uncovered.cato",
        "E204",
        7,
        "no method hit for this variant combination",
    ),
    (
        "generics_uncovered_vector.cato",
        "E204",
        5,
        "no method size for ((Vector str))",
    ),
    (
        "generics_unreachable.cato",
        "E205",
        8,
        "can never be selected",
    ),
    ("unbound_variable.cato", "E206", 2, "unbound variable undefined-var"),
    ("unknown_function.cato", "E207", 2, "unknown function mystery-fn"),
    (
        "unknown_type_name.cato",
        "E208",
        3,
        "unknown type name il64",
    ),
    (
        "move_borrowed.cato",
        "E303",
        8,
        "cannot move b: it is a borrowed parameter",
    ),
];

#[test]
fn errors_suite_reports_code_location_and_caret() {
    for (file, code, line, msg) in CASES {
        let path = format!("{}/{}", DIR, file);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {}", path, e));
        let err = compile_named(file, &src)
            .err()
            .unwrap_or_else(|| panic!("{} should fail to compile", file));
        assert!(err.contains(code), "{}: expected code {} in:\n{}", file, code, err);
        assert!(err.contains(msg), "{}: expected '{}' in:\n{}", file, msg, err);
        assert!(
            err.contains(&format!("--> {}:{}", file, line)),
            "{}: expected location {}:{} in:\n{}",
            file,
            file,
            line,
            err
        );
        // Caret snippet: the source line echoed under a location line.
        assert!(err.contains("| ^"), "{}: expected caret snippet in:\n{}", file, err);
    }
}

#[test]
fn reader_errors_carry_exact_spans() {
    let err = compile_named("t.cato", "(+ 1\n(2 3").err().unwrap();
    assert!(err.contains("E004"), "got: {}", err);
    assert!(err.contains("--> t.cato:1:1"), "got: {}", err);

    let err = compile_named("t.cato", "(defn main [] \"abc").err().unwrap();
    assert!(err.contains("E002"), "got: {}", err);
    assert!(err.contains("--> t.cato:1:15"), "got: {}", err);

    let err = compile_named("t.cato", "{1 2 3}").err().unwrap();
    assert!(err.contains("E003"), "got: {}", err);
    assert!(err.contains("--> t.cato:1:1"), "got: {}", err);
}

#[test]
fn explain_docs_exist_for_all_codes() {
    for code in cato_core::diagnostics::all_codes() {
        let doc = cato_core::diagnostics::explain(code).expect("doc");
        assert!(doc.len() > 20, "{}: doc too short", code);
    }
    assert!(cato_core::diagnostics::explain("E999").is_none());
}

#[test]
fn classify_maps_pipeline_messages_to_codes() {
    let f = cato_core::diagnostics::classify;
    assert_eq!(f("use of moved value xs"), "E301");
    assert_eq!(f("match on Shape does not cover: Circle (add arms or a _ wildcard)"), "E401");
    assert_eq!(f("binary op + expects 2 operands"), "E201");
    assert_eq!(f("conj source must be a vector, got I64"), "E202");
    assert_eq!(f("unbalanced delimiter ')'"), "E001");
    assert_eq!(f("unterminated string"), "E002");
    assert_eq!(f("map literal must have key/value pairs"), "E003");
    assert_eq!(f("unclosed delimiter"), "E004");
    assert_eq!(f("unbound variable undefined-var"), "E206");
    assert_eq!(f("unknown function mystery-fn"), "E207");
    assert_eq!(f("unknown type name il64"), "E208");
    assert_eq!(
        f("cannot move b: it is a borrowed parameter and still belongs to the caller"),
        "E303"
    );
    assert_eq!(
        f("cannot move the borrowed result of get-l into an owned parameter of take: it belongs to the caller"),
        "E303"
    );
    assert_eq!(
        f("Borrowed return requires a borrowed value. A `&T` function may only return a value derived from its `&` parameters, never an owned or freshly-cloned value."),
        "E304"
    );
    assert_eq!(
        f("borrowed return: `&T` requires a boxed (recursive or owning) ADT return type"),
        "E304"
    );
    assert_eq!(f("mystery failure"), "E000");
}
