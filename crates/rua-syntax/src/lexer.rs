//! Syntax-owned, trivia-aware lexer for the CST.
//!
//! This lexer is independent of `ruac` — it uses only SyntaxKind and returns
//! gap-free `LexToken` streams (trivia included).

use crate::kind::SyntaxKind;

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
///
/// Known difference from ruac lexer: whitespace aggregation differs slightly,
/// which causes 9 format/symbol tests to fail (blank-line detection). These
/// will be addressed when the formatting layer is updated.
pub fn lex(text: &str) -> Vec<LexToken> {
    native_lex(text)
}

fn native_lex(text: &str) -> Vec<LexToken> {
    let mut tokens = Vec::new();
    let bytes = text.as_bytes();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let start = cursor;
        let byte = bytes[cursor];
        let (kind, len) = match byte {
            // Whitespace — aggregate all consecutive whitespace (spaces, tabs,
            // newlines, CRLF) into a single token. This matches the ruac transition
            // lexer behaviour that the format layer depends on for blank-line
            // detection (two \n in one Whitespace token = blank line).
            b' ' | b'\t' | b'\n' | b'\r' => {
                let mut end = cursor;
                while end < bytes.len() {
                    match bytes[end] {
                        b' ' | b'\t' | b'\n' => end += 1,
                        b'\r' => {
                            end += 1;
                            if bytes.get(end) == Some(&b'\n') {
                                end += 1; // \r\n as one unit
                            }
                        }
                        _ => break,
                    }
                }
                (SyntaxKind::Whitespace, end - cursor)
            }

            // Line comment
            b'/' if bytes.get(cursor + 1) == Some(&b'/') => {
                let mut end = cursor + 2;
                while end < bytes.len() && bytes[end] != b'\n' {
                    end += 1;
                }
                (SyntaxKind::LineComment, end - cursor)
            }

            // Block comment (supports nesting)
            b'/' if bytes.get(cursor + 1) == Some(&b'*') => {
                let mut end = cursor + 2;
                let mut depth = 1u32;
                while end < bytes.len() && depth > 0 {
                    if bytes[end] == b'/' && bytes.get(end + 1) == Some(&b'*') {
                        depth += 1;
                        end += 2;
                    } else if bytes[end] == b'*' && bytes.get(end + 1) == Some(&b'/') {
                        depth -= 1;
                        end += 2;
                    } else {
                        end += 1;
                    }
                }
                (SyntaxKind::BlockComment, end - cursor)
            }

            // String literal
            b'"' => {
                let mut end = cursor + 1;
                while end < bytes.len() && bytes[end] != b'"' {
                    if bytes[end] == b'\\' {
                        end += 1;
                    }
                    end += 1;
                }
                if end < bytes.len() {
                    end += 1; // closing quote
                }
                (SyntaxKind::Str, end - cursor)
            }

            // Identifiers and keywords
            byte if byte == b'_' || byte.is_ascii_alphabetic() => {
                let mut end = cursor + 1;
                while end < bytes.len()
                    && (bytes[end] == b'_' || bytes[end].is_ascii_alphanumeric())
                {
                    end += 1;
                }
                let ident = std::str::from_utf8(&bytes[cursor..end]).unwrap_or("");
                let kind = keyword_or_ident(ident);
                (kind, end - cursor)
            }

            // Numbers
            byte if byte.is_ascii_digit() => {
                let mut end = cursor + 1;
                let mut is_float = false;
                while end < bytes.len() {
                    match bytes[end] {
                        b'0'..=b'9' => end += 1,
                        b'.' if !is_float
                            && bytes.get(end + 1).is_some_and(|b| b.is_ascii_digit()) =>
                        {
                            is_float = true;
                            end += 1;
                        }
                        b'_' => end += 1,
                        _ => break,
                    }
                }
                (
                    if is_float {
                        SyntaxKind::Float
                    } else {
                        SyntaxKind::Int
                    },
                    end - cursor,
                )
            }

            // Operators and punctuation
            b'+' => (SyntaxKind::Plus, 1),
            b'-' if bytes.get(cursor + 1) == Some(&b'>') => (SyntaxKind::Arrow, 2),
            b'-' => (SyntaxKind::Minus, 1),
            b'*' => (SyntaxKind::Star, 1),
            b'/' => (SyntaxKind::Slash, 1),
            b'%' => (SyntaxKind::Percent, 1),

            b'<' if bytes.get(cursor + 1) == Some(&b'=') => (SyntaxKind::Le, 2),
            b'<' => (SyntaxKind::Lt, 1),
            b'>' if bytes.get(cursor + 1) == Some(&b'=') => (SyntaxKind::Ge, 2),
            b'>' => (SyntaxKind::Gt, 1),

            b'=' if bytes.get(cursor + 1) == Some(&b'=') => (SyntaxKind::EqEq, 2),
            b'=' if bytes.get(cursor + 1) == Some(&b'>') => (SyntaxKind::FatArrow, 2),
            b'=' => (SyntaxKind::Eq, 1),

            b'!' if bytes.get(cursor + 1) == Some(&b'=') => (SyntaxKind::Ne, 2),
            b'!' => (SyntaxKind::Not, 1),

            b'&' if bytes.get(cursor + 1) == Some(&b'&') => (SyntaxKind::AndAnd, 2),
            b'&' => (SyntaxKind::Amp, 1),
            b'|' if bytes.get(cursor + 1) == Some(&b'|') => (SyntaxKind::OrOr, 2),
            b'|' => (SyntaxKind::Pipe, 1),

            b'.' if bytes.get(cursor + 1) == Some(&b'.') && bytes.get(cursor + 2) == Some(&b'=') => {
                (SyntaxKind::DotDotEq, 3)
            }
            b'.' if bytes.get(cursor + 1) == Some(&b'.') => (SyntaxKind::DotDot, 2),
            b'.' => (SyntaxKind::Dot, 1),

            b':' if bytes.get(cursor + 1) == Some(&b':') => (SyntaxKind::ColonColon, 2),
            b':' => (SyntaxKind::Colon, 1),

            b';' => (SyntaxKind::Semi, 1),
            b',' => (SyntaxKind::Comma, 1),
            b'?' => (SyntaxKind::Question, 1),

            // Brackets / parens / braces
            b'(' => (SyntaxKind::LParen, 1),
            b')' => (SyntaxKind::RParen, 1),
            b'{' => (SyntaxKind::LBrace, 1),
            b'}' => (SyntaxKind::RBrace, 1),
            b'[' => (SyntaxKind::LBracket, 1),
            b']' => (SyntaxKind::RBracket, 1),

            // Error / non-ASCII — produce an error token and skip one char.
            _ => {
                let len = char_len(byte);
                (SyntaxKind::Error, len)
            }
        };
        tokens.push(LexToken::new(kind, start, len));
        cursor = start + len;
    }
    tokens
}

