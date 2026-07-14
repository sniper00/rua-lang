//! Token cursor with two-token lookahead.
//!
//! Mirrors lua-rs `parser/mod.rs` `LuaLexer`: holds `current` + `next` tokens,
//! `bump()` advances, `peek_next()` looks one further. The parser drives this.

use crate::token::{RuaTokenKind, SourceRange, TokenData};
use crate::tokenize::{RuaTokenize, StrictTokenStream, TokenizeError};

pub struct RuaLexer<'a> {
    text: &'a str,
    tokenizer: RuaTokenize<'a>,
    current: TokenData,
    next: TokenData,
    previous: Option<SourceRange>,
}

impl<'a> RuaLexer<'a> {
    pub fn new(text: &'a str) -> Result<RuaLexer<'a>, TokenizeError> {
        Self::from_stream(StrictTokenStream::unlimited(text))
    }

    pub fn from_stream(stream: StrictTokenStream<'a>) -> Result<RuaLexer<'a>, TokenizeError> {
        let text = stream.text;
        let mut tokenizer = RuaTokenize::from_stream(stream);
        let current = tokenizer.next_token()?;
        let next = tokenizer.next_token()?;
        Ok(RuaLexer {
            text,
            tokenizer,
            current,
            next,
            previous: None,
        })
    }

    pub fn origin_text(&self) -> &'a str {
        self.text
    }

    pub fn current(&self) -> RuaTokenKind {
        self.current.kind
    }

    pub fn current_range(&self) -> SourceRange {
        self.current.range
    }

    pub fn current_line(&self) -> usize {
        self.current.range.line
    }

    pub fn previous_range(&self) -> SourceRange {
        self.previous.unwrap_or(SourceRange::EMPTY)
    }

    pub fn peek_next(&self) -> RuaTokenKind {
        self.next.kind
    }

    /// The source text covered by the current token.
    pub fn current_text(&self) -> &'a str {
        let r = self.current.range;
        &self.text[r.start..r.end()]
    }

    pub fn current_leading_trivia(&self) -> &'a str {
        let range = self.current.leading_trivia;
        &self.text[range.start..range.end()]
    }

    pub fn bump(&mut self) -> Result<(), TokenizeError> {
        self.previous = Some(self.current.range);
        self.current = self.next;
        if self.current.kind != RuaTokenKind::Eof {
            self.next = self.tokenizer.next_token()?;
        }
        Ok(())
    }
}
