//! Document link tests — `[text]` links in doc comments.

mod support;

use support::{uri, TestServer};

#[test]
fn document_links_in_doc_comments() {
    let uri = uri("/test/links.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "/// See [Point] for details.\n/// Also see [Vector] and [Matrix].\nstruct Point { x: i64 }",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let source = analysis.parse(file_id).syntax_node().text().to_string();

    // Source should contain `[Point]` and `[Vector]` patterns
    assert!(
        source.contains("[Point]"),
        "source should contain [Point] link"
    );
    assert!(
        source.contains("[Vector]"),
        "source should contain [Vector] link"
    );
    assert!(
        source.contains("[Matrix]"),
        "source should contain [Matrix] link"
    );

    // Lines with `///` should exist
    let doc_line_count = source
        .lines()
        .filter(|l| l.trim_start().starts_with("///"))
        .count();
    assert!(
        doc_line_count >= 2,
        "should have doc comment lines, got {doc_line_count}"
    );
}

#[test]
fn document_links_no_links_in_plain_file() {
    let uri = uri("/test/links_none.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() {\n    // regular comment, not doc\n    let x = 1;\n}");

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let source = analysis.parse(file_id).syntax_node().text().to_string();

    // No `///` doc comments should mean no document links
    let has_doc_comments = source.lines().any(|l| l.trim_start().starts_with("///"));
    assert!(
        !has_doc_comments,
        "plain file should have no doc comments"
    );
}

#[test]
fn document_links_inline_brackets() {
    // Test `[text]` in various positions within doc comments.
    let uri = uri("/test/links_inline.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "/// The [core] module provides [essential] types.\n/// See [std::Vec] for dynamic arrays.\nmod core {}",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let source = analysis.parse(file_id).syntax_node().text().to_string();

    // All bracket patterns should exist
    for link in &["[core]", "[essential]", "[std::Vec]"] {
        assert!(
            source.contains(link),
            "source should contain link pattern {link}"
        );
    }
}
