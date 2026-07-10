use rua_analysis::{
    AnalysisHost, Change, Diagnostic, DiagnosticOrigin, FileId, SemanticTokenKind, TextRange,
    reconcile_diagnostics,
};

fn analysis(source: &str) -> (rua_analysis::Analysis, FileId) {
    let file_id = FileId::new(0);
    let mut change = Change::new();
    change.set_file_text(file_id, source);
    let mut host = AnalysisHost::new();
    host.apply_change(change);
    (host.analysis(), file_id)
}

fn nth_word(source: &str, needle: &str, occurrence: usize) -> usize {
    source
        .match_indices(needle)
        .filter(|(offset, _)| {
            let end = offset + needle.len();
            let bytes = source.as_bytes();
            (*offset == 0 || !is_ident(bytes[*offset - 1]))
                && (end == bytes.len() || !is_ident(bytes[end]))
        })
        .nth(occurrence)
        .unwrap_or_else(|| panic!("missing occurrence {occurrence} of {needle:?}"))
        .0
}

fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

#[test]
fn closure_iterator_ide_exposes_types_and_structural_tokens() {
    let source = concat!(
        "fn main() -> i64 {\n",
        "  let values = vec![1, 2, 3];\n",
        "  (0..3).map(|value| value + 1).filter(|item| item > 1).count()\n",
        "}\n",
    );
    let (analysis, file_id) = analysis(source);
    let parameters = analysis.closure_parameters(file_id);
    assert_eq!(parameters.len(), 2);
    assert!(parameters.iter().all(|parameter| parameter.ty() == "i64"));
    assert_eq!(
        parameters
            .iter()
            .map(|parameter| parameter.name())
            .collect::<Vec<_>>(),
        ["value", "item"]
    );

    let tokens = analysis.semantic_tokens(file_id);
    assert_eq!(
        tokens
            .iter()
            .filter(|token| token.kind() == SemanticTokenKind::ClosureParameter)
            .count(),
        4
    );
    assert!(tokens.iter().any(|token| {
        token.kind() == SemanticTokenKind::RangeOperator
            && &source[token.range().start() as usize..token.range().end() as usize] == ".."
    }));
    for method in ["map", "filter", "count"] {
        assert!(tokens.iter().any(|token| {
            token.kind() == SemanticTokenKind::Method
                && &source[token.range().start() as usize..token.range().end() as usize] == method
        }));
    }
}

#[test]
fn closure_iterator_ide_supports_goto_completion_references_and_rename() {
    let source = concat!(
        "fn main() {\n",
        "  let values = vec![1, 2, 3];\n",
        "  let count = values.iter().map(|item| item + 1).count();\n",
        "}\n",
    );
    let analysis = rua_syntax::analysis::Analysis::new(source);
    let definition = nth_word(source, "item", 0);
    let use_site = nth_word(source, "item", 1);
    let resolved = analysis
        .definition_at(use_site)
        .expect("closure parameter goto");
    assert_eq!(resolved.target_range.0, definition);
    assert_eq!(resolved.detail, "closure parameter item: i64");
    assert!(
        analysis
            .scope_locals(use_site)
            .iter()
            .any(|local| { local.name == "item" && local.detail == "closure parameter item: i64" })
    );
    assert_eq!(analysis.references_at(use_site).len(), 2);
    assert_eq!(analysis.rename_edits(use_site, "element").unwrap().len(), 2);
}

#[test]
fn closure_iterator_ide_reconciles_fast_and_compiler_diagnostics() {
    let source = concat!(
        "fn main() {\n",
        "  let values = vec![1, 2, 3];\n",
        "  let count = values.iter().filter(|value| value + 1).count();\n",
        "}\n",
    );
    let (analysis, file_id) = analysis(source);
    let fast = analysis.diagnostics(file_id);
    assert!(
        fast.is_empty(),
        "syntax-fast path should accept the program"
    );

    let (compiler, _) = ruac::check_diags(source);
    let compiler: Vec<_> = compiler
        .into_iter()
        .map(|diagnostic| {
            Diagnostic::new(
                file_id,
                TextRange::new(
                    diagnostic.start as u32,
                    (diagnostic.start + diagnostic.len) as u32,
                ),
                diagnostic.msg,
                DiagnosticOrigin::Compiler,
            )
        })
        .collect();
    let reconciled = reconcile_diagnostics(fast, compiler);
    assert_eq!(reconciled.len(), 1);
    assert_eq!(
        reconciled[0].message(),
        "iterator filter predicate must be `bool`, found `i64`"
    );
    assert_eq!(reconciled[0].origin(), DiagnosticOrigin::Compiler);
}

#[test]
fn closure_iterator_ide_degrades_uninferred_parameters_to_unknown() {
    let source = "fn main() { let unknown = |value| value; }";
    let (analysis, file_id) = analysis(source);
    let parameters = analysis.closure_parameters(file_id);
    assert_eq!(parameters.len(), 1);
    assert_eq!(parameters[0].ty(), "Unknown");
}
