//! Tokenizer: turns the character stream into `TokenData`.
//!
//! Mirrors lua-rs `parser/lua_tokenize.rs`: trivia (whitespace/newlines/
//! comments) is skipped in `next_token`, then a single real token is lexed by
//! `lex_one`. Adapted to Rust-subset lexemes (`->`, `=>`, `::`, `..`, `..=`,
//! `&&`, `||`, `//` line comments, `/* */` block comments).

use crate::reader::Reader;
use crate::token::{RuaTokenKind, SourceRange, TokenData, keyword_kind};

pub struct RuaTokenize<'a> {
    reader: Reader<'a>,
    line: usize,
    error: Option<String>,
}

impl<'a> RuaTokenize<'a> {
    pub fn new(text: &'a str) -> Self {
        RuaTokenize {
            reader: Reader::new(text),
            line: 1,
            error: None,
        }
    }

    pub fn take_error(&mut self) -> Option<String> {
        self.error.take()
    }

    /// Produce the next meaningful token, skipping whitespace and comments.
    pub fn next_token(&mut self) -> Result<TokenData, String> {
        self.skip_trivia();
        if self.error.is_some() {
            return Err(format!("{}: {}", self.line, self.error.take().unwrap()));
        }
        if self.reader.is_eof() {
            let at = self.reader.pos();
            return Ok(TokenData::new(
                RuaTokenKind::Eof,
                SourceRange::new(at, 0, self.line),
            ));
        }

        self.reader.reset_buff();
        let start = self.reader.buff_start();
        let line = self.line;
        let kind = self.lex_one();

        if let Some(err) = &self.error {
            return Err(format!("{}: {}", line, err));
        }

        let len = self.reader.pos() - start;
        Ok(TokenData::new(kind, SourceRange::new(start, len, line)))
    }

    fn error<F: FnOnce() -> String>(&mut self, f: F) {
        if self.error.is_none() {
            self.error = Some(f());
        }
    }

    /// Consume whitespace, newlines, `//` line comments and `/* */` block
    /// comments (nested) until the next real token or EOF.
    fn skip_trivia(&mut self) {
        loop {
            match self.reader.current_char() {
                ' ' | '\t' | '\x0B' | '\x0C' => {
                    self.reader
                        .eat_while(|c| matches!(c, ' ' | '\t' | '\x0B' | '\x0C'));
                }
                '\n' | '\r' => self.lex_newline(),
                '/' if self.reader.next_char() == '/' => {
                    self.reader.eat_while(|c| c != '\n' && c != '\r');
                }
                '/' if self.reader.next_char() == '*' => {
                    self.reader.bump();
                    self.reader.bump();
                    self.lex_block_comment();
                    if self.error.is_some() {
                        return;
                    }
                }
                _ => break,
            }
        }
    }

    /// Lex a single real token (no trivia).
    fn lex_one(&mut self) -> RuaTokenKind {
        use RuaTokenKind::*;
        match self.reader.current_char() {
            '+' => {
                self.reader.bump();
                Plus
            }
            '-' => {
                self.reader.bump();
                if self.reader.current_char() == '>' {
                    self.reader.bump();
                    Arrow
                } else {
                    Minus
                }
            }
            '*' => {
                self.reader.bump();
                Star
            }
            '/' => {
                self.reader.bump();
                Slash
            }
            '%' => {
                self.reader.bump();
                Percent
            }
            '=' => {
                self.reader.bump();
                match self.reader.current_char() {
                    '=' => {
                        self.reader.bump();
                        EqEq
                    }
                    '>' => {
                        self.reader.bump();
                        FatArrow
                    }
                    _ => Eq,
                }
            }
            '!' => {
                self.reader.bump();
                if self.reader.current_char() == '=' {
                    self.reader.bump();
                    Ne
                } else {
                    Not
                }
            }
            '<' => {
                self.reader.bump();
                if self.reader.current_char() == '=' {
                    self.reader.bump();
                    Le
                } else {
                    Lt
                }
            }
            '>' => {
                self.reader.bump();
                if self.reader.current_char() == '=' {
                    self.reader.bump();
                    Ge
                } else {
                    Gt
                }
            }
            '&' => {
                self.reader.bump();
                if self.reader.current_char() == '&' {
                    self.reader.bump();
                    AndAnd
                } else {
                    Amp
                }
            }
            '|' => {
                self.reader.bump();
                if self.reader.current_char() == '|' {
                    self.reader.bump();
                    OrOr
                } else {
                    Pipe
                }
            }
            '?' => {
                self.reader.bump();
                Question
            }
            ':' => {
                self.reader.bump();
                if self.reader.current_char() == ':' {
                    self.reader.bump();
                    ColonColon
                } else {
                    Colon
                }
            }
            ';' => {
                self.reader.bump();
                Semi
            }
            ',' => {
                self.reader.bump();
                Comma
            }
            '.' => {
                if self.reader.next_char().is_ascii_digit() {
                    return self.lex_number();
                }
                self.reader.bump();
                if self.reader.current_char() == '.' {
                    self.reader.bump();
                    if self.reader.current_char() == '=' {
                        self.reader.bump();
                        DotDotEq
                    } else {
                        DotDot
                    }
                } else {
                    Dot
                }
            }
            '(' => {
                self.reader.bump();
                LParen
            }
            ')' => {
                self.reader.bump();
                RParen
            }
            '{' => {
                self.reader.bump();
                LBrace
            }
            '}' => {
                self.reader.bump();
                RBrace
            }
            '[' => {
                self.reader.bump();
                LBracket
            }
            ']' => {
                self.reader.bump();
                RBracket
            }
            '"' => self.lex_string(),
            '0'..='9' => self.lex_number(),
            c if is_name_start(c) => {
                self.reader.bump();
                self.reader.eat_while(is_name_continue);
                keyword_kind(self.reader.current_text())
            }
            _ => {
                // Unrecognized byte(s). Advance by a whole UTF-8 char so the
                // token span never ends inside a multibyte character (e.g. a
                // stray CJK char), which would panic any later source slice.
                self.reader.bump_char();
                Unknown
            }
        }
    }

