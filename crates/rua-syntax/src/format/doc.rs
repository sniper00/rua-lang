//! Doc IR and printer — a Wadler/Prettier-style pretty-printing core.
//!
//! The formatter lowers the CST into a [`Doc`] tree (see `lower.rs`), then
//! [`print`] renders it to a string within a target width. A [`Doc::Group`] is
//! first tried *flat* (all its soft breaks become spaces/nothing); if that would
//! overflow the line width, the group is re-rendered in *break* mode (its
//! [`Doc::Line`]/[`Doc::SoftLine`] become newlines). This is what gives
//! automatic, stable wrapping of long argument lists, `match` arms, etc.
//!
//! [`Doc::LineSuffix`] defers text (used for trailing `// comments`) until just
//! before the next newline, so a comment stays on the line it annotates even
//! though it is emitted mid-stream.

/// The indentation unit (spaces per level) — matches the codegen backend.
pub const INDENT: usize = 4;

/// Default maximum line width the printer aims for.
pub const DEFAULT_WIDTH: usize = 100;

/// A pretty-printing document.
#[derive(Debug, Clone)]
pub enum Doc {
    /// Empty.
    Nil,
    /// Literal text (must not contain `\n`; use the line variants for breaks).
    Text(String),
    /// A space when flat, a newline+indent when broken.
    Line,
    /// Nothing when flat, a newline+indent when broken.
    SoftLine,
    /// Always a newline+indent; forces its enclosing group to break.
    HardLine,
    /// Sequence of docs.
    Concat(Vec<Doc>),
    /// Increase the indentation level of the contained doc by one [`INDENT`].
    Indent(Box<Doc>),
    /// A break group: rendered flat if it fits, otherwise broken.
    Group(Box<Doc>),
    /// `IfBreak(broken, flat)` — pick `broken` in break mode, `flat` in flat mode
    /// (e.g. a trailing comma only when the enclosing group breaks).
    IfBreak(Box<Doc>, Box<Doc>),
    /// Text deferred until just before the next newline (trailing comments).
    LineSuffix(String),
}

impl Doc {
    pub fn text(s: impl Into<String>) -> Doc {
        Doc::Text(s.into())
    }

    pub fn concat(docs: impl IntoIterator<Item = Doc>) -> Doc {
        Doc::Concat(docs.into_iter().collect())
    }

    pub fn group(doc: Doc) -> Doc {
        Doc::Group(Box::new(doc))
    }

    pub fn indent(doc: Doc) -> Doc {
        Doc::Indent(Box::new(doc))
    }

    pub fn if_break(broken: Doc, flat: Doc) -> Doc {
        Doc::IfBreak(Box::new(broken), Box::new(flat))
    }

    /// Join `docs` with `sep` between adjacent elements.
    pub fn join(sep: Doc, docs: impl IntoIterator<Item = Doc>) -> Doc {
        let mut out = Vec::new();
        for (i, d) in docs.into_iter().enumerate() {
            if i > 0 {
                out.push(sep.clone());
            }
            out.push(d);
        }
        Doc::Concat(out)
    }

