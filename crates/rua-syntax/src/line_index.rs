//! Byte-offset ↔ (line, column) conversion for LSP.
//!
//! Columns are counted in **UTF-16 code units**, which is the LSP default
//! position encoding. Following emmylua/rust-analyzer, each line records whether
//! it is pure ASCII so the common case is an O(1) subtraction; only lines that
//! contain non-ASCII bytes fall back to scanning for UTF-16 widths.

/// A precomputed index of line starts for one source string.
#[derive(Debug, Clone)]
pub struct LineIndex {
    /// Byte offset of the start of each line (line 0 starts at 0).
    line_starts: Vec<usize>,
    /// Whether each line contains only ASCII bytes (fast-path flag).
    line_ascii: Vec<bool>,
    len: usize,
}

impl LineIndex {
    /// Build an index over `text`. Recognizes `\n` line breaks (a preceding
    /// `\r` stays on the previous line, matching LSP's line counting).
    pub fn new(text: &str) -> LineIndex {
        let mut line_starts = vec![0usize];
        let mut line_ascii = Vec::new();
        let mut ascii = true;
        for (i, b) in text.bytes().enumerate() {
            if b >= 0x80 {
                ascii = false;
            }
            if b == b'\n' {
                line_starts.push(i + 1);
                line_ascii.push(ascii);
                ascii = true;
            }
        }
        line_ascii.push(ascii);
        LineIndex {
            line_starts,
            line_ascii,
            len: text.len(),
        }
    }

    /// Number of lines (always ≥ 1).
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// 0-based line containing `offset` (clamped to the last line for offsets at
    /// or past the end).
    pub fn line(&self, offset: usize) -> usize {
        match self.line_starts.binary_search(&offset) {
            Ok(line) => line,
            Err(next) => next - 1,
        }
    }

    /// Byte offset of the start of a 0-based `line` (`None` if out of range).
    pub fn line_start(&self, line: usize) -> Option<usize> {
        self.line_starts.get(line).copied()
    }

    /// Convert a byte `offset` into a 0-based `(line, utf16_col)` position.
    pub fn line_col(&self, offset: usize, text: &str) -> (usize, usize) {
        let offset = offset.min(self.len);
        let line = self.line(offset);
        let start = self.line_starts[line];
        let col = if self.line_ascii.get(line).copied().unwrap_or(false) {
            offset - start
        } else {
            text[start..offset].chars().map(char::len_utf16).sum()
        };
        (line, col)
    }

    /// Convert a 0-based `(line, utf16_col)` position back into a byte offset.
    /// Out-of-range positions clamp to the end of the requested line's content
    /// (excluding its trailing line break) or to the end of the string.
    pub fn offset(&self, line: usize, utf16_col: usize, text: &str) -> usize {
        let Some(&start) = self.line_starts.get(line) else {
            return self.len;
        };
        let content_end = self.line_content_end(line, text);
        if self.line_ascii.get(line).copied().unwrap_or(false) {
            return (start + utf16_col).min(content_end);
        }
        // Walk the line accumulating UTF-16 widths until we reach the column.
        let mut remaining = utf16_col;
        let mut byte = start;
        for ch in text[start..content_end].chars() {
            let w = ch.len_utf16();
            if remaining < w {
                break;
            }
            remaining -= w;
            byte += ch.len_utf8();
        }
        byte.min(content_end)
    }

    /// End of a line's visible content: the next line's start with a trailing
    /// `\n` (and optional preceding `\r`) removed.
    fn line_content_end(&self, line: usize, text: &str) -> usize {
        let end = self.line_starts.get(line + 1).copied().unwrap_or(self.len);
        let start = self.line_starts[line];
        let bytes = text.as_bytes();
        let mut e = end;
        if e > start && bytes[e - 1] == b'\n' {
            e -= 1;
            if e > start && bytes[e - 1] == b'\r' {
                e -= 1;
            }
        }
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_line_col_round_trip() {
        let text = "abc\ndef\n\nghi";
        let li = LineIndex::new(text);
        assert_eq!(li.line_count(), 4);
        assert_eq!(li.line_col(0, text), (0, 0));
        assert_eq!(li.line_col(1, text), (0, 1));
        assert_eq!(li.line_col(4, text), (1, 0)); // start of "def"
        assert_eq!(li.line_col(8, text), (2, 0)); // the empty line
        assert_eq!(li.line_col(9, text), (3, 0)); // start of "ghi"
        for off in 0..=text.len() {
            let (l, c) = li.line_col(off, text);
            assert_eq!(li.offset(l, c, text), off, "round trip at {off}");
        }
    }

    #[test]
    fn utf16_columns_for_non_ascii() {
        // `é` is 2 UTF-8 bytes but 1 UTF-16 unit; `𝄞` is 4 UTF-8 bytes, 2 UTF-16.
        let text = "aé𝄞b";
        let li = LineIndex::new(text);
        // Offsets: a=0, é=1..3, 𝄞=3..7, b=7..8
        assert_eq!(li.line_col(0, text), (0, 0)); // a
        assert_eq!(li.line_col(1, text), (0, 1)); // é
        assert_eq!(li.line_col(3, text), (0, 2)); // 𝄞 (after é: 1 unit)
        assert_eq!(li.line_col(7, text), (0, 4)); // b (é=1 + 𝄞=2 = 3, +a = 4)
        // Reverse conversions land on char boundaries.
        assert_eq!(li.offset(0, 1, text), 1);
        assert_eq!(li.offset(0, 2, text), 3);
        assert_eq!(li.offset(0, 4, text), 7);
    }

    #[test]
    fn out_of_range_clamps() {
        let text = "hi\nyo";
        let li = LineIndex::new(text);
        assert_eq!(li.line_col(999, text), li.line_col(text.len(), text));
        assert_eq!(li.offset(99, 0, text), text.len());
        assert_eq!(li.offset(0, 99, text), 2); // clamps to end of line 0
    }
}
