//! Type hierarchy tests — supertype/subtype navigation.

mod support;

use support::{TestServer, uri};

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

    let subtypes = srv.snapshot().type_hierarchy_subtypes(&item);
    let mut names = subtypes
        .iter()
        .map(|subtype| subtype.name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(names, ["Circle", "Square"]);
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

    let supertypes = srv.snapshot().type_hierarchy_supertypes(&item);
    let names = supertypes
        .iter()
        .map(|supertype| supertype.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["Drawable"]);
}

#[test]
fn type_hierarchy_on_function_returns_none() {
    let uri = uri("/test/typehier_none.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "fn my_function() -> i64 { 42 }\nfn main() { my_function(); }",
    );

    // cursor on `my_function` name
    let pp = srv.pp(&uri, 0, 3).unwrap();
    let item = srv.snapshot().type_hierarchy_prepare(pp);
    // Functions are not type-hierarchy items
    assert!(
        item.is_none(),
        "type hierarchy should return None for a function"
    );
}

#[test]
fn type_hierarchy_isolates_same_named_traits_by_definition_identity() {
    let main_uri = uri("/test/main.rua");
    let first_uri = uri("/test/first.rua");
    let second_uri = uri("/test/second.rua");
    let mut srv = TestServer::new();
    srv.open(&main_uri, "fn main() {}");
    srv.open(
        &first_uri,
        "pub trait Marker {}\npub struct One {}\nimpl Marker for One {}\n",
    );
    srv.open(
        &second_uri,
        "pub trait Marker {}\npub struct Two {}\nimpl Marker for Two {}\n",
    );

    let first = srv
        .snapshot()
        .type_hierarchy_prepare(srv.pp(&first_uri, 0, 10).unwrap())
        .unwrap();
    let second = srv
        .snapshot()
        .type_hierarchy_prepare(srv.pp(&second_uri, 0, 10).unwrap())
        .unwrap();
    assert_ne!(first.target, second.target);

    let subtypes = srv.snapshot().type_hierarchy_subtypes(&first);
    assert_eq!(
        subtypes
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        ["One"]
    );
}
