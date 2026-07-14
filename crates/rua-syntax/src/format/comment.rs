//! Comment extraction + blank-line detection from the rowan CST (P5e-B2, B3).
//!
//! The CST is lossless — comments are trivia tokens (`LineComment`,
//! `BlockComment`) that live among real tokens in `children_with_tokens()`.
//! This module extracts them and classifies each as *leading* (appears before a
//! construct) or *trailing* (appears after a construct on the same line), and
//! records whether a blank line precedes each node (B3). Some containers nest
//! the preceding trivia *inside* the following child node rather than as a
//! sibling; [`peel_leading_trivia`] recovers it so field/variant comments and
//! blank lines are not lost.
//!
//! The main entry point is [`extract_children`], which walks a container node
//! (SourceFile, Block, etc.) and pairs each child node with the comments that
//! precede and follow it.

use crate::{SyntaxElement, SyntaxKind as K, SyntaxNode};

/// A comment extracted from the CST.
#[derive(Debug, Clone)]
pub(crate) struct Comment {
    /// Full comment text including `//` or `/* */` delimiters.
    pub text: String,
}

/// One entry from [`extract_children`]: either a child node with attached
/// comments, or a standalone comment at the end of the container.
#[derive(Debug)]
pub(crate) enum Entry {
    /// A child syntax node with leading and trailing comments.
    Node {
        leading: Vec<Comment>,
        trailing: Vec<Comment>,
        /// True when ≥2 newlines separate this node from the preceding sibling
        /// (or the container's opening delimiter). Collapses N≥2 blank lines
        /// to a single blank line so the formatter is idempotent.
        blank_line_before: bool,
        node: SyntaxNode,
    },
    /// A comment at the end of the container with no following node.
    Comment(Comment),
}

/// Trivia the parser absorbed as a node's own *leading* children, split by how
/// it should be reattached. See [`peel_leading_trivia`].
struct Peeled {
    /// Comments before the first newline — they sit on the previous sibling's
    /// line, so they are that sibling's *trailing* comments.
    same_line: Vec<Comment>,
    /// Comments after a newline — genuine leading comments for this node.
    leading: Vec<Comment>,
    /// Whether a blank line (≥2 newlines in one whitespace token) precedes the
    /// node's content.
    blank: bool,
}

/// Peel the leading trivia that lives *inside* `node` (before its first
/// non-trivia token). Some containers (struct field lists, enum variant lists,
/// extern blocks) let each child node absorb the whitespace/comments that
/// precede it, rather than keeping them as container-level siblings. Blocks and
/// the file/mod item lists eat that trivia at the container level, so for those
/// this returns empty — making it safe to call unconditionally (no
/// double-counting).
fn peel_leading_trivia(node: &SyntaxNode) -> Peeled {
    let mut same_line = Vec::new();
    let mut leading = Vec::new();
    let mut blank = false;
    let mut seen_nl = false;
    for elem in node.children_with_tokens() {
        match elem {
            SyntaxElement::Token(t) if t.kind().is_trivia() => {
                if t.kind().is_comment() {
                    if seen_nl {
                        leading.push(Comment {
                            text: t.text().to_string(),
                        });
                    } else {
                        same_line.push(Comment {
                            text: t.text().to_string(),
                        });
                    }
                } else if t.kind() == K::Whitespace {
                    let nls = t.text().matches('\n').count();
                    if nls >= 1 {
                        seen_nl = true;
                    }
                    if nls >= 2 {
                        blank = true;
                    }
                }
            }
            // First real token or child node — trivia region ends.
            _ => break,
        }
    }
    Peeled {
        same_line,
        leading,
        blank,
    }
}

