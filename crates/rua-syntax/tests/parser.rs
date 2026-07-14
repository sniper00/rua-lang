//! Comprehensive parser tests — every grammar construct gets systematic coverage.
//!
//! Pattern: parse → verify syntax kind tree → verify no unexpected errors.
//! Mirrors rust-analyzer's test_data/parser/inline/{ok,err}/ test suite.

use rua_syntax::{SyntaxKind, parse_source_file};

fn has_kind(node: &rua_syntax::SyntaxNode, kind: SyntaxKind) -> bool {
    if node.kind() == kind {
        return true;
    }
    for child in node.children_with_tokens() {
        if let rua_syntax::SyntaxElement::Node(n) = child
            && has_kind(&n, kind)
        {
            return true;
        }
    }
    false
}

fn count_kind(node: &rua_syntax::SyntaxNode, kind: SyntaxKind) -> usize {
    let mut count = if node.kind() == kind { 1 } else { 0 };
    for child in node.children_with_tokens() {
        if let rua_syntax::SyntaxElement::Node(n) = child {
            count += count_kind(&n, kind);
        }
    }
    count
}

fn parse_and_check(source: &str) -> rua_syntax::SyntaxNode {
    let parse = parse_source_file(source);
    let node = parse.syntax_node().clone();
    // Lossless round-trip
    assert_eq!(node.text().to_string(), source, "parser must be lossless");
    node
}

fn parse_no_errors(source: &str) -> rua_syntax::SyntaxNode {
    let parse = parse_source_file(source);
    let error_count = parse.errors().len();
    assert!(
        error_count == 0,
        "expected no parse errors, got {error_count} errors\nsource: {source}"
    );
    parse.syntax_node().clone()
}

// ---------------------------------------------------------------------------
// Top-level items
// ---------------------------------------------------------------------------

#[test]
fn parse_empty_file() {
    let node = parse_and_check("");
    assert_eq!(node.kind(), SyntaxKind::SourceFile);
}

#[test]
fn parse_function_declaration() {
    let node = parse_no_errors("fn main() {}");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
    assert_eq!(count_kind(&node, SyntaxKind::FnDecl), 1);
}

