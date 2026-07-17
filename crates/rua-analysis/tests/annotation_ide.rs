use rua_analysis::{
    AnalysisHost, Change, FileId, FileKind, ProjectData, ProjectFile, ProjectId, ProjectPosition,
    ProjectRoot, SemanticTokenModifiers, SourceRootId, SourceRootKind,
};

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

#[test]
fn annotation_usage_has_hover_and_goto_definition() {
    let source = concat!(
        "/// HTTP route metadata.\n",
        "#[targets(function)]\n",
        "pub annotation Route(path: String);\n",
        "\n",
        "#[Route(path = \"/users\")]\n",
        "pub fn users() {}\n",
    );
    let declaration = source.find("Route(path: String)").unwrap();
    let usage = source.rfind("Route(path").unwrap();
    let (analysis, file_id, project_id) = analysis(source);
    let position = ProjectPosition::at(project_id, file_id, usage as u32 + 1);

    let target = analysis
        .goto_definition(position)
        .expect("annotation goto definition");
    assert_eq!(target.target_range().range.start() as usize, declaration);

    let hover = analysis.hover(position).expect("annotation hover");
    assert_eq!(
        hover.signature(),
        "annotation Route(path: String)\ntargets: function\nretention: build\nrepeatable: false"
    );
    assert_eq!(hover.documentation(), Some("HTTP route metadata."));

    let references = analysis.references(position, true);
    assert_eq!(references.len(), 2);
    let rename = analysis.rename(position, "Endpoint").unwrap();
    assert_eq!(rename.file_edits()[0].edits().len(), 2);
}

#[test]
fn inactive_member_tokens_keep_the_inactive_modifier() {
    let source = concat!(
        "struct Config {\n",
        "    #[cfg(feature = \"server\")]\n",
        "    endpoint: String,\n",
        "}\n",
    );
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
    let endpoint = source.find("endpoint").unwrap() as u32;
    let analysis = host.analysis();
    let token = analysis
        .semantic_tokens_in_project(ProjectFile::new(project_id, file_id))
        .into_iter()
        .find(|token| token.range().contains(endpoint))
        .expect("inactive field token");
    assert!(token.modifiers().contains(SemanticTokenModifiers::INACTIVE));
    assert_eq!(
        analysis
            .hover(ProjectPosition::at(project_id, file_id, endpoint))
            .unwrap()
            .signature(),
        "inactive: `#[cfg(feature = \"server\")]` is false for the current project configuration"
    );
}

#[test]
fn annotation_completion_is_schema_aware() {
    let source = concat!(
        "#[targets(function)]\n",
        "annotation Route(method: String, path: String);\n",
        "#[Rou]\n",
        "fn users() {}\n",
    );
    let cursor = source.find("Rou]").unwrap() + 3;
    let (first_analysis, file_id, project_id) = analysis(source);
    let completions =
        first_analysis.completions(ProjectPosition::at(project_id, file_id, cursor as u32));
    let route = completions
        .iter()
        .find(|item| item.label() == "Route")
        .expect("annotation name completion");
    assert_eq!(route.kind(), rua_analysis::CompletionKind::Annotation);
    assert_eq!(
        route.detail(),
        Some("annotation Route(method: String, path: String)")
    );

    let source = concat!(
        "#[targets(function)]\n",
        "annotation Route(method: String, path: String);\n",
        "#[Route()]\n",
        "fn users() {}\n",
    );
    let cursor = source.find("Route()").unwrap() + "Route(".len();
    let (analysis, file_id, project_id) = analysis(source);
    let completions = analysis.completions(ProjectPosition::at(project_id, file_id, cursor as u32));
    let labels = completions
        .iter()
        .map(|item| item.label())
        .collect::<Vec<_>>();
    assert!(labels.contains(&"path"), "{labels:?}");
    assert!(labels.contains(&"method"), "{labels:?}");
}

#[test]
fn annotation_diagnostics_report_schema_argument_errors() {
    let source = concat!(
        "#[targets(function)]\n",
        "annotation Route(method: String, path: String);\n",
        "#[Route(method = \"GET\", extra = 1)]\n",
        "fn users() {}\n",
    );
    let (analysis, file_id, project_id) = analysis(source);
    let diagnostics = analysis.diagnostics_in_project(ProjectFile::new(project_id, file_id));
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == Some(rua_analysis::DiagnosticCode::AnnotationInvalidArguments)
                && diagnostic
                    .message()
                    .contains("unknown annotation argument `extra`")
        }),
        "{diagnostics:?}"
    );
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == Some(rua_analysis::DiagnosticCode::AnnotationInvalidArguments)
            && diagnostic
                .message()
                .contains("missing annotation argument `path`")
    }));
}

#[test]
fn annotation_argument_name_links_to_schema_parameter() {
    let source = concat!(
        "#[targets(function)]\n",
        "annotation Route(method: String, path: String);\n",
        "#[Route(method = \"GET\", path = \"/users\")]\n",
        "fn users() {}\n",
    );
    let declaration = source.find("method: String").unwrap();
    let usage = source.rfind("method =").unwrap();
    let (analysis, file_id, project_id) = analysis(source);
    let position = ProjectPosition::at(project_id, file_id, usage as u32 + 1);
    let target = analysis.goto_definition(position).unwrap();
    assert_eq!(target.target_range().range.start() as usize, declaration);
    assert_eq!(
        analysis.hover(position).unwrap().signature(),
        "annotation parameter method: String"
    );
}
