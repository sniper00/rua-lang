//! In-memory files, source roots, workspace configuration, and changes.
//!
//! This module does not perform filesystem IO. Loaders and protocol adapters
//! translate external state into explicit changes at the crate boundary.

use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};

/// Stable identity of a file for the lifetime of an analysis session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileId(u32);

impl FileId {
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn index(self) -> u32 {
        self.0
    }
}

/// Stable identity of a group of files sharing resolution rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceRootId(u32);

impl SourceRootId {
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn index(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileKind {
    Source,
    Declaration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceRootKind {
    Workspace,
    Library,
    Std,
    Virtual,
}

impl SourceRootKind {
    pub const fn is_read_only(self) -> bool {
        matches!(self, Self::Library | Self::Std)
    }
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
pub enum FileChange {
    SetText {
        file_id: FileId,
        text: Arc<str>,
    },
    SetFile {
        file_id: FileId,
        source_root_id: SourceRootId,
        kind: FileKind,
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

    pub fn is_empty(&self) -> bool {
        self.file_changes.is_empty() && self.source_root_changes.is_empty()
    }

    pub fn file_changes(&self) -> &[FileChange] {
        &self.file_changes
    }

    pub fn source_root_changes(&self) -> &[SourceRootChange] {
        &self.source_root_changes
    }
}

/// The text input store used by the analysis database.
///
/// File contents enter through [`Change`]; this type never reads from disk.
#[derive(Clone, Debug, Default)]
pub struct Vfs {
    file_texts: HashMap<FileId, Arc<str>>,
    file_kinds: HashMap<FileId, FileKind>,
    file_roots: HashMap<FileId, SourceRootId>,
    source_roots: HashMap<SourceRootId, SourceRoot>,
}

impl Vfs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_file_text(&mut self, file_id: FileId, text: impl Into<Arc<str>>) {
        self.file_texts.insert(file_id, text.into());
        self.file_kinds.entry(file_id).or_insert(FileKind::Source);
    }

    pub fn set_file(
        &mut self,
        file_id: FileId,
        source_root_id: SourceRootId,
        kind: FileKind,
        text: impl Into<Arc<str>>,
    ) {
        assert!(
            self.source_roots.contains_key(&source_root_id),
            "cannot add {file_id:?} to unknown source root {source_root_id:?}"
        );

        self.set_file_text(file_id, text);
        self.file_kinds.insert(file_id, kind);

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
        self.file_kinds.remove(&file_id);
        if let Some(source_root_id) = self.file_roots.remove(&file_id)
            && let Some(source_root) = self.source_roots.get_mut(&source_root_id)
        {
            source_root.files.remove(&file_id);
        }
        self.file_texts.remove(&file_id)
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
            self.file_texts.remove(&file_id);
            self.file_kinds.remove(&file_id);
            self.file_roots.remove(&file_id);
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
                    text,
                } => self.set_file(file_id, source_root_id, kind, text),
                FileChange::Remove { file_id } => {
                    self.remove_file(file_id);
                }
            }
        }
    }

    pub fn file_text(&self, file_id: FileId) -> Option<Arc<str>> {
        self.file_texts.get(&file_id).cloned()
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

    pub fn is_file_read_only(&self, file_id: FileId) -> bool {
        self.source_root_id(file_id)
            .and_then(|source_root_id| self.source_root(source_root_id))
            .is_some_and(SourceRoot::is_read_only)
    }
}

#[cfg(test)]
mod tests {
    use super::{Change, FileId, Vfs};

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
}
