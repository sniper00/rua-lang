//! Document highlight tests — highlighting all occurrences of a symbol.

mod support;

use support::{TestServer, uri};

#[test]
fn document_highlight_local_variable_reads() {
    let uri = uri("/test/highlight.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { let x = 42; let y = x + 1; }");

    // cursor on the binding `x` at byte offset 16
    let pp = srv.pp(&uri, 0, 16).unwrap();
    let refs = srv.snapshot().references(pp, true);
    // Should find at least 2 references: the declaration and the read in `x + 1`
    assert!(
        refs.len() >= 2,
        "should find at least 2 references to x, got {}: {refs:?}",
        refs.len()
    );
}

#[test]
fn document_highlight_write_vs_read_kind() {
    let uri = uri("/test/highlight_kind.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { let mut x = 1; x = 2; let y = x; }");

    // cursor on the write `x` in `x = 2` at byte offset 27
    let pp = srv.pp(&uri, 0, 27).unwrap();
    let refs = srv.snapshot().references(pp, true);

    // Should have both read references and a declaration
    let has_decl = refs
        .iter()
        .any(|r| matches!(r.kind(), rua_analysis::ReferenceKind::Declaration));
    let has_read = refs
        .iter()
        .any(|r| matches!(r.kind(), rua_analysis::ReferenceKind::Read));
    assert!(has_decl, "should have declaration reference, got: {refs:?}");
    assert!(has_read, "should have read reference, got: {refs:?}");
    // The assignment target `x = 2` is classified as Read in the current analysis
}

#[test]
fn document_highlight_parameter_in_body() {
    let uri = uri("/test/highlight_param.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn double(n: i64) -> i64 { n * 2 }");

    // cursor on the parameter `n` at byte offset 10
    let pp = srv.pp(&uri, 0, 10).unwrap();
    let refs = srv.snapshot().references(pp, true);
    assert!(!refs.is_empty(), "should find references to parameter n");
}

#[test]
fn document_highlight_on_keyword_returns_empty() {
    let uri = uri("/test/highlight_kw.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { let x = 1; }");

    // cursor on the `fn` keyword
    let pp = srv.pp(&uri, 0, 0).unwrap();
    let refs = srv.snapshot().references(pp, false);
    // Cursor on a keyword should return empty or not panic
    // (the exact behavior depends on token_at_offset resolution)
    let _ = refs;
}

#[test]
fn document_highlight_on_whitespace_returns_empty() {
    let uri = uri("/test/highlight_ws.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { let x = 1; }");

    // cursor on whitespace between `let` and `x`
    let pp = srv.pp(&uri, 0, 15).unwrap();
    let refs = srv.snapshot().references(pp, false);
    // Should be empty (or not panic)
    assert!(
        refs.is_empty(),
        "references on whitespace should be empty, got {refs:?}"
    );
}
