//! Public protocol-neutral IDE contract tests.

use rua_analysis::{
    AnalysisHost, Change, CompletionInsert, CompletionItem, CompletionKind, Diagnostic,
    DiagnosticCode, DiagnosticOrigin, DiagnosticRelated, DiagnosticSeverity, FileId, FileKind,
    FilePosition, FileRange, HoverResult, MacroDelimiter, NavigationTarget, ProjectId,
    ProjectPosition, QueryContext, ReferenceKind, ReferenceResult, RenameError, SemanticToken,
    SemanticTokenKind, SemanticTokenModifiers, SourceChange, SourceRootId, SourceRootKind,
    TextEdit, TextRange, normalize_diagnostics,
};

#[test]
fn ide_contract_legacy_reexports_are_the_same_types() {
    let range = TextRange::new(1, 4);
    let hir_range: rua_analysis::hir::TextRange = range;
    let ide_range: rua_analysis::ide::TextRange = hir_range;
    assert_eq!(ide_range, range);

    let position = FilePosition::new(FileId::new(2), 3);
    let semantic_position: rua_analysis::semantic::FilePosition = position;
    let ide_position: rua_analysis::ide::FilePosition = semantic_position;
    assert_eq!(ide_position, position);
}

#[test]
fn ide_contract_text_ranges_are_half_open_utf8_byte_ranges() {
    let source = "a中b";
    let range = TextRange::new(1, 4);
    assert_eq!(&source[range.start() as usize..range.end() as usize], "中");
    assert_eq!(range.len(), 3);
    assert!(range.contains(1));
    assert!(range.contains(3));
    assert!(!range.contains(4));
    assert!(range.contains_range(TextRange::new(2, 4)));
    assert!(TextRange::new(4, 4).is_empty());
}

#[test]
#[should_panic(expected = "text range start must not exceed end")]
fn ide_contract_rejects_inverted_text_ranges() {
    let _ = TextRange::new(4, 3);
}

#[test]
fn ide_contract_file_and_project_positions_preserve_context() {
    let file_id = FileId::new(7);
    let position = FilePosition::new(file_id, 11);
    let first = ProjectPosition::new(ProjectId::new(1), position);
    let second = ProjectPosition::at(ProjectId::new(2), file_id, 11);
    assert_ne!(first, second);
    assert_eq!(first.position, position);
    assert_eq!(first.project_file().file_id, file_id);

    let context = QueryContext::new(ProjectId::new(3));
    assert_eq!(context.position(position).project_id, ProjectId::new(3));
    assert_eq!(context.file(file_id).project_id, ProjectId::new(3));
}

#[test]
fn ide_contract_diagnostic_compatibility_and_normalization() {
    let file_id = FileId::new(1);
    let primary = TextRange::new(8, 12);
    let related = DiagnosticRelated::new(
        FileRange::new(file_id, TextRange::new(1, 4)),
        "declared here",
    );
    let diagnostic = Diagnostic::new(
        file_id,
        primary,
        "type mismatch",
        DiagnosticOrigin::FastAnalysis,
    )
    .with_code(DiagnosticCode::TypeMismatch)
    .with_severity(DiagnosticSeverity::Warning)
    .with_related([related.clone(), related]);

    assert_eq!(diagnostic.file_id(), file_id);
    assert_eq!(diagnostic.range(), primary);
    assert_eq!(diagnostic.file_range(), FileRange::new(file_id, primary));
    assert_eq!(diagnostic.code(), Some(DiagnosticCode::TypeMismatch));
    assert_eq!(diagnostic.severity(), DiagnosticSeverity::Warning);
    assert_eq!(diagnostic.related().len(), 1);
    assert_eq!(diagnostic.origin(), DiagnosticOrigin::FastAnalysis);

    let earlier = Diagnostic::new(
        file_id,
        TextRange::new(0, 1),
        "earlier",
        DiagnosticOrigin::FastAnalysis,
    );
    let mut diagnostics = vec![diagnostic.clone(), earlier.clone(), diagnostic];
    normalize_diagnostics(&mut diagnostics);
    assert_eq!(diagnostics.len(), 2);
    assert_eq!(diagnostics[0], earlier);
    assert_eq!(diagnostics[1].message(), "type mismatch");
}

