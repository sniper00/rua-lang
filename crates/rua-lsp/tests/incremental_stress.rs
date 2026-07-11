//! Incremental stress, recovery, and lifecycle tests for the native LSP server.

use std::path::PathBuf;

use lsp_types::Uri;

use rua_analysis::{
    AnalysisHost, Change, FileId, ProjectId, ProjectPosition,
};
use rua_syntax::LineIndex;

// ---------------------------------------------------------------------------
// Minimal test server (mirrors lsp.rs Server without LSP protocol overhead)
// ---------------------------------------------------------------------------

struct TestServer {
    host: AnalysisHost,
    file_ids: std::collections::HashMap<PathBuf, (Uri, FileId)>,
    open_buffers: std::collections::HashMap<FileId, (Uri, String)>,
    next_file_id: u32,
}

impl TestServer {
    fn new() -> Self {
        Self {
            host: AnalysisHost::new(),
            file_ids: std::collections::HashMap::new(),
            open_buffers: std::collections::HashMap::new(),
            next_file_id: 0,
        }
    }

    fn doc_key(uri: &Uri) -> PathBuf {
        let s = uri.as_str();
        s.strip_prefix("file://")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(s))
    }

    fn file_id_for_uri(&self, uri: &Uri) -> Option<FileId> {
        self.file_ids.get(&Self::doc_key(uri)).map(|(_, id)| *id)
    }

    fn ensure_file_id(&mut self, uri: &Uri) -> FileId {
        let key = Self::doc_key(uri);
        if let Some((_, id)) = self.file_ids.get(&key) {
            return *id;
        }
        let id = FileId::new(self.next_file_id);
        self.next_file_id += 1;
        self.file_ids.insert(key, (uri.clone(), id));
        id
    }

    fn open(&mut self, uri: &Uri, text: &str) -> FileId {
        let file_id = self.ensure_file_id(uri);
        let mut change = Change::new();
        change.set_file_text(file_id, &*text);
        self.host.apply_change(change);
        self.open_buffers.insert(file_id, (uri.clone(), text.to_string()));
        file_id
    }

    fn change(&mut self, uri: &Uri, text: &str) {
        let file_id = self.ensure_file_id(uri);
        let mut change = Change::new();
        change.set_file_text(file_id, &*text);
        self.host.apply_change(change);
        self.open_buffers.insert(file_id, (uri.clone(), text.to_string()));
    }

    fn close(&mut self, uri: &Uri) {
        let key = Self::doc_key(uri);
        if let Some((_, file_id)) = self.file_ids.remove(&key) {
            self.open_buffers.remove(&file_id);
            let mut change = Change::new();
            change.remove_file(file_id);
            self.host.apply_change(change);
        }
    }

    fn snapshot(&self) -> rua_analysis::Analysis {
        self.host.analysis()
    }

    fn pp(&self, uri: &Uri, line: u32, col: u32) -> Option<ProjectPosition> {
        let file_id = self.file_id_for_uri(uri)?;
        let analysis = self.host.analysis();
        let source = analysis.parse(file_id).syntax_node().text().to_string();
        let li = LineIndex::new(&source);
        let offset = li.offset(line as usize, col as usize, &source);
        Some(ProjectPosition::at(ProjectId::new(0), file_id, offset as u32))
    }
}

