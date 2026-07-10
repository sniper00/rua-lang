//! Root input database and hand-written per-file caches.

use std::{cell::RefCell, collections::HashMap, sync::Arc};

use rua_syntax::{Parse, ast::SourceFile, parse_source_file};

use crate::vfs::{Change, FileId, Vfs};

/// In-memory analysis inputs and their derived per-file data.
#[derive(Clone, Debug, Default)]
pub struct BaseDb {
    vfs: Vfs,
    parse_cache: RefCell<HashMap<FileId, Arc<Parse<SourceFile>>>>,
}

impl BaseDb {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_file_text(&mut self, file_id: FileId, text: impl Into<Arc<str>>) {
        self.vfs.set_file_text(file_id, text);
        self.parse_cache.get_mut().remove(&file_id);
    }

    pub fn remove_file(&mut self, file_id: FileId) {
        self.vfs.remove_file(file_id);
        self.parse_cache.get_mut().remove(&file_id);
    }

    pub fn apply_change(&mut self, change: Change) {
        for file_change in change.file_changes() {
            self.parse_cache.get_mut().remove(&file_change.file_id());
        }
        self.vfs.apply_change(change);
    }

    pub fn file_text(&self, file_id: FileId) -> Option<Arc<str>> {
        self.vfs.file_text(file_id)
    }

    // Rowan red nodes are thread-local; Arc provides shared cache identity for
    // same-thread database snapshots, not cross-thread transfer.
    #[allow(clippy::arc_with_non_send_sync)]
    pub fn parse(&self, file_id: FileId) -> Arc<Parse<SourceFile>> {
        if let Some(parse) = self.parse_cache.borrow().get(&file_id).cloned() {
            return parse;
        }

        let text = self
            .file_text(file_id)
            .unwrap_or_else(|| panic!("cannot parse unknown file {file_id:?}"));
        let parse = Arc::new(parse_source_file(&text));
        self.parse_cache.borrow_mut().insert(file_id, parse.clone());
        parse
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::BaseDb;
    use crate::vfs::FileId;

    #[test]
    fn parse_cache_reads_current_vfs_text() {
        let file_id = FileId::new(0);
        let mut db = BaseDb::new();
        db.set_file_text(file_id, "fn main() { let value = 42; }");

        let parse = db.parse(file_id);

        assert!(parse.errors().is_empty());
        assert_eq!(
            parse.syntax_node().text().to_string(),
            "fn main() { let value = 42; }"
        );
    }

    #[test]
    fn parse_cache_reuses_unchanged_parse() {
        let file_id = FileId::new(0);
        let mut db = BaseDb::new();
        db.set_file_text(file_id, "fn main() {}");

        let first = db.parse(file_id);
        let second = db.parse(file_id);

        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn parse_cache_invalidates_only_changed_file() {
        let changed_file = FileId::new(0);
        let unchanged_file = FileId::new(1);
        let mut db = BaseDb::new();
        db.set_file_text(changed_file, "fn before() {}");
        db.set_file_text(unchanged_file, "fn stable() {}");
        let changed_before = db.parse(changed_file);
        let unchanged_before = db.parse(unchanged_file);

        db.set_file_text(changed_file, "fn after() {}");
        let changed_after = db.parse(changed_file);
        let unchanged_after = db.parse(unchanged_file);

        assert!(!Arc::ptr_eq(&changed_before, &changed_after));
        assert!(Arc::ptr_eq(&unchanged_before, &unchanged_after));
        assert_eq!(
            changed_after.syntax_node().text().to_string(),
            "fn after() {}"
        );
    }

    #[test]
    fn parse_cache_is_dropped_when_file_is_removed() {
        let file_id = FileId::new(0);
        let mut db = BaseDb::new();
        db.set_file_text(file_id, "fn main() {}");
        let before = db.parse(file_id);

        db.remove_file(file_id);
        assert_eq!(db.file_text(file_id), None);
        db.set_file_text(file_id, "fn main() {}");
        let after = db.parse(file_id);

        assert!(!Arc::ptr_eq(&before, &after));
    }
}