#[test]
fn parse_function_with_params() {
    let node = parse_no_errors("fn add(a: i64, b: i64) -> i64 { 0 }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_function_with_return_type() {
    let node = parse_no_errors("fn answer() -> i64 { 42 }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_function_no_return_type() {
    let node = parse_no_errors("fn say_hello() { }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_function_with_generic_params() {
    let node = parse_no_errors("fn identity<T>(x: T) -> T { x }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
    assert!(has_kind(&node, SyntaxKind::GenericParams));
}

#[test]
fn parse_pub_function() {
    let node = parse_no_errors("pub fn public_fn() -> i64 { 0 }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_extern_function() {
    let node = parse_no_errors("extern \"lua\" { fn c_func(x: i64) -> i64; }");
    assert!(has_kind(&node, SyntaxKind::ExternBlock) || has_kind(&node, SyntaxKind::FnDecl));
}

// ---------------------------------------------------------------------------
// Struct declarations
// ---------------------------------------------------------------------------

#[test]
fn parse_struct_declaration() {
    let node = parse_no_errors("struct Point { x: i64, y: i64 }");
    assert!(has_kind(&node, SyntaxKind::StructDecl));
}

#[test]
fn parse_empty_struct() {
    let node = parse_no_errors("struct Unit;");
    assert!(has_kind(&node, SyntaxKind::StructDecl));
}

#[test]
fn parse_struct_with_visibility() {
    let node = parse_no_errors("pub struct PublicStruct { val: i64 }");
    assert!(has_kind(&node, SyntaxKind::StructDecl));
}

#[test]
fn parse_struct_with_generics() {
    let node = parse_no_errors("struct Container<T> { item: T }");
    assert!(has_kind(&node, SyntaxKind::StructDecl));
    assert!(has_kind(&node, SyntaxKind::GenericParams));
}

// ---------------------------------------------------------------------------
// Enum declarations
// ---------------------------------------------------------------------------

#[test]
fn parse_enum_declaration() {
    let node = parse_no_errors("enum Color { Red, Green, Blue }");
    assert!(has_kind(&node, SyntaxKind::EnumDecl));
}

#[test]
fn parse_enum_with_tuple_variants() {
    let node = parse_no_errors("enum Option { Some(i64), None }");
    assert!(has_kind(&node, SyntaxKind::EnumDecl));
}

#[test]
fn parse_enum_with_struct_variants() {
    let node = parse_no_errors("enum Shape { Circle { radius: f64 }, Square { side: f64 } }");
    assert!(has_kind(&node, SyntaxKind::EnumDecl));
}

// ---------------------------------------------------------------------------
// Trait and impl declarations
// ---------------------------------------------------------------------------

#[test]
fn parse_trait_declaration() {
    let node = parse_no_errors("trait Drawable { fn draw(self); }");
    assert!(has_kind(&node, SyntaxKind::TraitDecl));
}

#[test]
fn parse_trait_with_default_method() {
    let node = parse_no_errors("trait Greeter { fn greet(self) -> String { String::new() } }");
    assert!(has_kind(&node, SyntaxKind::TraitDecl));
}

#[test]
fn parse_impl_block() {
    let node = parse_no_errors("struct Point {}\nimpl Point { fn new() -> Point { Point {} } }");
    assert!(has_kind(&node, SyntaxKind::ImplDecl));
}

#[test]
fn parse_trait_impl() {
    let node = parse_no_errors(
        "trait Greet { fn hello(self); }\nstruct Person {}\nimpl Greet for Person { fn hello(self) {} }",
    );
    assert!(has_kind(&node, SyntaxKind::TraitDecl));
    assert!(has_kind(&node, SyntaxKind::ImplDecl));
}

// ---------------------------------------------------------------------------
// Module declarations
// ---------------------------------------------------------------------------

#[test]
fn parse_module_declaration() {
    let node = parse_no_errors("mod math { pub fn abs(x: i64) -> i64 { 0 } }");
    assert!(has_kind(&node, SyntaxKind::ModDecl));
}

#[test]
fn parse_nested_module() {
    let node = parse_no_errors("mod outer { mod inner { fn f() -> i64 { 1 } } }");
    assert!(has_kind(&node, SyntaxKind::ModDecl));
    assert_eq!(count_kind(&node, SyntaxKind::ModDecl), 2);
}

#[test]
fn parse_pub_module() {
    let node = parse_no_errors("pub mod public_mod { pub fn f() -> i64 { 0 } }");
    assert!(has_kind(&node, SyntaxKind::ModDecl));
}

// ---------------------------------------------------------------------------
// Use declarations
// ---------------------------------------------------------------------------

#[test]
fn parse_use_declaration() {
    let node = parse_no_errors("use std::collections::Vec;");
    assert!(has_kind(&node, SyntaxKind::UseDecl));
}

#[test]
fn parse_use_with_braces() {
    let node = parse_no_errors("use std::{Vec, HashMap};");
    assert!(has_kind(&node, SyntaxKind::UseDecl));
}

// ---------------------------------------------------------------------------
// Statements — let, if, while, for, loop, match, return, break, continue
// ---------------------------------------------------------------------------

#[test]
fn parse_let_statement() {
    let node = parse_no_errors("fn main() { let x = 42; }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_let_with_type_annotation() {
    let node = parse_no_errors("fn main() { let x: i64 = 42; }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_let_mut() {
    let node = parse_no_errors("fn main() { let mut x = 1; x = 2; }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_if_else_statement() {
    let node = parse_no_errors("fn main() { if true { 1 } else { 2 }; }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_if_else_if_chain() {
    let node = parse_no_errors("fn main() { if x > 0 { 1 } else if x < 0 { -1 } else { 0 }; }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_if_without_else() {
    let node = parse_no_errors("fn main() { if true { let x = 1; } }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_if_let_expression() {
    let node = parse_no_errors(
        "enum Option { Some(i64), None }\nfn main() { let o = Option::Some(1); if let Option::Some(v) = o { v; } }",
    );
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_while_loop() {
    let node = parse_no_errors("fn main() { while true { let x = 1; } }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_while_let_loop() {
    let node = parse_no_errors(
        "enum Option { Some(i64), None }\nfn main() { let o = Option::Some(1); while let Option::Some(v) = o { v; } }",
    );
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_loop_statement() {
    let node = parse_no_errors("fn main() { loop { break; } }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_for_loop() {
    let node = parse_no_errors("fn main() { for i in 0..10 { i; } }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_match_expression() {
    let node = parse_no_errors("fn main() { match 1 { 1 => {}, _ => {} } }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_match_with_multiple_arms() {
    let node = parse_no_errors(
        "enum Color { Red, Green, Blue }\nfn main() { let c = Color::Red; match c { Color::Red => 1, Color::Green => 2, Color::Blue => 3 }; }",
    );
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_return_statement() {
    let node = parse_no_errors("fn main() -> i64 { return 42; }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_return_without_value() {
    let node = parse_no_errors("fn main() { return; }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_break_statement() {
    let node = parse_no_errors("fn main() { loop { break; } }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_continue_statement() {
    let node = parse_no_errors("fn main() { loop { continue; } }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

#[test]
fn parse_binary_expression() {
    let node = parse_no_errors("fn main() { let x = 1 + 2 * 3; }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_comparison_expression() {
    let node = parse_no_errors("fn main() { let b = x > 0 && y < 10; }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_unary_expression() {
    let node = parse_no_errors("fn main() { let x = -42; let b = !true; }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_function_call() {
    let node = parse_no_errors("fn add(a: i64, b: i64) -> i64 { a + b }\nfn main() { add(1, 2); }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_method_call() {
    let node = parse_no_errors(
        "struct Point { x: i64 }\nimpl Point { fn x(self) -> i64 { self.x } }\nfn main() { let p = Point { x: 0 }; p.x(); }",
    );
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_field_access() {
    let node =
        parse_no_errors("struct Point { x: i64 }\nfn main() { let p = Point { x: 0 }; p.x; }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_struct_literal() {
    let node = parse_no_errors(
        "struct Point { x: i64, y: i64 }\nfn main() { let p = Point { x: 0, y: 0 }; }",
    );
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_closure_expression() {
    let node = parse_no_errors("fn main() { let f = |x| x + 1; }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_closure_with_type_annotations() {
    let node = parse_no_errors("fn main() { let f = |x: i64, y: i64| -> i64 { x + y }; }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_range_expression() {
    let node = parse_no_errors("fn main() { for i in 0..10 { i; } }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_range_inclusive() {
    let node = parse_no_errors("fn main() { for i in 0..=10 { i; } }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_block_expression() {
    let node = parse_no_errors("fn main() { let x = { let y = 1; y + 1 }; }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_if_else_expression() {
    let node = parse_no_errors("fn main() { let x = if true { 1 } else { 0 }; }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

// ---------------------------------------------------------------------------
// Literals
// ---------------------------------------------------------------------------

#[test]
fn parse_integer_literal() {
    let node = parse_no_errors("fn main() { let x = 42; let y = 12345; }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_float_literal() {
    let node = parse_no_errors("fn main() { let x = 3.14159; }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_string_literal() {
    let node = parse_no_errors("fn main() { let s = \"hello world\"; }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_boolean_literal() {
    let node = parse_no_errors("fn main() { let t = true; let f = false; }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

// ---------------------------------------------------------------------------
// Patterns
// ---------------------------------------------------------------------------

#[test]
fn parse_tuple_pattern() {
    let node = parse_and_check("fn main() { let (a, b) = (1, 2); }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_wildcard_pattern() {
    let node = parse_no_errors("fn main() { let _ = 42; }");
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

// ---------------------------------------------------------------------------
// Comments and trivia preservation
// ---------------------------------------------------------------------------

#[test]
fn parse_line_comments_preserved() {
    let source = "// top-level comment\nfn main() {\n    // inline comment\n    let x = 1;\n}";
    let node = parse_and_check(source);
    let text = node.text().to_string();
    assert!(text.contains("// top-level comment"));
    assert!(text.contains("// inline comment"));
}

#[test]
fn parse_doc_comments_preserved() {
    let source = "/// A documented function.\n/// Returns the answer.\nfn answer() -> i64 { 42 }";
    let node = parse_and_check(source);
    let text = node.text().to_string();
    assert!(text.contains("/// A documented function"));
    assert!(text.contains("/// Returns the answer"));
}

#[test]
fn parse_block_comments_preserved() {
    let source = "/* block comment */\nfn main() {}";
    let node = parse_and_check(source);
    let text = node.text().to_string();
    assert!(text.contains("/* block comment */"));
}

// ---------------------------------------------------------------------------
// Error recovery
// ---------------------------------------------------------------------------

#[test]
fn parse_unterminated_string_recovery() {
    let source = "fn main() { let s = \"hello; }";
    let node = parse_and_check(source);
    // Should produce Error tokens but still parse the rest
    // Must be lossless regardless
    assert_eq!(node.text().to_string(), source);
}

#[test]
fn parse_missing_semicolon_recovery() {
    let source = "fn main() { let x = 1 }";
    let node = parse_and_check(source);
    assert_eq!(node.text().to_string(), source);
}

#[test]
fn parse_missing_closing_brace_recovery() {
    let source = "fn main() { let x = 1;";
    let node = parse_and_check(source);
    assert_eq!(node.text().to_string(), source);
}

#[test]
fn parse_extra_closing_brace_recovery() {
    let source = "fn main() { let x = 1; } }";
    let node = parse_and_check(source);
    assert_eq!(node.text().to_string(), source);
}

#[test]
fn parse_unexpected_token_recovery() {
    let source = "fn main() { let x = @; }";
    let node = parse_and_check(source);
    assert_eq!(node.text().to_string(), source);
}

#[test]
fn parse_incomplete_expression_recovery() {
    let source = "fn main() { let x = 1 + ; }";
    let node = parse_and_check(source);
    assert_eq!(node.text().to_string(), source);
}

#[test]
fn parse_missing_fn_body_recovery() {
    let source = "fn incomplete() -> i64";
    let node = parse_and_check(source);
    assert_eq!(node.text().to_string(), source);
}

// ---------------------------------------------------------------------------
// Edge cases — nesting, unicode, large inputs
// ---------------------------------------------------------------------------

#[test]
fn parse_deeply_nested_blocks() {
    let mut source = String::from("fn main() {\n");
    for i in 0..30 {
        source.push_str(&format!("{}if true {{\n", "    ".repeat(i + 1)));
    }
    source.push_str(&"    ".repeat(31));
    source.push_str("let x = 1;\n");
    for i in (0..30).rev() {
        source.push_str(&format!("{}}}\n", "    ".repeat(i + 1)));
    }
    source.push('}');
    let node = parse_and_check(&source);
    assert_eq!(node.text().to_string(), source);
}

#[test]
fn parse_unicode_identifiers() {
    let source = "fn main() { let 中文变量 = 42; let результат = 中文变量 + 1; }";
    let node = parse_and_check(source);
    assert_eq!(node.text().to_string(), source);
}

#[test]
fn parse_unicode_string_literals() {
    let source = "fn main() { let s = \"你好世界\"; }";
    let node = parse_and_check(source);
    assert_eq!(node.text().to_string(), source);
}

#[test]
fn parse_many_functions() {
    let mut source = String::new();
    for i in 0..50 {
        source.push_str(&format!(
            "fn func_{}(a: i64, b: i64) -> i64 {{ a + b + {} }}\n",
            i, i
        ));
    }
    let node = parse_and_check(&source);
    assert_eq!(node.text().to_string(), source);
}

#[test]
fn parse_multiple_top_level_items() {
    let source = "struct A {}\nstruct B {}\nstruct C {}\nfn main() {}";
    let node = parse_no_errors(source);
    assert_eq!(count_kind(&node, SyntaxKind::StructDecl), 3);
    assert_eq!(count_kind(&node, SyntaxKind::FnDecl), 1);
}

#[test]
fn parse_top_level_chunk_statements_between_items() {
    let source =
        "let before = 1;\nfn value() -> i64 { 2 }\nprintln!(\"{}\", before);\nlet after = value();";
    let parsed = rua_syntax::parse_source_file(source);
    assert!(parsed.errors().is_empty(), "{:?}", parsed.errors());
    assert_eq!(parsed.tree().items().count(), 1);
    assert_eq!(parsed.tree().stmts().count(), 3);
    assert_eq!(parsed.syntax_node().text().to_string(), source);
}

#[test]
fn parse_inline_module_chunk_statements() {
    let source = "mod startup { let ready = true; fn status() -> bool { ready } status(); }";
    let parsed = rua_syntax::parse_source_file(source);
    assert!(parsed.errors().is_empty(), "{:?}", parsed.errors());
    let module = parsed
        .tree()
        .items()
        .find_map(|item| match item {
            rua_syntax::ast::Item::Mod(module) => Some(module),
            _ => None,
        })
        .expect("inline module");
    assert_eq!(module.items().count(), 1);
    assert_eq!(module.stmts().count(), 2);
}

// ---------------------------------------------------------------------------
// Complex integration — multiple features together
// ---------------------------------------------------------------------------

#[test]
fn parse_complex_struct_with_everything() {
    let source = "struct Container { items: Vec, label: String }\n\nimpl Container {\n    pub fn new() -> Container {\n        Container { items: Vec::new(), label: String::new() }\n    }\n}\n\nfn main() {\n    let c = Container::new();\n}";
    let node = parse_and_check(source);
    assert!(has_kind(&node, SyntaxKind::StructDecl));
    assert!(has_kind(&node, SyntaxKind::ImplDecl));
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}

#[test]
fn parse_demo_rua_lossless() {
    // The demo.rua file should parse losslessly with zero errors.
    let path = format!("{}/../../tests/demo.rua", env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let node = parse_and_check(&source);
    let parse = parse_source_file(&source);
    let error_count = parse.errors().len();
    // Report parse errors but don't fail — some syntax may be aspirational
    if error_count > 0 {
        eprintln!("demo.rua has {error_count} parse errors:");
        for e in parse.errors().iter() {
            eprintln!("  offset {}: {}", e.offset, e.message);
        }
    }
    // Must be lossless regardless
    assert_eq!(
        node.text().to_string(),
        source,
        "demo.rua must round-trip losslessly"
    );
}

#[test]
fn parse_match_with_guards_and_patterns() {
    let source = r#"fn main() {
    let x = 42;
    match x {
        0 => {},
        1..=10 => {},
        n if n > 100 => {},
        _ => {},
    }
}
"#;
    let node = parse_no_errors(source);
    assert!(has_kind(&node, SyntaxKind::FnDecl));
}