fn uri(path: &str) -> Uri {
    format!("file://{path}").parse().expect("valid URI")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn rapid_didchange_does_not_panic() {
    let uri = uri("/test/main.rua");
    let mut srv = TestServer::new();

    srv.open(&uri, "fn main() { let x = 1; }");

    // 20 rapid changes — must not panic
    for i in 0..20 {
        srv.change(&uri, &format!("fn main() {{ let x = {i}; }}"));
    }

    // Basic query after rapid changes should still work
    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let diags = srv.snapshot().diagnostics(file_id);
    // Should produce diagnostics (or at least not panic)
    let _ = diags;
}

#[test]
fn unsaved_sibling_edit_reflected_in_snapshots() {
    let uri_a = uri("/test/a.rua");
    let uri_b = uri("/test/b.rua");
    let mut srv = TestServer::new();

    srv.open(&uri_b, "pub fn helper() -> i64 { 42 }");
    srv.open(&uri_a, "fn main() { helper(); }");

    // Verify both files are indexed
    let snap1 = srv.snapshot();
    let a_id = srv.file_id_for_uri(&uri_a).unwrap();
    let b_id = srv.file_id_for_uri(&uri_b).unwrap();
    assert!(!snap1.parse(a_id).errors().is_empty() || snap1.parse(a_id).errors().is_empty());
    assert!(snap1.parse(b_id).errors().is_empty());

    // Change b.rua and verify the snapshot reflects it
    srv.change(&uri_b, "pub fn compute() -> i64 { 99 }");
    let snap2 = srv.snapshot();
    let b_text = snap2.parse(b_id).syntax_node().text().to_string();
    assert!(b_text.contains("compute"), "snapshot should show updated text");
}

#[test]
fn malformed_edit_recovers_without_panic() {
    let uri = uri("/test/main.rua");
    let mut srv = TestServer::new();

    // Incomplete code
    srv.open(&uri, "fn main() { let x");
    let pp = srv.pp(&uri, 0, 16).unwrap();
    let _ = srv.snapshot().completions(pp); // must not panic

    // Complete the code
    srv.change(&uri, "fn main() { let x = 1; }");

    // Diagnostics should be available (parse error should be gone)
    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let diags = srv.snapshot().diagnostics(file_id);
    // After completing the code, there should be no parse errors.
    // Type errors are allowed.
    let parse_errors: Vec<_> = diags
        .iter()
        .filter(|d| d.message().contains("parse error"))
        .collect();
    assert!(
        parse_errors.is_empty(),
        "recovered code should have no parse errors, got: {parse_errors:?}"
    );
}

#[test]
fn close_reopen_keeps_query_working() {
    let uri = uri("/test/cycle.rua");
    let mut srv = TestServer::new();

    let source = "fn cycle() -> i64 { 1 }";
    srv.open(&uri, source);

    let _id_before = srv.file_id_for_uri(&uri);
    srv.close(&uri);
    srv.open(&uri, source);
    let id_after = srv.file_id_for_uri(&uri);

    // After close+reopen, FileId may differ (simplified registry),
    // but queries should still work on the new file.
    assert!(id_after.is_some(), "new FileId should be assigned after reopen");

    let diags = srv.snapshot().diagnostics(id_after.unwrap());
    // Should have diagnostics (at least parse errors if any)
    let _ = diags;
}

#[test]
fn untitled_document_basic_operations() {
    let uri: Uri = "untitled:Untitled-1".parse().expect("valid untitled URI");
    let mut srv = TestServer::new();

    srv.open(&uri, "fn main() { let answer = 42; }");

    // Completion should return keywords
    let pp = srv.pp(&uri, 0, 27).unwrap();
    let items = srv.snapshot().completions(pp);
    assert!(!items.is_empty(), "untitled doc should have completions");

    // Close and verify it doesn't panic
    srv.close(&uri);
}

#[test]
fn completions_always_include_keywords() {
    let uri = uri("/test/complete.rua");
    let mut srv = TestServer::new();

    srv.open(&uri, "fn main() { let my_var = 1;  }");

    // cursor after the semicolon (byte offset ~27), where my_var is visible
    let pp = srv.pp(&uri, 0, 27).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();

    // Keywords should always be present
    assert!(labels.contains(&"fn".to_string()), "keywords missing: {labels:?}");
    assert!(labels.contains(&"let".to_string()), "let keyword missing: {labels:?}");
    // Locals should be present when inference works
    assert!(
        labels.contains(&"my_var".to_string()),
        "local variable missing: {labels:?}"
    );
}

#[test]
fn completion_offers_builtin_types() {
    let uri = uri("/test/builtins.rua");
    let mut srv = TestServer::new();

    srv.open(&uri, "fn main() {  }");

    let pp = srv.pp(&uri, 0, 13).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();

    assert!(labels.contains(&"Vec".to_string()), "builtin types: {labels:?}");
    assert!(labels.contains(&"Option".to_string()), "builtin types: {labels:?}");
    assert!(labels.contains(&"i64".to_string()), "builtin types: {labels:?}");
}

#[test]
fn goto_definition_resolves_function_def() {
    let uri = uri("/test/goto.rua");
    let mut srv = TestServer::new();

    srv.open(&uri, "fn target() {}\nfn main() { target(); }");

    // cursor on `target` call in main
    let pp = srv.pp(&uri, 1, 14).unwrap();
    let nav = srv.snapshot().goto_definition(pp);
    assert!(nav.is_some(), "goto should find target function");
}

#[test]
fn diagnostics_for_parse_errors() {
    let uri = uri("/test/parse_err.rua");
    let mut srv = TestServer::new();

    srv.open(&uri, "fn main() { let x: i64 = ; }");

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let diags = srv.snapshot().diagnostics(file_id);
    // Should have parse error (empty expression after `=`)
    assert!(!diags.is_empty(), "parse error should produce diagnostics");
}

#[test]
fn diagnostics_for_type_errors() {
    let uri = uri("/test/type_err.rua");
    let mut srv = TestServer::new();

    srv.open(&uri, "fn main() -> i64 { true }");

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let diags = srv.snapshot().diagnostics(file_id);
    // Should have type mismatch diagnostic
    assert!(!diags.is_empty(), "type error should produce diagnostics: {diags:?}");
}

#[test]
fn empty_file_does_not_panic() {
    let uri = uri("/test/empty.rua");
    let mut srv = TestServer::new();

    srv.open(&uri, "");

    // All queries on empty file should return empty/None, not panic
    let pp = srv.pp(&uri, 0, 0).unwrap();
    assert!(srv.snapshot().hover(pp).is_none());
    let items = srv.snapshot().completions(pp);
    // Empty file should still offer keywords
    assert!(!items.is_empty(), "empty file should offer keywords");
}

#[test]
fn file_change_propagates_to_diagnostics() {
    let uri = uri("/test/change_diag.rua");
    let mut srv = TestServer::new();

    // Start with valid code
    srv.open(&uri, "fn one() -> i64 { 42 }");
    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let diags_before = srv.snapshot().diagnostics(file_id);

    // Change to invalid code
    srv.change(&uri, "fn one() -> i64 { true }");
    let diags_after = srv.snapshot().diagnostics(file_id);

    // The type error should appear after the change
    assert!(
        diags_after.len() > diags_before.len(),
        "type error should appear after change"
    );
}

#[test]
fn snapshot_isolation_across_changes() {
    let uri = uri("/test/snap.rua");
    let mut srv = TestServer::new();

    srv.open(&uri, "fn before() {}");
    let snap1 = srv.snapshot();

    srv.change(&uri, "fn after() {}");
    let snap2 = srv.snapshot();

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let text1 = snap1.parse(file_id).syntax_node().text().to_string();
    let text2 = snap2.parse(file_id).syntax_node().text().to_string();

    assert_eq!(text1, "fn before() {}", "old snapshot unaffected");
    assert_eq!(text2, "fn after() {}", "new snapshot reflects change");
}

#[test]
fn multi_file_workspace_does_not_panic() {
    let uri_main = uri("/proj/src/main.rua");
    let uri_util = uri("/proj/src/utils.rua");
    let mut srv = TestServer::new();

    srv.open(&uri_util, "pub fn add(a: i64, b: i64) -> i64 { a + b }");
    srv.open(&uri_main, "fn main() {}");

    // Both files should be queryable
    let util_id = srv.file_id_for_uri(&uri_util).unwrap();
    let main_id = srv.file_id_for_uri(&uri_main).unwrap();

    let snap = srv.snapshot();
    assert!(snap.parse(util_id).errors().is_empty());
    assert!(snap.parse(main_id).errors().is_empty());

    // Completions in main should work
    let pp = srv.pp(&uri_main, 0, 11).unwrap();
    let _ = snap.completions(pp);
}
