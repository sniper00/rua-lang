//! Analysis prerequisites for diagnostic code actions.
//!
//! Actual LSP edits are covered by `protocol_lifecycle` through stdio JSON-RPC.

mod support;

use support::{TestServer, uri};

// ---------------------------------------------------------------------------
// Diagnostic quick-fixes
// ---------------------------------------------------------------------------

#[test]
fn analysis_reports_unused_variable_for_suppression_action() {
    // W0300 fires on unused `x`. The quick-fix renames it to `_x`.
    let uri = uri("/test/ca_unused.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { let x = 1; }");

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let diags = srv.snapshot().diagnostics(file_id);
    assert!(
        diags
            .iter()
            .any(|d| d.code() == Some(rua_analysis::DiagnosticCode::LintUnusedVariable)),
        "unused variable must produce W0300 for quick-fix to activate, got: {diags:?}"
    );
}

#[test]
fn analysis_reports_immutable_assignment_for_add_mut_action() {
    let uri = uri("/test/ca_immut.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { let x = 1; x = 2; }");

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let diagnostics = analysis.diagnostics(file_id);
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == Some(rua_analysis::DiagnosticCode::TypeImmutableAssignment)
                && diagnostic.message().contains("`x`")
        }),
        "immutable assignment must produce E0212: {diagnostics:?}"
    );
}
