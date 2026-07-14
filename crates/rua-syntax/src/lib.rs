//! Lossless rowan CST for Rua — the **IDE/LSP-facing** syntax tree
//! (see `docs/rua-design.md` §8 "two-tree" design).
//!
//! This crate provides a lossless **rowan CST** (native lexer → green/red tree
//! with trivia) plus a typed **AstNode view layer**, consumed only by tooling
//! (formatter / LSP / `rua-analysis`).
//!
pub mod ast;
pub mod format;
mod kind;
mod lexer;
mod line_index;
mod parser;
pub mod symbols;
pub mod text;

pub use ast::{AstNode, Named};
pub use kind::{RuaLanguage, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};
pub use lexer::{LexToken, lex};
pub use line_index::LineIndex;
pub use parser::{Parse, ParseError, parse, parse_source_file};

use rowan::GreenNodeBuilder;

/// A thin wrapper over rowan's [`GreenNodeBuilder`] that speaks [`SyntaxKind`].
#[derive(Default)]
pub struct TreeBuilder {
    inner: GreenNodeBuilder<'static>,
}

impl TreeBuilder {
    pub fn new() -> Self {
        TreeBuilder::default()
    }

    pub fn start_node(&mut self, kind: SyntaxKind) {
        self.inner.start_node(kind.into());
    }

    pub fn token(&mut self, kind: SyntaxKind, text: &str) {
        self.inner.token(kind.into(), text);
    }

    pub fn finish_node(&mut self) {
        self.inner.finish_node();
    }

    pub fn checkpoint(&self) -> rowan::Checkpoint {
        self.inner.checkpoint()
    }

    pub fn start_node_at(&mut self, checkpoint: rowan::Checkpoint, kind: SyntaxKind) {
        self.inner.start_node_at(checkpoint, kind.into());
    }

    /// Finish building and return the root [`SyntaxNode`].
    pub fn finish(self) -> SyntaxNode {
        SyntaxNode::new_root(self.inner.finish())
    }
}

/// Build a flat CST: a single [`SyntaxKind::SourceFile`] node holding every
/// lexed token (trivia included) as a leaf. This is not the real grammar yet,
/// but it exercises the full lex → green-tree → red-tree path and guarantees the
/// lossless invariant `parse_flat(src).text() == src`.
pub fn parse_flat(text: &str) -> SyntaxNode {
    let tokens = lex(text);
    let mut builder = TreeBuilder::new();
    builder.start_node(SyntaxKind::SourceFile);
    for t in tokens {
        builder.token(t.kind, &text[t.start..t.start + t.len]);
    }
    builder.finish_node();
    builder.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<SyntaxKind> {
        lex(src).into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn lex_covers_entire_source() {
        let src = "fn main() {\n    let x = 1;\n}\n";
        let toks = lex(src);
        let total: usize = toks.iter().map(|t| t.len).sum();
        assert_eq!(total, src.len(), "lexer must cover every byte");
        // Reassembling the token texts reproduces the source exactly.
        let mut reassembled = String::new();
        for t in &toks {
            reassembled.push_str(&src[t.start..t.start + t.len]);
        }
        assert_eq!(reassembled, src);
    }

    #[test]
    fn flat_tree_round_trips() {
        let src = "fn main() {\n    // hi there\n    let x = 1; /* blk */\n}\n";
        let node = parse_flat(src);
        assert_eq!(node.kind(), SyntaxKind::SourceFile);
        assert_eq!(node.text().to_string(), src);
    }

    #[test]
    fn trivia_is_captured() {
        let src = "let x = 1; // trailing\n/* block */ let y = 2;";
        let ks = kinds(src);
        assert!(
            ks.contains(&SyntaxKind::LineComment),
            "line comment retained"
        );
        assert!(
            ks.contains(&SyntaxKind::BlockComment),
            "block comment retained"
        );
        assert!(ks.contains(&SyntaxKind::Whitespace), "whitespace retained");
        // Real tokens still classified via the shared lexer.
        assert!(ks.contains(&SyntaxKind::KwLet));
        assert!(ks.contains(&SyntaxKind::Int));
    }

    #[test]
    fn nested_block_comment_is_one_token() {
        let src = "let a = 1; /* outer /* inner */ still */ let b = 2;";
        let toks = lex(src);
        let block: Vec<_> = toks
            .iter()
            .filter(|t| t.kind == SyntaxKind::BlockComment)
            .collect();
        assert_eq!(block.len(), 1, "nested block comment is a single token");
        assert_eq!(
            &src[block[0].start..block[0].start + block[0].len],
            "/* outer /* inner */ still */"
        );
    }

    #[test]
    fn leading_and_trailing_trivia_round_trip() {
        let src = "  \n// lead\nfn f() {}\n\n// tail\n";
        assert_eq!(parse_flat(src).text().to_string(), src);
    }

    #[test]
    fn empty_source_round_trips() {
        assert_eq!(parse_flat("").text().to_string(), "");
    }
}