    /// Whether this doc contains a [`Doc::HardLine`] anywhere — such a group can
    /// never be laid out flat, so the printer forces it to break.
    fn has_hardline(&self) -> bool {
        match self {
            Doc::HardLine => true,
            Doc::Text(_) | Doc::Nil | Doc::Line | Doc::SoftLine | Doc::LineSuffix(_) => false,
            Doc::Indent(d) | Doc::Group(d) => d.has_hardline(),
            // A hardline only in the broken branch does not force the group.
            Doc::IfBreak(_, flat) => flat.has_hardline(),
            Doc::Concat(ds) => ds.iter().any(Doc::has_hardline),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Flat,
    Break,
}

/// Render `doc` to a string, wrapping groups that exceed `width`.
pub fn print(doc: &Doc, width: usize) -> String {
    let mut out = String::new();
    let mut col = 0usize;
    let mut suffixes: Vec<String> = Vec::new();
    // Work stack of (indent, mode, doc); processed LIFO.
    let mut cmds: Vec<(usize, Mode, &Doc)> = vec![(0, Mode::Break, doc)];

    while let Some((ind, mode, d)) = cmds.pop() {
        match d {
            Doc::Nil => {}
            Doc::Text(s) => {
                out.push_str(s);
                col += s.chars().count();
            }
            Doc::Concat(ds) => {
                for c in ds.iter().rev() {
                    cmds.push((ind, mode, c));
                }
            }
            Doc::Indent(b) => cmds.push((ind + INDENT, mode, b)),
            Doc::Group(b) => {
                let flat_ok = !b.has_hardline()
                    && fits(width.saturating_sub(col), ind, b, &cmds);
                let m = if flat_ok { Mode::Flat } else { Mode::Break };
                cmds.push((ind, m, b));
            }
            Doc::IfBreak(broken, flat) => {
                let chosen = if mode == Mode::Break { broken } else { flat };
                cmds.push((ind, mode, chosen));
            }
            Doc::LineSuffix(s) => suffixes.push(s.clone()),
            Doc::Line | Doc::SoftLine | Doc::HardLine => {
                let is_hard = matches!(d, Doc::HardLine);
                if mode == Mode::Flat && !is_hard {
                    if matches!(d, Doc::Line) {
                        out.push(' ');
                        col += 1;
                    }
                    // SoftLine flat → nothing.
                } else {
                    flush_suffixes(&mut out, &mut suffixes);
                    trim_trailing_spaces(&mut out);
                    out.push('\n');
                    for _ in 0..ind {
                        out.push(' ');
                    }
                    col = ind;
                }
            }
        }
    }
    flush_suffixes(&mut out, &mut suffixes);
    trim_trailing_spaces(&mut out);
    out
}

/// Emit any pending line-suffix text (trailing comments) at the current point.
fn flush_suffixes(out: &mut String, suffixes: &mut Vec<String>) {
    if suffixes.is_empty() {
        return;
    }
    for s in suffixes.drain(..) {
        out.push_str(&s);
    }
}

/// Remove spaces immediately before a newline we are about to write, so broken
/// groups never leave trailing whitespace.
fn trim_trailing_spaces(out: &mut String) {
    while out.ends_with(' ') {
        out.pop();
    }
}

/// Does the group starting at `doc` fit in `remaining` columns if laid out flat?
/// Scans `doc` (in flat mode) and then the already-queued `rest` commands until a
/// forced line break or the width is exhausted.
fn fits(remaining: usize, ind: usize, doc: &Doc, rest: &[(usize, Mode, &Doc)]) -> bool {
    let mut remaining = remaining as isize;
    // Local scan stack, seeded with the group doc, then the outer commands
    // (which continue the current line after the group).
    let mut stack: Vec<(usize, Mode, &Doc)> = Vec::new();
    // `rest` is a LIFO stack; its top (end) runs next, so push front-to-back.
    for cmd in rest.iter() {
        stack.push(*cmd);
    }
    stack.push((ind, Mode::Flat, doc));

    while remaining >= 0 {
        let Some((ind, mode, d)) = stack.pop() else {
            return true; // consumed everything without overflowing
        };
        match d {
            Doc::Nil => {}
            Doc::Text(s) => remaining -= s.chars().count() as isize,
            Doc::Concat(ds) => {
                for c in ds.iter().rev() {
                    stack.push((ind, mode, c));
                }
            }
            Doc::Indent(b) => stack.push((ind + INDENT, mode, b)),
            Doc::Group(b) => stack.push((ind, mode, b)),
            Doc::IfBreak(broken, flat) => {
                let chosen = if mode == Mode::Break { broken } else { flat };
                stack.push((ind, mode, chosen));
            }
            Doc::LineSuffix(_) => {}
            Doc::HardLine => return true, // a break ends the current line: it fits
            Doc::Line => {
                if mode == Mode::Flat {
                    remaining -= 1;
                } else {
                    return true;
                }
            }
            Doc::SoftLine => {
                if mode == Mode::Break {
                    return true;
                }
                // flat → contributes nothing
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_group_stays_on_one_line() {
        let d = Doc::group(Doc::concat([
            Doc::text("f("),
            Doc::SoftLine,
            Doc::join(Doc::concat([Doc::text(","), Doc::Line]), [Doc::text("a"), Doc::text("b")]),
            Doc::SoftLine,
            Doc::text(")"),
        ]));
        assert_eq!(print(&d, 80), "f(a, b)");
    }

    #[test]
    fn group_breaks_when_too_wide() {
        let d = Doc::group(Doc::concat([
            Doc::text("f("),
            Doc::indent(Doc::concat([
                Doc::SoftLine,
                Doc::join(
                    Doc::concat([Doc::text(","), Doc::Line]),
                    [Doc::text("alpha"), Doc::text("beta"), Doc::text("gamma")],
                ),
            ])),
            Doc::SoftLine,
            Doc::text(")"),
        ]));
        // Width 10 forces the arg list to break, one per line, indented.
        assert_eq!(print(&d, 10), "f(\n    alpha,\n    beta,\n    gamma\n)");
    }

    #[test]
    fn hardline_forces_break() {
        let d = Doc::group(Doc::concat([
            Doc::text("do"),
            Doc::indent(Doc::concat([Doc::HardLine, Doc::text("x")])),
            Doc::HardLine,
            Doc::text("end"),
        ]));
        assert_eq!(print(&d, 999), "do\n    x\nend");
    }

    #[test]
    fn if_break_adds_trailing_comma_only_when_broken() {
        let mk = || {
            Doc::group(Doc::concat([
                Doc::text("["),
                Doc::indent(Doc::concat([
                    Doc::SoftLine,
                    Doc::join(
                        Doc::concat([Doc::text(","), Doc::Line]),
                        [Doc::text("aaaa"), Doc::text("bbbb")],
                    ),
                    Doc::if_break(Doc::text(","), Doc::Nil),
                ])),
                Doc::SoftLine,
                Doc::text("]"),
            ]))
        };
        assert_eq!(print(&mk(), 80), "[aaaa, bbbb]");
        assert_eq!(print(&mk(), 6), "[\n    aaaa,\n    bbbb,\n]");
    }

    #[test]
    fn line_suffix_sticks_to_current_line() {
        let d = Doc::concat([
            Doc::text("local x = 1"),
            Doc::LineSuffix(" -- one".into()),
            Doc::HardLine,
            Doc::text("local y = 2"),
        ]);
        assert_eq!(print(&d, 80), "local x = 1 -- one\nlocal y = 2");
    }

    #[test]
    fn no_trailing_spaces_on_broken_lines() {
        let d = Doc::group(Doc::concat([
            Doc::text("a"),
            Doc::indent(Doc::concat([Doc::Line, Doc::text("b")])),
        ]));
        let out = print(&d, 1);
        assert!(!out.lines().any(|l| l.ends_with(' ')), "no trailing spaces: {out:?}");
    }
}