#[test]
fn ide_contract_navigation_hover_and_completion_are_protocol_neutral() {
    let file_id = FileId::new(4);
    let full = FileRange::new(file_id, TextRange::new(10, 30));
    let navigation = NavigationTarget::new(full, Some(TextRange::new(14, 18)));
    assert_eq!(
        navigation.target_range(),
        FileRange::new(file_id, TextRange::new(14, 18))
    );

    let hover = HoverResult::new(
        FileRange::new(file_id, TextRange::new(40, 44)),
        "fn area(point: Point) -> i64",
    )
    .with_documentation("Returns the area.");
    assert_eq!(hover.range().file_id, file_id);
    assert_eq!(hover.signature(), "fn area(point: Point) -> i64");
    assert_eq!(hover.documentation(), Some("Returns the area."));

    let completion = CompletionItem::new("area", CompletionKind::Function)
        .with_detail("fn area(point: Point) -> i64")
        .with_documentation("Returns the area.")
        .with_lookup("area")
        .with_insert(CompletionInsert::Call {
            callee: "area".to_string(),
            has_arguments: true,
        })
        .with_replacement_range(TextRange::new(40, 42))
        .with_target(navigation.target_range())
        .with_relevance(10);
    assert_eq!(completion.label(), "area");
    assert!(matches!(
        completion.insert(),
        Some(CompletionInsert::Call { .. })
    ));
    assert_eq!(completion.replacement_range(), Some(TextRange::new(40, 42)));

    let macro_completion = CompletionItem::new("vec!", CompletionKind::Macro).with_insert(
        CompletionInsert::MacroCall {
            name: "vec!".to_string(),
            delimiter: MacroDelimiter::Brackets,
        },
    );
    assert!(matches!(
        macro_completion.insert(),
        Some(CompletionInsert::MacroCall {
            delimiter: MacroDelimiter::Brackets,
            ..
        })
    ));
}

#[test]
fn ide_contract_list_sort_keys_are_deterministic() {
    let target = FileRange::new(FileId::new(0), TextRange::new(1, 2));
    let input = vec![
        CompletionItem::new("zeta", CompletionKind::Variable),
        CompletionItem::new("alpha", CompletionKind::Variable).with_relevance(1),
        CompletionItem::new("alpha", CompletionKind::Variable),
        CompletionItem::new("beta", CompletionKind::Variable),
        CompletionItem::new("beta", CompletionKind::Variable).with_documentation("documented"),
        CompletionItem::new("renamed", CompletionKind::Function)
            .with_target(target)
            .with_relevance(2),
        CompletionItem::new("same-target", CompletionKind::Function).with_target(target),
    ];
    let mut completions = input.clone();
    let mut reversed = input;
    reversed.reverse();
    CompletionItem::normalize(&mut completions);
    CompletionItem::normalize(&mut reversed);
    assert_eq!(completions, reversed);
    assert_eq!(
        completions
            .iter()
            .map(CompletionItem::label)
            .collect::<Vec<_>>(),
        ["renamed", "alpha", "beta", "same-target", "zeta"]
    );
    assert_eq!(completions[2].documentation(), Some("documented"));

    let range = FileRange::new(FileId::new(1), TextRange::new(4, 8));
    let mut references = vec![
        ReferenceResult::new(range, ReferenceKind::Read),
        ReferenceResult::new(range, ReferenceKind::Declaration),
        ReferenceResult::new(
            FileRange::new(FileId::new(0), TextRange::new(9, 10)),
            ReferenceKind::Write,
        ),
    ];
    ReferenceResult::normalize(&mut references);
    assert_eq!(references.len(), 2);
    assert_eq!(references[1].kind(), ReferenceKind::Declaration);

    let mut tokens = vec![
        SemanticToken::new(
            FileRange::new(FileId::new(1), TextRange::new(8, 10)),
            SemanticTokenKind::Method,
            SemanticTokenModifiers::NONE,
        ),
        SemanticToken::new(
            FileRange::new(FileId::new(0), TextRange::new(1, 2)),
            SemanticTokenKind::Parameter,
            SemanticTokenModifiers::DECLARATION,
        ),
        SemanticToken::new(
            FileRange::new(FileId::new(0), TextRange::new(1, 2)),
            SemanticTokenKind::Parameter,
            SemanticTokenModifiers::READ_ONLY,
        ),
        SemanticToken::new(
            FileRange::new(FileId::new(1), TextRange::new(8, 10)),
            SemanticTokenKind::Method,
            SemanticTokenModifiers::NONE,
        ),
    ];
    SemanticToken::normalize(&mut tokens);
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0].file_id(), FileId::new(0));
    assert!(tokens[0].is_declaration());
    assert!(
        tokens[0]
            .modifiers()
            .contains(SemanticTokenModifiers::READ_ONLY)
    );
}

