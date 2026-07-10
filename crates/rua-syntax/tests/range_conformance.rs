//! Byte-range conformance between the IDE CST and compiler parser.
//!
//! The parsers intentionally build different trees. These tests compare only
//! the source anchors required by IDE navigation: tokens and identifiers,
//! declaration names, paths, and field/method use sites.

use std::path::{Path, PathBuf};

use rua_syntax::ast::{
    ClosureExpr as SyntaxClosureExpr, FieldExpr as SyntaxFieldExpr, Item as SyntaxItem,
    MethodCallExpr as SyntaxMethodCallExpr, PathExpr as SyntaxPathExpr, Stmt as SyntaxStmt,
};
use rua_syntax::{AstNode, Named, SyntaxKind, SyntaxNode, SyntaxToken, lex, parse_source_file};
use ruac::ast::{
    ClosureBody, Expr, ExprKind, Item as CompilerItem, Stmt as CompilerStmt,
};
use ruac::token::{RuaTokenKind, SourceRange};
use ruac::tokenize::RuaTokenize;

const MIN_RANGE_CASES: usize = 15;

fn range_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden/parser/ranges")
}

fn range_cases() -> Vec<PathBuf> {
    let root = range_root();
    let mut cases: Vec<_> = std::fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("read {}: {error}", root.display()))
        .map(|entry| entry.expect("read range fixture entry").path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("rua"))
        .collect();
    cases.sort();
    assert!(
        cases.len() >= MIN_RANGE_CASES,
        "range corpus has {} cases; expected at least {MIN_RANGE_CASES}",
        cases.len()
    );
    cases
}

fn byte_range(range: rowan::TextRange) -> (usize, usize) {
    (usize::from(range.start()), usize::from(range.end()))
}

fn compiler_range(range: SourceRange) -> (usize, usize) {
    (range.start, range.end())
}

fn assert_range_eq(label: &str, source: &str, compiler: SourceRange, syntax: rowan::TextRange) {
    let compiler = compiler_range(compiler);
    let syntax = byte_range(syntax);
    assert_eq!(
        compiler,
        syntax,
        "{label} range drifted: compiler={:?}, syntax={:?}, compiler_text={:?}, syntax_text={:?}",
        compiler,
        syntax,
        &source[compiler.0..compiler.1],
        &source[syntax.0..syntax.1],
    );
}

fn assert_range_inside(label: &str, inner: SourceRange, outer: rowan::TextRange) {
    let inner = compiler_range(inner);
    let outer = byte_range(outer);
    assert!(
        inner.0 >= outer.0 && inner.1 <= outer.1,
        "{label} compiler anchor {inner:?} is outside CST item {outer:?}"
    );
}

fn compiler_tokens(source: &str) -> Vec<(String, usize, usize, bool)> {
    let mut tokenizer = RuaTokenize::new(source);
    let mut tokens = Vec::new();
    loop {
        let token = tokenizer
            .next_token()
            .expect("range fixture must lex in ruac");
        if token.kind == RuaTokenKind::Eof {
            break;
        }
        tokens.push((
            source[token.range.start..token.range.end()].to_owned(),
            token.range.start,
            token.range.end(),
            token.kind == RuaTokenKind::Ident,
        ));
    }
    tokens
}

fn syntax_tokens(source: &str) -> Vec<(String, usize, usize, bool)> {
    lex(source)
        .into_iter()
        .filter(|token| !token.kind.is_trivia())
        .map(|token| {
            let end = token.start + token.len;
            (
                source[token.start..end].to_owned(),
                token.start,
                end,
                token.kind == SyntaxKind::Ident,
            )
        })
        .collect()
}

fn function(program: &ruac::ast::Program, index: usize) -> &ruac::ast::FnDecl {
    let CompilerItem::Fn(function) = &program.items[index] else {
        panic!("compiler item {index} is not a function")
    };
    function
}

fn tail_expr(function: &ruac::ast::FnDecl) -> &Expr {
    function
        .body
        .tail
        .as_deref()
        .expect("range test function must have a tail expression")
}

fn token_named(token: Option<SyntaxToken>, expected: &str) -> SyntaxToken {
    let token = token.unwrap_or_else(|| panic!("missing CST identifier `{expected}`"));
    assert_eq!(token.text(), expected);
    token
}

fn node_text(node: &SyntaxNode) -> String {
    node.text().to_string()
}

#[test]
fn range_conformance_tokens_and_identifiers() {
    for path in range_cases() {
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        assert_eq!(
            compiler_tokens(&source),
            syntax_tokens(&source),
            "token range drift in {}",
            path.display()
        );
    }
}

#[test]
fn range_conformance_item_function_and_field_definitions() {
    let source = "pub struct Point { pub x: i64, y: i64 }\n\
                  fn read(point: Point) -> i64 { point.x }\n";
    let compiler = ruac::parser::parse(source).expect("compiler parser accepts definition case");
    let parsed = parse_source_file(source);
    assert!(parsed.errors().is_empty(), "{:?}", parsed.errors());

    let CompilerItem::Struct(compiler_struct) = &compiler.items[0] else {
        panic!("first compiler item is not a struct")
    };
    let SyntaxItem::Struct(syntax_struct) = parsed.tree.items().next().expect("CST struct item")
    else {
        panic!("first CST item is not a struct")
    };
    for (compiler_field, syntax_field) in compiler_struct.fields.iter().zip(
        syntax_struct
            .field_list()
            .expect("CST struct field list")
            .fields(),
    ) {
        let name = token_named(syntax_field.name(), &compiler_field.name);
        assert_range_eq(
            "struct field definition",
            source,
            compiler_field.name_span,
            name.text_range(),
        );
        assert_range_inside(
            "struct field item",
            compiler_field.name_span,
            syntax_field.syntax().text_range(),
        );
    }

    let compiler_fn = function(&compiler, 1);
    let SyntaxItem::Fn(syntax_fn) = parsed.tree.items().nth(1).expect("CST function item") else {
        panic!("second CST item is not a function")
    };
    let fn_name = token_named(syntax_fn.name(), &compiler_fn.name);
    assert_range_eq(
        "function definition",
        source,
        compiler_fn.name_span,
        fn_name.text_range(),
    );
    assert_range_inside(
        "function item",
        compiler_fn.name_span,
        syntax_fn.syntax().text_range(),
    );

    let compiler_param = &compiler_fn.params[0];
    let syntax_param = syntax_fn.params().next().expect("CST function parameter");
    let param_name = token_named(syntax_param.name(), &compiler_param.name);
    assert_range_eq(
        "function parameter",
        source,
        compiler_param.name_span,
        param_name.text_range(),
    );
}

