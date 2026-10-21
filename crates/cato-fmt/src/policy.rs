//! Style policy: walks the CST into `Doc` per Cato's surface forms.
//! Pinned decisions (plan 2026-10-18-seed-cst-formatter Task 4):
//!
//! - Top-level forms: gap of 1 newline → adjacent; gap of ≥2 → exactly
//!   one blank line (the gap collapses, never grows).
//! - Inside delimited forms: elements separated by `Soft`; a line
//!   comment forces a hard break (a comment ends the line it sits on).
//! - Head-position atoms (`defn`, `let`, `match`, …): parameter/binding
//!   vectors stay in the head group; the first body element shares the
//!   head line when it fits, subsequent body elements start new lines
//!   at `nest(2)`.
//! - `let` bindings: one binding per line when the group does not fit;
//!   each binding is its own group.
//! - Other vectors/maps/sets: break after the open delimiter, one
//!   element per line at `nest(2)`, close on its own line at the outer
//!   indent.
//! - Empty forms print as-is; every other node is one `Group`.

use crate::cst::{Elem, Node, NodeKind, Top, TriviaKind};
use crate::doc::{self, Doc};

/// Atoms that get head-form treatment (plan list, no extras).
const HEADS: &[&str] = &[
    "defn",
    "deftype",
    "extern",
    "defgeneric",
    "defmethod",
    "let",
    "match",
    "fn",
    "do",
    "if",
    "cond",
    "when",
    "for",
    "->",
    "->>",
];

/// Heads whose `:- type` annotation after the parameter vector belongs
/// to the signature (stays on the head line).
const SIGNATURE_HEADS: &[&str] = &["defn", "defmethod", "defgeneric", "extern"];

/// Top-level elements → one `Doc`. Separator state machine: `line_dirty`
/// tracks content on the current output line, `newlines` counts source
/// newlines since the last content.
pub(crate) fn tops_to_doc(tops: &[Top]) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    let mut line_dirty = false;
    let mut emitted_any = false;
    let mut newlines = 0usize;
    for top in tops {
        match top {
            Top::Trivia(tr) => match tr.kind {
                TriviaKind::Ws => {
                    newlines += tr.text.matches('\n').count();
                }
                TriviaKind::Comment => {
                    push_top_sep(&mut parts, line_dirty, emitted_any, newlines);
                    parts.push(doc::text(tr.text.clone()));
                    line_dirty = true;
                    emitted_any = true;
                    newlines = 0;
                }
            },
            Top::Node(n) => {
                push_top_sep(&mut parts, line_dirty, emitted_any, newlines);
                parts.push(node_to_doc(n));
                line_dirty = true;
                emitted_any = true;
                newlines = 0;
            }
        }
    }
    doc::concat(parts)
}

fn push_top_sep(parts: &mut Vec<Doc>, line_dirty: bool, emitted_any: bool, newlines: usize) {
    if !emitted_any {
        return;
    }
    if newlines >= 1 {
        parts.push(Doc::Line);
        if newlines >= 2 {
            parts.push(Doc::Line);
        }
    } else if line_dirty {
        parts.push(doc::text(" "));
    }
}

/// One element position inside a delimited form: the element node, any
/// comments that led it on their own lines, and any same-line trailing
/// comment.
struct Item {
    node: Option<Node>,
    own_comments: Vec<String>,
    trailing_comment: Option<String>,
}

fn collect_items(kids: &[Elem]) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    let mut mid_line = false;
    for kid in kids {
        match kid {
            Elem::Node(nd) => {
                items.push(Item {
                    node: Some(nd.clone()),
                    own_comments: std::mem::take(&mut pending),
                    trailing_comment: None,
                });
                mid_line = true;
            }
            Elem::Token(_) => {}
            Elem::Trivia(tr) => match tr.kind {
                TriviaKind::Ws => {
                    if tr.text.contains('\n') {
                        mid_line = false;
                    }
                }
                TriviaKind::Comment => {
                    if mid_line {
                        if let Some(last) = items.last_mut() {
                            last.trailing_comment = Some(tr.text.clone());
                        } else {
                            pending.push(tr.text.clone());
                        }
                        mid_line = false;
                    } else {
                        pending.push(tr.text.clone());
                    }
                }
            },
        }
    }
    if !pending.is_empty() {
        items.push(Item {
            node: None,
            own_comments: pending,
            trailing_comment: None,
        });
    }
    items
}

