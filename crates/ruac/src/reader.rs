//! Character reader over the source text.
//!
//! Mirrors lua-rs `parser/reader.rs`: a cursor with a "buffer" region (the span
//! of the token currently being built), `current_char`/`next_char` lookahead,
//! `bump`, and `eat_while`/`eat_when` helpers.

pub struct Reader<'a> {
    text: &'a str,
    bytes: &'a [u8],
    /// Start byte offset of the token currently being scanned.
    buff_start: usize,
    /// Current byte offset (cursor).
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(text: &'a str) -> Self {
        Reader {
            text,
            bytes: text.as_bytes(),
            buff_start: 0,
            pos: 0,
        }
    }

    pub fn is_eof(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    /// Current char, or `'\0'` at EOF. Rua source is ASCII-oriented for tokens;
    /// non-ASCII bytes only appear inside string/comment bodies and are consumed
    /// byte-wise, which is fine for span tracking.
    pub fn current_char(&self) -> char {
        if self.pos < self.bytes.len() {
            self.bytes[self.pos] as char
        } else {
            '\0'
        }
    }

    pub fn next_char(&self) -> char {
        if self.pos + 1 < self.bytes.len() {
            self.bytes[self.pos + 1] as char
        } else {
            '\0'
        }
    }

    pub fn bump(&mut self) {
        if self.pos < self.bytes.len() {
            self.pos += 1;
        }
    }

    /// Advance the cursor past one whole UTF-8 character. Unlike [`bump`], which
    /// steps a single byte (safe for the ASCII lexemes and byte-scanned
    /// string/comment bodies), this keeps `pos` on a char boundary when an
    /// unexpected multibyte char is encountered, so later slicing never panics.
    pub fn bump_char(&mut self) {
        if self.pos >= self.bytes.len() {
            return;
        }
        let lead = self.bytes[self.pos];
        let width = match lead {
            0x00..=0x7F => 1,
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            // Continuation or invalid lead byte: step one byte to make progress.
            _ => 1,
        };
        self.pos = (self.pos + width).min(self.bytes.len());
    }

    /// Begin a new token: mark the buffer start at the cursor.
    pub fn reset_buff(&mut self) {
        self.buff_start = self.pos;
    }

    pub fn buff_start(&self) -> usize {
        self.buff_start
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Text of the token currently being scanned (from `buff_start` to cursor).
    pub fn current_text(&self) -> &'a str {
        &self.text[self.buff_start..self.pos]
    }

    pub fn is_start_of_line(&self) -> bool {
        self.pos == 0 || matches!(self.bytes.get(self.pos - 1), Some(b'\n') | Some(b'\r'))
    }

    /// Consume while `pred` holds; returns the number of chars eaten.
    pub fn eat_while<F: Fn(char) -> bool>(&mut self, pred: F) -> usize {
        let mut n = 0;
        while !self.is_eof() && pred(self.current_char()) {
            self.bump();
            n += 1;
        }
        n
    }

    /// Consume a run of a specific char; returns the count.
    pub fn eat_when(&mut self, ch: char) -> usize {
        let mut n = 0;
        while self.current_char() == ch {
            self.bump();
            n += 1;
        }
        n
    }
}
