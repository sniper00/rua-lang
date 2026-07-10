//! File-module resolution over source-root paths already present in the VFS.

use std::path::Path;

use crate::{
    BaseDb,
    vfs::{FileId, SourceRootKind, VfsPath},
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
    let candidates = module_file_candidates(&directory, name)?;
    let mut source_roots: Vec<_> = db.source_roots().collect();
    source_roots.sort_by_key(|(source_root_id, source_root)| {
        (root_priority(source_root.kind()), source_root_id.index())
    });

    for (source_root_id, _) in source_roots {
        for candidate in &candidates {
            if let Some(file_id) = db.file_for_path_in_root(candidate, source_root_id) {
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
    use crate::{AnalysisHost, Change, FileId, FileKind, SourceRootId, SourceRootKind, VfsPath};

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
        let mut host = AnalysisHost::new();
        host.apply_change(change);

        assert_eq!(
            host.analysis().resolve_module(main_id, "foo"),
            Some(workspace_file)
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
