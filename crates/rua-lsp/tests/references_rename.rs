//! Enhanced references and rename tests — cross-file, write tracking, validation.

mod support;

use support::{uri, TestServer};

#[test]
fn references_cross_file_parses_and_intra_file_works() {
    // Cross-file references require module/project linking. Verify both
    // files parse correctly and intra-file references work in each.
    let uri_a = uri("/proj/src/a.rua");
    let uri_b = uri("/proj/src/b.rua");
    let mut srv = TestServer::new();

    srv.open(&uri_b, "pub fn helper() -> i64 { 42 }");
    srv.open(&uri_a, "fn main() { helper(); }");

    let analysis = srv.snapshot();

    // Both files parse cleanly
    let a_id = srv.file_id_for_uri(&uri_a).unwrap();
    let b_id = srv.file_id_for_uri(&uri_b).unwrap();
    assert!(analysis.parse(a_id).errors().is_empty());
    assert!(analysis.parse(b_id).errors().is_empty());

    // Intra-file references on `helper` in b.rua should find the declaration
    // `helper` starts at byte 7 in `pub fn helper() -> i64 { 42 }`
    let pp_b = srv.pp(&uri_b, 0, 7).unwrap();
    let refs_b = analysis.references(pp_b, true);
    assert!(
        !refs_b.is_empty(),
        "intra-file refs to helper should find declaration, got {refs_b:?}"
    );

    // References on `main` in a.rua should find the function
    // `main` starts at byte 3 in `fn main() { helper(); }`
    let pp_a = srv.pp(&uri_a, 0, 3).unwrap();
    let refs_a = analysis.references(pp_a, true);
    assert!(
        !refs_a.is_empty(),
        "intra-file refs to main should find declaration, got {refs_a:?}"
    );
}

#[test]
fn references_include_declaration() {
    let uri = uri("/test/refs_decl.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { let x = 42; x }");

    // cursor on the tail `x`
    let pp = srv.pp(&uri, 0, 24).unwrap();

    let without_decl = srv.snapshot().references(pp, false);
    let with_decl = srv.snapshot().references(pp, true);

    // With declaration=true should return more references
    assert!(
        with_decl.len() >= without_decl.len(),
        "with_decl ({}) should be >= without_decl ({})",
        with_decl.len(),
        without_decl.len()
    );
}

#[test]
fn references_function_called_multiple_times() {
    let uri = uri("/test/refs_multi.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "fn helper() -> i64 { 42 }\nfn main() { helper(); helper(); }",
    );

    // cursor on `helper` definition
    let pp = srv.pp(&uri, 0, 3).unwrap();
    let refs = srv.snapshot().references(pp, true);
    // Should find: 1 declaration + 2 call sites = 3 references
    assert!(
        refs.len() >= 3,
        "should find at least 3 references, got {refs:?}"
    );
}

#[test]
fn rename_prepare_returns_range() {
    let uri = uri("/test/rename_prep.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { let count = 0; count }");

    // cursor on the tail `count`
    let pp = srv.pp(&uri, 0, 27).unwrap();
    let target = srv.snapshot().prepare_rename(pp);
    assert!(
        target.is_some(),
        "prepare_rename should return target for local variable"
    );
}

#[test]
fn rename_prepare_on_keyword_returns_none() {
    let uri = uri("/test/rename_kw.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { let x = 1; }");

    // cursor on `fn` keyword
    let pp = srv.pp(&uri, 0, 0).unwrap();
    let target = srv.snapshot().prepare_rename(pp);
    assert!(
        target.is_none(),
        "prepare_rename should return None for keyword"
    );
}

#[test]
fn rename_verifies_new_name_is_valid() {
    let uri = uri("/test/rename_valid.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { let count = 0; count }");

    // cursor on tail `count`
    let pp = srv.pp(&uri, 0, 27).unwrap();
    let result = srv.snapshot().rename(pp, "total");
    assert!(
        result.is_ok(),
        "rename to valid name should succeed, got: {result:?}"
    );
}
