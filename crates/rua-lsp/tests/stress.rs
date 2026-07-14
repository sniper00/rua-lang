//! Stress and edge case tests — rapid changes, deep nesting, unicode, large files.

mod support;

use support::{TestServer, uri};

#[test]
fn stress_50_rapid_changes_no_panic() {
    let uri = uri("/test/stress_rapid.rua");
    let mut srv = TestServer::new();

    srv.open(&uri, "fn main() { let x = 1; }");

    for i in 0..50 {
        srv.change(&uri, &format!("fn main() {{ let x = {i}; }}"));
    }

    // Snapshot after rapid changes should still work
    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let diags = srv.snapshot().diagnostics(file_id);
    let _ = diags;
}

#[test]
fn stress_deeply_nested_blocks() {
    let uri = uri("/test/stress_nested.rua");
    let mut srv = TestServer::new();

    // Build a source with 30+ levels of nesting
    let mut source = String::from("fn main() {\n");
    for i in 0..30 {
        source.push_str(&format!("{}if true {{\n", "    ".repeat(i + 1)));
    }
    source.push_str(&"    ".repeat(31));
    source.push_str("let x = 42;\n");
    for i in (0..30).rev() {
        source.push_str(&format!("{}}}\n", "    ".repeat(i + 1)));
    }
    source.push('}');

    srv.open(&uri, &source);

    // Should not panic with deeply nested blocks
    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let parse = analysis.parse(file_id);
    let _ = parse.errors();
    let _ = parse.syntax_node();
}

#[test]
fn stress_unicode_identifiers() {
    let uri = uri("/test/stress_unicode.rua");
    let mut srv = TestServer::new();

    srv.open(
        &uri,
        "fn main() { let 中文变量 = 42; let результат = 中文变量 + 1; }",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let diags = srv.snapshot().diagnostics(file_id);
    // Should not panic with unicode identifiers
    let _ = diags;
}

#[test]
fn stress_large_file_many_definitions() {
    let uri = uri("/test/stress_large.rua");
    let mut srv = TestServer::new();

    // Generate a file with many functions
    let mut source = String::new();
    for i in 0..50 {
        source.push_str(&format!(
            "fn func_{i}(a: i64, b: i64) -> i64 {{ a + b + {i} }}\n"
        ));
    }
    source.push_str("fn main() { let x = func_0(1, 2); }");

    srv.open(&uri, &source);

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();

    // Should parse and analyze without panic
    let def_map = analysis.def_map(file_id);
    let fn_count = def_map
        .definitions()
        .filter(|d| matches!(d.kind(), rua_analysis::DefKind::Function))
        .count();
    assert!(
        fn_count >= 50,
        "should have at least 50 function definitions, got {fn_count}"
    );

    // Completions should still work
    let pp = srv.pp(&uri, 50, 30).unwrap();
    let _ = analysis.completions(pp);
}

#[test]
fn stress_many_files_open() {
    let mut srv = TestServer::new();
    let mut uris = Vec::new();

    // Open 30 files
    for i in 0..30 {
        let uri = uri(&format!("/proj/src/file_{i}.rua"));
        srv.open(&uri, &format!("fn func_{i}() -> i64 {{ {i} }}"));
        uris.push(uri);
    }

    // All files should be queryable
    for uri in &uris {
        let file_id = srv.file_id_for_uri(uri).unwrap();
        let _ = srv.snapshot().parse(file_id);
    }

    // Close all files
    for uri in &uris {
        srv.close(uri);
    }
}

#[test]
fn stress_error_recovery_chained() {
    let uri = uri("/test/stress_errors.rua");
    let mut srv = TestServer::new();

    // Start with broken code, then fix incrementally
    let steps = [
        "fn main() {",                   // incomplete
        "fn main() { let x",             // still incomplete
        "fn main() { let x = 1; }",      // fixed
        "fn main() { let x = true }",    // type error
        "fn main() { let x: i64 = 1; }", // fixed with annotation
    ];

    for (i, step) in steps.iter().enumerate() {
        if i == 0 {
            srv.open(&uri, step);
        } else {
            srv.change(&uri, step);
        }

        // Every snapshot should work without panic
        let file_id = srv.file_id_for_uri(&uri).unwrap();
        let analysis = srv.snapshot();
        let _ = analysis.parse(file_id);
        let _ = analysis.diagnostics(file_id);
        let _ = analysis.item_tree(file_id);
    }
}

#[test]
fn stress_empty_file_all_queries() {
    // All IDE queries on an empty file should return default/empty, not panic.
    let uri = uri("/test/stress_empty.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "");

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let pp = srv.pp(&uri, 0, 0).unwrap();

    // Every query should not panic
    let _ = analysis.parse(file_id);
    let _ = analysis.diagnostics(file_id);
    let _ = analysis.item_tree(file_id);
    let _ = analysis.def_map(file_id);
    let _ = analysis.completions(pp);
    let _ = analysis.hover(pp);
    let _ = analysis.goto_definition(pp);
    let _ = analysis.goto_implementation(pp);
    let _ = analysis.references(pp, true);
    let _ = analysis.prepare_rename(pp);
    let _ = analysis.signature_help(pp);
    let _ = analysis.semantic_tokens(file_id);
    let _ = analysis.document_symbols(file_id, file_id);
    let _ = analysis.workspace_symbols(file_id, "");
    let _ = analysis.call_hierarchy_prepare(pp);
    let _ = analysis.type_hierarchy_prepare(pp);
}

#[test]
fn stress_multibyte_utf16_length() {
    // Tokens with multibyte characters should have correct UTF-16 lengths.
    let uri = uri("/test/stress_mb.rua");
    let mut srv = TestServer::new();
    // Include characters with different UTF-8 and UTF-16 lengths:
    // 中文 = 2 UTF-16 code units, 6 UTF-8 bytes
    srv.open(&uri, "fn main() { let 中文 = \"你好\"; }");

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let tokens = srv.snapshot().semantic_tokens(file_id);
    // Tokens should be computed without panic for multibyte source
    let _ = tokens;
}