fn item_doc(item: &Item) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    for (i, c) in item.own_comments.iter().enumerate() {
        parts.push(doc::text(c.clone()));
        // A comment-only item (no node) must not end its own line: its
        // line-end is `final_sep`, emitted outside the nest so a close
        // delimiter after trailing comments aligns at the outer indent.
        if item.node.is_some() || i + 1 < item.own_comments.len() {
            parts.push(Doc::Line);
        }
    }
    if let Some(n) = &item.node {
        parts.push(node_to_doc(n));
    }
    if let Some(c) = &item.trailing_comment {
        parts.push(doc::text(" "));
        parts.push(doc::text(c.clone()));
    }
    doc::concat(parts)
}

fn sep_between(prev: &Item, next: &Item) -> Doc {
    if prev.trailing_comment.is_some() || !next.own_comments.is_empty() {
        Doc::Line
    } else {
        Doc::Soft
    }
}

/// Separator before the close delimiter: a trailing comment or a
/// comment-only tail ends the line at the *outer* indent (so the close
/// delimiter never lands indented inside the form); otherwise a
/// (possibly breaking) hug.
fn final_sep(items: &[Item]) -> Doc {
    match items.last() {
        Some(l) if l.node.is_none() || l.trailing_comment.is_some() => Doc::Line,
        Some(_) => Doc::Softline,
        None => Doc::Nil,
    }
}

fn is_empty_form(n: &Node) -> bool {
    !n.kids.iter().any(|k| match k {
        Elem::Node(_) => true,
        Elem::Token(_) => true,
        Elem::Trivia(t) => t.kind == TriviaKind::Comment,
    })
}

fn open_close_text(n: &Node) -> Doc {
    match &n.close {
        Some(c) => doc::text(format!("{}{}", n.open.text, c.text)),
        None => doc::text(n.open.text.clone()),
    }
}

fn node_to_doc(n: &Node) -> Doc {
    node_to_doc_styled(n, false)
}

fn node_to_doc_styled(n: &Node, hug: bool) -> Doc {
    match n.kind {
        NodeKind::Atom | NodeKind::Str => doc::text(n.open.text.clone()),
        NodeKind::List => list_to_doc(n),
        NodeKind::Vector | NodeKind::Map | NodeKind::Set => {
            if is_empty_form(n) {
                open_close_text(n)
            } else if hug {
                params_hug_doc(n)
            } else {
                delimited_to_doc(n)
            }
        }
    }
}

/// Break-after-open style: `Softline` after the open, `Soft` between
/// elements, close on its own line at the outer indent when broken.
fn delimited_to_doc(n: &Node) -> Doc {
    let items = collect_items(&n.kids);
    let close = n.close.as_ref().map(|c| c.text.clone()).unwrap_or_default();
    let mut inner = vec![Doc::Softline];
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            inner.push(sep_between(&items[i - 1], item));
        }
        inner.push(item_doc(item));
    }
    doc::group(doc::concat(vec![
        doc::text(n.open.text.clone()),
        doc::nest(2, doc::concat(inner)),
        final_sep(&items),
        doc::text(close),
    ]))
}