    fn lex_newline(&mut self) {
        match self.reader.current_char() {
            '\n' => {
                self.reader.bump();
                if self.reader.current_char() == '\r' {
                    self.reader.bump();
                }
            }
            '\r' => {
                self.reader.bump();
                if self.reader.current_char() == '\n' {
                    self.reader.bump();
                }
            }
            _ => {}
        }
        self.line += 1;
    }

    fn lex_block_comment(&mut self) {
        // Supports nested /* */.
        let mut depth = 1usize;
        while !self.reader.is_eof() && depth > 0 {
            match self.reader.current_char() {
                '/' if self.reader.next_char() == '*' => {
                    self.reader.bump();
                    self.reader.bump();
                    depth += 1;
                }
                '*' if self.reader.next_char() == '/' => {
                    self.reader.bump();
                    self.reader.bump();
                    depth -= 1;
                }
                '\n' | '\r' => self.lex_newline(),
                _ => self.reader.bump(),
            }
        }
        if depth != 0 {
            self.error(|| "unterminated block comment".to_string());
        }
    }

    fn lex_string(&mut self) -> RuaTokenKind {
        self.reader.bump(); // opening quote
        while !self.reader.is_eof() {
            match self.reader.current_char() {
                '"' => {
                    self.reader.bump();
                    return RuaTokenKind::Str;
                }
                '\\' => {
                    self.reader.bump();
                    // Consume the escaped char (basic handling; validation is
                    // left to a later pass).
                    if !self.reader.is_eof() {
                        self.reader.bump();
                    }
                }
                '\n' | '\r' => {
                    self.error(|| "unterminated string".to_string());
                    return RuaTokenKind::Str;
                }
                _ => self.reader.bump(),
            }
        }
        self.error(|| "unterminated string near <eof>".to_string());
        RuaTokenKind::Str
    }

    fn lex_number(&mut self) -> RuaTokenKind {
        let mut is_float = false;
        // hex / bin integer prefixes
        if self.reader.current_char() == '0'
            && matches!(self.reader.next_char(), 'x' | 'X' | 'b' | 'B')
        {
            self.reader.bump();
            self.reader.bump();
            self.reader
                .eat_while(|c| c.is_ascii_alphanumeric() || c == '_');
            return RuaTokenKind::Int;
        }

        self.reader.eat_while(|c| c.is_ascii_digit() || c == '_');

        // fractional part: `.` not part of a range `..` and not a field access
        if self.reader.current_char() == '.'
            && self.reader.next_char() != '.'
            && !is_name_start(self.reader.next_char())
        {
            is_float = true;
            self.reader.bump();
            self.reader.eat_while(|c| c.is_ascii_digit() || c == '_');
        }

        // exponent
        if matches!(self.reader.current_char(), 'e' | 'E') {
            is_float = true;
            self.reader.bump();
            if matches!(self.reader.current_char(), '+' | '-') {
                self.reader.bump();
            }
            self.reader.eat_while(|c| c.is_ascii_digit() || c == '_');
        }

        if is_float {
            RuaTokenKind::Float
        } else {
            RuaTokenKind::Int
        }
    }
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_name_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}
