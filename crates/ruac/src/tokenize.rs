//! Strict compiler token cursor over the shared lossless lexer.

use rua_lex::{LexErrorKind, LexToken, TokenLimitError, lex, lex_with_limit};
use std::fmt;

use crate::token::{RuaTokenKind, SourceRange, TokenData};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenizeError {
    pub kind: LexErrorKind,
    pub range: SourceRange,
}

pub struct StrictTokenStream<'a> {
    pub(crate) text: &'a str,
    tokens: Vec<LexToken>,
}

impl<'a> StrictTokenStream<'a> {
    pub fn new(text: &'a str, max_tokens: usize) -> Result<Self, TokenLimitError> {
        Ok(Self {
            text,
            tokens: lex_with_limit(text, max_tokens)?,
        })
    }

    pub fn unlimited(text: &'a str) -> Self {
        Self {
            text,
            tokens: lex(text),
        }
    }
}

impl fmt::Display for TokenizeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.range.line, self.kind.message())
    }
}

pub struct RuaTokenize<'a> {
    text: &'a str,
    tokens: Vec<LexToken>,
    cursor: usize,
    line: usize,
}

impl<'a> RuaTokenize<'a> {
    pub fn new(text: &'a str) -> Self {
        Self::from_stream(StrictTokenStream::unlimited(text))
    }

    pub fn from_stream(stream: StrictTokenStream<'a>) -> Self {
        Self {
            text: stream.text,
            tokens: stream.tokens,
            cursor: 0,
            line: 1,
        }
    }

    pub fn next_token(&mut self) -> Result<TokenData, TokenizeError> {
        let trivia_start = self
            .tokens
            .get(self.cursor)
            .map_or(self.text.len(), |token| token.range.start() as usize);
        let trivia_line = self.line;
        self.skip_trivia()?;
        let trivia_end = self
            .tokens
            .get(self.cursor)
            .map_or(self.text.len(), |token| token.range.start() as usize);
        let leading_trivia = SourceRange::new(trivia_start, trivia_end - trivia_start, trivia_line);
        let Some(token) = self.tokens.get(self.cursor).copied() else {
            return Ok(TokenData::new(
                RuaTokenKind::Eof,
                SourceRange::new(self.text.len(), 0, self.line),
                leading_trivia,
            ));
        };

        let line = self.line;
        self.cursor += 1;
        self.advance_lines(token);

        if let Some(error) = token.error
            && error != LexErrorKind::UnknownCharacter
        {
            return Err(TokenizeError {
                kind: error,
                range: SourceRange::new(
                    token.range.start() as usize,
                    token.range.len() as usize,
                    line,
                ),
            });
        }

        Ok(TokenData::new(
            token.kind,
            SourceRange::new(
                token.range.start() as usize,
                token.range.len() as usize,
                line,
            ),
            leading_trivia,
        ))
    }

    fn skip_trivia(&mut self) -> Result<(), TokenizeError> {
        while let Some(token) = self.tokens.get(self.cursor).copied() {
            if !token.kind.is_trivia() {
                break;
            }
            let line = self.line;
            self.cursor += 1;
            self.advance_lines(token);
            if let Some(error) = token.error {
                return Err(TokenizeError {
                    kind: error,
                    range: SourceRange::new(
                        token.range.start() as usize,
                        token.range.len() as usize,
                        line,
                    ),
                });
            }
        }
        Ok(())
    }

    fn advance_lines(&mut self, token: LexToken) {
        let text = &self.text[token.range.start() as usize..token.range.end() as usize];
        let bytes = text.as_bytes();
        let mut cursor = 0;
        while cursor < bytes.len() {
            match bytes[cursor] {
                b'\r' => {
                    self.line += 1;
                    cursor += usize::from(bytes.get(cursor + 1) == Some(&b'\n')) + 1;
                }
                b'\n' => {
                    self.line += 1;
                    cursor += 1;
                }
                _ => cursor += 1,
            }
        }
    }
}
