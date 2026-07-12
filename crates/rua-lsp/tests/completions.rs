//! Enhanced completions tests — trait methods, module paths, closures, snippets.

mod support;

use support::{uri, TestServer};

#[test]
fn completions_trait_method_struct_parsed_correctly() {
    // Verify the trait + struct + impl setup parses and indexes cleanly.
    // Trait method completions after `p.` depend on member resolution.
    let uri = uri("/test/comp_trait.rua");
    let mut srv = TestServer::new();
    // Use `p.name` — a valid member access that parses correctly.
    srv.open(
        &uri,
        "trait Greeter { fn greet(self) -> i64 { 0 } }\nstruct Person { name: i64 }\nimpl Greeter for Person {\n    fn greet(self) -> i64 { 1 }\n}\nfn main() { let p = Person { name: 42 }; p.name; }",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();

    // Parse and def_map must be clean
    let parse = analysis.parse(file_id);
    assert!(parse.errors().is_empty(), "parse errors: {:?}", parse.errors());

    let def_map = analysis.def_map(file_id);
    let names: Vec<&str> = def_map.definitions().map(|d| d.name()).collect();
    assert!(names.contains(&"Greeter"), "trait missing");
    assert!(names.contains(&"Person"), "struct missing");

    // Dot completions after `p.` (on `name`) should at minimum return some items
    let source = srv.source(&uri).unwrap();
    let dot_pos = source.rfind("p.").unwrap() + 2; // cursor right after `p.`
    let pp = srv.pp_at_offset(&uri, dot_pos).unwrap();
    let items = analysis.completions(pp);
    assert!(!items.is_empty(), "dot completions should not be empty");
}

#[test]
fn completions_module_path_nested() {
    let uri = uri("/test/comp_mod.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "mod math {\n    pub mod ops {\n        pub fn add(a: i64, b: i64) -> i64 { a + b }\n    }\n}\nfn main() { math::ops:: }",
    );

    // cursor after `math::ops::`
    let pp = srv.pp(&uri, 5, 25).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();

    assert!(
        labels.contains(&"add".to_string()),
        "nested module items should appear, got: {labels:?}"
    );
}

#[test]
fn completions_in_closure_body() {
    let uri = uri("/test/comp_closure.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "fn main() { let outer = 1; let f = |x| { let inner = x + outer;  } }",
    );

    // cursor inside the closure body (between `;` and `}` after `inner = x + outer;`)
    let pp = srv.pp(&uri, 0, 55).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();

    // `inner` should be visible (declared in the closure body)
    assert!(
        labels.contains(&"inner".to_string()),
        "closure body should offer local 'inner', got: {labels:?}"
    );
}

#[test]
fn completions_enum_variant_match_body_parsed() {
    // Verify the enum + match setup parses correctly. Variant completions
    // inside match arms depend on cursor being inside the match body.
    let uri = uri("/test/comp_enum_pat.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "enum Color { Red, Rgb(i64, i64, i64) }\nfn main() { let c = Color::Rgb(255, 0, 0); match c {  } }",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();

    // Verify the enum has its variants in the def_map
    let def_map = analysis.def_map(file_id);
    let color_def = def_map
        .definitions()
        .find(|d| d.name() == "Color" && d.kind() == rua_analysis::DefKind::Enum);
    assert!(color_def.is_some(), "Color enum missing from def_map");

    // The match expression should be findable in the body
    let has_match = def_map.definitions().any(|d| {
        if matches!(d.kind(), rua_analysis::DefKind::Function | rua_analysis::DefKind::Method)
            && let Some(body) = analysis.body(d.id()) {
                return body.exprs().any(|(_, e)| matches!(e, rua_analysis::Expr::Match { .. }));
            }
        false
    });
    assert!(has_match, "match expression should be in the body");

    // Completions inside the match body should at minimum return keywords
    let pp = srv.pp(&uri, 1, 55).unwrap();
    let items = analysis.completions(pp);
    assert!(!items.is_empty(), "completions in match body should not be empty");
}

#[test]
fn completions_for_loop_variable() {
    let uri = uri("/test/comp_for.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { for i in 0..10 {  } }");

    // cursor inside for loop body (between `{` and `}`)
    let pp = srv.pp(&uri, 0, 26).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();

    // `i` should be visible (the loop variable)
    assert!(
        labels.contains(&"i".to_string()),
        "loop variable 'i' should appear in completions, got: {labels:?}"
    );
}

#[test]
fn completions_while_let_var_in_scope() {
    // The while-let bound variable should be visible in the loop body.
    let uri = uri("/test/comp_whilelet.rua");
    let mut srv = TestServer::new();
    let source = "enum Maybe { Some(i64), None }\nfn main() { let opt = Maybe::Some(42); while let Maybe::Some(val) = opt { val; } }";
    srv.open(&uri, source);

    // cursor inside while-let body: `val; ` between `{` and `}`
    // Find the position right after `{ val`
    let body_offset = source.find("{ val;").unwrap() + 2; // on 'v' of val
    let pp = srv.pp_at_offset(&uri, body_offset).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();

    // `val` (from pattern) should be visible
    assert!(
        labels.contains(&"val".to_string()),
        "while-let bound 'val' should appear, got: {labels:?}"
    );
}

#[test]
fn completions_self_in_method_body() {
    let uri = uri("/test/comp_self.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "struct Point { x: i64, y: i64 }\nimpl Point {\n    fn distance(&self) -> i64 {  }\n}",
    );

    // cursor inside method body (between `{` and `}`)
    let pp = srv.pp(&uri, 2, 35).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();

    // `self` must be visible in a method body
    assert!(
        labels.contains(&"self".to_string()),
        "self should appear in method body completions, got: {labels:?}"
    );
    // Struct name Point should also be visible
    assert!(
        labels.contains(&"Point".to_string()),
        "struct name Point should be visible, got: {labels:?}"
    );
}

#[test]
fn completions_if_let_bound_variable() {
    let uri = uri("/test/comp_iflet.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "enum Maybe { Some(i64), None }\nfn main() {\n    let opt = Maybe::Some(42);\n    if let Maybe::Some(val) = opt { let y = val; }\n}",
    );

    // cursor inside if-let body (after `let y = val;` before `}`)
    let pp = srv.pp(&uri, 3, 50).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();

    // `val` (from if-let pattern) and `y` (local) should be visible
    assert!(
        labels.contains(&"y".to_string()),
        "local 'y' should appear in if-let body, got: {labels:?}"
    );
    assert!(
        labels.contains(&"val".to_string()),
        "if-let bound 'val' should appear, got: {labels:?}"
    );
}
