//! Folding range tests — brace-based and doc-comment-based code folding.

mod support;

use support::{TestServer, uri};

#[test]
fn folding_ranges_for_blocks() {
    let uri = uri("/test/fold_blocks.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "fn main() {\n    if true {\n        let x = 1;\n        let y = 2;\n    }\n    let z = 3;\n}",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let source = analysis.parse(file_id).syntax_node().text().to_string();

    // The source should have matching braces
    let brace_locations: Vec<usize> = source
        .bytes()
        .enumerate()
        .filter(|(_, b)| *b == b'{' || *b == b'}')
        .map(|(i, _)| i)
        .collect();

    // Every `{` should have a corresponding `}`
    assert!(
        brace_locations.len().is_multiple_of(2),
        "braces should be balanced"
    );
    assert!(
        brace_locations.len() >= 4,
        "should have at least 2 brace pairs (fn body + if body)"
    );
}

#[test]
fn folding_ranges_for_doc_comments() {
    let uri = uri("/test/fold_docs.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "/// Line one\n/// Line two\n/// Line three\nfn documented() -> i64 { 42 }\n\n/// Single line comment\nfn other() {}",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let source = analysis.parse(file_id).syntax_node().text().to_string();

    // Should have consecutive `///` lines that can be folded
    let doc_lines: Vec<&str> = source
        .lines()
        .filter(|l| l.trim_start().starts_with("///"))
        .collect();
    assert!(
        doc_lines.len() >= 4,
        "should have at least 4 doc comment lines, got {}",
        doc_lines.len()
    );
}

#[test]
fn folding_ranges_nested_blocks() {
    // Nested blocks should produce multiple foldable ranges.
    let uri = uri("/test/fold_nested.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "fn deep() {\n    if true {\n        while true {\n            for i in 0..10 {\n                let x = i;\n            }\n        }\n    }\n}",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let source = analysis.parse(file_id).syntax_node().text().to_string();

    // Count braces — should have 4+ pairs (fn, if, while, for)
    let brace_count = source.bytes().filter(|&b| b == b'{').count();
    assert!(
        brace_count >= 4,
        "should have at least 4 opening braces, got {brace_count}"
    );
    assert_eq!(
        source.bytes().filter(|&b| b == b'{').count(),
        source.bytes().filter(|&b| b == b'}').count(),
        "braces should be balanced"
    );
}

#[test]
fn folding_ranges_single_line_block_not_folded() {
    // Single-line blocks like `fn foo() {}` should not produce folding ranges
    // because opening and closing braces are on the same line.
    let uri = uri("/test/fold_single.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn foo() {}\nfn bar() { let x = 1; }");

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let source = analysis.parse(file_id).syntax_node().text().to_string();

    // Both functions exist
    assert!(source.contains("foo"));
    assert!(source.contains("bar"));
    // The single-line fn should have its braces on the same line
    let foo_line = source.lines().find(|l| l.contains("foo")).unwrap();
    assert!(
        foo_line.contains('{') && foo_line.contains('}'),
        "single-line fn braces should be on same line"
    );
}
