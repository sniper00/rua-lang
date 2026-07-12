//! Workspace symbol tests — fuzzy symbol search across files.

mod support;

use support::{uri, TestServer};

#[test]
fn workspace_symbol_finds_function_by_name() {
    let uri = uri("/test/ws_symbol.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "fn calculate_total() -> i64 { 42 }\nfn render_page() {}\nstruct Calculator { value: i64 }",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let symbols = analysis.workspace_symbols(file_id, "calculate");

    // Should find "calculate_total"
    assert!(
        !symbols.is_empty(),
        "should find at least one symbol matching 'calculate', got {symbols:?}"
    );
}

#[test]
fn workspace_symbol_fuzzy_match() {
    let uri = uri("/test/ws_fuzzy.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "fn handle_request() {}\nfn handle_response() {}\nfn process_data() {}",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();

    // Search for "handle" should find both handlers
    let symbols = analysis.workspace_symbols(file_id, "handle");
    assert!(
        symbols.len() >= 2,
        "fuzzy search should find multiple matches, got {symbols:?}"
    );
}

#[test]
fn workspace_symbol_case_insensitive() {
    let uri = uri("/test/ws_case.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn MyFunction() -> i64 { 0 }\nfn main() {}");

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();

    // Lowercase search should match the uppercase function
    let symbols = analysis.workspace_symbols(file_id, "myfunction");
    assert!(
        !symbols.is_empty(),
        "case-insensitive search should match MyFunction, got {symbols:?}"
    );
}

#[test]
fn workspace_symbol_no_match() {
    let uri = uri("/test/ws_none.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() {}");

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();

    let symbols = analysis.workspace_symbols(file_id, "nonexistent");
    assert!(
        symbols.is_empty(),
        "should find no matches, got {symbols:?}"
    );
}