#[test]
fn range_conformance_path_and_member_access() {
    let source = "fn read(service: Service) -> i64 {\n\
                      outer::inner::normalize(service.fetch(1).value)\n\
                  }\n";
    let compiler = ruac::parser::parse(source).expect("compiler parser accepts expression case");
    let parsed = parse_source_file(source);
    assert!(parsed.errors().is_empty(), "{:?}", parsed.errors());

    let ExprKind::Call { callee, args } = &tail_expr(function(&compiler, 0)).kind else {
        panic!("compiler tail is not a call")
    };
    let ExprKind::Path(path) = &callee.kind else {
        panic!("compiler callee is not a path")
    };
    assert_eq!(path, &["outer", "inner", "normalize"]);
    let syntax_path = parsed
        .syntax_node()
        .descendants()
        .filter_map(SyntaxPathExpr::cast)
        .find(|path| node_text(path.syntax()) == "outer::inner::normalize")
        .expect("qualified CST path");
    assert_range_eq(
        "qualified path expression",
        source,
        callee.span,
        syntax_path.syntax().text_range(),
    );

    let ExprKind::Field {
        base,
        name,
        name_span,
    } = &args[0].kind
    else {
        panic!("compiler call argument is not a field access")
    };
    let syntax_field = parsed
        .syntax_node()
        .descendants()
        .filter_map(SyntaxFieldExpr::cast)
        .find(|field| field.field_name().is_some_and(|token| token.text() == name))
        .expect("CST field access");
    let syntax_field_name = token_named(syntax_field.field_name(), name);
    assert_range_eq(
        "field use-site identifier",
        source,
        *name_span,
        syntax_field_name.text_range(),
    );
    assert_range_eq(
        "field access expression",
        source,
        args[0].span,
        syntax_field.syntax().text_range(),
    );

    let ExprKind::MethodCall {
        method,
        method_span,
        ..
    } = &base.kind
    else {
        panic!("compiler field base is not a method call")
    };
    let syntax_method = parsed
        .syntax_node()
        .descendants()
        .filter_map(SyntaxMethodCallExpr::cast)
        .find(|call| {
            call.method_name()
                .is_some_and(|token| token.text() == method)
        })
        .expect("CST method call");
    let syntax_method_name = token_named(syntax_method.method_name(), method);
    assert_range_eq(
        "method use-site identifier",
        source,
        *method_span,
        syntax_method_name.text_range(),
    );
    assert_range_eq(
        "method call expression",
        source,
        base.span,
        syntax_method.syntax().text_range(),
    );
}

#[test]
fn range_conformance_closure_params_and_body() {
    let source =
        "fn main() { let add = |left: i64, right| -> i64 left + right; }\n";
    let compiler = ruac::parser::parse(source).expect("compiler parser accepts closure case");
    let parsed = parse_source_file(source);
    assert!(parsed.errors().is_empty(), "{:?}", parsed.errors());

    let CompilerStmt::Let { init, .. } = &function(&compiler, 0).body.stmts[0] else {
        panic!("compiler statement is not a closure binding")
    };
    let ExprKind::Closure {
        params,
        body: ClosureBody::Expr(compiler_body),
        ..
    } = &init.kind
    else {
        panic!("compiler initializer is not an expression closure")
    };
    let SyntaxItem::Fn(syntax_function) =
        parsed.tree.items().next().expect("CST function item")
    else {
        panic!("CST item is not a function")
    };
    let SyntaxStmt::Let(syntax_binding) = syntax_function
        .body()
        .expect("CST function body")
        .stmts()
        .next()
        .expect("CST closure binding")
    else {
        panic!("CST statement is not a closure binding")
    };
    let syntax_closure = match syntax_binding.init().expect("CST closure initializer") {
        rua_syntax::ast::Expr::Closure(closure) => closure,
        _ => panic!("CST initializer is not a closure"),
    };

    assert_range_eq(
        "closure expression",
        source,
        init.span,
        syntax_closure.syntax().text_range(),
    );
    for (compiler_param, syntax_param) in params.iter().zip(syntax_closure.params()) {
        let syntax_name = token_named(syntax_param.name(), &compiler_param.name);
        assert_range_eq(
            "closure parameter",
            source,
            compiler_param.name_span,
            syntax_name.text_range(),
        );
    }
    let syntax_body = syntax_closure.body().expect("CST closure body");
    assert_range_eq(
        "closure body",
        source,
        compiler_body.span,
        syntax_body.syntax().text_range(),
    );

    let cast = parsed
        .syntax_node()
        .descendants()
        .find_map(SyntaxClosureExpr::cast)
        .expect("typed closure cast");
    assert_eq!(cast.params().count(), 2);
    assert!(cast.ret_type().is_some());
}
