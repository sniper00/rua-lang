//! Lossless shared lexer for both Rua parsers.

pub use rua_core::TextRange;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TokenKind {
    Whitespace,
    LineComment,
    BlockComment,
    KwFn,
    KwLet,
    KwMut,
    KwIf,
    KwElse,
    KwWhile,
    KwLoop,
    KwFor,
    KwIn,
    KwReturn,
    KwBreak,
    KwContinue,
    KwDyn,
    KwTrue,
    KwFalse,
    KwStruct,
    KwEnum,
    KwTrait,
    KwImpl,
    KwPub,
    KwUse,
    KwMod,
    KwAs,
    KwMatch,
    KwSelf,
    KwExtern,
    /// A raw `lua! { ... }` block, consumed as one lossless token.
    LuaBlock,
    Ident,
    Int,
    Float,
    Str,
    Plus,
    PlusEq,
    Minus,
    MinusEq,
    Star,
    StarEq,
    Slash,
    SlashEq,
    Percent,
    PercentEq,
    Eq,
    EqEq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    AndAnd,
    OrOr,
    Not,
    Amp,
    Pipe,
    Question,
    QuestionQuestion,
    QuestionDot,
    Arrow,
    FatArrow,
    ColonColon,
    Colon,
    Semi,
    Comma,
    Dot,
    DotDot,
    DotDotEq,
    Hash,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Eof,
    Unknown,
}

impl TokenKind {
    pub const fn is_trivia(self) -> bool {
        matches!(
            self,
            Self::Whitespace | Self::LineComment | Self::BlockComment
        )
    }

    pub const fn is_comment(self) -> bool {
        matches!(self, Self::LineComment | Self::BlockComment)
    }

