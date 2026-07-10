//! Trivia-aware lexer for the CST.
//!
//! Strategy (zero drift): the *real* tokens are produced by the crate's own
//! [`RuaTokenize`] — the exact same lexer the semantic pipeline in `ruac`
//! uses — so token classification has a single source of truth. Trivia
//! (whitespace + comments) is reconstructed from the byte gaps *between*
//! consecutive real tokens, since every real token carries an absolute
//! `start`/`len`. The result is a flat, gap-free token stream covering the
//! entire source (`sum(len) == source.len()`).

use ruac::tokenize::RuaTokenize;

use crate::kind::{SyntaxKind, from_token};

/// A flat lexed token: kind plus its absolute byte span into the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LexToken {
    pub kind: SyntaxKind,
    pub start: usize,
    pub len: usize,
}

impl LexToken {
    fn new(kind: SyntaxKind, start: usize, len: usize) -> Self {
        LexToken { kind, start, len }
    }
}

/// Lex `text` into a gap-free flat token stream (trivia included, no `Eof`).
pub fn lex(text: &str) -> Vec<LexToken> {
    let mut out: Vec<LexToken> = Vec::new();
    let mut tz = RuaTokenize::new(text);
    let mut pos = 0usize;

    loop {
        match tz.next_token() {
            Ok(tok) => {
                let start = tok.range.start;
                // Everything the semantic lexer skipped between the previous
                // token and this one is trivia; classify and emit it.
                if start > pos {
                    push_trivia(&mut out, text, pos, start);
                }
                if tok.kind == ruac::token::RuaTokenKind::Eof {
                    // Trailing trivia (if any) was already emitted by the gap
                    // above; the zero-length Eof marker itself is dropped.
                    break;
                }
                out.push(LexToken::new(from_token(tok.kind), start, tok.range.len));
                pos = tok.range.end();
            }
            Err(_) => {
                // Error resilience: emit the unlexable remainder as one Error
                // token so the stream still round-trips, then stop.
                if pos < text.len() {
                    out.push(LexToken::new(SyntaxKind::Error, pos, text.len() - pos));
                }
                break;
            }
        }
    }

    out
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

/// Split the trivia gap `text[from..to]` into runs of whitespace / line
/// comments / block comments. The gap is guaranteed to contain only trivia, so
/// any non-comment byte is whitespace.
fn push_trivia(out: &mut Vec<LexToken>, text: &str, from: usize, to: usize) {
    let bytes = text.as_bytes();
    let mut i = from;
    while i < to {
        let c = bytes[i];
        if c == b'/' && i + 1 < to && bytes[i + 1] == b'/' {
            // Line comment: up to (not including) the newline.
            let mut j = i + 2;
            while j < to && bytes[j] != b'\n' && bytes[j] != b'\r' {
                j += 1;
            }
            out.push(LexToken::new(SyntaxKind::LineComment, i, j - i));
            i = j;
        } else if c == b'/' && i + 1 < to && bytes[i + 1] == b'*' {
            // Block comment (nested), spanning to its matching close.
            let mut j = i + 2;
            let mut depth = 1usize;
            while j < to && depth > 0 {
                if bytes[j] == b'/' && j + 1 < to && bytes[j + 1] == b'*' {
                    depth += 1;
                    j += 2;
                } else if bytes[j] == b'*' && j + 1 < to && bytes[j + 1] == b'/' {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            out.push(LexToken::new(SyntaxKind::BlockComment, i, j - i));
            i = j;
        } else {
            // Whitespace run: until the next comment start.
            let mut j = i;
            while j < to {
                if bytes[j] == b'/'
                    && j + 1 < to
                    && (bytes[j + 1] == b'/' || bytes[j + 1] == b'*')
                {
                    break;
                }
                j += 1;
            }
            out.push(LexToken::new(SyntaxKind::Whitespace, i, j - i));
            i = j;
        }
    }
}
