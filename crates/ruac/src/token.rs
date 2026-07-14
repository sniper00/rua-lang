//! Compiler token data built on the shared lossless token kind.

pub use rua_lex::{TokenKind as RuaTokenKind, keyword_kind};

/// Byte-offset span plus the 1-based line and compiler source-file index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceRange {
    pub start: usize,
    pub len: usize,
    pub line: usize,
    pub file: u32,
}

impl SourceRange {
    pub const EMPTY: SourceRange = SourceRange {
        start: 0,
        len: 0,
        line: 0,
        file: 0,
    };

    pub const fn new(start: usize, len: usize, line: usize) -> Self {
        SourceRange {
            start,
            len,
            line,
            file: 0,
        }
    }

    pub const fn end(&self) -> usize {
        self.start + self.len
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenData {
    pub kind: RuaTokenKind,
    pub range: SourceRange,
    pub leading_trivia: SourceRange,
}

impl TokenData {
    pub const fn new(kind: RuaTokenKind, range: SourceRange, leading_trivia: SourceRange) -> Self {
        TokenData {
            kind,
            range,
            leading_trivia,
        }
    }
}
