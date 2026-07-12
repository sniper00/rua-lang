//! Token kinds, source spans, and token data.
//!
//! Structure mirrors the lua-rs parser (`crates/luars/src/compiler/parser`):
//! a `SourceRange` span per token plus a flat `RuaTokenKind` enum, adapted to
//! the Rust-subset syntax that Rua accepts.

/// Byte-offset span into the source, plus the 1-based line the token starts on
/// and the id of the source file it came from (index into the compile-time file
/// registry; `0` is the root/primary source).
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

    pub fn new(start: usize, len: usize, line: usize) -> Self {
        SourceRange {
            start,
            len,
            line,
            file: 0,
        }
    }

    pub fn end(&self) -> usize {
        self.start + self.len
    }
}

/// A single lexed token: its kind and where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenData {
    pub kind: RuaTokenKind,
    pub range: SourceRange,
}

impl TokenData {
    pub fn new(kind: RuaTokenKind, range: SourceRange) -> Self {
        TokenData { kind, range }
    }
}

#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuaTokenKind {
    // Keywords
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
    KwDyn,

    // Literals / identifiers
    Ident,
    Int,
    Float,
    Str,

    // Punctuation / operators
    Plus,      // +
    Minus,     // -
    Star,      // *
    Slash,     // /
    Percent,   // %
    Eq,        // =
    EqEq,      // ==
    Ne,        // !=
    Lt,        // <
    Le,        // <=
    Gt,        // >
    Ge,        // >=
    AndAnd,    // &&
    OrOr,      // ||
    Not,       // !
    Amp,       // &
    Pipe,      // |
    Question,  // ?
    Arrow,     // ->
    FatArrow,  // =>
    ColonColon, // ::
    Colon,     // :
    Semi,      // ;
    Comma,     // ,
    Dot,       // .
    DotDot,    // ..
    DotDotEq,  // ..=
    LParen,    // (
    RParen,    // )
    LBrace,    // {
    RBrace,    // }
    LBracket,  // [
    RBracket,  // ]

    Eof,
    Unknown,
}

impl RuaTokenKind {
    /// Human-readable rendering, used in diagnostics (cf. lua-rs `to_user_string`).
    pub fn to_user_string(self) -> &'static str {
        use RuaTokenKind::*;
        match self {
            KwFn => "fn",
            KwLet => "let",
            KwMut => "mut",
            KwIf => "if",
            KwElse => "else",
            KwWhile => "while",
            KwLoop => "loop",
            KwFor => "for",
            KwIn => "in",
            KwReturn => "return",
            KwBreak => "break",
            KwContinue => "continue",
            KwTrue => "true",
            KwFalse => "false",
            KwStruct => "struct",
            KwEnum => "enum",
            KwTrait => "trait",
            KwImpl => "impl",
            KwPub => "pub",
            KwUse => "use",
            KwMod => "mod",
            KwAs => "as",
            KwMatch => "match",
            KwSelf => "self",
            KwExtern => "extern",
            KwDyn => "dyn",
            Ident => "<ident>",
            Int => "<int>",
            Float => "<float>",
            Str => "<string>",
            Plus => "+",
            Minus => "-",
            Star => "*",
            Slash => "/",
            Percent => "%",
            Eq => "=",
            EqEq => "==",
            Ne => "!=",
            Lt => "<",
            Le => "<=",
            Gt => ">",
            Ge => ">=",
            AndAnd => "&&",
            OrOr => "||",
            Not => "!",
            Amp => "&",
            Pipe => "|",
            Question => "?",
            Arrow => "->",
            FatArrow => "=>",
            ColonColon => "::",
            Colon => ":",
            Semi => ";",
            Comma => ",",
            Dot => ".",
            DotDot => "..",
            DotDotEq => "..=",
            LParen => "(",
            RParen => ")",
            LBrace => "{",
            RBrace => "}",
            LBracket => "[",
            RBracket => "]",
            Eof => "<eof>",
            Unknown => "<unknown>",
        }
    }
}

/// Map an identifier string to its keyword kind, or `Ident` otherwise
/// (cf. lua-rs `name_to_kind`).
pub fn keyword_kind(name: &str) -> RuaTokenKind {
    use RuaTokenKind::*;
    match name {
        "fn" => KwFn,
        "let" => KwLet,
        "mut" => KwMut,
        "if" => KwIf,
        "else" => KwElse,
        "while" => KwWhile,
        "loop" => KwLoop,
        "for" => KwFor,
        "in" => KwIn,
        "return" => KwReturn,
        "break" => KwBreak,
        "continue" => KwContinue,
        "true" => KwTrue,
        "false" => KwFalse,
        "struct" => KwStruct,
        "enum" => KwEnum,
        "trait" => KwTrait,
        "impl" => KwImpl,
        "pub" => KwPub,
        "use" => KwUse,
        "mod" => KwMod,
        "as" => KwAs,
        "match" => KwMatch,
        "self" => KwSelf,
        "extern" => KwExtern,
        "dyn" => KwDyn,
        _ => Ident,
    }
}
