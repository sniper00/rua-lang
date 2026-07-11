use std::{fmt::Write as _, fs, path::PathBuf};

use rua_analysis::{
    AnalysisHost, Change, Diagnostic, DiagnosticOrigin, FileId, SemanticTokenKind, TextRange,
    reconcile_diagnostics,
};

const UPDATE_ENV: &str = "RUA_UPDATE_GOLDENS";

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
    // Native inference assigns Unknown until iterator adapter types are
    // implemented (4B.7c). After 4B.7c these become "i64".
    assert!(parameters.iter().all(|parameter| parameter.ty() == "?"));
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
            .filter(|token| token.kind() == SemanticTokenKind::Parameter)
            .count(),
        4
    );
    assert!(tokens.iter().any(|token| {
        token.kind() == SemanticTokenKind::Operator
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
    // Native Ty::Display uses "?" for Unknown.
    assert_eq!(parameters[0].ty(), "?");
}

fn closure_iterator_golden_paths() -> (PathBuf, PathBuf) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden/ide");
    (
        root.join("closure_iterator.rua"),
        root.join("closure_iterator.snap"),
    )
}

fn closure_iterator_snapshot() -> String {
    let (source_path, _) = closure_iterator_golden_paths();
    let source = fs::read_to_string(&source_path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", source_path.display()));
    let (analysis, file_id) = analysis(&source);
    let syntax = rua_syntax::analysis::Analysis::new(&source);
    let mut output = String::new();

    for parameter in analysis.closure_parameters(file_id) {
        writeln!(
            output,
            "parameter: {} {}..{} type={}",
            parameter.name(),
            parameter.range().start(),
            parameter.range().end(),
            parameter.ty()
        )
        .unwrap();
    }
    for token in analysis.semantic_tokens(file_id) {
        let range = token.range();
        writeln!(
            output,
            "token: {:?} {}..{} text={:?} declaration={}",
            token.kind(),
            range.start(),
            range.end(),
            &source[range.start() as usize..range.end() as usize],
            token.is_declaration()
        )
        .unwrap();
    }

    let use_offset = nth_word(&source, "item", 1);
    let definition = syntax
        .definition_at(use_offset)
        .expect("closure parameter definition");
    let completion = syntax
        .scope_locals(use_offset)
        .into_iter()
        .find(|local| local.name == "item")
        .expect("closure parameter completion");
    writeln!(output, "query: item at {use_offset}").unwrap();
    writeln!(
        output,
        "completion: {} {:?}",
        completion.name, completion.detail
    )
    .unwrap();
    writeln!(
        output,
        "definition: {}..{} {:?}",
        definition.target_range.0, definition.target_range.1, definition.detail
    )
    .unwrap();
    for (start, end) in syntax.references_at(use_offset) {
        writeln!(
            output,
            "reference: {start}..{end} {:?}",
            &source[start..end]
        )
        .unwrap();
    }
    for (start, end, replacement) in syntax.rename_edits(use_offset, "element").unwrap() {
        writeln!(
            output,
            "rename: {start}..{end} {:?} -> {:?}",
            &source[start..end],
            replacement
        )
        .unwrap();
    }
    writeln!(
        output,
        "fast diagnostics: {}",
        analysis.diagnostics(file_id).len()
    )
    .unwrap();
    output
}

fn assert_closure_iterator_snapshot(update: bool) {
    let (_, snapshot_path) = closure_iterator_golden_paths();
    let actual = closure_iterator_snapshot();
    if update {
        fs::write(&snapshot_path, actual)
            .unwrap_or_else(|error| panic!("cannot write {}: {error}", snapshot_path.display()));
        return;
    }
    let expected = fs::read_to_string(&snapshot_path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", snapshot_path.display()));
    assert_eq!(expected, actual, "closure/iterator IDE snapshot changed");
}

#[test]
fn closure_iterator_ide_golden() {
    assert_closure_iterator_snapshot(false);
}

#[test]
#[ignore = "updates repository IDE snapshot; run the documented explicit command"]
fn update_closure_iterator_ide_golden() {
    assert_eq!(
        std::env::var(UPDATE_ENV).as_deref(),
        Ok("1"),
        "refusing to update without {UPDATE_ENV}=1"
    );
    assert_closure_iterator_snapshot(true);
}