/// Number of bytes in the UTF-8 character starting with `byte`.
fn char_len(byte: u8) -> usize {
    if byte & 0x80 == 0 {
        1
    } else if byte & 0xE0 == 0xC0 {
        2
    } else if byte & 0xF0 == 0xE0 {
        3
    } else {
        4
    }
}

fn keyword_or_ident(word: &str) -> SyntaxKind {
    match word {
        "fn" => SyntaxKind::KwFn,
        "let" => SyntaxKind::KwLet,
        "mut" => SyntaxKind::KwMut,
        "if" => SyntaxKind::KwIf,
        "else" => SyntaxKind::KwElse,
        "while" => SyntaxKind::KwWhile,
        "loop" => SyntaxKind::KwLoop,
        "for" => SyntaxKind::KwFor,
        "in" => SyntaxKind::KwIn,
        "return" => SyntaxKind::KwReturn,
        "break" => SyntaxKind::KwBreak,
        "continue" => SyntaxKind::KwContinue,
        "dyn" => SyntaxKind::KwDyn,
        "match" => SyntaxKind::KwMatch,
        "struct" => SyntaxKind::KwStruct,
        "enum" => SyntaxKind::KwEnum,
        "trait" => SyntaxKind::KwTrait,
        "impl" => SyntaxKind::KwImpl,
        "mod" => SyntaxKind::KwMod,
        "pub" => SyntaxKind::KwPub,
        "use" => SyntaxKind::KwUse,
        "as" => SyntaxKind::KwAs,
        "self" => SyntaxKind::KwSelf,
        "true" => SyntaxKind::KwTrue,
        "false" => SyntaxKind::KwFalse,
        "extern" => SyntaxKind::KwExtern,
        "where" => SyntaxKind::Ident,  // not a keyword, mapped to Ident
        "type" => SyntaxKind::Ident,   // not a keyword, mapped to Ident
        "move" => SyntaxKind::Ident,   // not a keyword, mapped to Ident
        _ => SyntaxKind::Ident,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn lex_basic_tokens() {
        let src = "fn main() { let x: i64 = 42; }";
        let tokens = lex(src);
        let kinds: Vec<SyntaxKind> = tokens.iter().map(|t| t.kind).collect();
        let non_trivia: Vec<_> = kinds
            .iter()
            .filter(|k| !k.is_trivia())
            .copied()
            .collect();
        assert_eq!(
            non_trivia,
            [
                SyntaxKind::KwFn,
                SyntaxKind::Ident,
                SyntaxKind::LParen,
                SyntaxKind::RParen,
                SyntaxKind::LBrace,
                SyntaxKind::KwLet,
                SyntaxKind::Ident,
                SyntaxKind::Colon,
                SyntaxKind::Ident,
                SyntaxKind::Eq,
                SyntaxKind::Int,
                SyntaxKind::Semi,
                SyntaxKind::RBrace,
            ]
        );
    }

    #[test]
    fn lex_string_and_comments() {
        let src = r#""hello" // line comment
"#;
        let tokens = lex(src);
        let has_str = tokens.iter().any(|t| t.kind == SyntaxKind::Str);
        let has_line = tokens.iter().any(|t| t.kind == SyntaxKind::LineComment);
        assert!(has_str, "should lex string literal");
        assert!(has_line, "should lex line comment");
    }

    #[test]
    fn lex_block_comment_nested() {
        let src = "/* outer /* inner */ still comment */ fn";
        let tokens = lex(src);
        let has_fn = tokens.iter().any(|t| t.kind == SyntaxKind::KwFn);
        assert!(has_fn, "should find fn after nested block comment");
    }

    #[test]
    fn lex_range_operators() {
        let src = "0..10 0..=10 .. ..=";
        let tokens = lex(src);
        let has_dotdot = tokens.iter().any(|t| t.kind == SyntaxKind::DotDot);
        let has_dotdoteq = tokens.iter().any(|t| t.kind == SyntaxKind::DotDotEq);
        assert!(has_dotdot, "should lex ..");
        assert!(has_dotdoteq, "should lex ..=");
    }
}
