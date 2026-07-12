//! Text scanning utilities for the Rua syntax crate.

/// Return `true` when `b` is part of an identifier (`[A-Za-z0-9_]`).
pub fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Find `keyword` in `text` at a word boundary — the character before
/// the match (if any) and the character after the match must not be
/// alphanumeric or `_`.  This prevents matching inside identifiers
/// (e.g. `return_value`) or string literals.
pub fn word_boundary_find(text: &str, keyword: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let kw_bytes = keyword.as_bytes();
    let mut search_from = 0;
    loop {
        let pos = text[search_from..].find(keyword)?;
        let abs_pos = search_from + pos;
        // Check left boundary.
        if abs_pos > 0 {
            let before = bytes[abs_pos - 1];
            if is_ident_byte(before) {
                search_from = abs_pos + 1;
                continue;
            }
        }
        // Check right boundary.
        let after_idx = abs_pos + kw_bytes.len();
        if after_idx < bytes.len() {
            let after = bytes[after_idx];
            if is_ident_byte(after) {
                search_from = abs_pos + 1;
                continue;
            }
        }
        return Some(abs_pos);
    }
}