/// Head-position parameter vector: each `name [:- Type]` chunk is its
/// own group, so an annotated argument never breaks between the name,
/// `:-`, and the type; chunks separate on break. Falls back to plain
/// hug style when comments inside the vector make chunking ambiguous.
fn params_hug_doc(n: &Node) -> Doc {
    let items = collect_items(&n.kids);
    if items
        .iter()
        .any(|i| !i.own_comments.is_empty() || i.trailing_comment.is_some() || i.node.is_none())
    {
        return hug_delimited(n);
    }
    let elems: Vec<&Node> = items.iter().filter_map(|i| i.node.as_ref()).collect();
    let mut chunks: Vec<Doc> = Vec::new();
    let mut i = 0;
    while i < elems.len() {
        let start = i;
        i += 1;
        if i < elems.len() && is_atom_text(Some(elems[i]), ":-") && i + 1 < elems.len() {
            i += 2;
        }
        let parts: Vec<Doc> = elems[start..i].iter().map(|e| node_to_doc(e)).collect();
        let mut sep = Vec::new();
        for (j, p) in parts.into_iter().enumerate() {
            if j > 0 {
                sep.push(Doc::Soft);
            }
            sep.push(p);
        }
        chunks.push(doc::group(doc::concat(sep)));
    }
    let mut inner: Vec<Doc> = Vec::new();
    for (i, b) in chunks.into_iter().enumerate() {
        if i > 0 {
            inner.push(Doc::Soft);
        }
        inner.push(b);
    }
    doc::group(doc::concat(vec![
        doc::text(n.open.text.clone()),
        doc::nest(2, doc::concat(inner)),
        final_sep(&items),
        doc::text(n.close.as_ref().map(|c| c.text.clone()).unwrap_or_default()),
    ]))
}

/// Hug style (head-position vectors): first element stays on the open
/// delimiter's line when broken; close hugs the last element.
fn hug_delimited(n: &Node) -> Doc {
    let items = collect_items(&n.kids);
    let close = n.close.as_ref().map(|c| c.text.clone()).unwrap_or_default();
    let mut inner: Vec<Doc> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            inner.push(sep_between(&items[i - 1], item));
        }
        inner.push(item_doc(item));
    }
    doc::group(doc::concat(vec![
        doc::text(n.open.text.clone()),
        doc::nest(2, doc::concat(inner)),
        final_sep(&items),
        doc::text(close),
    ]))
}

/// `let` binding vector: bindings (each `name`, optional `:- type`,
/// value) form groups separated by `Soft`; one binding per line when the
/// vector does not fit. Falls back to hug style when comments make the
/// segmentation ambiguous.
fn let_bindings_doc(n: &Node) -> Doc {
    let items = collect_items(&n.kids);
    let has_comments = items
        .iter()
        .any(|i| !i.own_comments.is_empty() || i.trailing_comment.is_some() || i.node.is_none());
    if has_comments {
        return hug_delimited(n);
    }
    let elems: Vec<&Node> = items.iter().filter_map(|i| i.node.as_ref()).collect();
    let mut bindings: Vec<Doc> = Vec::new();
    let mut i = 0;
    while i < elems.len() {
        let start = i;
        i += 1;
        if i < elems.len() && is_atom_text(Some(elems[i]), ":-") && i + 1 < elems.len() {
            i += 2;
        }
        if i < elems.len() {
            i += 1;
        }
        let parts: Vec<Doc> = elems[start..i].iter().map(|e| node_to_doc(e)).collect();
        let mut sep = Vec::new();
        for (j, p) in parts.into_iter().enumerate() {
            if j > 0 {
                sep.push(Doc::Soft);
            }
            sep.push(p);
        }
        bindings.push(doc::group(doc::concat(sep)));
    }
    let mut inner: Vec<Doc> = Vec::new();
    for (i, b) in bindings.into_iter().enumerate() {
        if i > 0 {
            inner.push(Doc::Soft);
        }
        inner.push(b);
    }
    doc::group(doc::concat(vec![
        doc::text(n.open.text.clone()),
        doc::nest(2, doc::concat(inner)),
        doc::text(n.close.as_ref().map(|c| c.text.clone()).unwrap_or_default()),
    ]))
}

