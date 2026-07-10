//! Root input database and hand-written per-file caches.

use std::{cell::RefCell, collections::HashMap, sync::Arc};

use rua_syntax::{Parse, ast::SourceFile, parse_source_file};

use crate::{
    hir::{DefMap, ItemTree},
    vfs::{
        Change, FileId, FileKind, SourceRoot, SourceRootChange, SourceRootId, SourceRootKind, Vfs,
        VfsPath,
    },
};

/// In-memory analysis inputs and their derived per-file data.
#[derive(Clone, Debug, Default)]
pub struct BaseDb {
    vfs: Vfs,
    parse_cache: RefCell<HashMap<FileId, Arc<Parse<SourceFile>>>>,
    item_tree_cache: RefCell<HashMap<FileId, Arc<ItemTree>>>,
    def_map_cache: RefCell<HashMap<FileId, Arc<DefMap>>>,
}

impl BaseDb {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_file_text(&mut self, file_id: FileId, text: impl Into<Arc<str>>) {
        self.vfs.set_file_text(file_id, text);
        self.invalidate_file(file_id);
    }

    pub fn remove_file(&mut self, file_id: FileId) {
        self.vfs.remove_file(file_id);
        self.invalidate_file(file_id);
    }

    pub fn apply_change(&mut self, change: Change) {
        for source_root_change in change.source_root_changes() {
            let SourceRootChange::Remove { source_root_id } = source_root_change else {
                continue;
            };
            if let Some(source_root) = self.vfs.source_root(*source_root_id) {
                let file_ids: Vec<_> = source_root.files().collect();
                for file_id in file_ids {
                    self.invalidate_file(file_id);
                }
            }
        }
        for file_change in change.file_changes() {
            self.invalidate_file(file_change.file_id());
        }
        self.vfs.apply_change(change);
    }

    pub fn file_text(&self, file_id: FileId) -> Option<Arc<str>> {
        self.vfs.file_text(file_id)
    }

    pub fn file_kind(&self, file_id: FileId) -> Option<FileKind> {
        self.vfs.file_kind(file_id)
    }

    pub fn source_root_id(&self, file_id: FileId) -> Option<SourceRootId> {
        self.vfs.source_root_id(file_id)
    }

    pub fn source_root(&self, source_root_id: SourceRootId) -> Option<&SourceRoot> {
        self.vfs.source_root(source_root_id)
    }

    pub fn source_root_kind(&self, file_id: FileId) -> Option<SourceRootKind> {
        self.source_root_id(file_id)
            .and_then(|source_root_id| self.source_root(source_root_id))
            .map(SourceRoot::kind)
    }

    pub fn is_file_read_only(&self, file_id: FileId) -> bool {
        self.vfs.is_file_read_only(file_id)
    }

    pub fn file_path(&self, file_id: FileId) -> Option<&VfsPath> {
        self.vfs.file_path(file_id)
    }

    pub(crate) fn source_roots(&self) -> impl Iterator<Item = (SourceRootId, &SourceRoot)> {
        self.vfs.source_roots()
    }

    pub(crate) fn file_for_path_in_root(
        &self,
        path: &VfsPath,
        source_root_id: SourceRootId,
    ) -> Option<FileId> {
        self.vfs.file_for_path_in_root(path, source_root_id)
    }

    fn invalidate_file(&mut self, file_id: FileId) {
        self.parse_cache.get_mut().remove(&file_id);
        self.item_tree_cache.get_mut().remove(&file_id);
        self.def_map_cache.get_mut().clear();
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

    pub fn item_tree(&self, file_id: FileId) -> Arc<ItemTree> {
        if let Some(item_tree) = self.item_tree_cache.borrow().get(&file_id).cloned() {
            return item_tree;
        }

        let parse = self.parse(file_id);
        let item_tree = Arc::new(ItemTree::lower(parse.tree()));
        self.item_tree_cache
            .borrow_mut()
            .insert(file_id, item_tree.clone());
        item_tree
    }

    pub fn def_map(&self, root_file: FileId) -> Arc<DefMap> {
        if let Some(def_map) = self.def_map_cache.borrow().get(&root_file).cloned() {
            return def_map;
        }

        let def_map = Arc::new(DefMap::build(self, root_file));
        self.def_map_cache
            .borrow_mut()
            .insert(root_file, def_map.clone());
        def_map
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::BaseDb;
    use crate::vfs::{Change, FileId, FileKind, SourceRootId, SourceRootKind};

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

    #[test]
    fn item_tree_cache_invalidates_only_the_changed_file() {
        let changed_file = FileId::new(0);
        let unchanged_file = FileId::new(1);
        let mut db = BaseDb::new();
        db.set_file_text(changed_file, "fn before() {}");
        db.set_file_text(unchanged_file, "struct Stable {}");
        let changed_before = db.item_tree(changed_file);
        let unchanged_before = db.item_tree(unchanged_file);

        assert!(Arc::ptr_eq(&changed_before, &db.item_tree(changed_file)));
        db.set_file_text(changed_file, "fn after() {}");
        let changed_after = db.item_tree(changed_file);
        let unchanged_after = db.item_tree(unchanged_file);

        assert!(!Arc::ptr_eq(&changed_before, &changed_after));
        assert!(Arc::ptr_eq(&unchanged_before, &unchanged_after));
        assert_eq!(changed_after.items()[0].name(), "after");
    }

    #[test]
    fn def_map_cache_invalidates_when_a_module_dependency_changes() {
        let root_id = SourceRootId::new(0);
        let main_id = FileId::new(0);
        let child_id = FileId::new(1);
        let mut initial = Change::new();
        initial.set_source_root(root_id, SourceRootKind::Workspace);
        initial.set_file_with_path(
            main_id,
            root_id,
            FileKind::Source,
            "src/main.rua",
            "mod child;",
        );
        initial.set_file_with_path(
            child_id,
            root_id,
            FileKind::Source,
            "src/child.rua",
            "fn before() {}",
        );
        let mut db = BaseDb::new();
        db.apply_change(initial);
        let before = db.def_map(main_id);

        assert!(Arc::ptr_eq(&before, &db.def_map(main_id)));
        db.set_file_text(child_id, "fn after() {}");
        let after = db.def_map(main_id);
        let child_before = before
            .resolve_name(before.root(), "child")
            .and_then(|definition| definition.target_module())
            .expect("child module before update");
        let child_after = after
            .resolve_name(after.root(), "child")
            .and_then(|definition| definition.target_module())
            .expect("child module after update");

        assert!(!Arc::ptr_eq(&before, &after));
        assert!(before.resolve_name(child_before, "before").is_some());
        assert!(before.resolve_name(child_before, "after").is_none());
        assert!(after.resolve_name(child_after, "before").is_none());
        assert!(after.resolve_name(child_after, "after").is_some());
    }
}
