//! Selection range tests — syntax-tree parent walking.

mod support;

use support::{uri, TestServer};

#[test]
fn selection_range_expands_from_token() {
    let uri = uri("/test/selrange.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { let x = 42; }");

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let parse = analysis.parse(file_id);
    let root = parse.syntax_node();
    let _source = root.text().to_string();

    // Find a token at some offset
    let offset: u32 = 16; // the `x` in `let x`
    let token = match root.token_at_offset(offset.into()) {
        rowan::TokenAtOffset::Single(t) => Some(t),
        rowan::TokenAtOffset::Between(l, _) => Some(l),
        _ => None,
    };

    assert!(token.is_some(), "should find a token at offset 16");

    // Walk up from token to root, collecting parent ranges
    let mut ranges = Vec::new();
    if let Some(t) = token {
        let mut current = t.parent();
        while let Some(node) = current {
            let rng = node.text_range();
            let start: u32 = rng.start().into();
            let end: u32 = rng.end().into();
            if start < end {
                ranges.push((start, end));
            }
            current = node.parent();
        }
    }

    // Each ancestor should be larger than or equal to the previous
    for w in ranges.windows(2) {
        let (s1, e1) = w[0];
        let (s2, e2) = w[1];
        assert!(
            s2 <= s1 && e2 >= e1,
            "parent range ({s2}, {e2}) should contain child ({s1}, {e1})"
        );
    }

    // Should have multiple levels of nesting
    assert!(
        ranges.len() >= 2,
        "should have at least 2 selection levels, got {}",
        ranges.len()
    );
}

#[test]
fn selection_range_top_level_item() {
    let uri = uri("/test/selrange_top.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn one() -> i64 { 1 }\nfn two() -> i64 { 2 }");

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let parse = analysis.parse(file_id);
    let root = parse.syntax_node();

    // Start at the first function name token
    let offset: u32 = 3; // 'o' in `one`
    let token = match root.token_at_offset(offset.into()) {
        rowan::TokenAtOffset::Single(t) => Some(t),
        rowan::TokenAtOffset::Between(l, _) => Some(l),
        _ => None,
    };

    assert!(token.is_some(), "should find a token at offset 3");

    // Walk up to the root
    let mut depth = 0;
    if let Some(t) = token {
        let mut current = t.parent();
        while let Some(node) = current {
            depth += 1;
            current = node.parent();
        }
    }

    assert!(depth >= 2, "should have at least 2 levels, got {depth}");
}

#[test]
fn selection_range_in_empty_file() {
    let uri = uri("/test/selrange_empty.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "");

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let parse = analysis.parse(file_id);
    let root = parse.syntax_node();

    // offset 0 in empty file — should return None for token
    let token = match root.token_at_offset(0.into()) {
        rowan::TokenAtOffset::Single(t) => Some(t),
        rowan::TokenAtOffset::Between(l, _) => Some(l),
        _ => None,
    };

    // Empty file may have a token or not; either way no panic.
    let _ = token;
}
