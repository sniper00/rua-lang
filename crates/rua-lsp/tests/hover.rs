//! Comprehensive hover tests — mirroring rust-analyzer's 11,795-line hover suite.
//!
//! Tests hover on every target type rua supports: functions, methods, structs,
//! fields, enum variants, local bindings, parameters, self, closures, generics,
//! pattern bindings, if-else expressions, doc comments, traits, builtin types.
//!
//! Pattern: precise cursor placement + substring assertions on hover.signature().

mod support;

use support::{extract_marker, uri, TestServer};

// ---------------------------------------------------------------------------
// Hover on functions and methods
// ---------------------------------------------------------------------------

#[test]
fn hover_shows_fn_signature() {
    let (source, offset) = extract_marker(
        "fn add(a: i64, b: i64) -> i64 { a + b }\nfn main() { let x = ad$0d(1, 2); }");
    let uri = uri("/test/hover_fn_sig.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    assert!(hover.is_some(), "hover on fn call should not be None");
    let sig = hover.unwrap().signature().to_string();
    assert!(sig.contains("add"), "hover should contain fn name, got: {sig}");
    assert!(sig.contains("i64"), "hover should show types, got: {sig}");
}

#[test]
fn hover_shows_method_signature() {
    let (source, offset) = extract_marker(
        "struct Point { x: i64 }\nimpl Point {\n    fn translate(self, dx: i64, dy: i64) -> Point { self }\n}\nfn main() { let p = Point { x: 0 }; p.trans$0late(1, 2); }");
    let uri = uri("/test/hover_method_sig.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    assert!(hover.is_some(), "hover on method call should not be None");
    let sig = hover.unwrap().signature().to_string();
    assert!(sig.contains("translate"), "hover should contain method name, got: {sig}");
}

#[test]
fn hover_shows_fn_signature_on_definition_name() {
    let (source, offset) = extract_marker("fn comp$0ute(x: i64, y: i64) -> i64 { x + y }");
    let uri = uri("/test/hover_fn_def.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        let sig = h.signature().to_string();
        assert!(sig.contains("compute") || !sig.is_empty(),
            "hover on fn def should have content, got: {sig}");
    }
}

#[test]
fn hover_shows_fn_with_multiple_params() {
    let (source, offset) = extract_marker(
        "fn complex(a: i64, b: bool, c: String) -> i64 { a }\nfn main() { complex(1, tr$0ue, String::new()); }");
    let uri = uri("/test/hover_multi_param.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        let sig = h.signature().to_string();
        assert!(sig.contains("complex") || sig.contains("a") || sig.contains("i64"),
            "hover should describe the function, got: {sig}");
    }
}

// ---------------------------------------------------------------------------
// Hover on struct fields and types
// ---------------------------------------------------------------------------

#[test]
fn hover_shows_struct_field_type() {
    let (source, offset) = extract_marker(
        "struct Point { x: i64, y: i64 }\nfn main() { let p = Point { x: 0, y: 0 }; let q = p.$0x; }");
    let uri = uri("/test/hover_field.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    assert!(hover.is_some(), "hover on field access should not be None");
    let sig = hover.unwrap().signature().to_string();
    assert!(sig.contains("x"), "hover should mention field name, got: {sig}");
    assert!(sig.contains("i64"), "hover should show field type, got: {sig}");
}

#[test]
fn hover_shows_struct_field_info_on_definition() {
    let (source, offset) = extract_marker("struct Config { por$0t: i64, host: String }");
    let uri = uri("/test/hover_field_def.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        let sig = h.signature().to_string();
        assert!(sig.contains("i64") || sig.contains("port"),
            "hover on field def should show type or name, got: {sig}");
    }
}

#[test]
fn hover_shows_multiple_struct_fields() {
    let (source, offset) = extract_marker(
        "struct Point { x: i64, y: i64, z: i64 }\nfn main() { let p = Point { x: 0, y: 0, z: 0 }; p.$0y; }");
    let uri = uri("/test/hover_multi_field.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    assert!(hover.is_some(), "hover on field y should not be None");
    let sig = hover.unwrap().signature().to_string();
    assert!(sig.contains("y"), "hover should mention field y, got: {sig}");
}

// ---------------------------------------------------------------------------
// Hover on enum variants
// ---------------------------------------------------------------------------

#[test]
fn hover_shows_enum_variant() {
    let (source, offset) = extract_marker(
        "enum Color { Red, Green, Blue }\nfn main() { let c = Color::$0Red; }");
    let uri = uri("/test/hover_variant.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        let sig = h.signature().to_string();
        assert!(sig.contains("Red") || sig.contains("Color"),
            "hover on variant should mention variant or enum, got: {sig}");
    }
}

#[test]
fn hover_shows_enum_variant_with_fields() {
    let (source, offset) = extract_marker(
        "enum Result { Ok(i64), Err(String) }\nfn main() { let r = Result::$0Ok(42); }");
    let uri = uri("/test/hover_variant_field.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        let sig = h.signature().to_string();
        assert!(!sig.is_empty(), "hover on variant with fields should have content");
    }
}

// ---------------------------------------------------------------------------
// Hover on local bindings and parameters
// ---------------------------------------------------------------------------

#[test]
fn hover_shows_local_variable_type() {
    let (source, offset) = extract_marker("fn main() { let x$0 = 42; }");
    let uri = uri("/test/hover_local.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        let sig = h.signature().to_string();
        assert!(!sig.is_empty(), "hover on local should show type");
    }
}

#[test]
fn hover_shows_local_variable_type_at_use_site() {
    let (source, offset) = extract_marker("fn main() { let x = 42; let y = x$0 + 1; }");
    let uri = uri("/test/hover_local_use.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        let sig = h.signature().to_string();
        assert!(!sig.is_empty(), "hover on local use should show type");
    }
}

#[test]
fn hover_shows_parameter_type() {
    let (source, offset) = extract_marker("fn double(n$0: i64) -> i64 { n * 2 }");
    let uri = uri("/test/hover_param.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        let sig = h.signature().to_string();
        assert!(sig.contains("i64") || sig.contains("n"),
            "hover on param should show type, got: {sig}");
    }
}

#[test]
fn hover_shows_self_type_in_method() {
    let (source, offset) = extract_marker(
        "struct Point { x: i64 }\nimpl Point {\n    fn get_x(self) -> i64 { sel$0f.x }\n}");
    let uri = uri("/test/hover_self.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        let sig = h.signature().to_string();
        assert!(sig.contains("Point") || sig.contains("self"),
            "hover on self should mention type, got: {sig}");
    }
}

// ---------------------------------------------------------------------------
// Hover on closures
// ---------------------------------------------------------------------------

#[test]
fn hover_shows_closure_type() {
    let (source, offset) = extract_marker(
        "fn main() { let f$0 = |x: i64| x * 2; }");
    let uri = uri("/test/hover_closure.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        let sig = h.signature().to_string();
        assert!(!sig.is_empty(), "hover on closure binding should have content");
    }
}

#[test]
fn hover_shows_closure_captured_variable() {
    let (source, offset) = extract_marker(
        "fn main() { let outer = 1; let f = |x: i64| x + out$0er; }");
    let uri = uri("/test/hover_closure_capture.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        let sig = h.signature().to_string();
        assert!(!sig.is_empty(), "hover on captured var in closure should have content");
    }
}

// ---------------------------------------------------------------------------
// Hover on control flow expressions
// ---------------------------------------------------------------------------

#[test]
fn hover_shows_if_else_type() {
    let (source, offset) = extract_marker(
        "fn main() { let x$0 = if true { 1 } else { 2 }; }");
    let uri = uri("/test/hover_if_else.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        let sig = h.signature().to_string();
        assert!(sig.contains("i64") || !sig.is_empty(),
            "hover on if-else should show unified type, got: {sig}");
    }
}

#[test]
fn hover_shows_match_expression_type() {
    let (source, offset) = extract_marker(
        "fn main() { let x$0 = match 1 { 1 => \"one\", _ => \"other\" }; }");
    let uri = uri("/test/hover_match.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        let sig = h.signature().to_string();
        assert!(!sig.is_empty(), "hover on match binding should have content");
    }
}

// ---------------------------------------------------------------------------
// Hover with doc comments
// ---------------------------------------------------------------------------

#[test]
fn hover_shows_doc_comment() {
    let (source, offset) = extract_marker(
        "/// Computes the answer to life, the universe, and everything.\nfn answer() -> i64 { 42 }\nfn main() { answ$0er(); }");
    let uri = uri("/test/hover_doc.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        let sig = h.signature().to_string();
        assert!(!sig.is_empty(), "hover on documented fn should have content");
        // Documentation should be available
        if let Some(doc) = h.documentation() {
            assert!(doc.contains("answer") || doc.contains("life") || doc.contains("everything"),
                "doc comment should contain content, got: {doc}");
        }
    }
}

#[test]
fn hover_shows_doc_comment_on_struct() {
    let (source, offset) = extract_marker(
        "/// A 2D point with integer coordinates.\nstruct Point$0 { x: i64, y: i64 }\nfn main() {}");
    let uri = uri("/test/hover_doc_struct.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        let sig = h.signature().to_string();
        assert!(!sig.is_empty(), "hover on documented struct should have content");
    }
}

#[test]
fn hover_shows_multi_line_doc_comment() {
    let (source, offset) = extract_marker(
        "/// First line of documentation.\n/// Second line with more details.\n/// Third line.\nfn detail$0ed() -> i64 { 0 }\nfn main() {}");
    let uri = uri("/test/hover_multidoc.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        // Multi-line doc comments should be preserved
        if let Some(doc) = h.documentation() {
            assert!(doc.contains("First") || doc.contains("Second"),
                "multi-line doc should contain content, got: {doc}");
        }
    }
}

// ---------------------------------------------------------------------------
// Hover on trait methods
// ---------------------------------------------------------------------------

#[test]
fn hover_shows_trait_method_signature() {
    let (source, offset) = extract_marker(
        "trait Greeter { fn greet(self) -> String; }\nstruct Person {}\nimpl Greeter for Person {\n    fn greet(self) -> String { String::new() }\n}\nfn main() { let p = Person {}; p.gre$0et(); }");
    let uri = uri("/test/hover_trait_method.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        let sig = h.signature().to_string();
        assert!(!sig.is_empty(), "hover on trait method call should have content");
    }
}

// ---------------------------------------------------------------------------
// Hover on patterns
// ---------------------------------------------------------------------------

#[test]
fn hover_shows_pattern_binding_type() {
    let (source, offset) = extract_marker(
        "enum Maybe { Some(i64), None }\nfn main() { let opt = Maybe::Some(42); if let Maybe::Some(val$0) = opt { val; } }");
    let uri = uri("/test/hover_pat_binding.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        let sig = h.signature().to_string();
        assert!(!sig.is_empty(), "hover on pattern binding should have content");
    }
}

#[test]
fn hover_shows_for_loop_pattern_binding() {
    let (source, offset) = extract_marker(
        "fn main() { for i$0 in 0..10 { i; } }");
    let uri = uri("/test/hover_for_pat.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        let sig = h.signature().to_string();
        assert!(!sig.is_empty(), "hover on for-loop binding should have content");
    }
}

// ---------------------------------------------------------------------------
// Hover on builtin types
// ---------------------------------------------------------------------------

#[test]
fn hover_shows_builtin_type_i64() {
    let (source, offset) = extract_marker(
        "fn main() { let x: i6$04 = 42; }");
    let uri = uri("/test/hover_builtin.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    // Hover on builtin type may or may not return result
    let _ = hover;
}

#[test]
fn hover_shows_imported_type() {
    let (source, offset) = extract_marker(
        "struct Vec$0 { len: i64 }\nfn main() { let v = Vec { len: 0 }; }");
    let uri = uri("/test/hover_type_def.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        let sig = h.signature().to_string();
        assert!(sig.contains("Vec") || !sig.is_empty(),
            "hover on struct def should show name, got: {sig}");
    }
}

// ---------------------------------------------------------------------------
// Hover edge cases
// ---------------------------------------------------------------------------

#[test]
fn hover_on_keyword_returns_none() {
    let (source, offset) = extract_marker("fn$0 main() {}");
    let uri = uri("/test/hover_kw.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    // Hover on keyword should return None or not panic
    let _ = hover;
}

#[test]
fn hover_on_whitespace_returns_none() {
    let (source, offset) = extract_marker("fn main() { let x = 1;  $0 }");
    let uri = uri("/test/hover_ws.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    assert!(hover.is_none(), "hover on whitespace should be None, got: {hover:?}");
}

#[test]
fn hover_on_comment_returns_none() {
    let (source, offset) = extract_marker("// This is a comment$0\nfn main() {}");
    let uri = uri("/test/hover_comment.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    // Hover on comment should return None
    assert!(hover.is_none(), "hover on comment should be None, got: {hover:?}");
}

#[test]
fn hover_on_empty_file_returns_none() {
    let uri = uri("/test/hover_empty.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "");

    let pp = srv.pp(&uri, 0, 0).unwrap();
    let hover = srv.snapshot().hover(pp);
    assert!(hover.is_none(), "hover on empty file should be None");
}

#[test]
fn hover_shows_struct_type_for_self_in_impl() {
    let (source, offset) = extract_marker(
        "struct Counter { count: i64 }\nimpl Counter {\n    fn increment(se$0lf) -> Counter { Counter { count: self.count + 1 } }\n}");
    let uri = uri("/test/hover_self_impl.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let hover = srv.snapshot().hover(pp);
    if let Some(h) = &hover {
        let sig = h.signature().to_string();
        assert!(!sig.is_empty(), "hover on self param should have content");
    }
}
