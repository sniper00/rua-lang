use rua_analysis::{
    AnalysisHost, Change, FileId, FileKind, ProjectData, ProjectFile, ProjectId, ProjectRoot,
    SourceRootId, SourceRootKind,
};

#[test]
fn project_diagnostics_reuse_reference_index_cache() {
    let file_id = FileId::new(0);
    let root_id = SourceRootId::new(0);
    let project_id = ProjectId::new(0);
    let mut change = Change::new();
    change.set_source_root(root_id, SourceRootKind::Workspace);
    change.set_file_with_path(
        file_id,
        root_id,
        FileKind::Source,
        "main.rua",
        "fn unused() {} fn main() {}",
    );
    change.set_project(
        project_id,
        ProjectData::new(file_id, [ProjectRoot::at_root(root_id)], []),
    );

    let mut host = AnalysisHost::new();
    host.apply_change(change);
    let analysis = host.analysis();
    let file = ProjectFile::new(project_id, file_id);

    let first = analysis.diagnostics_in_project(file);
    let after_first = analysis.query_stats();
    let second = analysis.diagnostics_in_project(file);
    let after_second = analysis.query_stats();

    assert_eq!(first, second);
    assert_eq!(after_first.reference_index, 1);
    assert_eq!(after_second.reference_index, after_first.reference_index);
}
