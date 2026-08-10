/// A Wadler-style pretty-printing IR. `Group` is the unit the layout
/// algorithm's fits-check operates on: everything inside one `Group` is
/// rendered either fully flat (every `Line` becomes a space) or fully
/// broken (every `Line` becomes a newline at the current indent),
/// decided once per `Group` by whether the flat rendering fits in the
/// remaining width on the current line.
#[derive(Debug, Clone)]
pub enum Doc {
    Text(String),
    /// A space if the enclosing `Group` fits; a newline + current indent
    /// otherwise.
    Line,
    /// Always a newline + current indent, regardless of any enclosing
    /// `Group`'s fit decision — used after `;`, between top-level items,
    /// and anywhere a real line break is mandatory rather than a style
    /// choice.
    HardLine,
    Nest(usize, Box<Doc>),
    Concat(Vec<Doc>),
    Group(Box<Doc>),
}

impl Doc {
    pub fn text(s: impl Into<String>) -> Doc {
        Doc::Text(s.into())
    }

    pub fn concat(docs: Vec<Doc>) -> Doc {
        Doc::Concat(docs)
    }

    pub fn nest(indent: usize, doc: Doc) -> Doc {
        Doc::Nest(indent, Box::new(doc))
    }

    pub fn group(doc: Doc) -> Doc {
        Doc::Group(Box::new(doc))
    }
}

/// True if `doc`, rendered with every `Line` as a space (never breaking),
/// fits within `remaining` columns. `HardLine` always fails a flat fit —
/// anything containing a mandatory break can never be flattened.
fn fits(doc: &Doc, remaining: i64) -> bool {
    if remaining < 0 {
        return false;
    }
    match doc {
        Doc::Text(s) => remaining - (s.chars().count() as i64) >= 0,
        Doc::Line => remaining > 0,
        Doc::HardLine => false,
        Doc::Nest(_, d) => fits(d, remaining),
        Doc::Group(d) => fits(d, remaining),
        Doc::Concat(docs) => {
            let mut r = remaining;
            for d in docs {
                if !fits(d, r) {
                    return false;
                }
                r -= flat_width(d);
            }
            true
        }
    }
}

/// Width of `doc` if rendered fully flat (every `Line` as one space).
/// Only ever called on subtrees already confirmed to contain no
/// `HardLine` (via `fits`'s own early return), so it never needs to
/// handle "infinite width" — every `Doc` here has a well-defined flat
/// width.
fn flat_width(doc: &Doc) -> i64 {
    match doc {
        Doc::Text(s) => s.chars().count() as i64,
        Doc::Line => 1,
        Doc::HardLine => 0, // unreachable in practice, see doc comment above
        Doc::Nest(_, d) => flat_width(d),
        Doc::Group(d) => flat_width(d),
        Doc::Concat(docs) => docs.iter().map(flat_width).sum(),
    }
}

/// Renders `doc` at `width` columns.
pub fn render(doc: Doc, width: usize) -> String {
    let mut out = String::new();
    let mut col: i64 = 0;
    render_doc(&doc, 0, false, width as i64, &mut col, &mut out);
    out
}

/// `flat`: true if an enclosing `Group` already decided to render flat —
/// propagates down so nested non-`Group` structure (`Concat`/`Nest`)
/// inherits the decision, and a NESTED `Group` gets its OWN independent
/// fits-check only when not already forced flat by an ancestor.
fn render_doc(doc: &Doc, indent: usize, flat: bool, width: i64, col: &mut i64, out: &mut String) {
    match doc {
        Doc::Text(s) => {
            out.push_str(s);
            *col += s.chars().count() as i64;
        }
        Doc::Line => {
            if flat {
                out.push(' ');
                *col += 1;
            } else {
                out.push('\n');
                out.push_str(&" ".repeat(indent));
                *col = indent as i64;
            }
        }
        Doc::HardLine => {
            out.push('\n');
            out.push_str(&" ".repeat(indent));
            *col = indent as i64;
        }
        Doc::Nest(n, d) => render_doc(d, indent + n, flat, width, col, out),
        Doc::Concat(docs) => {
            for d in docs {
                render_doc(d, indent, flat, width, col, out);
            }
        }
        Doc::Group(d) => {
            let should_flatten = !flat && fits(d, width - *col);
            render_doc(d, indent, flat || should_flatten, width, col, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_renders_verbatim() {
        assert_eq!(render(Doc::Text("hi".to_string()), 100), "hi");
    }

    #[test]
    fn a_group_that_fits_renders_flat_with_lines_as_spaces() {
        let doc = Doc::Group(Box::new(Doc::Concat(vec![
            Doc::Text("a".to_string()),
            Doc::Line,
            Doc::Text("b".to_string()),
        ])));
        assert_eq!(render(doc, 100), "a b");
    }

    #[test]
    fn a_group_that_does_not_fit_breaks_every_line_and_respects_nesting() {
        let doc = Doc::Group(Box::new(Doc::Concat(vec![
            Doc::Text("aaaaaaaaaa".to_string()),
            Doc::Nest(
                4,
                Box::new(Doc::Concat(vec![
                    Doc::Line,
                    Doc::Text("bbbbbbbbbb".to_string()),
                    Doc::Line,
                    Doc::Text("cccccccccc".to_string()),
                ])),
            ),
        ])));
        // width 5 forces a break; each Line becomes a newline + 4-space indent.
        assert_eq!(render(doc, 5), "aaaaaaaaaa\n    bbbbbbbbbb\n    cccccccccc");
    }

    #[test]
    fn hard_line_always_breaks_even_inside_a_fitting_group() {
        let doc = Doc::Group(Box::new(Doc::Concat(vec![
            Doc::Text("a".to_string()),
            Doc::HardLine,
            Doc::Text("b".to_string()),
        ])));
        assert_eq!(render(doc, 100), "a\nb");
    }

    #[test]
    fn nested_groups_fit_independently() {
        // The outer group doesn't fit at width 8, so it breaks — but the
        // inner group ("x y", 3 chars) still fits on its own line and stays
        // flat.
        let inner = Doc::Group(Box::new(Doc::Concat(vec![
            Doc::Text("x".to_string()),
            Doc::Line,
            Doc::Text("y".to_string()),
        ])));
        let doc = Doc::Group(Box::new(Doc::Concat(vec![
            Doc::Text("aaaaaaaa".to_string()),
            Doc::Line,
            inner,
        ])));
        assert_eq!(render(doc, 8), "aaaaaaaa\nx y");
    }
}
