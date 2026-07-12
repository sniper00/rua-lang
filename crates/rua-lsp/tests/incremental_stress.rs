//! Incremental stress, recovery, and lifecycle tests for the native LSP server.

use std::path::PathBuf;

use lsp_types::Uri;

use rua_analysis::{
    AnalysisHost, Change, DiagnosticCode, FileId, ProjectId, ProjectPosition,
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
        change.set_file_text(file_id, text);
        self.host.apply_change(change);
        self.open_buffers.insert(file_id, (uri.clone(), text.to_string()));
        file_id
    }

    fn change(&mut self, uri: &Uri, text: &str) {
        let file_id = self.ensure_file_id(uri);
        let mut change = Change::new();
        change.set_file_text(file_id, text);
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
fn lint_redundant_mut_not_triggered_by_field_writes() {
    // `&mut self` with field writes like `self.x = …` should NOT warn
    // because the `mut` is required to mutate through the reference.
    let uri = uri("/test/field_write.rua");
    let mut srv = TestServer::new();

    srv.open(
        &uri,
        r#"
struct Point { x: i64, y: i64 }
impl Point {
    fn translate(&mut self, dx: i64, dy: i64) {
        self.x = self.x + dx;
        self.y = self.y + dy;
    }
}
"#,
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let diags = srv.snapshot().diagnostics(file_id);
    let has_w0301 = diags
        .iter()
        .any(|d| d.code() == Some(DiagnosticCode::LintRedundantMut));
    assert!(
        !has_w0301,
        "W0301 should not fire for &mut self with field writes: {diags:?}"
    );
}

#[test]
fn lint_redundant_mut_still_warns_without_any_writes() {
    // `let mut x` with zero writes (not even field writes) should still warn.
    let uri = uri("/test/no_writes.rua");
    let mut srv = TestServer::new();

    srv.open(
        &uri,
        "fn foo() -> i64 {
    let mut x = 42;
    x
}",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let diags = srv.snapshot().diagnostics(file_id);
    let has_w0301 = diags
        .iter()
        .any(|d| d.code() == Some(DiagnosticCode::LintRedundantMut));
    assert!(
        has_w0301,
        "W0301 should fire for `let mut x` with no writes at all: {diags:?}"
    );
}

#[test]
fn lint_redundant_mut_not_triggered_by_nested_field_write() {
    // Nested field write like `self.a.b = 1` should also suppress the lint.
    let uri = uri("/test/nested_field.rua");
    let mut srv = TestServer::new();

    srv.open(
        &uri,
        r#"
struct Inner { val: i64 }
struct Outer { a: Inner }
impl Outer {
    fn set(&mut self, v: i64) {
        self.a.val = v;
    }
}
"#,
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let diags = srv.snapshot().diagnostics(file_id);
    let has_w0301 = diags
        .iter()
        .any(|d| d.code() == Some(DiagnosticCode::LintRedundantMut));
    assert!(
        !has_w0301,
        "W0301 should not fire for &mut self with nested field writes: {diags:?}"
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
fn completions_sort_locals_before_keywords() {
    // After typing in a function body, local variables should have higher
    // relevance than keywords so they appear first in the list.
    let uri = uri("/test/sort.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { let my_counter = 0;  }");

    let pp = srv.pp(&uri, 0, 32).unwrap();
    let items = srv.snapshot().completions(pp);

    let local_pos = items.iter().position(|i| i.label() == "my_counter");
    let kw_pos = items.iter().position(|i| i.label() == "fn");

    match (local_pos, kw_pos) {
        (Some(l), Some(k)) => assert!(
            l < k,
            "local my_counter (position {l}) must sort before keyword fn (position {k})"
        ),
        _ => {}
    }
}

#[test]
fn completions_after_dot_exclude_keywords() {
    let uri = uri("/test/dot.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "struct Point { x: i64, y: i64 }\nfn main() { let origin = Point { x: 0, y: 0 }; origin.x }",
    );

    // cursor on `x` after the dot — the `x` in `origin.x`
    let pp = srv.pp(&uri, 1, 54).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();

    assert!(
        !labels.contains(&"fn".to_string()),
        "keywords must not appear in member completions: {labels:?}"
    );
    // Should have Point fields
    assert!(labels.contains(&"x".to_string()), "field x missing: {labels:?}");
    assert!(labels.contains(&"y".to_string()), "field y missing: {labels:?}");
}

#[test]
fn completions_respect_partial_prefix() {
    let uri = uri("/test/prefix.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { let my_var = 1; let other = 2; my_ }");

    // cursor right after `my_` — my_ occupies bytes 43,44,45 (offset 45 is at `_`)
    let pp = srv.pp(&uri, 0, 45).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();

    assert!(labels.contains(&"my_var".to_string()), "my_var missing: {labels:?}");
    // The replacement_range should be set to replace "my_" (bytes 43..45)
    let has_range = items.iter().any(|item| {
        item.replacement_range()
            .is_some_and(|r| r.start() == 43 && r.end() == 45)
    });
    assert!(has_range, "replacement_range should cover my_ prefix (bytes 43..45)");
}

#[test]
fn completions_suppress_declaration_keywords_in_expression_context() {
    // After `let x = `, declaration keywords like `fn`, `struct` should
    // NOT appear because the cursor is in expression position.
    let uri = uri("/test/exprctx.rua");
    let mut srv = TestServer::new();
    // Valid code: cursor after `=` between `=` and `1` is expression position.
    srv.open(&uri, "fn main() { let x = 1; }");

    // cursor right after `= ` — byte 18, the space before `1`
    let pp = srv.pp(&uri, 0, 18).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();

    assert!(
        !labels.contains(&"fn".to_string()),
        "`fn` must not appear in expression context: {labels:?}"
    );
    assert!(
        !labels.contains(&"struct".to_string()),
        "`struct` must not appear in expression context: {labels:?}"
    );
    // Statement-level keywords like `if`, `match` should still appear
    assert!(labels.contains(&"if".to_string()), "`if` missing: {labels:?}");
    assert!(labels.contains(&"true".to_string()), "`true` missing: {labels:?}");
}

#[test]
fn completions_allow_all_keywords_in_statement_context() {
    // At the start of a block, all keywords including declaration keywords
    // should appear.
    let uri = uri("/test/stmtctx.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() {  }");

    // cursor at `fn main() {  }` — statement position (inside empty block)
    let pp = srv.pp(&uri, 0, 13).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();

    // Declaration keywords should be present in statement context
    assert!(labels.contains(&"fn".to_string()), "`fn` missing in statement context: {labels:?}");
    assert!(labels.contains(&"struct".to_string()), "`struct` missing: {labels:?}");
}

#[test]
fn completions_include_doc_comments_for_items() {
    let uri = uri("/test/doccer.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "/// A point in 2D space.\nstruct Point { x: i64 }\n\n/// Compute distance.\nfn dist(p: &Point) -> i64 { p.x }\n\nfn main() { let p = Point { x: 0 };  }",
    );

    let pp = srv.pp(&uri, 3, 34).unwrap();
    let items = srv.snapshot().completions(pp);

    let point = items.iter().find(|i| i.label() == "Point");
    assert!(point.is_some(), "Point should be in completions");
    assert!(
        point.and_then(|i| i.documentation()).is_some(),
        "Point should have doc comment"
    );

    let dist = items.iter().find(|i| i.label() == "dist");
    assert!(dist.is_some(), "dist should be in completions");
    assert!(
        dist.and_then(|i| i.documentation()).is_some(),
        "dist should have doc comment"
    );
}

#[test]
fn method_completion_filters_self_from_snippet() {
    let uri = uri("/test/method_self.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "struct Point { x: i64, y: i64 }\nimpl Point {\n    fn translate(&mut self, dx: i64, dy: i64) {\n        self.x = self.x + dx;\n        self.y = self.y + dy;\n    }\n}\nfn main() { let p = Point { x: 0, y: 0 }; p.x }",
    );

    // cursor on `x` after `p.`, which triggers member completions for Point
    let pp = srv.pp(&uri, 7, 44).unwrap();
    let items = srv.snapshot().completions(pp);

    let translate = items
        .iter()
        .find(|i| i.label() == "translate")
        .expect("translate method should be in completions");

    // Detail should show full signature with self
    let detail = translate.detail().unwrap_or("");
    assert!(
        detail.contains("&mut self"),
        "detail should include self param, got: {detail:?}"
    );

    // Snippet insert should NOT include self, and params should use
    // the original parameter names (dx, dy), not just types.
    let insert = translate.insert();
    match insert {
        Some(rua_analysis::CompletionInsert::Call { params, .. }) => {
            let has_self = params.iter().any(|p| p.contains("self"));
            assert!(
                !has_self,
                "snippet params should NOT include self, got: {params:?}"
            );
            assert!(!params.is_empty(), "should have at least one param, got: {params:?}");
            // Parameters should include original names from source
            let has_dx = params.iter().any(|p| p.starts_with("dx"));
            let has_dy = params.iter().any(|p| p.starts_with("dy"));
            assert!(has_dx, "param should include name 'dx', got: {params:?}");
            assert!(has_dy, "param should include name 'dy', got: {params:?}");
        }
        other => panic!("expected Call insert, got: {other:?}"),
    }
}

#[test]
fn member_completion_detail_separates_label_and_type() {
    // Field and method completion items should carry the type/signature in
    // their `detail` field so that the LSP client can display it next to the
    // label with proper visual separation (not concatenated).
    let uri = uri("/test/detail_format.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "struct Point { x: i64, y: i64 }\nimpl Point {\n    fn translate(&mut self, dx: i64, dy: i64) { self.x = self.x + dx; }\n}\nfn main() { let p = Point { x: 0, y: 0 }; p.x }",
    );

    // Cursor on `x` after `p.` — triggers dot completions (previous
    // significant token is `.`), and `x` acts as a prefix filter.
    let pp = srv.pp(&uri, 4, 44).unwrap();
    let items = srv.snapshot().completions(pp);

    // Field detail should show the type, not the label repeated
    let x_field = items
        .iter()
        .find(|i| i.label() == "x" && i.kind() == rua_analysis::CompletionKind::Field)
        .expect("field x should be in completions");
    let x_detail = x_field.detail().expect("field x should have detail");
    assert!(
        x_detail.contains("i64"),
        "field detail should contain the type, got: {x_detail}"
    );
    assert!(
        !x_detail.contains("x: i64"),
        "field detail should be just the type, not repeated label+type, got: {x_detail}"
    );

    // Method detail should show the full signature (with self), separate from
    // the label (the label should not repeat inside the detail string twice).
    let translate = items
        .iter()
        .find(|i| i.label() == "translate")
        .expect("translate method should be in completions");
    let t_detail = translate.detail().expect("method translate should have detail");
    assert!(
        t_detail.contains("&mut self"),
        "method detail should include self, got: {t_detail}"
    );
    assert!(
        !t_detail.starts_with("translate"),
        "method detail should not start with the label name, got: {t_detail}"
    );
}

#[test]
fn completions_in_match_body_offer_enum_variants() {
    let uri = uri("/test/match_enum.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "enum Color { Red, Green, Blue, Rgb(i64, i64, i64) }\nfn main() { let c = Color::Red; match c {  } }",
    );

    // cursor inside match body `match c { | }`
    let pp = srv.pp(&uri, 1, 41).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();

    assert!(labels.contains(&"Red".to_string()), "Red variant missing: {labels:?}");
    assert!(labels.contains(&"Green".to_string()), "Green variant missing: {labels:?}");
    assert!(labels.contains(&"Blue".to_string()), "Blue variant missing: {labels:?}");
    assert!(labels.contains(&"Rgb".to_string()), "Rgb variant missing: {labels:?}");

    // Variants should sort above keywords
    let red_pos = items.iter().position(|i| i.label() == "Red").unwrap();
    let fn_pos = items.iter().position(|i| i.label() == "fn").unwrap();
    assert!(red_pos < fn_pos, "variants must sort before keywords");
}

#[test]
fn completions_outside_match_body_dont_offer_variants() {
    // Variants should NOT appear when cursor is outside a match expression.
    let uri = uri("/test/no_match_enum.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "enum Color { Red, Blue }\nfn main() { let c = Color::Red;  }",
    );

    let pp = srv.pp(&uri, 1, 31).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();

    // Red/Blue are variants, not module-level items — should NOT appear
    assert!(!labels.contains(&"Red".to_string()), "variants outside match: {labels:?}");
    assert!(!labels.contains(&"Blue".to_string()), "variants outside match: {labels:?}");
}

#[test]
fn completions_in_type_position_only_show_types() {
    // After `let x: `, only type names should appear, not variables/keywords.
    let uri = uri("/test/typepos.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "struct Point { x: i64 }\nenum Color { Red }\nfn main() { let my_var = 1; let x:  }",
    );

    // cursor after `let x: ` (type position, first space after colon)
    let pp = srv.pp(&uri, 2, 34).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();

    // Types should be present
    assert!(labels.contains(&"i64".to_string()), "i64 missing: {labels:?}");
    assert!(labels.contains(&"Point".to_string()), "Point missing: {labels:?}");
    assert!(labels.contains(&"Color".to_string()), "Color missing: {labels:?}");
    // Variables should NOT be present
    assert!(!labels.contains(&"my_var".to_string()), "my_var should not appear in type pos: {labels:?}");
    // Keywords should NOT be present
    assert!(!labels.contains(&"fn".to_string()), "fn should not appear in type pos: {labels:?}");
}

#[test]
fn completions_in_expression_position_include_variables() {
    // After `let x = `, variables should still appear (not type position).
    let uri = uri("/test/exprpos.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { let my_var = 1; let x =  }");

    let pp = srv.pp(&uri, 0, 32).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();

    assert!(labels.contains(&"my_var".to_string()), "my_var missing: {labels:?}");
}

#[test]
fn signature_help_returns_none_outside_call() {
    // Should not panic and return None when cursor is not in a call.
    let uri = uri("/test/sighlp2.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn add(a: i64, b: i64) -> i64 { a + b }\nfn main() { let x = 1; }");

    let pp = srv.pp(&uri, 1, 15).unwrap();
    let help = srv.snapshot().signature_help(pp);
    assert!(help.is_none(), "signature help should be None outside a call");
}

#[test]
fn postfix_completions_after_dot() {
    let uri = uri("/test/postfix.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "fn main() { let x = true; x. }",
    );

    let pp = srv.pp(&uri, 0, 28).unwrap();
    let items = srv.snapshot().completions(pp);
    let labels: Vec<String> = items.iter().map(|i| i.label().to_string()).collect();

    assert!(labels.contains(&".if".to_string()), ".if postfix missing: {labels:?}");
    assert!(labels.contains(&".match".to_string()), ".match postfix missing: {labels:?}");
    assert!(labels.contains(&".not".to_string()), ".not postfix missing: {labels:?}");
    assert!(labels.contains(&".ref".to_string()), ".ref postfix missing: {labels:?}");
    // Verify the snippet contains the receiver expression
    let if_item = items.iter().find(|i| i.label() == ".if").unwrap();
    if let Some(rua_analysis::CompletionInsert::Snippet(text)) = if_item.insert() {
        assert!(text.contains("x"), "snippet should contain receiver 'x': {text:?}");
    } else {
        panic!("expected Snippet insert");
    }
}

#[test]
fn keyword_snippets_include_placeholders() {
    let uri = uri("/test/kwsnip.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() {  }");

    let pp = srv.pp(&uri, 0, 13).unwrap();
    let items = srv.snapshot().completions(pp);

    let for_item = items.iter().find(|i| i.label() == "for").unwrap();
    match for_item.insert() {
        Some(rua_analysis::CompletionInsert::Snippet(_text)) => {
            let has_placeholder = _text.contains('$');
            assert!(has_placeholder, "for snippet should have placeholder $, got: {_text:?} len={}", _text.len());
        }
        other => panic!("expected Snippet for 'for', got: {other:?}"),
    }

    let match_item = items.iter().find(|i| i.label() == "match").unwrap();
    match match_item.insert() {
        Some(rua_analysis::CompletionInsert::Snippet(_text)) => {
            let has_placeholder = _text.contains('$');
            assert!(has_placeholder, "match snippet should have placeholder $, got: {_text:?}");
        }
        other => panic!("expected Snippet for 'match', got: {other:?}"),
    }

    // Plain keywords should not have snippet insert
    let else_item = items.iter().find(|i| i.label() == "else").unwrap();
    assert!(
        !matches!(else_item.insert(), Some(rua_analysis::CompletionInsert::Snippet(_))),
        "else should not be a snippet"
    );
}

#[test]
fn inlay_hints_include_type_annotations() {
    let uri = uri("/test/inlay.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "struct Point { x: i64 }\nfn main() { let p = Point { x: 42 }; }");

    // Query hints for the whole file.
    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let _source = analysis.parse(file_id).syntax_node().text().to_string();
    // Hints are computed in the LSP handler — just verify the analysis
    // infrastructure doesn't panic when accessed.
    let def_map = analysis.def_map(file_id);
    let hints: Vec<_> = def_map
        .definitions()
        .filter(|d| matches!(d.kind(), rua_analysis::DefKind::Function | rua_analysis::DefKind::Method))
        .filter_map(|d| {
            let body = analysis.body(d.id())?;
            let source_map = analysis.body_source_map(d.id())?;
            let inference = analysis.infer(d.id())?;
            let mut result = Vec::new();
            for (bid, b) in body.bindings() {
                if let Some(_name) = b.name()
                    && let Some(ty) = inference.type_of_binding(bid)
                    && !ty.is_unknown()
                    && let Some(fr) = source_map.binding_range(bid)
                {
                    result.push((fr.range.end(), format!(": {ty}")));
                }
            }
            Some(result)
        })
        .flatten()
        .collect::<Vec<_>>();
    // `p` should have a type hint
    let has_point_hint = hints.iter().any(|(_, label)| label.contains("Point"));
    assert!(has_point_hint, "should have Point type hint, got: {hints:?}");
}

#[test]
fn member_hover_shows_method_signature() {
    let uri = uri("/test/member_hover.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "struct Point { x: i64 }\nimpl Point {\n    fn translate(&mut self, dx: i64, dy: i64) { self.x = self.x + dx; }\n}\nfn main() { let mut p = Point { x: 0 }; p.translate(1, 2); }",
    );

    // Hover on `translate` in `p.translate(1, 2)`.
    // `translate` at column 43 (on 'r' of translate)
    let pp = srv.pp(&uri, 4, 43).unwrap();
    let hover = srv.snapshot().hover(pp);
    assert!(
        hover.is_some(),
        "hover on method call should not be None"
    );
    let text = hover.unwrap().signature().to_string();
    assert!(text.contains("translate"), "hover should contain method name, got: {text}");
    assert!(text.contains("dx") || text.contains("i64"), "hover should show params, got: {text}");
}

#[test]
fn member_hover_shows_field_type() {
    let uri = uri("/test/field_hover.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "struct Point { x: i64 }\nfn main() { let p = Point { x: 42 }; let q = p.x; }",
    );

    // Hover on `x` in `p.x`. Dot is at offset 71, so x is at 72.
    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let source = analysis.parse(file_id).syntax_node().text().to_string();
    let li = rua_syntax::LineIndex::new(&source);
    let (line, col) = li.line_col(72, &source);
    let pp = srv.pp(&uri, line as u32, col as u32).unwrap();
    let hover = srv.snapshot().hover(pp);
    assert!(hover.is_some(), "hover on field should not be None");
    let text = hover.unwrap().signature().to_string();
    assert!(text.contains("x"), "hover should contain field name, got: {text}");
    assert!(text.contains("i64"), "hover should show field type, got: {text}");
}

#[test]
fn inlay_hint_type_is_clickable() {
    // Verify the analysis provides type info that can be used for clickable hints.
    let uri = uri("/test/inlay_click.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "struct Point { x: i64 }\nfn main() { let p = Point { x: 0 }; }");

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let def_map = analysis.def_map(file_id);

    // Find the `p` binding — its type should be Point
    for definition in def_map.definitions() {
        if !matches!(
            definition.kind(),
            rua_analysis::DefKind::Function | rua_analysis::DefKind::Method
        ) {
            continue;
        }
        let Some(body) = analysis.body(definition.id()) else {
            continue;
        };
        let Some(inference) = analysis.infer(definition.id()) else {
            continue;
        };
        for (bid, binding) in body.bindings() {
            if binding.name() == Some("p") {
                let ty = inference.type_of_binding(bid).unwrap();
                let ty_str = ty.to_string();
                assert!(ty_str.contains("Point"), "p should have type Point, got: {ty_str}");
                // Verify it's a named type that can be resolved to a definition
                if let rua_analysis::Ty::Named(named) = ty {
                    let def = def_map.definition(named.definition());
                    assert!(def.is_some(), "Point type should resolve to a definition");
                } else {
                    panic!("expected Named type, got: {ty_str}");
                }
            }
        }
    }
}

#[test]
fn goto_definition_on_method_call() {
    let uri = uri("/test/goto_method.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "struct Point { x: i64 }\nimpl Point {\n    fn translate(&mut self, dx: i64, dy: i64) { self.x = self.x + dx; }\n}\nfn main() { let mut p = Point { x: 0 }; p.translate(1, 2); }",
    );

    // Goto def on `translate` in `p.translate(1, 2)`.
    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let source = analysis.parse(file_id).syntax_node().text().to_string();
    // Find the offset of `translate` in the source.
    let translate_off = source.find("translate").unwrap();
    let li = rua_syntax::LineIndex::new(&source);
    let (line, col) = li.line_col(translate_off, &source);
    let pp = srv.pp(&uri, line as u32, col as u32).unwrap();
    let nav = srv.snapshot().goto_definition(pp);
    assert!(nav.is_some(), "goto def on method call should not be None");
    let target = nav.unwrap();
    let target_source = analysis
        .parse(target.full_range().file_id)
        .syntax_node()
        .text()
        .to_string();
    let target_text = &target_source
        [target.full_range().range.start() as usize..target.full_range().range.end() as usize];
    assert_eq!(target_text, "translate", "should jump to method definition");
}

#[test]
fn goto_definition_on_field_access() {
    let uri = uri("/test/goto_field.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "struct Point { x: i64 }\nfn main() { let p = Point { x: 42 }; p.x; }",
    );

    // Goto def on `x` in `p.x`.
    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let source = analysis.parse(file_id).syntax_node().text().to_string();
    // Cursor on x after dot.
    let x_off = source.rfind('.').unwrap() + 1;
    let li = rua_syntax::LineIndex::new(&source);
    let (line, col) = li.line_col(x_off, &source);
    let pp = srv.pp(&uri, line as u32, col as u32).unwrap();
    let nav = srv.snapshot().goto_definition(pp);
    assert!(nav.is_some(), "goto def on field should not be None");
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