/// Walk the direct children of a container node and return each child node
/// annotated with its leading and trailing comments.
///
/// **Leading comments**: comment tokens that appear in the parent's
/// `children_with_tokens()` before this child node (separated by arbitrary
/// whitespace/trivia).
///
/// **Trailing comments**: comment tokens that appear after this child node
/// but before the next `\n`-containing whitespace token — i.e. comments on
/// the same logical line as the child.
///
/// Standalone comments at the end of the container (no following node) are
/// yielded as `Entry::Comment`.
pub(crate) fn extract_children(parent: &SyntaxNode) -> Vec<Entry> {
    let children: Vec<SyntaxElement> = parent.children_with_tokens().collect();
    let len = children.len();
    let mut i = 0;
    let mut entries = Vec::new();
    let mut leading: Vec<Comment> = Vec::new();
    // Whether any whitespace token since the last child node contained ≥2
    // newlines — i.e. the author wrote a blank line before the next node.
    // Reset to false after each [`Entry::Node`] is pushed.
    let mut saw_blank: bool = false;

    while i < len {
        match &children[i] {
            // Remember comment tokens; they become leading comments for the
            // next child node (or standalone entries at the end).
            SyntaxElement::Token(t) if t.kind().is_comment() => {
                leading.push(Comment {
                    text: t.text().to_string(),
                });
                i += 1;
            }
            // Detect blank lines: ≥2 newlines in a single whitespace token.
            // We check per-token (not cumulative across comments) because
            // `\n// c\n` has two newlines but no blank line — the comment
            // occupies the line between them.
            SyntaxElement::Token(t) if t.kind() == K::Whitespace => {
                if t.text().matches('\n').count() >= 2 {
                    saw_blank = true;
                }
                i += 1;
            }
            // A child node: pair it with any accumulated leading comments,
            // then look ahead for trailing comments on the same line.
            SyntaxElement::Node(n) => {
                let node = n.clone();
                i += 1;

                // Some containers (struct/enum field lists) let each child node
                // absorb the whitespace/comments before it. Peel that trivia so
                // it participates in comment attachment and blank-line detection.
                let peeled = peel_leading_trivia(&node);
                if !peeled.same_line.is_empty() {
                    // Same-line comments belong to the previous sibling.
                    if let Some(Entry::Node { trailing, .. }) = entries.last_mut() {
                        trailing.extend(peeled.same_line);
                    } else {
                        leading.extend(peeled.same_line);
                    }
                }
                leading.extend(peeled.leading);
                if peeled.blank {
                    saw_blank = true;
                }

                // Scan forward for trailing comments — comments before the
                // next newline-containing whitespace (or end of children).
                let mut trailing: Vec<Comment> = Vec::new();
                while i < len {
                    match &children[i] {
                        SyntaxElement::Token(t) if t.kind().is_comment() => {
                            trailing.push(Comment {
                                text: t.text().to_string(),
                            });
                            i += 1;
                        }
                        SyntaxElement::Token(t) if t.kind() == K::Whitespace => {
                            if t.text().contains('\n') {
                                break; // newline → trailing zone ends
                            }
                            i += 1; // inline whitespace (space/tab), skip
                        }
                        _ => break, // next node or other token
                    }
                }

                entries.push(Entry::Node {
                    leading: std::mem::take(&mut leading),
                    trailing,
                    blank_line_before: std::mem::take(&mut saw_blank),
                    node,
                });
            }
            // Other tokens at odd positions — skip.
            _ => {
                i += 1;
            }
        }
    }

    // Any comments left after the last child node become standalone entries.
    for c in leading {
        entries.push(Entry::Comment(c));
    }

    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AstNode, parse_source_file};

    fn test_extract(src: &str) -> Vec<Entry> {
        let parsed = parse_source_file(src);
        extract_children(parsed.tree.syntax())
    }

    #[test]
    fn leading_comment_before_fn() {
        let entries = test_extract("// doc\nfn foo() {}");
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            Entry::Node {
                leading, trailing, ..
            } => {
                assert_eq!(leading.len(), 1, "one leading comment");
                assert_eq!(leading[0].text, "// doc");
                assert!(trailing.is_empty());
            }
            _ => panic!("expected Node entry"),
        }
    }

    #[test]
    fn multiple_leading_comments() {
        let entries = test_extract("// a\n// b\nfn foo() {}");
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            Entry::Node { leading, .. } => {
                assert_eq!(leading.len(), 2);
                assert_eq!(leading[0].text, "// a");
                assert_eq!(leading[1].text, "// b");
            }
            _ => panic!("expected Node entry"),
        }
    }

    #[test]
    fn trailing_comment_on_stmt() {
        // Block content: let x = 1; // t
        let src = "fn foo() {\n    let x = 1; // t\n}";
        let parsed = parse_source_file(src);
        let root = parsed.tree.syntax();
        // Navigate: SourceFile → FnDecl → Block
        let block = root
            .descendants_with_tokens()
            .filter_map(|e| e.into_node())
            .find(|n| n.kind() == K::Block)
            .expect("block exists");
        let entries = extract_children(&block);
        // Should have one entry (LetStmt) with a trailing comment
        assert_eq!(entries.len(), 1, "one entry in block");
        match &entries[0] {
            Entry::Node {
                leading, trailing, ..
            } => {
                assert!(leading.is_empty());
                assert_eq!(trailing.len(), 1, "one trailing comment");
                assert_eq!(trailing[0].text, "// t");
            }
            _ => panic!("expected Node entry"),
        }
    }

    #[test]
    fn comment_between_items() {
        let entries = test_extract("fn a() {}\n// between\nfn b() {}");
        assert_eq!(entries.len(), 2);
        match &entries[1] {
            Entry::Node { leading, .. } => {
                assert_eq!(leading.len(), 1);
                assert_eq!(leading[0].text, "// between");
            }
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn trailing_comment_at_eof() {
        let entries = test_extract("fn foo() {}\n// eof");
        // One fn entry + one standalone comment entry
        assert_eq!(entries.len(), 2);
        assert!(matches!(&entries[1], Entry::Comment(_)));
        match &entries[1] {
            Entry::Comment(c) => assert_eq!(c.text, "// eof"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn block_comment_leading() {
        let entries = test_extract("/* header */\nfn foo() {}");
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            Entry::Node { leading, .. } => {
                assert_eq!(leading.len(), 1);
                assert_eq!(leading[0].text, "/* header */");
            }
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn no_comments() {
        let entries = test_extract("fn foo() {}");
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            Entry::Node {
                leading, trailing, ..
            } => {
                assert!(leading.is_empty());
                assert!(trailing.is_empty());
            }
            _ => panic!("expected Node"),
        }
    }

    // --- blank_line_before -------------------------------------------------

    #[test]
    fn blank_line_between_items() {
        // Two fns separated by \n\n → second one has blank_line_before.
        let entries = test_extract("fn a() {}\n\nfn b() {}");
        assert_eq!(entries.len(), 2);
        match &entries[0] {
            Entry::Node {
                blank_line_before, ..
            } => assert!(!blank_line_before, "first item"),
            _ => panic!("expected Node"),
        }
        match &entries[1] {
            Entry::Node {
                blank_line_before, ..
            } => assert!(blank_line_before, "second item has blank line before"),
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn single_newline_no_blank_line() {
        let entries = test_extract("fn a() {}\nfn b() {}");
        assert_eq!(entries.len(), 2);
        match &entries[1] {
            Entry::Node {
                blank_line_before, ..
            } => assert!(!blank_line_before),
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn blank_line_with_leading_comment() {
        // \n\n then comment then \n → blank_line = true for the fn.
        let entries = test_extract("fn a() {}\n\n// doc\nfn b() {}");
        assert_eq!(entries.len(), 2);
        match &entries[1] {
            Entry::Node {
                leading,
                blank_line_before,
                ..
            } => {
                assert_eq!(leading.len(), 1);
                assert_eq!(leading[0].text, "// doc");
                assert!(blank_line_before);
            }
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn no_blank_line_with_leading_comment() {
        // Single \n then comment then \n → no blank line.
        let entries = test_extract("fn a() {}\n// doc\nfn b() {}");
        assert_eq!(entries.len(), 2);
        match &entries[1] {
            Entry::Node {
                blank_line_before, ..
            } => assert!(!blank_line_before),
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn multiple_blank_lines_collapsed() {
        // Three \n\n\n → count is 3, still blank_line_before = true (≥2).
        let entries = test_extract("fn a() {}\n\n\nfn b() {}");
        assert_eq!(entries.len(), 2);
        match &entries[1] {
            Entry::Node {
                blank_line_before, ..
            } => assert!(blank_line_before),
            _ => panic!("expected Node"),
        }
    }

    // --- peeled in-node trivia (struct fields / enum variants) -------------
    //
    // Field lists let each field node absorb the whitespace/comments before it
    // (unlike blocks). These verify `peel_leading_trivia` recovers them.

    fn extract_field_list(src: &str) -> Vec<Entry> {
        let parsed = parse_source_file(src);
        let root = parsed.tree.syntax();
        let fl = root
            .descendants()
            .find(|n| n.kind() == K::FieldList)
            .expect("field list exists");
        extract_children(&fl)
    }

    #[test]
    fn struct_field_blank_line_detected() {
        let entries = extract_field_list("struct S {\n    a: i64,\n\n    b: i64,\n}");
        assert_eq!(entries.len(), 2, "two fields");
        match &entries[0] {
            Entry::Node {
                blank_line_before, ..
            } => assert!(!blank_line_before, "first field"),
            _ => panic!("expected Node"),
        }
        match &entries[1] {
            Entry::Node {
                blank_line_before, ..
            } => {
                assert!(
                    blank_line_before,
                    "blank before second field peeled from node"
                )
            }
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn struct_field_leading_comment_peeled() {
        let entries =
            extract_field_list("struct S {\n    a: i64,\n    // note on b\n    b: i64,\n}");
        assert_eq!(entries.len(), 2);
        match &entries[1] {
            Entry::Node {
                leading,
                blank_line_before,
                ..
            } => {
                assert_eq!(leading.len(), 1, "leading comment recovered");
                assert_eq!(leading[0].text, "// note on b");
                assert!(!blank_line_before);
            }
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn struct_field_same_line_comment_is_trailing_of_previous() {
        let entries = extract_field_list("struct S {\n    a: i64, // trailing a\n    b: i64,\n}");
        assert_eq!(entries.len(), 2);
        // The `// trailing a` was absorbed as field b's leading trivia but sits
        // on field a's line, so it must attach to field a as trailing.
        match &entries[0] {
            Entry::Node { trailing, .. } => {
                assert_eq!(
                    trailing.len(),
                    1,
                    "same-line comment attached to previous field"
                );
                assert_eq!(trailing[0].text, "// trailing a");
            }
            _ => panic!("expected Node"),
        }
        match &entries[1] {
            Entry::Node { leading, .. } => assert!(leading.is_empty(), "not leading of b"),
            _ => panic!("expected Node"),
        }
    }
}
