//! Lossless lexer contract: every byte of input becomes some token's
//! text (concat identity), trivia is retained, and errors carry offsets.

use cato_fmt::lexer::{TokKind, lex};

fn fixtures() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    for dir in ["examples", "selfhost/cato/reader/fixtures"] {
        let d = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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
fn concat_of_token_texts_equals_input() {
    for path in fixtures() {
        let src = std::fs::read_to_string(&path).expect("read fixture");
        let tokens =
            lex(&src).unwrap_or_else(|e| panic!("lex failed for {}: {:?}", path.display(), e));
        let joined: String = tokens.iter().map(|t| t.text.clone()).collect();
        assert_eq!(
            joined,
            src,
            "lossless identity failed for {}",
            path.display()
        );
    }
}

#[test]
fn comment_is_one_token_to_end_of_line() {
    let tokens = lex("; hi\n(x)").unwrap();
    let kinds: Vec<_> = tokens.iter().map(|t| t.kind).collect();
    assert_eq!(
        kinds,
        vec![
            TokKind::Comment,
            TokKind::Ws,
            TokKind::Open,
            TokKind::Atom,
            TokKind::Close,
        ],
        "tokens: {:?}",
        tokens
    );
    assert_eq!(tokens[0].text, "; hi");
    assert_eq!(tokens[1].text, "\n");
    assert_eq!(
        tokens.iter().map(|t| t.text.clone()).collect::<String>(),
        "; hi\n(x)"
    );
}

#[test]
fn escaped_quote_does_not_end_string() {
    let tokens = lex(r#""\"x\"""#).unwrap();
    assert_eq!(tokens.len(), 1, "tokens: {:?}", tokens);
    assert_eq!(tokens[0].kind, TokKind::Str);
    assert_eq!(tokens[0].text, r#""\"x\"""#);
}

#[test]
fn unterminated_string_errors_with_offset() {
    let src = "abc \"def";
    let err = lex(src).expect_err("must error");
    assert_eq!(err.offset, 4, "offset of opening quote");
    assert!(err.msg.contains("unterminated string"), "msg: {}", err.msg);
}

#[test]
fn set_literal_opens_as_one_token() {
    let tokens = lex("#{1}").unwrap();
    assert_eq!(tokens[0].kind, TokKind::Open);
    assert_eq!(tokens[0].text, "#{");
    let kinds: Vec<_> = tokens.iter().map(|t| t.kind).collect();
    assert_eq!(kinds, vec![TokKind::Open, TokKind::Atom, TokKind::Close]);
}

#[test]
fn empty_input_is_empty_token_list() {
    assert!(lex("").unwrap().is_empty());
}
