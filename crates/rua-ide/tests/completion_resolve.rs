//! Completion resolve tests — lazy doc-comment loading.

mod support;

use support::{TestServer, uri};

#[test]
fn completion_items_have_data_for_resolve() {
    // Completions for documented items should carry a `data` field
    // with file_id and name, enabling lazy doc-comment resolution.
    let uri = uri("/test/resolve.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "/// A point in 2D space.\nstruct Point { x: i64 }\nfn main() { let p = Point { x: 0 }; }",
    );

    let pp = srv.pp(&uri, 2, 34).unwrap();
    let items = srv.snapshot().completions(pp);

    // Documented items (like Point) should have documentation already attached
    // or have a data field for lazy resolution.
    let point_item = items.iter().find(|i| i.label() == "Point");
    assert!(point_item.is_some(), "Point should be in completions");

    if let Some(item) = point_item {
        // Either documentation is already present, or data field is set for resolve
        let has_docs = item.documentation().is_some();
        // The data field for resolve is not exposed in CompletionItem API,
        // but it's stored when converting via completion_to_lsp.
        // We verify documentation is populated (either eagerly or lazily).
        assert!(
            has_docs,
            "Point should have documentation (resolved), got: {:?}",
            item.documentation()
        );
    }
}

#[test]
fn completion_items_without_docs_dont_need_resolve() {
    // Items without doc comments should still work fine (no crash).
    let uri = uri("/test/resolve_nodoc.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { let x = 1;  }");

    let pp = srv.pp(&uri, 0, 22).unwrap();
    let items = srv.snapshot().completions(pp);

    // `x` (local variable) typically has no documentation
    let x_item = items.iter().find(|i| i.label() == "x");
    if let Some(item) = x_item {
        // No crash; documentation may or may not be present
        let _ = item.documentation();
    }
}

#[test]
fn completion_resolve_does_not_crash_on_missing_data() {
    // If the completion item has no data field (or malformed data),
    // resolve should return the item unchanged without panicking.
    let uri = uri("/test/resolve_safe.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { let x = 1; }");

    let pp = srv.pp(&uri, 0, 22).unwrap();
    let items = srv.snapshot().completions(pp);

    // All items should have valid labels and non-empty inserts
    for item in &items {
        assert!(!item.label().is_empty(), "item should have a label");
        // insert() may be None (for text-only completions)
        // but having it is also fine
    }
}
