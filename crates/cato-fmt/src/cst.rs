//! Trivia-preserving concrete syntax tree: a delimiter-matching recursive
//! descent over the token stream. Whitespace and comments become
//! `Trivia` elements *in position* — never dropped, never reordered.

use crate::FmtError;
use crate::lexer::{TokKind, Token, lex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriviaKind {
    Ws,
    Comment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trivia {
    pub kind: TriviaKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    List,
    Vector,
    Set,
    Map,
    Atom,
    Str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub kind: NodeKind,
    pub open: Token,
    pub close: Option<Token>,
    pub kids: Vec<Elem>,
    /// Sum of every leaf text length in this subtree (invariant checks).
    pub text_len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Elem {
    Node(Node),
    Token(Token),
    Trivia(Trivia),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Top {
    Node(Node),
    Trivia(Trivia),
}

/// The closers that terminate each opener (and the reader's kind name
/// for the error message).
fn pair(open: &str) -> Option<(char, NodeKind)> {
    match open {
        "(" => Some((')', NodeKind::List)),
        "[" => Some((']', NodeKind::Vector)),
        "{" => Some(('}', NodeKind::Map)),
        "#{" => Some(('}', NodeKind::Set)),
        _ => None,
    }
}

fn trivia_of(t: &Token) -> Option<Trivia> {
    match t.kind {
        TokKind::Ws => Some(Trivia {
            kind: TriviaKind::Ws,
            text: t.text.clone(),
        }),
        TokKind::Comment => Some(Trivia {
            kind: TriviaKind::Comment,
            text: t.text.clone(),
        }),
        _ => None,
    }
}

struct Frame {
    open: Token,
    open_offset: usize,
    kind: NodeKind,
    expected: char,
    kids: Vec<Elem>,
}

/// Reassemble the exact source text the tree was parsed from (every leaf
/// in document order). The identity `flatten(parse(src)) == src` is the
/// CST's lossless invariant.
pub fn flatten(tops: &[Top]) -> String {
    fn walk_node(n: &Node, out: &mut String) {
        out.push_str(&n.open.text);
        for k in &n.kids {
            match k {
                Elem::Node(c) => walk_node(c, out),
                Elem::Token(t) => out.push_str(&t.text),
                Elem::Trivia(t) => out.push_str(&t.text),
            }
        }
        if let Some(c) = &n.close {
            out.push_str(&c.text);
        }
    }
    let mut out = String::new();
    for t in tops {
        match t {
            Top::Node(n) => walk_node(n, &mut out),
            Top::Trivia(tr) => out.push_str(&tr.text),
        }
    }
    out
}

/// Parse `src` into top-level elements, preserving every byte.
pub fn parse(src: &str) -> Result<Vec<Top>, FmtError> {
    let tokens = lex(src)?;
    let mut tops: Vec<Top> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    // Track byte offset of each token for error positions.
    let mut offset = 0usize;
    for tok in &tokens {
        let tok_offset = offset;
        offset += tok.text.len();
        match tok.kind {
            TokKind::Ws | TokKind::Comment => {
                let tr = trivia_of(tok).expect("trivia kind");
                match stack.last_mut() {
                    Some(f) => f.kids.push(Elem::Trivia(tr)),
                    None => tops.push(Top::Trivia(tr)),
                }
            }
            TokKind::Open => {
                let (expected, kind) =
                    pair(&tok.text).ok_or_else(|| FmtError::new("stray opener", tok_offset))?;
                stack.push(Frame {
                    open: tok.clone(),
                    open_offset: tok_offset,
                    kind,
                    expected,
                    kids: Vec::new(),
                });
            }
            TokKind::Close => {
                let f = stack.pop().ok_or_else(|| {
                    FmtError::new(format!("unbalanced delimiter '{}'", tok.text), tok_offset)
                })?;
                if !tok.text.starts_with(f.expected) {
                    return Err(FmtError::new(
                        format!("unbalanced delimiter '{}'", tok.text),
                        tok_offset,
                    ));
                }
                // Map parity, like the reader: trivia doesn't count.
                let values = f.kids.iter().filter(|e| matches!(e, Elem::Node(_))).count();
                if f.kind == NodeKind::Map && values % 2 != 0 {
                    return Err(FmtError::new(
                        "map literal must have key/value pairs",
                        tok_offset,
                    ));
                }
                let kids = f.kids;
                let node = Node {
                    kind: f.kind,
                    open: f.open,
                    close: Some(tok.clone()),
                    kids,
                    text_len: 0,
                };
                let node = Node {
                    text_len: compute_len(&node),
                    ..node
                };
                push_node(node, &mut stack, &mut tops);
            }
            TokKind::Str | TokKind::Atom => {
                let kind = if tok.kind == TokKind::Str {
                    NodeKind::Str
                } else {
                    NodeKind::Atom
                };
                let node = Node {
                    kind,
                    open: tok.clone(),
                    close: None,
                    kids: Vec::new(),
                    text_len: tok.text.len(),
                };
                push_node(node, &mut stack, &mut tops);
            }
        }
    }
    if let Some(f) = stack.first() {
        return Err(FmtError::new("unclosed delimiter", f.open_offset));
    }
    Ok(tops)
}

fn push_node(node: Node, stack: &mut [Frame], tops: &mut Vec<Top>) {
    match stack.last_mut() {
        Some(f) => f.kids.push(Elem::Node(node)),
        None => tops.push(Top::Node(node)),
    }
}

fn elem_len(e: &Elem) -> usize {
    match e {
        Elem::Node(n) => n.text_len,
        Elem::Token(t) => t.text.len(),
        Elem::Trivia(t) => t.text.len(),
    }
}

fn compute_len(n: &Node) -> usize {
    if n.close.is_none() && n.kids.is_empty() {
        return n.open.text.len();
    }
    let kids: usize = n.kids.iter().map(elem_len).sum();
    let close = n.close.as_ref().map(|c| c.text.len()).unwrap_or(0);
    n.open.text.len() + kids + close
}
