//! In-memory files, source roots, workspace configuration, and changes.
//!
//! This module does not perform filesystem IO. Loaders and protocol adapters
//! translate external state into explicit changes at the crate boundary.

use std::{
    collections::{BTreeSet, HashMap},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

pub use rua_core::{CfgOptions, FileId, ProjectId, SourceRootId};
pub use rua_project::SourceRootKind;

/// Logical path within a source root. Construction is lexical and performs no
/// filesystem access, so virtual and unsaved files follow the same rules.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VfsPath(PathBuf);

impl VfsPath {
    pub fn new(path: impl AsRef<Path>) -> Self {
        let mut normalized = PathBuf::new();
        for component in path.as_ref().components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    if !normalized.pop() {
                        normalized.push(component);
                    }
                }
                _ => normalized.push(component),
            }
        }
        Self(normalized)
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn parent(&self) -> Option<Self> {
        self.0.parent().map(Self::new)
    }

    pub fn join(&self, path: impl AsRef<Path>) -> Self {
        Self::new(self.0.join(path))
    }

    pub fn strip_prefix(&self, base: &Self) -> Option<Self> {
        self.0.strip_prefix(&base.0).ok().map(Self::new)
    }
}

impl<P: AsRef<Path>> From<P> for VfsPath {
    fn from(path: P) -> Self {
        Self::new(path)
    }
}

/// One source root mounted at a logical module base within a project.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectRoot {
    source_root_id: SourceRootId,
    logical_base: VfsPath,
}

impl ProjectRoot {
    pub fn new(source_root_id: SourceRootId, logical_base: impl Into<VfsPath>) -> Self {
        Self {
            source_root_id,
            logical_base: logical_base.into(),
        }
    }

    pub fn at_root(source_root_id: SourceRootId) -> Self {
        Self::new(source_root_id, VfsPath::new(""))
    }

    pub const fn source_root_id(&self) -> SourceRootId {
        self.source_root_id
    }

    pub const fn logical_base(&self) -> &VfsPath {
        &self.logical_base
    }
}

/// Explicit resolution boundary for one workspace and its ordered dependencies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectData {
    root_file: FileId,
    workspace_roots: Vec<ProjectRoot>,
    dependency_roots: Vec<ProjectRoot>,
    cfg: CfgOptions,
}

impl ProjectData {
    pub fn new(
        root_file: FileId,
        workspace_roots: impl IntoIterator<Item = ProjectRoot>,
        dependency_roots: impl IntoIterator<Item = ProjectRoot>,
    ) -> Self {
        Self {
            root_file,
            workspace_roots: workspace_roots.into_iter().collect(),
            dependency_roots: dependency_roots.into_iter().collect(),
            cfg: CfgOptions::default(),
        }
    }

    pub fn with_cfg(mut self, cfg: CfgOptions) -> Self {
        self.cfg = cfg;
        self
    }

    pub const fn root_file(&self) -> FileId {
        self.root_file
    }

    pub fn workspace_roots(&self) -> &[ProjectRoot] {
        &self.workspace_roots
    }

    pub fn dependency_roots(&self) -> &[ProjectRoot] {
        &self.dependency_roots
    }

    pub fn roots(&self) -> impl Iterator<Item = &ProjectRoot> {
        self.workspace_roots.iter().chain(&self.dependency_roots)
    }

    pub const fn cfg(&self) -> &CfgOptions {
        &self.cfg
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileKind {
    Source,
    Declaration,
}

/// A set of files sharing module resolution and editability rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRoot {
    kind: SourceRootKind,
    files: BTreeSet<FileId>,
}

impl SourceRoot {
    pub fn new(kind: SourceRootKind) -> Self {
        Self {
            kind,
            files: BTreeSet::new(),
        }
    }

    pub const fn kind(&self) -> SourceRootKind {
        self.kind
    }

    pub const fn is_read_only(&self) -> bool {
        self.kind.is_read_only()
    }

