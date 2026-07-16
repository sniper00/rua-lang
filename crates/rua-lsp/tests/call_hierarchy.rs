//! Call hierarchy tests — incoming/outgoing call navigation.

mod support;

use support::{TestServer, uri};

#[test]
fn call_hierarchy_prepare_on_function() {
    let uri = uri("/test/callhier.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "fn target() -> i64 { 42 }\nfn caller() -> i64 { target() }\nfn main() { caller(); }",
    );

    // cursor on `target` definition name
    let pp = srv.pp(&uri, 0, 3).unwrap();
    let item = srv.snapshot().call_hierarchy_prepare(pp);
    assert!(
        item.is_some(),
        "call hierarchy prepare should return Some for a function"
    );
    let item = item.unwrap();
    assert!(
        item.name.contains("target"),
        "item name should contain 'target', got: {}",
        item.name
    );
}

#[test]
fn call_hierarchy_incoming_finds_callers() {
    let uri = uri("/test/callhier_in.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "fn target() -> i64 { 42 }\nfn caller_a() -> i64 { target() }\nfn caller_b() -> i64 { target() }\nfn main() { caller_a(); }",
    );

    // prepare on target
    let pp = srv.pp(&uri, 0, 3).unwrap();
    let item = srv.snapshot().call_hierarchy_prepare(pp);
    assert!(item.is_some(), "call hierarchy prepare should succeed");

    let incoming = srv.snapshot().call_hierarchy_incoming(&item.unwrap());
    let mut names = incoming
        .iter()
        .map(|caller| caller.name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(names, ["caller_a", "caller_b"]);
    assert!(
        incoming.iter().all(|caller| caller.call_sites.len() == 1),
        "each caller should expose its exact call site: {incoming:?}"
    );
}

#[test]
fn call_hierarchy_outgoing_finds_callees() {
    let uri = uri("/test/callhier_out.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "fn helper_a() -> i64 { 1 }\nfn helper_b() -> i64 { 2 }\nfn caller() -> i64 { helper_a() + helper_b() }\nfn main() { caller(); }",
    );

    // prepare on caller
    let pp = srv.pp(&uri, 2, 3).unwrap();
    let item = srv.snapshot().call_hierarchy_prepare(pp);
    assert!(item.is_some(), "call hierarchy prepare should succeed");

    let outgoing = srv.snapshot().call_hierarchy_outgoing(&item.unwrap());
    let mut names = outgoing
        .iter()
        .map(|callee| callee.name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(names, ["helper_a", "helper_b"]);
    assert!(
        outgoing.iter().all(|callee| callee.call_sites.len() == 1),
        "each callee should expose its exact call site: {outgoing:?}"
    );
}

#[test]
fn call_hierarchy_on_non_callable_returns_none() {
    let uri = uri("/test/callhier_none.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "struct Point { x: i64 }\nfn main() { let p = Point { x: 0 }; }",
    );

    // cursor on `Point` struct name
    let pp = srv.pp(&uri, 0, 7).unwrap();
    let item = srv.snapshot().call_hierarchy_prepare(pp);
    // Struct is not callable — should return None
    assert!(
        item.is_none(),
        "call hierarchy should return None for a struct"
    );
}

#[test]
fn call_hierarchy_on_keyword_returns_none() {
    let uri = uri("/test/callhier_kw.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { let x = 1; }");

    // cursor on `let` keyword
    let pp = srv.pp(&uri, 0, 12).unwrap();
    let item = srv.snapshot().call_hierarchy_prepare(pp);
    assert!(
        item.is_none(),
        "call hierarchy should return None for a keyword"
    );
}

#[test]
fn call_hierarchy_isolates_same_named_functions_by_definition_identity() {
    let main_uri = uri("/test/main.rua");
    let first_uri = uri("/test/first.rua");
    let second_uri = uri("/test/second.rua");
    let mut srv = TestServer::new();
    srv.open(&main_uri, "fn main() {}");
    srv.open(
        &first_uri,
        "pub fn helper() {}\npub fn caller() { helper(); }\n",
    );
    srv.open(
        &second_uri,
        "pub fn helper() {}\npub fn caller() { helper(); }\n",
    );

    let first_helper = srv
        .snapshot()
        .call_hierarchy_prepare(srv.pp(&first_uri, 0, 7).unwrap())
        .unwrap();
    let second_helper = srv
        .snapshot()
        .call_hierarchy_prepare(srv.pp(&second_uri, 0, 7).unwrap())
        .unwrap();
    let first_caller = srv
        .snapshot()
        .call_hierarchy_prepare(srv.pp(&first_uri, 1, 7).unwrap())
        .unwrap();
    let outgoing = srv.snapshot().call_hierarchy_outgoing(&first_caller);

    assert_eq!(outgoing.len(), 1, "{outgoing:?}");
    assert_eq!(outgoing[0].target, first_helper.target);
    assert_ne!(outgoing[0].target, second_helper.target);
}
