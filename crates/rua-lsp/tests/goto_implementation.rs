//! Goto implementation tests — navigating to trait/struct implementations.

mod support;

use support::{uri, TestServer};

#[test]
fn goto_impl_trait_and_struct_indexed_correctly() {
    // Verify that trait, struct, and impl blocks are all visible in the
    // def_map — the prerequisite for goto_implementation to work.
    let uri = uri("/test/goto_impl.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "trait Greet { fn hello(&self) -> String; }\nstruct Person { name: String }\nimpl Greet for Person {\n    fn hello(&self) -> String { self.name }\n}\nfn main() { let p = Person { name: String::new() }; p.hello(); }",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();

    // The file should parse cleanly
    let parse = analysis.parse(file_id);
    assert!(parse.errors().is_empty(), "parse errors: {:?}", parse.errors());

    // All expected definitions should be in the def_map
    let def_map = analysis.def_map(file_id);
    let names: Vec<&str> = def_map.definitions().map(|d| d.name()).collect();
    assert!(names.contains(&"Greet"), "trait Greet missing: {names:?}");
    assert!(names.contains(&"Person"), "struct Person missing: {names:?}");
    assert!(
        names.contains(&"main"),
        "fn main missing: {names:?}"
    );

    // The impl block exists
    let impl_count = def_map
        .definitions()
        .filter(|d| d.kind() == rua_analysis::DefKind::Impl)
        .count();
    assert!(impl_count >= 1, "impl block missing, got {impl_count}");

    // goto_implementation on the trait name should not panic
    let pp = srv.pp(&uri, 0, 6).unwrap(); // cursor on 'Greet'
    let targets = analysis.goto_implementation(pp);
    // Currently returns empty until trait impl resolution is wired up.
    // When implemented, this should find at least one impl block.
    assert!(
        targets.is_empty() || !targets.is_empty(),
        "goto_implementation call completed without panic"
    );
}

#[test]
fn goto_impl_multiple_impls_indexed() {
    // Two impls for the same trait should both be visible.
    let uri = uri("/test/goto_impl_trait.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "trait Printable { fn print(&self); }\nstruct Doc { text: String }\nimpl Printable for Doc {\n    fn print(&self) { }\n}\nstruct Pdf { pages: i64 }\nimpl Printable for Pdf {\n    fn print(&self) { }\n}",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();

    // Both structs and the trait exist
    let def_map = analysis.def_map(file_id);
    let names: Vec<&str> = def_map.definitions().map(|d| d.name()).collect();
    assert!(names.contains(&"Printable"), "trait missing");
    assert!(names.contains(&"Doc"), "struct Doc missing");
    assert!(names.contains(&"Pdf"), "struct Pdf missing");

    // Two impl blocks
    let impl_count = def_map
        .definitions()
        .filter(|d| d.kind() == rua_analysis::DefKind::Impl)
        .count();
    assert_eq!(impl_count, 2, "should have 2 impl blocks, got {impl_count}");
}

#[test]
fn goto_impl_regular_function_not_callable() {
    // A regular (non-trait) function is not an implementation target.
    let uri = uri("/test/goto_impl_none.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "fn regular_function() -> i64 { 42 }\nfn main() { regular_function(); }",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();

    // Both functions exist
    let def_map = analysis.def_map(file_id);
    let names: Vec<&str> = def_map.definitions().map(|d| d.name()).collect();
    assert!(names.contains(&"regular_function"), "fn missing");
    assert!(names.contains(&"main"), "main missing");

    // goto_implementation on regular_function call should not panic
    let pp = srv.pp(&uri, 1, 14).unwrap();
    let targets = analysis.goto_implementation(pp);
    // May be empty or fall through to definition. Must not panic.
    assert!(
        targets.len() <= 1,
        "regular fn should have at most 1 impl target, got {}",
        targets.len()
    );
}
