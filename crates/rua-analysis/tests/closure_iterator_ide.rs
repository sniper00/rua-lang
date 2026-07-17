use std::{fmt::Write as _, fs, path::PathBuf};

use rua_analysis::{
    AnalysisHost, Change, Diagnostic, DiagnosticOrigin, FileId, FileKind, ProjectData, ProjectId,
    ProjectPosition, ProjectRoot, SemanticTokenKind, SourceRootId, SourceRootKind, TextRange,
    reconcile_diagnostics,
};

const UPDATE_ENV: &str = "RUA_UPDATE_GOLDENS";

fn analysis(source: &str) -> (rua_analysis::Analysis, FileId, ProjectId) {
    let file_id = FileId::new(0);
    let root_id = SourceRootId::new(0);
    let project_id = ProjectId::new(0);
    let mut change = Change::new();
    change.set_source_root(root_id, SourceRootKind::Workspace);
    change.set_file_with_path(file_id, root_id, FileKind::Source, "main.rua", source);
    change.set_project(
        project_id,
        ProjectData::new(file_id, [ProjectRoot::at_root(root_id)], []),
    );
    let mut host = AnalysisHost::new();
    host.apply_change(change);
    (host.analysis(), file_id, project_id)
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
        "  let values = [1, 2, 3];\n",
        "  (0..3).map(|value| value + 1).filter(|item| item > 1).count()\n",
        "}\n",
    );
    let (analysis, file_id, _) = analysis(source);
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
        "  let values = [1, 2, 3];\n",
        "  let count = values.iter().map(|item| item + 1).count();\n",
        "}\n",
    );
    let (analysis, file_id, project_id) = analysis(source);
    let definition = nth_word(source, "item", 0);
    let use_site = nth_word(source, "item", 1);
    let resolved = analysis
        .goto_definition(ProjectPosition::at(project_id, file_id, use_site as u32))
        .expect("closure parameter goto");
    assert_eq!(resolved.target_range().range.start() as usize, definition);
    assert!(
        analysis
            .completions(ProjectPosition::at(project_id, file_id, use_site as u32))
            .iter()
            .any(|item| item.label() == "item" && item.detail() == Some("item: i64"))
    );
    assert_eq!(
        analysis
            .references(
                ProjectPosition::at(project_id, file_id, use_site as u32),
                true,
            )
            .len(),
        2
    );
    assert_eq!(
        analysis
            .rename(
                ProjectPosition::at(project_id, file_id, use_site as u32),
                "element",
            )
            .unwrap()
            .file_edits()[0]
            .edits()
            .len(),
        2
    );
}

#[test]
fn closure_iterator_ide_reconciles_fast_and_compiler_diagnostics() {
    let source = concat!(
        "fn main() {\n",
        "  let values = [1, 2, 3];\n",
        "  let count = values.iter().filter(|value| value + 1).count();\n",
        "}\n",
    );
    let (analysis, file_id, _) = analysis(source);
    let fast = analysis.diagnostics(file_id);
    // Native type diagnostics now detect the filter predicate mismatch.
    assert!(
        !fast.is_empty(),
        "native diagnostics should detect filter type error"
    );

    let (compiler, _) = ruac::check_diags(source);
    let compiler: Vec<_> = compiler
        .into_iter()
        .map(|diagnostic| {
            Diagnostic::new(
                file_id,
                TextRange::new(
                    diagnostic.start() as u32,
                    (diagnostic.start() + diagnostic.len()) as u32,
                ),
                diagnostic.msg,
                DiagnosticOrigin::Compiler,
            )
        })
        .collect();
    let reconciled = reconcile_diagnostics(fast, compiler);
    // Both native and compiler diagnostics detect the same error; compiler
    // overrides same-location diagnostics so the origin should be Compiler.
    assert!(!reconciled.is_empty());
    assert!(
        reconciled
            .iter()
            .any(|d| d.message().contains("bool") || d.message().contains("`bool`")),
        "reconciled diagnostics should report filter predicate type error"
    );
}

#[test]
fn closure_iterator_ide_degrades_uninferred_parameters_to_unknown() {
    let source = "fn main() { let unknown = |value| value; }";
    let (analysis, file_id, _) = analysis(source);
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
    let (analysis, file_id, project_id) = analysis(&source);
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
    let position = ProjectPosition::at(project_id, file_id, use_offset as u32);
    let definition = analysis
        .goto_definition(position)
        .expect("closure parameter definition");
    let completion = analysis
        .completions(position)
        .into_iter()
        .find(|item| item.label() == "item")
        .expect("closure parameter completion");
    writeln!(output, "query: item at {use_offset}").unwrap();
    writeln!(
        output,
        "completion: {} {:?}",
        completion.label(),
        completion.detail()
    )
    .unwrap();
    writeln!(
        output,
        "definition: {}..{} {:?}",
        definition.target_range().range.start(),
        definition.target_range().range.end(),
        analysis
            .hover(position)
            .map(|hover| hover.signature().to_string())
    )
    .unwrap();
    for reference in analysis.references(position, true) {
        let range = reference.range().range;
        let (start, end) = (range.start() as usize, range.end() as usize);
        writeln!(
            output,
            "reference: {start}..{end} {:?}",
            &source[start..end]
        )
        .unwrap();
    }
    for file_edit in analysis.rename(position, "element").unwrap().file_edits() {
        for edit in file_edit.edits() {
            let range = edit.range();
            let (start, end) = (range.start() as usize, range.end() as usize);
            writeln!(
                output,
                "rename: {start}..{end} {:?} -> {:?}",
                &source[start..end],
                edit.new_text()
            )
            .unwrap();
        }
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
