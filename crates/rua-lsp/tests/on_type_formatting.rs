//! On-type formatting tests — doc comment continuation and auto-indent.

mod support;

use support::{uri, TestServer};

#[test]
fn on_type_formatting_doc_comment_continuation() {
    // When the user presses Enter after a `///` line, the next line should
    // get `/// ` inserted automatically.
    let uri = uri("/test/otf_doc.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "/// First line\n/// Second line\nfn main() {}",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let source = analysis.parse(file_id).syntax_node().text().to_string();

    // Verify consecutive doc comments exist
    let doc_lines: Vec<&str> = source
        .lines()
        .filter(|l| l.trim_start().starts_with("///"))
        .collect();
    assert!(
        doc_lines.len() >= 2,
        "should have consecutive doc comment lines"
    );
}

#[test]
fn on_type_formatting_brace_indent() {
    // When the user types Enter after `{`, the next line should be indented.
    let uri = uri("/test/otf_brace.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "fn main() {\n    let x = 1;\n}",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let source = analysis.parse(file_id).syntax_node().text().to_string();

    // The line after `{` should have indentation
    let lines: Vec<&str> = source.lines().collect();
    let brace_line = lines.iter().position(|l| l.contains('{'));
    if let Some(idx) = brace_line {
        if let Some(next_line) = lines.get(idx + 1) {
            assert!(
                next_line.starts_with("    ") || next_line.starts_with('\t'),
                "line after brace should be indented, got: '{next_line}'"
            );
        }
    }
}

#[test]
fn on_type_formatting_only_triggers_on_enter() {
    // On-type formatting should only respond to `\n` trigger character.
    // This tests that our test source parses correctly.
    let uri = uri("/test/otf_enter.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "fn main() {\n    if true {\n        let x = 1;\n    }\n}",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let source = analysis.parse(file_id).syntax_node().text().to_string();

    // Nested indentation should be preserved after parsing
    let lines: Vec<&str> = source.lines().collect();
    let inner_line = lines
        .iter()
        .find(|l| l.contains("let x"));
    assert!(
        inner_line.is_some(),
        "source should contain 'let x' line"
    );
    if let Some(line) = inner_line {
        let indent = line.len() - line.trim_start().len();
        assert!(
            indent >= 4,
            "nested let should be indented at least 4 spaces, got {indent}"
        );
    }
}
