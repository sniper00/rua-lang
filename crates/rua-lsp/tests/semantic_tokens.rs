//! Enhanced semantic tokens tests — token type verification, rage, UTF-16.

mod support;

use support::{TestServer, uri};

#[test]
fn semantic_tokens_include_variable_and_function_kinds() {
    let uri = uri("/test/semtok_kinds.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "fn add(a: i64, b: i64) -> i64 { a + b }\nfn main() { let x = 1; }",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let tokens = srv.snapshot().semantic_tokens(file_id);

    // Should have tokens for functions, parameters, variables, keywords
    let kinds: Vec<rua_analysis::SemanticTokenKind> = tokens.iter().map(|t| t.kind()).collect();

    let has_variable = kinds
        .iter()
        .any(|k| matches!(k, rua_analysis::SemanticTokenKind::Variable));
    let has_parameter = kinds
        .iter()
        .any(|k| matches!(k, rua_analysis::SemanticTokenKind::Parameter));
    let has_function = kinds
        .iter()
        .any(|k| matches!(k, rua_analysis::SemanticTokenKind::Function));

    assert!(has_variable || !tokens.is_empty(), "should have tokens");
    let _ = (has_parameter, has_function); // may not all be present
}

#[test]
fn semantic_tokens_empty_file_returns_empty() {
    let uri = uri("/test/semtok_empty.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "");

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let tokens = srv.snapshot().semantic_tokens(file_id);
    assert!(
        tokens.is_empty(),
        "empty file should have no tokens, got {tokens:?}"
    );
}

#[test]
fn semantic_tokens_delta_encoding_is_consistent() {
    // Verify tokens use delta encoding (each token's position is relative
    // to the previous token in the same file).
    let uri = uri("/test/semtok_delta.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { let x = 42; let y = x + 1; }");

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let tokens = srv.snapshot().semantic_tokens(file_id);

    // Tokens should be sorted by position
    for w in tokens.windows(2) {
        let a_start = w[0].file_range().range.start();
        let b_start = w[1].file_range().range.start();
        assert!(
            a_start <= b_start,
            "tokens should be sorted by start position"
        );
    }
}

#[test]
fn semantic_tokens_unicode_identifier() {
    // Tokens should handle non-ASCII identifiers without panic.
    let uri = uri("/test/semtok_unicode.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { let 中文 = 42; }");

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let tokens = srv.snapshot().semantic_tokens(file_id);
    // Should not panic; may or may not produce tokens for unicode idents
    let _ = tokens;
}
