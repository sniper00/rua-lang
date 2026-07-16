//! CST token adapter over the shared lossless lexer.

use rua_lex::{LexErrorKind, TokenKind};

use crate::kind::SyntaxKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LexToken {
    pub kind: SyntaxKind,
    pub start: usize,
    pub len: usize,
    pub error: Option<LexErrorKind>,
}

pub fn lex(text: &str) -> Vec<LexToken> {
    rua_lex::lex(text)
        .into_iter()
        .map(|token| LexToken {
            kind: syntax_kind(token.kind),
            start: token.range.start() as usize,
            len: token.range.len() as usize,
            error: token.error,
        })
        .collect()
}

const fn syntax_kind(kind: TokenKind) -> SyntaxKind {
    match kind {
        TokenKind::Whitespace => SyntaxKind::Whitespace,
        TokenKind::LineComment => SyntaxKind::LineComment,
        TokenKind::BlockComment => SyntaxKind::BlockComment,
        TokenKind::KwFn => SyntaxKind::KwFn,
        TokenKind::KwLet => SyntaxKind::KwLet,
        TokenKind::KwMut => SyntaxKind::KwMut,
        TokenKind::KwIf => SyntaxKind::KwIf,
        TokenKind::KwElse => SyntaxKind::KwElse,
        TokenKind::KwWhile => SyntaxKind::KwWhile,
        TokenKind::KwLoop => SyntaxKind::KwLoop,
        TokenKind::KwFor => SyntaxKind::KwFor,
        TokenKind::KwIn => SyntaxKind::KwIn,
        TokenKind::KwReturn => SyntaxKind::KwReturn,
        TokenKind::KwBreak => SyntaxKind::KwBreak,
        TokenKind::KwContinue => SyntaxKind::KwContinue,
        TokenKind::KwDyn => SyntaxKind::KwDyn,
        TokenKind::KwTrue => SyntaxKind::KwTrue,
        TokenKind::KwFalse => SyntaxKind::KwFalse,
        TokenKind::KwStruct => SyntaxKind::KwStruct,
        TokenKind::KwEnum => SyntaxKind::KwEnum,
        TokenKind::KwTrait => SyntaxKind::KwTrait,
        TokenKind::KwImpl => SyntaxKind::KwImpl,
        TokenKind::KwPub => SyntaxKind::KwPub,
        TokenKind::KwUse => SyntaxKind::KwUse,
        TokenKind::KwMod => SyntaxKind::KwMod,
        TokenKind::KwAs => SyntaxKind::KwAs,
        TokenKind::KwMatch => SyntaxKind::KwMatch,
        TokenKind::KwSelf => SyntaxKind::KwSelf,
        TokenKind::KwExtern => SyntaxKind::KwExtern,
        TokenKind::Ident => SyntaxKind::Ident,
        TokenKind::Int => SyntaxKind::Int,
        TokenKind::Float => SyntaxKind::Float,
        TokenKind::Str => SyntaxKind::Str,
        TokenKind::Plus => SyntaxKind::Plus,
        TokenKind::PlusEq => SyntaxKind::PlusEq,
        TokenKind::Minus => SyntaxKind::Minus,
        TokenKind::MinusEq => SyntaxKind::MinusEq,
        TokenKind::Star => SyntaxKind::Star,
        TokenKind::StarEq => SyntaxKind::StarEq,
        TokenKind::Slash => SyntaxKind::Slash,
        TokenKind::SlashEq => SyntaxKind::SlashEq,
        TokenKind::Percent => SyntaxKind::Percent,
        TokenKind::PercentEq => SyntaxKind::PercentEq,
        TokenKind::Eq => SyntaxKind::Eq,
        TokenKind::EqEq => SyntaxKind::EqEq,
        TokenKind::Ne => SyntaxKind::Ne,
        TokenKind::Lt => SyntaxKind::Lt,
        TokenKind::Le => SyntaxKind::Le,
        TokenKind::Gt => SyntaxKind::Gt,
        TokenKind::Ge => SyntaxKind::Ge,
        TokenKind::AndAnd => SyntaxKind::AndAnd,
        TokenKind::OrOr => SyntaxKind::OrOr,
        TokenKind::Not => SyntaxKind::Not,
        TokenKind::Amp => SyntaxKind::Amp,
        TokenKind::Pipe => SyntaxKind::Pipe,
        TokenKind::Question => SyntaxKind::Question,
        TokenKind::QuestionQuestion => SyntaxKind::QuestionQuestion,
        TokenKind::QuestionDot => SyntaxKind::QuestionDot,
        TokenKind::Arrow => SyntaxKind::Arrow,
        TokenKind::FatArrow => SyntaxKind::FatArrow,
        TokenKind::ColonColon => SyntaxKind::ColonColon,
        TokenKind::Colon => SyntaxKind::Colon,
        TokenKind::Semi => SyntaxKind::Semi,
        TokenKind::Comma => SyntaxKind::Comma,
        TokenKind::Dot => SyntaxKind::Dot,
        TokenKind::DotDot => SyntaxKind::DotDot,
        TokenKind::DotDotEq => SyntaxKind::DotDotEq,
        TokenKind::Hash => SyntaxKind::Hash,
        TokenKind::LParen => SyntaxKind::LParen,
        TokenKind::RParen => SyntaxKind::RParen,
        TokenKind::LBrace => SyntaxKind::LBrace,
        TokenKind::RBrace => SyntaxKind::RBrace,
        TokenKind::LBracket => SyntaxKind::LBracket,
        TokenKind::RBracket => SyntaxKind::RBracket,
        TokenKind::Eof => SyntaxKind::Eof,
        TokenKind::Unknown => SyntaxKind::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_shared_tokens_without_gaps() {
        let source = "//! doc\nfn main() { let value = .5; }";
        let tokens = lex(source);
        assert_eq!(
            tokens.iter().map(|token| token.len).sum::<usize>(),
            source.len()
        );
        assert!(tokens.iter().any(|token| token.kind == SyntaxKind::Float));
    }
}
