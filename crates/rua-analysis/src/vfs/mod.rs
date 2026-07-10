//! In-memory files, source roots, workspace configuration, and changes.
//!
//! This module does not perform filesystem IO. Loaders and protocol adapters
//! translate external state into explicit changes at the crate boundary.

use std::{collections::HashMap, sync::Arc};

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileChange {
    SetText { file_id: FileId, text: Arc<str> },
    Remove { file_id: FileId },
}

/// A batch of input mutations submitted by a loader or protocol adapter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Change {
    file_changes: Vec<FileChange>,
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

    pub fn remove_file(&mut self, file_id: FileId) {
        self.file_changes.push(FileChange::Remove { file_id });
    }

    pub fn is_empty(&self) -> bool {
        self.file_changes.is_empty()
    }

    pub fn file_changes(&self) -> &[FileChange] {
        &self.file_changes
    }
}

/// The text input store used by the analysis database.
///
/// File contents enter through [`Change`]; this type never reads from disk.
#[derive(Clone, Debug, Default)]
pub struct Vfs {
    file_texts: HashMap<FileId, Arc<str>>,
}

impl Vfs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_file_text(&mut self, file_id: FileId, text: impl Into<Arc<str>>) {
        self.file_texts.insert(file_id, text.into());
    }

    pub fn remove_file(&mut self, file_id: FileId) -> Option<Arc<str>> {
        self.file_texts.remove(&file_id)
    }

    pub fn apply_change(&mut self, change: Change) {
        for file_change in change.file_changes {
            match file_change {
                FileChange::SetText { file_id, text } => {
                    self.set_file_text(file_id, text);
                }
                FileChange::Remove { file_id } => {
                    self.remove_file(file_id);
                }
            }
        }
    }

    pub fn file_text(&self, file_id: FileId) -> Option<Arc<str>> {
        self.file_texts.get(&file_id).cloned()
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