fn is_atom_text(n: Option<&Node>, s: &str) -> bool {
    matches!(n, Some(nd) if nd.kind == NodeKind::Atom && nd.open.text == s)
}

fn list_to_doc(n: &Node) -> Doc {
    if is_empty_form(n) {
        return open_close_text(n);
    }
    let items = collect_items(&n.kids);
    if let Some(head_node) = items[0].node.as_ref() {
        if head_node.kind == NodeKind::Atom
            && items[0].own_comments.is_empty()
            && items[0].trailing_comment.is_none()
            && HEADS.contains(&head_node.open.text.as_str())
        {
            return head_form_doc(&items, &head_node.open.text.clone());
        }
    }
    delimited_to_doc(n)
}

fn head_form_doc(items: &[Item], head: &str) -> Doc {
    let mut parts = vec![doc::text("("), doc::text(head.to_string())];
    let mut idx = 1;
    let mut comment_break = false;

    // Optional name atom.
    if let Some(it) = items.get(idx) {
        let clean = it.own_comments.is_empty() && it.trailing_comment.is_none();
        if clean && matches!(it.node.as_ref().map(|d| d.kind), Some(NodeKind::Atom)) {
            parts.push(doc::text(" "));
            parts.push(node_to_doc(it.node.as_ref().unwrap()));
            idx += 1;
        }
    }
    // Optional parameter/binding vector (its trailing comment ends the
    // head line, so body placement switches to a hard break).
    if let Some(it) = items.get(idx) {
        let clean = it.own_comments.is_empty();
        if clean && matches!(it.node.as_ref().map(|d| d.kind), Some(NodeKind::Vector)) {
            let nd = it.node.as_ref().unwrap();
            let vd = if head == "let" {
                let_bindings_doc(nd)
            } else {
                node_to_doc_styled(nd, true)
            };
            parts.push(doc::text(" "));
            parts.push(vd);
            idx += 1;
            if let Some(c) = &it.trailing_comment {
                parts.push(doc::text(" "));
                parts.push(doc::text(c.clone()));
                comment_break = true;
            } else if SIGNATURE_HEADS.contains(&head) {
                // `:- type` annotation belongs to the signature.
                let ann_clean = items
                    .get(idx)
                    .zip(items.get(idx + 1))
                    .map(|(a, t)| {
                        a.own_comments.is_empty()
                            && a.trailing_comment.is_none()
                            && t.own_comments.is_empty()
                            && t.trailing_comment.is_none()
                            && is_atom_text(a.node.as_ref(), ":-")
                            && t.node.is_some()
                    })
                    .unwrap_or(false);
                if ann_clean {
                    let t = items[idx + 1].node.as_ref().unwrap();
                    parts.push(doc::text(" "));
                    parts.push(doc::text(":-"));
                    parts.push(doc::text(" "));
                    parts.push(node_to_doc(t));
                    idx += 2;
                }
            }
        }
    }

    let body = &items[idx..];
    if !body.is_empty() {
        let first_sep = if comment_break { Doc::Line } else { Doc::Soft };
        let mut body_docs: Vec<Doc> = Vec::new();
        for (i, it) in body.iter().enumerate() {
            if i > 0 {
                body_docs.push(Doc::Line);
            }
            body_docs.push(item_doc(it));
        }
        parts.push(doc::nest(
            2,
            doc::concat(vec![first_sep, doc::concat(body_docs)]),
        ));
        // A trailing or comment-only last body item ends its line at the
        // outer indent, so `)` is never swallowed by the comment or
        // pushed into the body's nest.
        let tail = body.last().unwrap();
        if tail.node.is_none() || tail.trailing_comment.is_some() {
            parts.push(Doc::Line);
        }
    } else if comment_break {
        parts.push(Doc::Line);
    }
    parts.push(doc::text(")"));
    doc::group(doc::concat(parts))
}
