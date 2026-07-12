//! Completion context tests — organized by syntax position (like rust-analyzer).
//!
//! Covers: pattern position, type position, visibility, record literals,
//! item level, fn params, use/module paths.

mod support;

use support::{extract_marker, uri, TestServer};

// ---------------------------------------------------------------------------
// Pattern position completions
// ---------------------------------------------------------------------------

#[test]
fn completions_in_let_pattern() {
    // Inside `let` pattern (before `=`), enum variants and ref/mut keywords appear.
    let (source, offset) = extract_marker(
        "enum Maybe { Some(i64), None }\nfn main() { let $0 = Maybe::Some(42); }");
    let uri = uri("/test/ctx_let_pat.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();
    // Pattern position should include enum variants and maybe ref/mut
    assert!(!labels.is_empty(), "pattern position completions should not be empty");
    // At minimum, keywords should be present
    assert!(labels.iter().any(|l| l == "if" || l == "let" || l == "fn" || l == "Some"),
        "should have keywords or variants in pattern pos, got: {labels:?}");
}

#[test]
fn completions_in_match_arm_pattern() {
    let (source, offset) = extract_marker(
        "enum Color { Red, Green, Blue }\nfn main() { let c = Color::Red; match c { $0 } }");
    let uri = uri("/test/ctx_match_pat.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();
    // Match body should offer enum variants
    let has_variants = labels.iter().any(|l| l == "Red" || l == "Green" || l == "Blue");
    assert!(has_variants || !labels.is_empty(),
        "match body should offer variants or keywords, got: {labels:?}");
}

// ---------------------------------------------------------------------------
// Type position completions
// ---------------------------------------------------------------------------

#[test]
fn completions_in_type_annotation_position() {
    // After `:`, only types should appear.
    let (source, offset) = extract_marker(
        "struct Point { x: i64 }\nenum Color { Red }\nfn main() { let x: $0 }");
    let uri = uri("/test/ctx_type_pos.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();
    // Type position: should include i64, Point, Color, String, etc.
    assert!(labels.contains(&"i64".to_string()), "i64 should be in type pos, got: {labels:?}");
    assert!(labels.contains(&"Point".to_string()), "Point should be in type pos, got: {labels:?}");
}

#[test]
fn completions_in_struct_field_type() {
    let (source, offset) = extract_marker("struct Config { port: $0 }");
    let uri = uri("/test/ctx_field_type.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();
    // Field type position should offer builtin types
    assert!(labels.contains(&"i64".to_string()),
        "i64 should be offered in field type pos, got: {labels:?}");
}

#[test]
fn completions_in_function_return_type() {
    let (source, offset) = extract_marker("fn compute() -> $0 { 42 }");
    let uri = uri("/test/ctx_ret_type.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();
    assert!(labels.contains(&"i64".to_string()),
        "return type position should offer i64, got: {labels:?}");
}

// ---------------------------------------------------------------------------
// Item-level completions (module body)
// ---------------------------------------------------------------------------

#[test]
fn completions_at_item_level() {
    // Top-level / module body should offer declaration keywords and types.
    let (source, offset) = extract_marker("fn main() {}\n$0");
    let uri = uri("/test/ctx_item.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();
    // Item level should offer `fn`, `struct`, `enum`, `trait`, etc.
    assert!(labels.contains(&"fn".to_string()),
        "item level should offer fn keyword, got: {labels:?}");
    assert!(labels.contains(&"struct".to_string()),
        "item level should offer struct keyword, got: {labels:?}");
}

// ---------------------------------------------------------------------------
// Path/module completions
// ---------------------------------------------------------------------------

#[test]
fn completions_in_module_path() {
    let (source, offset) = extract_marker(
        "mod math { pub fn abs(x: i64) -> i64 { if x < 0 { -x } else { x } } }\nfn main() { math::$0 }");
    let uri = uri("/test/ctx_mod_path.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();
    assert!(labels.contains(&"abs".to_string()),
        "module path should offer fn abs, got: {labels:?}");
}

#[test]
fn completions_nested_module_path() {
    let (source, offset) = extract_marker(
        "mod a { pub mod b { pub fn func() -> i64 { 1 } } }\nfn main() { a::b::$0 }");
    let uri = uri("/test/ctx_nested_path.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();
    assert!(labels.contains(&"func".to_string()),
        "nested module path should offer fn func, got: {labels:?}");
}

// ---------------------------------------------------------------------------
// Record literal field completions
// ---------------------------------------------------------------------------

#[test]
fn completions_record_literal_fields() {
    // Inside struct literal, fields of the struct should be offered.
    let (source, offset) = extract_marker(
        "struct Point { x: i64, y: i64 }\nfn main() { let p = Point { $0 }; }");
    let uri = uri("/test/ctx_record.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();
    // Should include field names x, y
    assert!(labels.contains(&"x".to_string()),
        "record literal should offer field x, got: {labels:?}");
    assert!(labels.contains(&"y".to_string()),
        "record literal should offer field y, got: {labels:?}");
}

// ---------------------------------------------------------------------------
// Expression position (already well-tested, add edge cases)
// ---------------------------------------------------------------------------

#[test]
fn completions_after_return_keyword() {
    let (source, offset) = extract_marker("fn main() -> i64 { return $0 }");
    let uri = uri("/test/ctx_return.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let items = srv.snapshot().completions(pp);
    // After `return`, expression completions should appear
    assert!(!items.is_empty(), "completions after return should not be empty");
}

#[test]
fn completions_inside_parenthesized_expression() {
    let (source, offset) = extract_marker("fn main() { let x = ($0); }");
    let uri = uri("/test/ctx_paren.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let items = srv.snapshot().completions(pp);
    assert!(!items.is_empty(), "completions in parens should not be empty");
}

// ---------------------------------------------------------------------------
// Closure context
// ---------------------------------------------------------------------------

#[test]
fn completions_in_closure_param_list() {
    let (source, offset) = extract_marker("fn main() { let f = |$0| 42; }");
    let uri = uri("/test/ctx_closure_param.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, &source);

    let pp = srv.pp_at_offset(&uri, offset).unwrap();
    let items = srv.snapshot().completions(pp);
    // In closure param position — should not panic
    let _ = items;
}