    pub const fn user_string(self) -> &'static str {
        match self {
            Self::Whitespace => "<whitespace>",
            Self::LineComment | Self::BlockComment => "<comment>",
            Self::KwFn => "fn",
            Self::KwLet => "let",
            Self::KwMut => "mut",
            Self::KwIf => "if",
            Self::KwElse => "else",
            Self::KwWhile => "while",
            Self::KwLoop => "loop",
            Self::KwFor => "for",
            Self::KwIn => "in",
            Self::KwReturn => "return",
            Self::KwBreak => "break",
            Self::KwContinue => "continue",
            Self::KwDyn => "dyn",
            Self::KwTrue => "true",
            Self::KwFalse => "false",
            Self::KwStruct => "struct",
            Self::KwEnum => "enum",
            Self::KwTrait => "trait",
            Self::KwImpl => "impl",
            Self::KwPub => "pub",
            Self::KwUse => "use",
            Self::KwMod => "mod",
            Self::KwAs => "as",
            Self::KwMatch => "match",
            Self::KwSelf => "self",
            Self::KwExtern => "extern",
            Self::LuaBlock => "lua! { ... }",
            Self::Ident => "<ident>",
            Self::Int => "<int>",
            Self::Float => "<float>",
            Self::Str => "<string>",
            Self::Plus => "+",
            Self::PlusEq => "+=",
            Self::Minus => "-",
            Self::MinusEq => "-=",
            Self::Star => "*",
            Self::StarEq => "*=",
            Self::Slash => "/",
            Self::SlashEq => "/=",
            Self::Percent => "%",
            Self::PercentEq => "%=",
            Self::Eq => "=",
            Self::EqEq => "==",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::AndAnd => "&&",
            Self::OrOr => "||",
            Self::Not => "!",
            Self::Amp => "&",
            Self::Pipe => "|",
            Self::Question => "?",
            Self::QuestionQuestion => "??",
            Self::QuestionDot => "?.",
            Self::Arrow => "->",
            Self::FatArrow => "=>",
            Self::ColonColon => "::",
            Self::Colon => ":",
            Self::Semi => ";",
            Self::Comma => ",",
            Self::Dot => ".",
            Self::DotDot => "..",
            Self::DotDotEq => "..=",
            Self::Hash => "#",
            Self::LParen => "(",
            Self::RParen => ")",
            Self::LBrace => "{",
            Self::RBrace => "}",
            Self::LBracket => "[",
            Self::RBracket => "]",
            Self::Eof => "<eof>",
            Self::Unknown => "<unknown>",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LexErrorKind {
    UnknownCharacter,
    UnterminatedString,
    UnterminatedBlockComment,
    UnterminatedLuaBlock,
}

impl LexErrorKind {
    pub const fn message(self) -> &'static str {
        match self {
            Self::UnknownCharacter => "unknown character",
            Self::UnterminatedString => "unterminated string",
            Self::UnterminatedBlockComment => "unterminated block comment",
            Self::UnterminatedLuaBlock => "unterminated lua block",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LexToken {
    pub kind: TokenKind,
    pub range: TextRange,
    pub error: Option<LexErrorKind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenLimitError {
    pub range: TextRange,
}

impl LexToken {
    const fn new(kind: TokenKind, start: usize, len: usize, error: Option<LexErrorKind>) -> Self {
        assert!(start <= u32::MAX as usize && len <= u32::MAX as usize);
        Self {
            kind,
            range: TextRange::at(start as u32, len as u32),
            error,
        }
    }
}

pub fn lex(text: &str) -> Vec<LexToken> {
    lex_with_limit(text, usize::MAX).expect("an unlimited lexer cannot exhaust its token budget")
}

pub fn lex_with_limit(text: &str, max_tokens: usize) -> Result<Vec<LexToken>, TokenLimitError> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let start = cursor;
        let (kind, end, error) = match bytes[cursor] {
            b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c => {
                let mut end = cursor + 1;
                while end < bytes.len()
                    && matches!(bytes[end], b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
                {
                    end += 1;
                }
                (TokenKind::Whitespace, end, None)
            }
            b'/' if bytes.get(cursor + 1) == Some(&b'/') => {
                let mut end = cursor + 2;
                while end < bytes.len() && !matches!(bytes[end], b'\n' | b'\r') {
                    end += 1;
                }
                (TokenKind::LineComment, end, None)
            }
            b'/' if bytes.get(cursor + 1) == Some(&b'*') => {
                let mut end = cursor + 2;
                let mut depth = 1usize;
                while end < bytes.len() && depth > 0 {
                    if bytes[end] == b'/' && bytes.get(end + 1) == Some(&b'*') {
                        depth += 1;
                        end += 2;
                    } else if bytes[end] == b'*' && bytes.get(end + 1) == Some(&b'/') {
                        depth -= 1;
                        end += 2;
                    } else {
                        end += char_width(text, end);
                    }
                }
                let error = (depth != 0).then_some(LexErrorKind::UnterminatedBlockComment);
                (TokenKind::BlockComment, end, error)
            }
            b'"' => {
                let mut end = cursor + 1;
                let mut terminated = false;
                while end < bytes.len() {
                    match bytes[end] {
                        b'"' => {
                            end += 1;
                            terminated = true;
                            break;
                        }
                        b'\\' => {
                            end += 1;
                            if end < bytes.len() {
                                end += char_width(text, end);
                            }
                        }
                        b'\n' | b'\r' => break,
                        _ => end += char_width(text, end),
                    }
                }
                let error = (!terminated).then_some(LexErrorKind::UnterminatedString);
                (TokenKind::Str, end, error)
            }
            b'l' if bytes.get(cursor..cursor + 4) == Some(b"lua!") => {
                if let Some((end, error)) = lex_lua_block(text, cursor) {
                    (TokenKind::LuaBlock, end, error)
                } else {
                    let mut end = cursor + 1;
                    while end < bytes.len() && is_name_continue(bytes[end]) {
                        end += 1;
                    }
                    let word = &text[cursor..end];
                    (keyword_kind(word), end, None)
                }
            }
            byte if is_name_start(byte) => {
                let mut end = cursor + 1;
                while end < bytes.len() && is_name_continue(bytes[end]) {
                    end += 1;
                }
                let word = &text[cursor..end];
                (keyword_kind(word), end, None)
            }
            byte if byte.is_ascii_digit()
                || (byte == b'.' && bytes.get(cursor + 1).is_some_and(u8::is_ascii_digit)) =>
            {
                let (kind, end) = lex_number(bytes, cursor);
                (kind, end, None)
            }
            b'+' if bytes.get(cursor + 1) == Some(&b'=') => simple(TokenKind::PlusEq, cursor, 2),
            b'+' => simple(TokenKind::Plus, cursor, 1),
            b'-' if bytes.get(cursor + 1) == Some(&b'>') => simple(TokenKind::Arrow, cursor, 2),
            b'-' if bytes.get(cursor + 1) == Some(&b'=') => simple(TokenKind::MinusEq, cursor, 2),
            b'-' => simple(TokenKind::Minus, cursor, 1),
            b'*' if bytes.get(cursor + 1) == Some(&b'=') => simple(TokenKind::StarEq, cursor, 2),
            b'*' => simple(TokenKind::Star, cursor, 1),
            b'/' if bytes.get(cursor + 1) == Some(&b'=') => simple(TokenKind::SlashEq, cursor, 2),
            b'/' => simple(TokenKind::Slash, cursor, 1),
            b'%' if bytes.get(cursor + 1) == Some(&b'=') => simple(TokenKind::PercentEq, cursor, 2),
            b'%' => simple(TokenKind::Percent, cursor, 1),
            b'<' if bytes.get(cursor + 1) == Some(&b'=') => simple(TokenKind::Le, cursor, 2),
            b'<' => simple(TokenKind::Lt, cursor, 1),
            b'>' if bytes.get(cursor + 1) == Some(&b'=') => simple(TokenKind::Ge, cursor, 2),
            b'>' => simple(TokenKind::Gt, cursor, 1),
            b'=' if bytes.get(cursor + 1) == Some(&b'=') => simple(TokenKind::EqEq, cursor, 2),
            b'=' if bytes.get(cursor + 1) == Some(&b'>') => simple(TokenKind::FatArrow, cursor, 2),
            b'=' => simple(TokenKind::Eq, cursor, 1),
            b'!' if bytes.get(cursor + 1) == Some(&b'=') => simple(TokenKind::Ne, cursor, 2),
            b'!' => simple(TokenKind::Not, cursor, 1),
            b'&' if bytes.get(cursor + 1) == Some(&b'&') => simple(TokenKind::AndAnd, cursor, 2),
            b'&' => simple(TokenKind::Amp, cursor, 1),
            b'|' if bytes.get(cursor + 1) == Some(&b'|') => simple(TokenKind::OrOr, cursor, 2),
            b'|' => simple(TokenKind::Pipe, cursor, 1),
            b'.' if bytes.get(cursor + 1) == Some(&b'.')
                && bytes.get(cursor + 2) == Some(&b'=') =>
            {
                simple(TokenKind::DotDotEq, cursor, 3)
            }
            b'.' if bytes.get(cursor + 1) == Some(&b'.') => simple(TokenKind::DotDot, cursor, 2),
            b'.' => simple(TokenKind::Dot, cursor, 1),
            b':' if bytes.get(cursor + 1) == Some(&b':') => {
                simple(TokenKind::ColonColon, cursor, 2)
            }
            b':' => simple(TokenKind::Colon, cursor, 1),
            b';' => simple(TokenKind::Semi, cursor, 1),
            b',' => simple(TokenKind::Comma, cursor, 1),
            b'?' if bytes.get(cursor + 1) == Some(&b'?') => {
                simple(TokenKind::QuestionQuestion, cursor, 2)
            }
            b'?' if bytes.get(cursor + 1) == Some(&b'.') => {
                simple(TokenKind::QuestionDot, cursor, 2)
            }
            b'?' => simple(TokenKind::Question, cursor, 1),
            b'#' => simple(TokenKind::Hash, cursor, 1),
            b'(' => simple(TokenKind::LParen, cursor, 1),
            b')' => simple(TokenKind::RParen, cursor, 1),
            b'{' => simple(TokenKind::LBrace, cursor, 1),
            b'}' => simple(TokenKind::RBrace, cursor, 1),
            b'[' => simple(TokenKind::LBracket, cursor, 1),
            b']' => simple(TokenKind::RBracket, cursor, 1),
            _ => (
                TokenKind::Unknown,
                cursor + char_width(text, cursor),
                Some(LexErrorKind::UnknownCharacter),
            ),
        };
        if tokens.len() == max_tokens {
            return Err(TokenLimitError {
                range: TextRange::at(start as u32, (end - start) as u32),
            });
        }
        tokens.push(LexToken::new(kind, start, end - start, error));
        cursor = end;
    }
    Ok(tokens)
}

const fn simple(
    kind: TokenKind,
    cursor: usize,
    len: usize,
) -> (TokenKind, usize, Option<LexErrorKind>) {
    (kind, cursor + len, None)
}

fn lex_number(bytes: &[u8], start: usize) -> (TokenKind, usize) {
    let mut end = start;
    let mut is_float = false;

    if bytes[end] == b'.' {
        is_float = true;
        end += 1;
    } else if bytes[end] == b'0' && matches!(bytes.get(end + 1), Some(b'x' | b'X' | b'b' | b'B')) {
        end += 2;
        while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
            end += 1;
        }
        return (TokenKind::Int, end);
    }

    while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'_') {
        end += 1;
    }

    if end < bytes.len()
        && bytes[end] == b'.'
        && bytes.get(end + 1) != Some(&b'.')
        && !bytes.get(end + 1).is_some_and(|byte| is_name_start(*byte))
    {
        is_float = true;
        end += 1;
        while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'_') {
            end += 1;
        }
    }

    if end < bytes.len() && matches!(bytes[end], b'e' | b'E') {
        is_float = true;
        end += 1;
        if end < bytes.len() && matches!(bytes[end], b'+' | b'-') {
            end += 1;
        }
        while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'_') {
            end += 1;
        }
    }

