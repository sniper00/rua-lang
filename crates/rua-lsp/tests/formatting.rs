//! Formatting tests — whole-document, range, and on-type formatting.

mod support;

use support::{uri, TestServer};

#[test]
fn formatting_produces_output() {
    let uri = uri("/test/fmt.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "fn   main()   {   let   x   =   1;   }",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let source = analysis.parse(file_id).syntax_node().text().to_string();

    // Call the formatter directly (same as LSP handler does)
    let formatted = rua_syntax::format::format_str(&source);
    // Formatted output should exist and not be empty
    assert!(!formatted.is_empty(), "formatted output should not be empty");
    // Formatted output should still contain the variable
    assert!(
        formatted.contains("let"),
        "formatted output should contain 'let', got: {formatted:?}"
    );
    assert!(
        formatted.contains("x"),
        "formatted output should contain 'x', got: {formatted:?}"
    );
}

#[test]
fn formatting_is_idempotent() {
    let uri = uri("/test/fmt_idem.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "fn   main()   {\n    let   x   =   1;\n    let   y   =   2;\n}",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let source = analysis.parse(file_id).syntax_node().text().to_string();

    let first = rua_syntax::format::format_str(&source);
    let second = rua_syntax::format::format_str(&first);

    assert_eq!(
        first, second,
        "formatting should be idempotent:\n--- first ---\n{first}\n--- second ---\n{second}"
    );
}

#[test]
fn formatting_preserves_comments() {
    let uri = uri("/test/fmt_comments.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "// top-level comment\nfn   main()   {\n    // inline comment\n    let   x   =   1;\n}",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let source = analysis.parse(file_id).syntax_node().text().to_string();

    let formatted = rua_syntax::format::format_str(&source);

    assert!(
        formatted.contains("// top-level comment"),
        "comments should survive formatting, got: {formatted:?}"
    );
    assert!(
        formatted.contains("// inline comment"),
        "inline comments should survive, got: {formatted:?}"
    );
}

#[test]
fn formatting_preserves_doc_comments() {
    let uri = uri("/test/fmt_doc.rs");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "/// A documented function.\n/// Returns the answer.\nfn   answer()   ->   i64   {   42   }",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let source = analysis.parse(file_id).syntax_node().text().to_string();

    let formatted = rua_syntax::format::format_str(&source);

    assert!(
        formatted.contains("/// A documented function"),
        "doc comments should survive, got: {formatted:?}"
    );
    assert!(
        formatted.contains("/// Returns the answer"),
        "doc comments should survive, got: {formatted:?}"
    );
}

#[test]
fn formatting_parse_error_returns_original() {
    let uri = uri("/test/fmt_err.rua");
    let mut srv = TestServer::new();
    let broken_source = "fn main() { let x: = ; }";
    srv.open(&uri, broken_source);

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let source = analysis.parse(file_id).syntax_node().text().to_string();

    let formatted = rua_syntax::format::format_str(&source);

    // Parse errors should return the original source unchanged
    assert_eq!(
        formatted.trim_end(),
        broken_source.trim_end(),
        "parse-error source should be returned unchanged"
    );
}
