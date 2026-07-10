//! `SyntaxKind`: the single flat kind enum for the Rua rowan CST.
//!
//! It merges three families into one `#[repr(u16)]` enum, as required by rowan:
//!   1. **trivia** tokens (whitespace, comments) — retained in the tree so the
//!      formatter/LSP see the exact source (`node.text() == source`),
//!   2. **real** tokens — one variant per [`ruac::token::RuaTokenKind`],
//!   3. **node** kinds — the grammar productions the CST parser (P6-1) builds.
//!
//! Token classification (keywords/operators/literals) is owned by this crate;
//! [`from_token`] is the single conversion point, so the CST cannot drift from
//! the semantic lexer.

use ruac::token::RuaTokenKind;

#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum SyntaxKind {
    // --- Trivia (only present in the CST, never produced by the semantic lexer) ---
    Whitespace,
    LineComment,
    BlockComment,

    // --- Real tokens (mirror of RuaTokenKind) ---
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
    // Literals / identifiers
    Ident,
    Int,
    Float,
    Str,
    // Punctuation / operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
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
    Arrow,
    FatArrow,
    ColonColon,
    Colon,
    Semi,
    Comma,
    Dot,
    DotDot,
    DotDotEq,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Eof,
    /// Lexer/parser error placeholder token (unrecognized input, recovery).
    Error,

    // --- Nodes (grammar productions; built by the CST parser in P6-1) ---
    /// Root node wrapping the whole file.
    SourceFile,
    // Items
    FnDecl,
    StructDecl,
    EnumDecl,
    TraitDecl,
    ImplDecl,
    UseDecl,
    ModDecl,
    ExternBlock,
    // Item pieces
    ParamList,
    Param,
    GenericParams,
    GenericParam,
    WhereClause,
    FieldDecl,
    FieldList,
    EnumVariant,
    VariantList,
    TraitMethod,
    ExternFn,
    // Types
    PathType,
    RefType,
    TupleType,
    TypeArgs,
    // Statements
    Block,
    LetStmt,
    ExprStmt,
    // Expressions
    BinExpr,
    UnaryExpr,
    RangeExpr,
    AssignExpr,
    TryExpr,
    CallExpr,
    MethodCallExpr,
    FieldExpr,
    IndexExpr,
    PathExpr,
    LiteralExpr,
    ParenExpr,
    IfExpr,
    MatchExpr,
    MatchArm,
    LoopExpr,
    WhileExpr,
    ForExpr,
    ReturnExpr,
    BreakExpr,
    ContinueExpr,
    StructLitExpr,
    FieldInit,
    ArrayExpr,
    ClosureExpr,
    MacroCallExpr,
    ArgList,
    // Patterns
    Pattern,
    // A generic error node the parser emits during recovery.
    ErrorNode,

    /// Sentinel marking the highest discriminant; must remain last.
    #[doc(hidden)]
    __Last,
}

impl SyntaxKind {
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            SyntaxKind::Whitespace | SyntaxKind::LineComment | SyntaxKind::BlockComment
        )
    }

    pub fn is_comment(self) -> bool {
        matches!(self, SyntaxKind::LineComment | SyntaxKind::BlockComment)
    }
}