    (
        if is_float {
            TokenKind::Float
        } else {
            TokenKind::Int
        },
        end,
    )
}

const fn is_name_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

const fn is_name_continue(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphanumeric()
}

fn char_width(text: &str, offset: usize) -> usize {
    text[offset..].chars().next().map_or(1, char::len_utf8)
}

/// Recognize and consume `lua! { ... }` while respecting Lua strings,
/// long-bracket strings, line comments, block comments, and nested braces.
fn lex_lua_block(text: &str, start: usize) -> Option<(usize, Option<LexErrorKind>)> {
    let bytes = text.as_bytes();
    if start + 4 > bytes.len() || &bytes[start..start + 4] != b"lua!" {
        return None;
    }
    if start > 0 && is_name_continue(bytes[start - 1]) {
        return None;
    }
    let mut cursor = start + 4;
    while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'{') {
        return None;
    }

    let mut depth = 0usize;
    while cursor < bytes.len() {
        if let Some(end) = skip_lua_quoted(bytes, cursor) {
            cursor = end;
            continue;
        }
        if bytes[cursor] == b'-' && bytes.get(cursor + 1) == Some(&b'-') {
            if let Some(end) = skip_lua_long_bracket(bytes, cursor + 2) {
                cursor = end;
            } else {
                cursor += 2;
                while cursor < bytes.len() && !matches!(bytes[cursor], b'\r' | b'\n') {
                    cursor += 1;
                }
            }
            continue;
        }
        if let Some(end) = skip_lua_long_bracket(bytes, cursor) {
            cursor = end;
            continue;
        }
        match bytes[cursor] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((cursor + 1, None));
                }
            }
            _ => {}
        }
        cursor += 1;
    }
    Some((bytes.len(), Some(LexErrorKind::UnterminatedLuaBlock)))
}