#[test]
fn ide_contract_source_change_is_sorted_deduplicated_and_non_overlapping() {
    let first_file = FileId::new(1);
    let second_file = FileId::new(2);
    let duplicate = TextEdit::new(TextRange::new(8, 10), "x");
    let change = SourceChange::from_edits(
        [
            (second_file, TextEdit::new(TextRange::new(4, 5), "b")),
            (first_file, duplicate.clone()),
            (first_file, TextEdit::new(TextRange::new(1, 2), "a")),
            (first_file, duplicate),
        ],
        |_| false,
    )
    .unwrap();
    assert_eq!(change.file_edits().len(), 2);
    assert_eq!(change.file_edits()[0].file_id(), first_file);
    assert_eq!(change.file_edits()[0].edits().len(), 2);
    assert_eq!(
        change.file_edits()[0].edits()[0].range(),
        TextRange::new(1, 2)
    );

    let conflict = SourceChange::from_edits(
        [
            (first_file, TextEdit::new(TextRange::new(1, 4), "a")),
            (first_file, TextEdit::new(TextRange::new(3, 5), "b")),
        ],
        |_| false,
    );
    assert!(matches!(
        conflict,
        Err(RenameError::ConflictingEdits { .. })
    ));

    let conflicting_insertions = SourceChange::from_edits(
        [
            (first_file, TextEdit::new(TextRange::new(3, 3), "a")),
            (first_file, TextEdit::new(TextRange::new(3, 3), "b")),
        ],
        |_| false,
    );
    assert!(matches!(
        conflicting_insertions,
        Err(RenameError::ConflictingEdits { .. })
    ));

    let read_only = SourceChange::from_edits(
        [(second_file, TextEdit::new(TextRange::new(1, 2), "x"))],
        |file_id| file_id == second_file,
    );
    assert!(matches!(read_only, Err(RenameError::ReadOnly { .. })));
}

#[test]
fn ide_contract_source_change_uses_analysis_readonly_policy() {
    let workspace_file = FileId::new(1);
    let library_file = FileId::new(2);
    let mut change = Change::new();
    change.set_source_root(SourceRootId::new(0), SourceRootKind::Workspace);
    change.set_source_root(SourceRootId::new(1), SourceRootKind::Library);
    change.set_file(
        workspace_file,
        SourceRootId::new(0),
        FileKind::Source,
        "fn main() {}",
    );
    change.set_file(
        library_file,
        SourceRootId::new(1),
        FileKind::Declaration,
        "pub fn api();",
    );
    let mut host = AnalysisHost::new();
    host.apply_change(change);
    let analysis = host.analysis();

    let read_only = SourceChange::from_edits(
        [(library_file, TextEdit::new(TextRange::new(7, 10), "sdk"))],
        |file_id| analysis.is_file_read_only(file_id),
    );
    assert!(matches!(read_only, Err(RenameError::ReadOnly { .. })));

    let writable = SourceChange::from_edits(
        [(workspace_file, TextEdit::new(TextRange::new(3, 7), "run"))],
        |file_id| analysis.is_file_read_only(file_id),
    )
    .unwrap();
    assert_eq!(writable.file_edits()[0].file_id(), workspace_file);
}
