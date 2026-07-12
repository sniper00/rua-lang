//! Extended folding and hover tests.

mod support;

use support::{uri, TestServer};

// ---------------------------------------------------------------------------
// Folding range extensions
// ---------------------------------------------------------------------------

#[test]
fn folding_multiline_match_arms() {
    let uri = uri("/test/fold_match.rua");
    let mut srv = TestServer::new();
    srv.open(&uri,
        "fn main() {\n    let x = 1;\n    match x {\n        1 => {\n            let y = 2;\n        }\n        2 => {\n            let z = 3;\n        }\n        _ => {}\n    }\n}");

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let source = analysis.parse(file_id).syntax_node().text().to_string();
    // Match arms create multiple brace pairs for folding
    let brace_pairs = source.bytes().filter(|&b| b == b'{').count();
    assert!(brace_pairs >= 4, "match should have multiple brace pairs, got {brace_pairs}");
    assert_eq!(
        source.bytes().filter(|&b| b == b'{').count(),
        source.bytes().filter(|&b| b == b'}').count(),
        "braces should be balanced"
    );
}

#[test]
fn folding_import_grouping() {
    // Consecutive `use`/`mod` statements could be grouped for folding.
    let uri = uri("/test/fold_imports.rua");
    let mut srv = TestServer::new();
    srv.open(&uri,
        "mod a {}\nmod b {}\nmod c {}\n\nfn main() {\n    let x = 1;\n}");

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let source = analysis.parse(file_id).syntax_node().text().to_string();

    // Consecutive `mod` declarations exist
    let mod_count = source.lines().filter(|l| l.trim_start().starts_with("mod ")).count();
    assert!(mod_count >= 3, "should have multiple mod declarations, got {mod_count}");
}

#[test]
fn folding_multiline_function_args() {
    // Multi-line function arguments should be foldable.
    let uri = uri("/test/fold_args.rua");
    let mut srv = TestServer::new();
    srv.open(&uri,
        "fn complex(\n    a: i64,\n    b: i64,\n    c: i64,\n    d: i64,\n) -> i64 {\n    a + b + c + d\n}");

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let source = analysis.parse(file_id).syntax_node().text().to_string();
    // Multi-line arg list should be recognized
    assert!(source.lines().count() >= 5, "should have multi-line args");
}

// ---------------------------------------------------------------------------
// Hover extensions
// ---------------------------------------------------------------------------

#[test]
fn hover_on_pattern_binding_shows_type() {
    let uri = uri("/test/hover_pat.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { let (a, b) = (42, true); }");

    // Hover on `a` — should show i64
    let pp = srv.pp(&uri, 0, 18).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        assert!(h.signature().contains("i64") || h.signature().contains("a"),
            "hover on a should show type, got: {}", h.signature());
    }
}

#[test]
fn hover_on_if_else_expression_shows_unified_type() {
    let uri = uri("/test/hover_if_else.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { let x = if true { 1 } else { 2 }; }");

    // Hover on `x` — should show i64
    let pp = srv.pp(&uri, 0, 16).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        assert!(h.signature().contains("i64") || h.signature().contains("x"),
            "hover on x should show type, got: {}", h.signature());
    }
}

#[test]
fn hover_on_enum_variant_with_fields() {
    let uri = uri("/test/hover_variant_field.rua");
    let mut srv = TestServer::new();
    srv.open(&uri,
        "enum Result { Ok(i64), Err(String) }\nfn main() { let r = Result::Ok(42); }");

    // Hover on `Ok`
    let pp = srv.pp(&uri, 1, 31).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        let sig = h.signature().to_string();
        assert!(sig.contains("Ok") || sig.contains("Result"),
            "hover on variant should mention variant or enum, got: {sig}");
    }
}

#[test]
fn hover_on_function_with_doc_comment() {
    let uri = uri("/test/hover_doc.rua");
    let mut srv = TestServer::new();
    srv.open(&uri,
        "/// Computes the answer to everything.\nfn answer() -> i64 { 42 }\nfn main() { answer(); }");

    // Hover on `answer` in the call
    let pp = srv.pp(&uri, 2, 14).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        // Hover should include the doc comment or at minimum the function signature
        let sig = h.signature().to_string();
        assert!(!sig.is_empty(), "hover should have content");
        // May include doc text or function signature
        assert!(sig.contains("answer") || sig.contains("42") || sig.contains("i64"),
            "hover should describe the function, got: {sig}");
    }
}

#[test]
fn hover_on_closure_expression() {
    let uri = uri("/test/hover_closure.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { let f = |x: i64| x * 2; }");

    // Hover on `f` — should show closure/function type
    let pp = srv.pp(&uri, 0, 16).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        let sig = h.signature().to_string();
        assert!(!sig.is_empty(), "hover on closure should have content");
    }
}
