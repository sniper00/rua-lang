//! File-module resolution over source-root paths already present in the VFS.

use std::path::Path;

use crate::{
    BaseDb,
    vfs::{FileId, ProjectId, ProjectRoot, SourceRootKind, VfsPath},
};

/// Ordered logical paths considered for a file module declaration.
pub fn module_file_candidates(directory: &VfsPath, name: &str) -> Option<[VfsPath; 4]> {
    if !is_module_name(name) {
        return None;
    }
    let child_directory = directory.join(name);
    Some([
        directory.join(format!("{name}.rua")),
        child_directory.join("mod.rua"),
        directory.join(format!("{name}.ruai")),
        child_directory.join("mod.ruai"),
    ])
}

pub(crate) fn resolve_module_file(db: &BaseDb, from_file: FileId, name: &str) -> Option<FileId> {
    let directory = db.file_path(from_file)?.parent()?;
    resolve_module_file_at(db, from_file, &directory, name)
}

pub(crate) fn resolve_module_file_at(
    db: &BaseDb,
    from_file: FileId,
    directory: &VfsPath,
    name: &str,
) -> Option<FileId> {
    let candidates = module_file_candidates(directory, name)?;
    let source_root_id = db.source_root_id(from_file)?;
    for candidate in &candidates {
        if let Some(file_id) = db.file_for_path_in_root(candidate, source_root_id) {
            return Some(file_id);
        }
    }
    None
}

pub(crate) fn project_file_logical_directory(
    db: &BaseDb,
    project_id: ProjectId,
    file_id: FileId,
) -> Option<VfsPath> {
    let project = db.project(project_id)?;
    let source_root_id = db.source_root_id(file_id)?;
    let directory = db.file_path(file_id)?.parent()?;
    project
        .roots()
        .filter(|root| root.source_root_id() == source_root_id)
        .find_map(|root| directory.strip_prefix(root.logical_base()))
}

pub(crate) fn resolve_module_file_in_project_at(
    db: &BaseDb,
    project_id: ProjectId,
    from_file: FileId,
    directory: &VfsPath,
    name: &str,
) -> Option<FileId> {
    let project = db.project(project_id)?;
    let from_root = db.source_root_id(from_file)?;
    if !project
        .roots()
        .any(|root| root.source_root_id() == from_root)
    {
        return None;
    }
    let mut roots: Vec<(usize, &ProjectRoot)> = project.roots().enumerate().collect();
    roots.sort_by_key(|(order, root)| {
        let priority = db
            .source_root(root.source_root_id())
            .map(|source_root| root_priority(source_root.kind()))
            .unwrap_or(u8::MAX);
        (priority, *order)
    });

    for (_, root) in roots {
        let directory = root.logical_base().join(directory.as_path());
        let candidates = module_file_candidates(&directory, name)?;
        for candidate in &candidates {
            if let Some(file_id) = db.file_for_path_in_root(candidate, root.source_root_id()) {
                return Some(file_id);
            }
        }
    }
    None
}

fn is_module_name(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none()
}