    pub fn files(&self) -> impl Iterator<Item = FileId> + '_ {
        self.files.iter().copied()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceRootChange {
    Set {
        source_root_id: SourceRootId,
        kind: SourceRootKind,
    },
    Remove {
        source_root_id: SourceRootId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectChange {
    Set {
        project_id: ProjectId,
        data: ProjectData,
    },
    Remove {
        project_id: ProjectId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileChange {
    SetText {
        file_id: FileId,
        text: Arc<str>,
    },
    SetFile {
        file_id: FileId,
        source_root_id: SourceRootId,
        kind: FileKind,
        path: Option<VfsPath>,
        text: Arc<str>,
    },
    Remove {
        file_id: FileId,
    },
}

impl FileChange {
    pub const fn file_id(&self) -> FileId {
        match self {
            Self::SetText { file_id, .. }
            | Self::SetFile { file_id, .. }
            | Self::Remove { file_id } => *file_id,
        }
    }
}

/// A batch of input mutations submitted by a loader or protocol adapter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Change {
    file_changes: Vec<FileChange>,
    source_root_changes: Vec<SourceRootChange>,
    project_changes: Vec<ProjectChange>,
}

impl Change {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_file_text(&mut self, file_id: FileId, text: impl Into<Arc<str>>) {
        self.file_changes.push(FileChange::SetText {
            file_id,
            text: text.into(),
        });
    }

    pub fn set_file(
        &mut self,
        file_id: FileId,
        source_root_id: SourceRootId,
        kind: FileKind,
        text: impl Into<Arc<str>>,
    ) {
        self.file_changes.push(FileChange::SetFile {
            file_id,
            source_root_id,
            kind,
            path: None,
            text: text.into(),
        });
    }

    pub fn set_file_with_path(
        &mut self,
        file_id: FileId,
        source_root_id: SourceRootId,
        kind: FileKind,
        path: impl Into<VfsPath>,
        text: impl Into<Arc<str>>,
    ) {
        self.file_changes.push(FileChange::SetFile {
            file_id,
            source_root_id,
            kind,
            path: Some(path.into()),
            text: text.into(),
        });
    }

    pub fn remove_file(&mut self, file_id: FileId) {
        self.file_changes.push(FileChange::Remove { file_id });
    }

    pub fn set_source_root(&mut self, source_root_id: SourceRootId, kind: SourceRootKind) {
        self.source_root_changes.push(SourceRootChange::Set {
            source_root_id,
            kind,
        });
    }

    pub fn remove_source_root(&mut self, source_root_id: SourceRootId) {
        self.source_root_changes
            .push(SourceRootChange::Remove { source_root_id });
    }

    pub fn set_project(&mut self, project_id: ProjectId, data: ProjectData) {
        self.project_changes
            .push(ProjectChange::Set { project_id, data });
    }

    pub fn remove_project(&mut self, project_id: ProjectId) {
        self.project_changes
            .push(ProjectChange::Remove { project_id });
    }

    pub fn is_empty(&self) -> bool {
        self.file_changes.is_empty()
            && self.source_root_changes.is_empty()
            && self.project_changes.is_empty()
    }

    pub fn file_changes(&self) -> &[FileChange] {
        &self.file_changes
    }

    pub fn source_root_changes(&self) -> &[SourceRootChange] {
        &self.source_root_changes
    }

    pub fn project_changes(&self) -> &[ProjectChange] {
        &self.project_changes
    }
}

/// The text input store used by the analysis database.
///
/// File contents enter through [`Change`]; this type never reads from disk.
#[derive(Clone, Debug, Default)]
pub struct Vfs {
    file_texts: HashMap<FileId, Arc<str>>,
    file_revisions: HashMap<FileId, u64>,
    next_revision: u64,
    file_kinds: HashMap<FileId, FileKind>,
    file_roots: HashMap<FileId, SourceRootId>,
    file_paths: HashMap<FileId, VfsPath>,
    path_files: HashMap<VfsPath, BTreeSet<FileId>>,
    source_roots: HashMap<SourceRootId, SourceRoot>,
    projects: HashMap<ProjectId, ProjectData>,
}

impl Vfs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_file_text(&mut self, file_id: FileId, text: impl Into<Arc<str>>) {
        let text = text.into();
        if self
            .file_texts
            .get(&file_id)
            .is_some_and(|old| old == &text)
        {
            return;
        }
        self.file_texts.insert(file_id, text);
        self.file_kinds.entry(file_id).or_insert(FileKind::Source);
        self.bump_file_revision(file_id);
    }

    pub fn set_file(
        &mut self,
        file_id: FileId,
        source_root_id: SourceRootId,
        kind: FileKind,
        text: impl Into<Arc<str>>,
    ) {
        self.set_file_with_path(file_id, source_root_id, kind, None, text);
    }

    fn set_file_with_path(
        &mut self,
        file_id: FileId,
        source_root_id: SourceRootId,
        kind: FileKind,
        path: Option<VfsPath>,
        text: impl Into<Arc<str>>,
    ) {
        assert!(
            self.source_roots.contains_key(&source_root_id),
            "cannot add {file_id:?} to unknown source root {source_root_id:?}"
        );

        self.set_file_text(file_id, text);
        self.file_kinds.insert(file_id, kind);
        self.remove_file_path(file_id);
        if let Some(path) = path {
            self.path_files
                .entry(path.clone())
                .or_default()
                .insert(file_id);
            self.file_paths.insert(file_id, path);
        }

        if let Some(previous_root_id) = self.file_roots.insert(file_id, source_root_id)
            && previous_root_id != source_root_id
            && let Some(previous_root) = self.source_roots.get_mut(&previous_root_id)
        {
            previous_root.files.remove(&file_id);
        }
        self.source_roots
            .get_mut(&source_root_id)
            .expect("source root existence checked above")
            .files
            .insert(file_id);
    }

    pub fn remove_file(&mut self, file_id: FileId) -> Option<Arc<str>> {
        self.bump_file_revision(file_id);
        self.file_kinds.remove(&file_id);
        self.remove_file_path(file_id);
        if let Some(source_root_id) = self.file_roots.remove(&file_id)
            && let Some(source_root) = self.source_roots.get_mut(&source_root_id)
        {
            source_root.files.remove(&file_id);
        }
        self.file_texts.remove(&file_id)
    }

    fn remove_file_path(&mut self, file_id: FileId) {
        let Some(path) = self.file_paths.remove(&file_id) else {
            return;
        };
        if let Some(file_ids) = self.path_files.get_mut(&path) {
            file_ids.remove(&file_id);
            if file_ids.is_empty() {
                self.path_files.remove(&path);
            }
        }
    }

    pub fn set_source_root(&mut self, source_root_id: SourceRootId, kind: SourceRootKind) {
        self.source_roots
            .entry(source_root_id)
            .and_modify(|source_root| source_root.kind = kind)
            .or_insert_with(|| SourceRoot::new(kind));
    }

    pub fn remove_source_root(&mut self, source_root_id: SourceRootId) -> Option<SourceRoot> {
        let source_root = self.source_roots.remove(&source_root_id)?;
        for file_id in source_root.files() {
            self.bump_file_revision(file_id);
            self.file_texts.remove(&file_id);
            self.file_kinds.remove(&file_id);
            self.file_roots.remove(&file_id);
            self.remove_file_path(file_id);
        }
        Some(source_root)
    }

    pub fn apply_change(&mut self, change: Change) {
        for source_root_change in change.source_root_changes {
            match source_root_change {
                SourceRootChange::Set {
                    source_root_id,
                    kind,
                } => self.set_source_root(source_root_id, kind),
                SourceRootChange::Remove { source_root_id } => {
                    self.remove_source_root(source_root_id);
                }
            }
        }

        for file_change in change.file_changes {
            match file_change {
                FileChange::SetText { file_id, text } => {
                    self.set_file_text(file_id, text);
                }
                FileChange::SetFile {
                    file_id,
                    source_root_id,
                    kind,
                    path,
                    text,
                } => self.set_file_with_path(file_id, source_root_id, kind, path, text),
                FileChange::Remove { file_id } => {
                    self.remove_file(file_id);
                }
            }
        }

        for project_change in change.project_changes {
            match project_change {
                ProjectChange::Set { project_id, data } => {
                    self.projects.insert(project_id, data);
                }
                ProjectChange::Remove { project_id } => {
                    self.projects.remove(&project_id);
                }
            }
        }
    }

    pub fn file_text(&self, file_id: FileId) -> Option<Arc<str>> {
        self.file_texts.get(&file_id).cloned()
    }

    pub fn file_revision(&self, file_id: FileId) -> Option<u64> {
        self.file_texts
            .contains_key(&file_id)
            .then(|| self.file_revisions.get(&file_id).copied())
            .flatten()
    }

    pub fn file_kind(&self, file_id: FileId) -> Option<FileKind> {
        self.file_kinds.get(&file_id).copied()
    }

    pub fn source_root_id(&self, file_id: FileId) -> Option<SourceRootId> {
        self.file_roots.get(&file_id).copied()
    }

    pub fn source_root(&self, source_root_id: SourceRootId) -> Option<&SourceRoot> {
        self.source_roots.get(&source_root_id)
    }

    pub fn source_roots(&self) -> impl Iterator<Item = (SourceRootId, &SourceRoot)> {
        self.source_roots
            .iter()
            .map(|(source_root_id, source_root)| (*source_root_id, source_root))
    }

    pub fn project(&self, project_id: ProjectId) -> Option<&ProjectData> {
        self.projects.get(&project_id)
    }

    pub fn file_path(&self, file_id: FileId) -> Option<&VfsPath> {
        self.file_paths.get(&file_id)
    }

    pub fn file_for_path_in_root(
        &self,
        path: &VfsPath,
        source_root_id: SourceRootId,
    ) -> Option<FileId> {
        self.path_files
            .get(path)?
            .iter()
            .copied()
            .find(|file_id| self.file_roots.get(file_id).copied() == Some(source_root_id))
    }

    pub fn is_file_read_only(&self, file_id: FileId) -> bool {
        self.source_root_id(file_id)
            .and_then(|source_root_id| self.source_root(source_root_id))
            .is_some_and(SourceRoot::is_read_only)
    }

    fn bump_file_revision(&mut self, file_id: FileId) {
        self.next_revision = self
            .next_revision
            .checked_add(1)
            .expect("VFS revision space exhausted");
        self.file_revisions.insert(file_id, self.next_revision);
    }
}

#[cfg(test)]
mod tests {
    use super::{Change, FileId, FileKind, SourceRootId, SourceRootKind, Vfs, VfsPath};

    #[test]
    fn vfs_applies_file_add_update_and_remove() {
        let file_id = FileId::new(7);
        let mut vfs = Vfs::new();

        let mut add = Change::new();
        add.set_file_text(file_id, "let answer = 41");
        vfs.apply_change(add);
        assert_eq!(vfs.file_text(file_id).as_deref(), Some("let answer = 41"));

        let mut update = Change::new();
        update.set_file_text(file_id, "let answer = 42");
        vfs.apply_change(update);
        assert_eq!(vfs.file_text(file_id).as_deref(), Some("let answer = 42"));

        let mut remove = Change::new();
        remove.remove_file(file_id);
        vfs.apply_change(remove);
        assert_eq!(vfs.file_text(file_id), None);
    }

    #[test]
    fn vfs_applies_changes_in_batch_order() {
        let file_id = FileId::new(3);
        let mut change = Change::new();
        change.set_file_text(file_id, "first");
        change.remove_file(file_id);
        change.set_file_text(file_id, "last");

        let mut vfs = Vfs::new();
        vfs.apply_change(change);

        assert_eq!(vfs.file_text(file_id).as_deref(), Some("last"));
    }

    #[test]
    fn vfs_indexes_logical_paths_without_filesystem_access() {
        let root_id = SourceRootId::new(1);
        let file_id = FileId::new(3);
        let path = VfsPath::new("src/./nested/../api.ruai");
        let mut change = Change::new();
        change.set_source_root(root_id, SourceRootKind::Library);
        change.set_file_with_path(
            file_id,
            root_id,
            FileKind::Declaration,
            path,
            "extern \"lua\" {}",
        );

        let mut vfs = Vfs::new();
        vfs.apply_change(change);

        let normalized = VfsPath::new("src/api.ruai");
        assert_eq!(vfs.file_path(file_id), Some(&normalized));
        assert_eq!(
            vfs.file_for_path_in_root(&normalized, root_id),
            Some(file_id)
        );
        assert_eq!(
            vfs.file_for_path_in_root(&VfsPath::new("src/missing.ruai"), root_id),
            None
        );
    }
}