fn skip_lua_quoted(bytes: &[u8], start: usize) -> Option<usize> {
    let quote = *bytes.get(start)?;
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    let mut cursor = start + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor = (cursor + 2).min(bytes.len()),
            byte if byte == quote => return Some(cursor + 1),
            _ => cursor += 1,
        }
    }
    Some(bytes.len())
}

fn skip_lua_long_bracket(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start) != Some(&b'[') {
        return None;
    }
    let mut cursor = start + 1;
    while bytes.get(cursor) == Some(&b'=') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'[') {
        return None;
    }
    let level = cursor - start - 1;
    cursor += 1;
    while cursor < bytes.len() {
        if bytes[cursor] == b']' {
            let mut close = cursor + 1;
            let mut matched = 0usize;
            while matched < level && bytes.get(close) == Some(&b'=') {
                matched += 1;
                close += 1;
            }
            if matched == level && bytes.get(close) == Some(&b']') {
                return Some(close + 1);
            }
        }
        cursor += 1;
    }
    Some(bytes.len())
}

pub fn keyword_kind(word: &str) -> TokenKind {
    match word {
        "fn" => TokenKind::KwFn,
        "let" => TokenKind::KwLet,
        "mut" => TokenKind::KwMut,
        "if" => TokenKind::KwIf,
        "else" => TokenKind::KwElse,
        "while" => TokenKind::KwWhile,
        "loop" => TokenKind::KwLoop,
        "for" => TokenKind::KwFor,
        "in" => TokenKind::KwIn,
        "return" => TokenKind::KwReturn,
        "break" => TokenKind::KwBreak,
        "continue" => TokenKind::KwContinue,
        "dyn" => TokenKind::KwDyn,
        "true" => TokenKind::KwTrue,
        "false" => TokenKind::KwFalse,
        "struct" => TokenKind::KwStruct,
        "enum" => TokenKind::KwEnum,
        "trait" => TokenKind::KwTrait,
        "impl" => TokenKind::KwImpl,
        "pub" => TokenKind::KwPub,
        "use" => TokenKind::KwUse,
        "mod" => TokenKind::KwMod,
        "as" => TokenKind::KwAs,
        "match" => TokenKind::KwMatch,
        "self" => TokenKind::KwSelf,
        "extern" => TokenKind::KwExtern,
        _ => TokenKind::Ident,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn assert_lossless(source: &str) {
        let tokens = lex(source);
        let mut cursor = 0u32;
        for token in tokens {
            assert_eq!(token.range.start(), cursor);
            assert!(source.is_char_boundary(token.range.start() as usize));
            assert!(source.is_char_boundary(token.range.end() as usize));
            cursor = token.range.end();
        }
        assert_eq!(cursor as usize, source.len());
    }

    #[test]
    fn stream_is_lossless_for_unicode_and_errors() {
        assert_lossless("//! 注释\nfn main() { let x = \"值\"; 🙂 }");
    }

    #[test]
    fn numbers_cover_compiler_forms() {
        let kinds: Vec<_> = lex("0xff 0b10 .5 1. 1e-2 1..2")
            .into_iter()
            .filter(|token| !token.kind.is_trivia())
            .map(|token| token.kind)
            .collect();
        assert_eq!(
            kinds,
            [
                TokenKind::Int,
                TokenKind::Int,
                TokenKind::Float,
                TokenKind::Float,
                TokenKind::Float,
                TokenKind::Int,
                TokenKind::DotDot,
                TokenKind::Int,
            ]
        );
    }

    #[test]
    fn unterminated_constructs_are_structured_errors() {
        assert_eq!(lex("\"")[0].error, Some(LexErrorKind::UnterminatedString));
        assert_eq!(
            lex("/*")[0].error,
            Some(LexErrorKind::UnterminatedBlockComment)
        );
        assert_eq!(
            lex("lua! {")[0].error,
            Some(LexErrorKind::UnterminatedLuaBlock)
        );
    }

    #[test]
    fn lua_block_is_one_lossless_token_with_nested_literals() {
        let source = "lua! { local text = [[ { not a block } ]]; -- }\n local t = { ok = true } } let value = 1;";
        let tokens: Vec<_> = lex(source)
            .into_iter()
            .filter(|token| !token.kind.is_trivia())
            .collect();
        assert_eq!(tokens[0].kind, TokenKind::LuaBlock);
        assert_eq!(
            &source[tokens[0].range.start() as usize..tokens[0].range.end() as usize],
            "lua! { local text = [[ { not a block } ]]; -- }\n local t = { ok = true } }"
        );
        assert_eq!(tokens[1].kind, TokenKind::KwLet);
    }

    proptest! {
        #[test]
        fn arbitrary_unicode_is_lossless_and_monotonic(source in any::<String>()) {
            let tokens = lex(&source);
            let mut cursor = 0u32;
            for token in tokens {
                prop_assert_eq!(token.range.start(), cursor);
                prop_assert!(token.range.start() <= token.range.end());
                prop_assert!(source.is_char_boundary(token.range.start() as usize));
                prop_assert!(source.is_char_boundary(token.range.end() as usize));
                cursor = token.range.end();
            }
            prop_assert_eq!(cursor as usize, source.len());
        }
    }
}
