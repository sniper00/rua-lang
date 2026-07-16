//! Enhanced references and rename tests — cross-file, write tracking, validation.

mod support;

use support::{TestServer, uri};

#[test]
fn references_cross_file_parses_and_intra_file_works() {
    // Cross-file references are evaluated in one explicit project graph.
    let uri_a = uri("/proj/src/main.rua");
    let uri_b = uri("/proj/src/b.rua");
    let mut srv = TestServer::new();

    srv.open(&uri_b, "pub fn helper() -> i64 { 42 }");
    let main_source = "fn main() { b::helper(); }";
    srv.open(&uri_a, main_source);

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

    // References on `main` in the project root include its declaration.
    let pp_a = srv
        .pp_at_offset(&uri_a, main_source.find("main").unwrap())
        .unwrap();
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
fn references_and_rename_isolate_same_named_methods_by_receiver_type() {
    let uri = uri("/test/method_identity.rua");
    let source = "struct A {}\nstruct B {}\nimpl A { fn ping(self) {} }\nimpl B { fn ping(self) {} }\nfn main() { let a = A {}; let b = B {}; a.ping(); b.ping(); }";
    let mut srv = TestServer::new();
    srv.open(&uri, source);
    let call = source.find("a.ping").unwrap() + 2;
    let position = srv.pp_at_offset(&uri, call).unwrap();

    let references = srv.snapshot().references(position, true);
    assert_eq!(references.len(), 2, "A::ping declaration and call only");
    for reference in &references {
        assert_eq!(
            &source
                [reference.range().range.start() as usize..reference.range().range.end() as usize],
            "ping"
        );
    }

    let change = srv.snapshot().rename(position, "touch").unwrap();
    assert_eq!(change.file_edits().len(), 1);
    assert_eq!(change.file_edits()[0].edits().len(), 2);
}

#[test]
fn readonly_ruai_variant_has_precise_navigation_hover_and_atomic_rename_rejection() {
    let api_uri = uri("/project/src/api.ruai");
    let main_uri = uri("/project/src/main.rua");
    let api = "pub enum State {\n    /// The host is ready.\n    Ready,\n}\n";
    let main = "let state = api::State::Ready;\nmatch state { api::State::Ready => {} }\n";
    let mut srv = TestServer::new();
    let api_id = srv.open(&api_uri, api);
    srv.open(&main_uri, main);
    let use_offset = main.find("Ready").unwrap();
    let position = srv.pp_at_offset(&main_uri, use_offset).unwrap();
    let analysis = srv.snapshot();

    let target = analysis.goto_definition(position).expect("variant target");
    assert_eq!(target.target_range().file_id, api_id);
    assert_eq!(
        &api[target.target_range().range.start() as usize
            ..target.target_range().range.end() as usize],
        "Ready"
    );
    assert_eq!(
        analysis.hover(position).unwrap().documentation(),
        Some("The host is ready.")
    );
    assert_eq!(analysis.references(position, true).len(), 3);
    assert!(analysis.prepare_rename(position).is_none());
    assert!(matches!(
        analysis.rename(position, "Available"),
        Err(rua_analysis::RenameError::ReadOnly { .. })
    ));
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

#[test]
fn references_resolve_enum_variant_by_identity() {
    let source = "enum Color { Red, Green }\nenum Signal { Red, Stop }\nfn first() { let a = Color::Red; let b = Color::Red; }\nfn second() { let s = Signal::Red; }";
    let uri = uri("/test/refs_variant.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, source);

    let color_red = source.find("Red").unwrap();
    let pp = srv.pp_at_offset(&uri, color_red).unwrap();
    let refs = srv.snapshot().references(pp, true);
    let starts = refs
        .iter()
        .map(|reference| reference.range().range.start() as usize)
        .collect::<Vec<_>>();
    let expected = source
        .match_indices("Red")
        .map(|(offset, _)| offset)
        .filter(|offset| *offset != source.find("Signal { Red").unwrap() + "Signal { ".len())
        .filter(|offset| !source[..*offset].ends_with("Signal::"))
        .collect::<Vec<_>>();

    assert_eq!(starts, expected);
}

#[test]
fn rename_enum_variant_does_not_touch_same_named_symbols() {
    let source = "enum Color { Red, Green }\nenum Signal { Red, Stop }\nfn first() { let a = Color::Red; let b = Color::Red; }\nfn second() { let Red = 1; let s = Signal::Red; Red }";
    let uri = uri("/test/rename_variant.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, source);

    let color_red = source.find("Red").unwrap();
    let pp = srv.pp_at_offset(&uri, color_red).unwrap();
    let change = srv
        .snapshot()
        .rename(pp, "Crimson")
        .expect("variant rename");
    let edits = change
        .file_edits()
        .iter()
        .flat_map(|file| file.edits())
        .collect::<Vec<_>>();
    let edited_text = edits
        .iter()
        .map(|edit| &source[edit.range().start() as usize..edit.range().end() as usize])
        .collect::<Vec<_>>();

    assert_eq!(edits.len(), 3, "unexpected rename edits: {edits:?}");
    assert!(edited_text.iter().all(|text| *text == "Red"));
    assert!(edits.iter().all(|edit| edit.new_text() == "Crimson"));
    let signal_red = source.find("Signal { Red").unwrap() + "Signal { ".len();
    let local_red = source.find("let Red").unwrap() + "let ".len();
    assert!(edits.iter().all(|edit| {
        let start = edit.range().start() as usize;
        start != signal_red && start != local_red
    }));
}

#[test]
fn references_and_rename_work_for_top_level_chunk_binding() {
    let source = "let count = 1;\nprintln!(\"{}\", count);\nlet next = count + 1;";
    let uri = uri("/test/rename_chunk_binding.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, source);

    let use_offset = source.find("count);").unwrap();
    let pp = srv.pp_at_offset(&uri, use_offset).unwrap();
    let refs = srv.snapshot().references(pp, true);
    assert_eq!(refs.len(), 3, "unexpected chunk references: {refs:?}");

    let change = srv
        .snapshot()
        .rename(pp, "total")
        .expect("top-level binding rename");
    let edits = change.file_edits()[0].edits();
    assert_eq!(edits.len(), 3);
    assert!(edits.iter().all(|edit| edit.new_text() == "total"));
}
