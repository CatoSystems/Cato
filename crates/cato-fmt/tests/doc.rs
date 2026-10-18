//! Wadler Doc engine contract: group fitting/breaking, nesting, hard
//! newlines, and exact width regressions.

use cato_fmt::doc::{concat, group, nest, print, text, Doc};

#[test]
fn group_fits_flat() {
    let d = group(concat(vec![text("foo"), Doc::Soft, text("bar")]));
    assert_eq!(print(&d, 10), "foo bar");
}

#[test]
fn group_breaks_when_too_wide() {
    let d = group(concat(vec![text("foo"), Doc::Soft, text("bar")]));
    assert_eq!(print(&d, 5), "foo\nbar");
}

#[test]
fn nest_indents_broken_lines() {
    // nest(2, group("aa" Soft Line)) at width 1: group breaks, the
    // newline indents by 2; trailing indent is stripped from output.
    let d = nest(2, group(concat(vec![text("aa"), Doc::Soft, Doc::Line])));
    assert_eq!(print(&d, 1), "aa");
}

#[test]
fn nest_indent_appears_before_next_content() {
    // The newline that leads into the nest content must itself live
    // inside the nest (Wadler doesn't retroindent).
    let d = concat(vec![
        text("root"),
        nest(2, concat(vec![Doc::Line, text("child"), Doc::Line, text("next")])),
    ]);
    assert_eq!(print(&d, 40), "root\n  child\n  next");
}

#[test]
fn hard_line_forces_break() {
    let d = concat(vec![text("a"), Doc::Line, text("b")]);
    assert_eq!(print(&d, 100), "a\nb");
    assert_eq!(print(&d, 1), "a\nb");
}

#[test]
fn width_40_regression() {
    // Flat width is 31: fits at 40, breaks one-item-per-line at 20.
    // nest(1) aligns items after the '(' when broken.
    let items = || {
        let names = ["item1", "item2", "item3", "item4", "item5"];
        let mut parts = Vec::new();
        for (i, n) in names.iter().enumerate() {
            if i > 0 {
                parts.push(Doc::Soft);
            }
            parts.push(text(*n));
        }
        concat(parts)
    };
    let d = group(concat(vec![text("("), nest(1, items()), text(")")]));
    assert_eq!(print(&d, 40), "(item1 item2 item3 item4 item5)");
    assert_eq!(
        print(&d, 20),
        "(item1\n item2\n item3\n item4\n item5)"
    );
}

#[test]
fn empty_doc_prints_empty() {
    assert_eq!(print(&Doc::Nil, 10), "");
}