const fn root_priority(kind: SourceRootKind) -> u8 {
    match kind {
        SourceRootKind::Workspace => 0,
        SourceRootKind::Library => 1,
        SourceRootKind::Std => 2,
        SourceRootKind::Virtual => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::module_file_candidates;
    use crate::{
        AnalysisHost, Change, FileId, FileKind, ProjectData, ProjectId, ProjectRoot, SourceRootId,
        SourceRootKind, VfsPath,
    };

    #[test]
    fn module_resolution_supports_all_file_layouts() {
        let cases = [
            ("src/foo.rua", FileKind::Source),
            ("src/foo/mod.rua", FileKind::Source),
            ("src/foo.ruai", FileKind::Declaration),
            ("src/foo/mod.ruai", FileKind::Declaration),
        ];

        for (index, (target_path, target_kind)) in cases.iter().copied().enumerate() {
            let root_id = SourceRootId::new(0);
            let main_id = FileId::new(0);
            let target_id = FileId::new(index as u32 + 1);
            let mut change = Change::new();
            change.set_source_root(root_id, SourceRootKind::Workspace);
            change.set_file_with_path(
                main_id,
                root_id,
                FileKind::Source,
                "src/main.rua",
                "mod foo;",
            );
            change.set_file_with_path(target_id, root_id, target_kind, target_path, "");
            let mut host = AnalysisHost::new();
            host.apply_change(change);

            assert_eq!(
                host.analysis().resolve_module(main_id, "foo"),
                Some(target_id),
                "failed to resolve {target_path}"
            );
        }
    }

    #[test]
    fn module_resolution_uses_candidate_order_within_a_root() {
        let root_id = SourceRootId::new(0);
        let main_id = FileId::new(0);
        let mut change = Change::new();
        change.set_source_root(root_id, SourceRootKind::Workspace);
        change.set_file_with_path(
            main_id,
            root_id,
            FileKind::Source,
            "src/main.rua",
            "mod foo;",
        );
        for (index, path) in [
            "src/foo.rua",
            "src/foo/mod.rua",
            "src/foo.ruai",
            "src/foo/mod.ruai",
        ]
        .into_iter()
        .enumerate()
        {
            change.set_file_with_path(
                FileId::new(index as u32 + 1),
                root_id,
                if path.ends_with(".ruai") {
                    FileKind::Declaration
                } else {
                    FileKind::Source
                },
                path,
                "",
            );
        }
        let mut host = AnalysisHost::new();
        host.apply_change(change);

        assert_eq!(
            host.analysis().resolve_module(main_id, "foo"),
            Some(FileId::new(1))
        );
    }

    #[test]
    fn module_resolution_prefers_workspace_then_library_then_std() {
        let project_id = ProjectId::new(0);
        let workspace_root = SourceRootId::new(10);
        let library_root = SourceRootId::new(1);
        let std_root = SourceRootId::new(0);
        let main_id = FileId::new(0);
        let workspace_file = FileId::new(1);
        let library_file = FileId::new(2);
        let std_file = FileId::new(3);
        let mut change = Change::new();
        for (root_id, kind) in [
            (std_root, SourceRootKind::Std),
            (library_root, SourceRootKind::Library),
            (workspace_root, SourceRootKind::Workspace),
        ] {
            change.set_source_root(root_id, kind);
        }
        change.set_file_with_path(
            main_id,
            workspace_root,
            FileKind::Source,
            "src/main.rua",
            "mod foo;",
        );
        change.set_file_with_path(std_file, std_root, FileKind::Source, "src/foo.rua", "");
        change.set_file_with_path(
            library_file,
            library_root,
            FileKind::Source,
            "src/foo.rua",
            "",
        );
        change.set_file_with_path(
            workspace_file,
            workspace_root,
            FileKind::Declaration,
            "src/foo.ruai",
            "",
        );
        change.set_project(
            project_id,
            ProjectData::new(
                main_id,
                [ProjectRoot::new(workspace_root, "src")],
                [
                    ProjectRoot::new(library_root, "src"),
                    ProjectRoot::new(std_root, "src"),
                ],
            ),
        );
        let mut host = AnalysisHost::new();
        host.apply_change(change);

        assert_eq!(
            host.analysis()
                .resolve_module_in_project(project_id, main_id, "foo"),
            Some(workspace_file)
        );

        let mut remove_workspace = Change::new();
        remove_workspace.remove_file(workspace_file);
        host.apply_change(remove_workspace);
        assert_eq!(
            host.analysis()
                .resolve_module_in_project(project_id, main_id, "foo"),
            Some(library_file)
        );

        let mut remove_library = Change::new();
        remove_library.remove_file(library_file);
        host.apply_change(remove_library);
        assert_eq!(
            host.analysis()
                .resolve_module_in_project(project_id, main_id, "foo"),
            Some(std_file)
        );
    }

    #[test]
    fn module_candidates_are_logical_and_reject_path_traversal() {
        assert_eq!(
            module_file_candidates(&VfsPath::new("src"), "foo"),
            Some([
                VfsPath::new("src/foo.rua"),
                VfsPath::new("src/foo/mod.rua"),
                VfsPath::new("src/foo.ruai"),
                VfsPath::new("src/foo/mod.ruai"),
            ])
        );
        assert_eq!(module_file_candidates(&VfsPath::new("src"), "../foo"), None);
    }
}
