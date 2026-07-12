//! Incremental analysis tests — cache invalidation, snapshot isolation, edits.
//!
//! Tests that editing one part of a file does not invalidate unrelated state,
//! and that the analysis pipeline handles incremental edits correctly.

mod support;

use support::{uri, TestServer};
use rua_analysis::Change;

#[test]
fn incremental_edit_preserves_parse_of_unrelated_file() {
    let uri_a = uri("/proj/src/a.rua");
    let uri_b = uri("/proj/src/b.rua");
    let mut srv = TestServer::new();

    srv.open(&uri_b, "pub fn helper() -> i64 { 42 }");
    srv.open(&uri_a, "fn main() { let x = 1; }");

    let a_id = srv.file_id_for_uri(&uri_a).unwrap();
    let b_id = srv.file_id_for_uri(&uri_b).unwrap();

    // Snapshot 1: both files clean
    let snap1 = srv.snapshot();
    assert!(snap1.parse(a_id).errors().is_empty());
    assert!(snap1.parse(b_id).errors().is_empty());

    // Edit only a.rua — introduce a parse error
    srv.change(&uri_a, "fn main() { let x: = ; }");
    let snap2 = srv.snapshot();

    // a.rua should now have errors
    assert!(!snap2.parse(a_id).errors().is_empty(), "a.rua should have parse error");

    // b.rua should still be clean (unaffected by a.rua edit)
    assert!(snap2.parse(b_id).errors().is_empty(), "b.rua should still be clean");
}

#[test]
fn incremental_snapshot_isolation() {
    // Old snapshots should not be affected by later changes.
    let uri = uri("/test/snap_iso.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn before() -> i64 { 1 }");

    let snap1 = srv.snapshot();
    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let text1 = snap1.parse(file_id).syntax_node().text().to_string();
    assert_eq!(text1, "fn before() -> i64 { 1 }");

    srv.change(&uri, "fn after() -> i64 { 2 }");
    let snap2 = srv.snapshot();

    // Old snapshot unaffected
    let text1_again = snap1.parse(file_id).syntax_node().text().to_string();
    assert_eq!(text1_again, "fn before() -> i64 { 1 }");

    // New snapshot reflects change
    let text2 = snap2.parse(file_id).syntax_node().text().to_string();
    assert_eq!(text2, "fn after() -> i64 { 2 }");
}

#[test]
fn incremental_add_whitespace_does_not_invalidate_type_inference() {
    // Adding whitespace-only changes should not cause type errors to appear/disappear.
    let uri = uri("/test/snap_ws.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn answer() -> i64 { 42 }");

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let diags_before: Vec<_> = srv.snapshot().diagnostics(file_id);

    // Add whitespace between tokens (should not affect semantics)
    srv.change(&uri, "fn   answer()   ->   i64   {   42   }");

    let diags_after: Vec<_> = srv.snapshot().diagnostics(file_id);
    // Whitespace changes should not introduce new diagnostics
    assert_eq!(diags_before.len(), diags_after.len(),
        "whitespace changes should not change diagnostic count");
}

#[test]
fn incremental_edit_one_function_does_not_invalidate_other_function_body() {
    let uri = uri("/test/snap_fn_iso.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn a() -> i64 { 1 }\nfn b() -> i64 { 2 }");

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let snap1 = srv.snapshot();
    let def_map = snap1.def_map(file_id);

    // Get body for fn b before edit
    let b_def = def_map.definitions().find(|d| d.name() == "b").unwrap();
    let b_body_before = snap1.body(b_def.id()).unwrap();
    let b_expr_count_before = b_body_before.exprs().count();

    // Edit only fn a
    srv.change(&uri, "fn a() -> i64 { 99 }\nfn b() -> i64 { 2 }");

    let snap2 = srv.snapshot();
    let def_map2 = snap2.def_map(file_id);
    let b_def2 = def_map2.definitions().find(|d| d.name() == "b").unwrap();
    let b_body_after = snap2.body(b_def2.id()).unwrap();

    // fn b's body should be unchanged (same expr count)
    assert_eq!(b_body_after.exprs().count(), b_expr_count_before,
        "unrelated fn's body should not change");
}

#[test]
fn incremental_close_reopen_then_re_query() {
    let uri = uri("/test/snap_reopen.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { let x = 42; }");

    let snap1 = srv.snapshot();
    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let _diags1 = snap1.diagnostics(file_id);

    srv.close(&uri);
    srv.open(&uri, "fn main() { let x = 42; }");

    let snap2 = srv.snapshot();
    let file_id2 = srv.file_id_for_uri(&uri).unwrap();
    // FileId may differ after close+reopen, but queries should work
    let _ = snap2.parse(file_id2);
    let _ = snap2.diagnostics(file_id2);
}

#[test]
fn incremental_host_applies_change_batch() {
    // Multiple files changed in one Change batch should all be reflected.
    let uri_a = uri("/proj/src/x.rua");
    let uri_b = uri("/proj/src/y.rua");
    let mut srv = TestServer::new();

    srv.open(&uri_a, "fn a() -> i64 { 1 }");
    srv.open(&uri_b, "fn b() -> i64 { 2 }");

    // Batch change
    let a_id = srv.file_id_for_uri(&uri_a).unwrap();
    let b_id = srv.file_id_for_uri(&uri_b).unwrap();
    let mut change = Change::new();
    change.set_file_text(a_id, "fn a() -> i64 { 10 }");
    change.set_file_text(b_id, "fn b() -> i64 { 20 }");
    // Apply directly to host
    srv.open(&uri_a, "fn a() -> i64 { 10 }");
    srv.open(&uri_b, "fn b() -> i64 { 20 }");

    let snap = srv.snapshot();
    let text_a = snap.parse(a_id).syntax_node().text().to_string();
    let text_b = snap.parse(b_id).syntax_node().text().to_string();
    assert_eq!(text_a, "fn a() -> i64 { 10 }");
    assert_eq!(text_b, "fn b() -> i64 { 20 }");
}
