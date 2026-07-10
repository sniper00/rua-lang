//! Trivia-aware lexer for the CST.
//!
//! During the Phase 1 migration the compiler tokenizer is accessed only through
//! the crate-private `transition` boundary. This module and its public API use
//! syntax-owned token kinds exclusively.

use crate::kind::SyntaxKind;

/// A flat lexed token: kind plus its absolute byte span into the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LexToken {
    pub kind: SyntaxKind,
    pub start: usize,
    pub len: usize,
}

impl LexToken {
    pub(crate) fn new(kind: SyntaxKind, start: usize, len: usize) -> Self {
        LexToken { kind, start, len }
    }
}

/// Lex `text` into a gap-free flat token stream (trivia included, no `Eof`).
pub fn lex(text: &str) -> Vec<LexToken> {
    crate::transition::lex(text)
}

#[cfg(test)]
mod multibyte_tests {
    use super::*;

    /// Every token in the flat stream must start and end on a UTF-8 char
    /// boundary and the stream must cover the whole source with no gaps. Stray
    /// multibyte chars (e.g. CJK punctuation) in code, comments and strings all
    /// used to split a char and panic downstream slicing.
    fn assert_boundary_safe(src: &str) {
        let toks = lex(src);
        let mut sum = 0usize;
        for t in &toks {
            assert!(
                src.is_char_boundary(t.start),
                "token start {} not on char boundary in {src:?}",
                t.start
            );
            assert!(
                src.is_char_boundary(t.start + t.len),
                "token end {} not on char boundary in {src:?}",
                t.start + t.len
            );
            // Slicing must not panic.
            let _ = &src[t.start..t.start + t.len];
            sum += t.len;
        }
        assert_eq!(sum, src.len(), "flat stream must cover the source: {src:?}");
    }

    #[test]
    fn multibyte_in_comment_string_and_code() {
        assert_boundary_safe("// 注释。\nfn main() { let s = \"值。\"; }\n");
        assert_boundary_safe("fn main() { 。 let x = 值; }\n// 悬空注释无换行。");
        assert_boundary_safe("fn f() -> i64 { 表情🙂 }");
        assert_boundary_safe("。。。");
        assert_boundary_safe("/* 块注释。 */ fn g() {}");
    }
}