/// Map a semantic-lexer token kind to its CST token kind. Single source of truth
/// for real-token classification (keeps the CST from drifting).
pub fn from_token(k: RuaTokenKind) -> SyntaxKind {
    use RuaTokenKind as T;
    match k {
        T::KwFn => SyntaxKind::KwFn,
        T::KwLet => SyntaxKind::KwLet,
        T::KwMut => SyntaxKind::KwMut,
        T::KwIf => SyntaxKind::KwIf,
        T::KwElse => SyntaxKind::KwElse,
        T::KwWhile => SyntaxKind::KwWhile,
        T::KwLoop => SyntaxKind::KwLoop,
        T::KwFor => SyntaxKind::KwFor,
        T::KwIn => SyntaxKind::KwIn,
        T::KwReturn => SyntaxKind::KwReturn,
        T::KwBreak => SyntaxKind::KwBreak,
        T::KwContinue => SyntaxKind::KwContinue,
        T::KwTrue => SyntaxKind::KwTrue,
        T::KwFalse => SyntaxKind::KwFalse,
        T::KwStruct => SyntaxKind::KwStruct,
        T::KwEnum => SyntaxKind::KwEnum,
        T::KwTrait => SyntaxKind::KwTrait,
        T::KwImpl => SyntaxKind::KwImpl,
        T::KwPub => SyntaxKind::KwPub,
        T::KwUse => SyntaxKind::KwUse,
        T::KwMod => SyntaxKind::KwMod,
        T::KwAs => SyntaxKind::KwAs,
        T::KwMatch => SyntaxKind::KwMatch,
        T::KwSelf => SyntaxKind::KwSelf,
        T::KwExtern => SyntaxKind::KwExtern,
        T::Ident => SyntaxKind::Ident,
        T::Int => SyntaxKind::Int,
        T::Float => SyntaxKind::Float,
        T::Str => SyntaxKind::Str,
        T::Plus => SyntaxKind::Plus,
        T::Minus => SyntaxKind::Minus,
        T::Star => SyntaxKind::Star,
        T::Slash => SyntaxKind::Slash,
        T::Percent => SyntaxKind::Percent,
        T::Eq => SyntaxKind::Eq,
        T::EqEq => SyntaxKind::EqEq,
        T::Ne => SyntaxKind::Ne,
        T::Lt => SyntaxKind::Lt,
        T::Le => SyntaxKind::Le,
        T::Gt => SyntaxKind::Gt,
        T::Ge => SyntaxKind::Ge,
        T::AndAnd => SyntaxKind::AndAnd,
        T::OrOr => SyntaxKind::OrOr,
        T::Not => SyntaxKind::Not,
        T::Amp => SyntaxKind::Amp,
        T::Pipe => SyntaxKind::Pipe,
        T::Question => SyntaxKind::Question,
        T::Arrow => SyntaxKind::Arrow,
        T::FatArrow => SyntaxKind::FatArrow,
        T::ColonColon => SyntaxKind::ColonColon,
        T::Colon => SyntaxKind::Colon,
        T::Semi => SyntaxKind::Semi,
        T::Comma => SyntaxKind::Comma,
        T::Dot => SyntaxKind::Dot,
        T::DotDot => SyntaxKind::DotDot,
        T::DotDotEq => SyntaxKind::DotDotEq,
        T::LParen => SyntaxKind::LParen,
        T::RParen => SyntaxKind::RParen,
        T::LBrace => SyntaxKind::LBrace,
        T::RBrace => SyntaxKind::RBrace,
        T::LBracket => SyntaxKind::LBracket,
        T::RBracket => SyntaxKind::RBracket,
        T::Eof => SyntaxKind::Eof,
        T::Unknown => SyntaxKind::Error,
    }
}

/// The rowan `Language` marker binding [`SyntaxKind`] to the CST.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuaLanguage {}

impl rowan::Language for RuaLanguage {
    type Kind = SyntaxKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> SyntaxKind {
        assert!(raw.0 <= SyntaxKind::__Last as u16, "invalid SyntaxKind raw value");
        // Safe: discriminants are the contiguous range `0..=__Last` because the
        // enum uses default discriminants, and the assert bounds `raw`.
        unsafe { std::mem::transmute::<u16, SyntaxKind>(raw.0) }
    }

    fn kind_to_raw(kind: SyntaxKind) -> rowan::SyntaxKind {
        rowan::SyntaxKind(kind as u16)
    }
}

impl From<SyntaxKind> for rowan::SyntaxKind {
    fn from(kind: SyntaxKind) -> Self {
        rowan::SyntaxKind(kind as u16)
    }
}

pub type SyntaxNode = rowan::SyntaxNode<RuaLanguage>;
pub type SyntaxToken = rowan::SyntaxToken<RuaLanguage>;
pub type SyntaxElement = rowan::SyntaxElement<RuaLanguage>;
