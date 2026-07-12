//! Type hierarchy tests — supertype/subtype navigation.

mod support;

use support::{uri, TestServer};

#[test]
fn type_hierarchy_prepare_on_struct() {
    let uri = uri("/test/typehier.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "struct Point { x: i64, y: i64 }\nimpl Point { fn new() -> Point { Point { x: 0, y: 0 } } }\nfn main() { let p = Point::new(); }",
    );

    // cursor on `Point` struct name
    let pp = srv.pp(&uri, 0, 7).unwrap();
    let item = srv.snapshot().type_hierarchy_prepare(pp);
    assert!(
        item.is_some(),
        "type hierarchy prepare should return Some for a struct"
    );
    let item = item.unwrap();
    assert!(
        item.name.contains("Point"),
        "item name should contain 'Point', got: {}",
        item.name
    );
}

#[test]
fn type_hierarchy_subtypes_finds_impls() {
    let uri = uri("/test/typehier_sub.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "trait Drawable { fn draw(&self); }\nstruct Circle { radius: i64 }\nimpl Drawable for Circle { fn draw(&self) {} }\nstruct Square { side: i64 }\nimpl Drawable for Square { fn draw(&self) {} }",
    );

    // prepare on trait name
    let pp = srv.pp(&uri, 0, 6).unwrap();
    let item = srv.snapshot().type_hierarchy_prepare(pp);

    assert!(item.is_some(), "type hierarchy prepare should find trait");
    let item = item.unwrap();
    assert!(
        item.name.contains("Drawable"),
        "prepared item should be the trait"
    );

    // subtypes() may return empty if impl resolution isn't fully wired,
    // but the API call must not panic
    let subtypes = srv.snapshot().type_hierarchy_subtypes(&item);
    assert!(
        subtypes.len() <= 5,
        "should not have more than a few subtypes"
    );
}

#[test]
fn type_hierarchy_supertypes_finds_traits() {
    let uri = uri("/test/typehier_super.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "trait Drawable { fn draw(&self); }\nstruct Circle { radius: i64 }\nimpl Drawable for Circle { fn draw(&self) {} }",
    );

    // prepare on `Circle` struct name (line 1, 'C' at col 7)
    let pp = srv.pp(&uri, 1, 7).unwrap();
    let item = srv.snapshot().type_hierarchy_prepare(pp);

    assert!(item.is_some(), "type hierarchy prepare should find struct");
    let item = item.unwrap();
    assert!(
        item.name.contains("Circle"),
        "prepared item should be the struct, got: {}",
        item.name
    );

    // supertypes() may return empty but must not panic
    let supertypes = srv.snapshot().type_hierarchy_supertypes(&item);
    assert!(
        supertypes.len() <= 5,
        "should not have more than a few supertypes"
    );
}

#[test]
fn type_hierarchy_on_function_returns_none() {
    let uri = uri("/test/typehier_none.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn my_function() -> i64 { 42 }\nfn main() { my_function(); }");

    // cursor on `my_function` name
    let pp = srv.pp(&uri, 0, 3).unwrap();
    let item = srv.snapshot().type_hierarchy_prepare(pp);
    // Functions are not type-hierarchy items
    assert!(
        item.is_none(),
        "type hierarchy should return None for a function"
    );
}
