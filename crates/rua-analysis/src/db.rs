//! Root input database and hand-written per-file caches.

use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc};

use rua_syntax::{
    AstNode, Parse,
    ast::{FnDecl, SourceFile, TraitMethod},
    parse_source_file,
};

use crate::{
    hir::{
        Body, BodySourceMap, DefId, DefKind, DefMap, IdentityContext, IdentityInterner,
        ItemSourceKind, ItemTree, ModuleId,
        body::{lower_fn_body, lower_trait_method_body},
    },
    vfs::{
        Change, FileId, FileKind, ProjectData, ProjectId, SourceRoot, SourceRootChange,
        SourceRootId, SourceRootKind, Vfs, VfsPath,
    },
};

/// In-memory analysis inputs and their derived per-file data.
#[derive(Clone, Debug, Default)]
pub struct BaseDb {
    vfs: Vfs,
    identity_interner: Rc<RefCell<IdentityInterner>>,
    parse_cache: RefCell<HashMap<FileId, Arc<Parse<SourceFile>>>>,
    item_tree_cache: RefCell<HashMap<FileId, Arc<ItemTree>>>,
    def_map_cache: RefCell<HashMap<DefMapKey, Arc<DefMap>>>,
    body_cache: RefCell<HashMap<DefId, BodyCacheEntry>>,
}

#[derive(Clone, Debug)]
struct BodyCacheEntry {
    file_revision: u64,
    body: Arc<Body>,
    source_map: Arc<BodySourceMap>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum DefMapKey {
    Implicit(FileId),
    Project(ProjectId),
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
        if !change.project_changes().is_empty() || !change.source_root_changes().is_empty() {
            self.def_map_cache.get_mut().clear();
        }
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

    pub(crate) fn file_revision(&self, file_id: FileId) -> Option<u64> {
        self.vfs.file_revision(file_id)
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

    pub fn project(&self, project_id: ProjectId) -> Option<&ProjectData> {
        self.vfs.project(project_id)
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

    pub(crate) fn intern_definition(
        &self,
        context: IdentityContext,
        file_id: FileId,
        structural_path: &str,
    ) -> DefId {
        self.identity_interner
            .borrow_mut()
            .intern_definition(context, file_id, structural_path)
    }

    pub(crate) fn intern_root_module(
        &self,
        context: IdentityContext,
        root_file: FileId,
    ) -> ModuleId {
        self.identity_interner
            .borrow_mut()
            .intern_root_module(context, root_file)
    }

    pub(crate) fn intern_child_module(
        &self,
        context: IdentityContext,
        parent: ModuleId,
        name: &str,
        occurrence: u32,
    ) -> ModuleId {
        self.identity_interner
            .borrow_mut()
            .intern_child_module(context, parent, name, occurrence)
    }

    fn definition_context(&self, def_id: DefId) -> Option<(IdentityContext, FileId)> {
        // Building a DefMap may intern more definitions, so the shared
        // interner borrow must not be held while the map is queried.
        self.identity_interner.borrow().definition_location(def_id)
    }

    pub(crate) fn current_definition_map(&self, def_id: DefId) -> Option<Arc<DefMap>> {
        let (context, file_id) = self.definition_context(def_id)?;
        let map = match context {
            IdentityContext::Implicit(root_file) => {
                self.file_text(root_file)?;
                self.def_map(root_file)
            }
            IdentityContext::Project(project_id) => self.project_def_map(project_id)?,
        };
        map.definition(def_id)
            .is_some_and(|definition| definition.file_id() == file_id)
            .then_some(map)
    }

    pub fn def_map(&self, root_file: FileId) -> Arc<DefMap> {
        let key = DefMapKey::Implicit(root_file);
        if let Some(def_map) = self.def_map_cache.borrow().get(&key).cloned() {
            return def_map;
        }

        let def_map = Arc::new(DefMap::build(self, root_file));
        self.def_map_cache.borrow_mut().insert(key, def_map.clone());
        def_map
    }

    pub fn project_def_map(&self, project_id: ProjectId) -> Option<Arc<DefMap>> {
        let key = DefMapKey::Project(project_id);
        if let Some(def_map) = self.def_map_cache.borrow().get(&key).cloned() {
            return Some(def_map);
        }

        let def_map = Arc::new(DefMap::build_project(self, project_id)?);
        self.def_map_cache.borrow_mut().insert(key, def_map.clone());
        Some(def_map)
    }

    /// Returns the semantic body owned by `def_id` in the current input revision.
    pub fn body(&self, def_id: DefId) -> Option<Arc<Body>> {
        self.body_with_source_map(def_id).map(|(body, _)| body)
    }

    /// Returns the current-revision syntax mapping for a semantic body.
    pub fn body_source_map(&self, def_id: DefId) -> Option<Arc<BodySourceMap>> {
        self.body_with_source_map(def_id)
            .map(|(_, source_map)| source_map)
    }

    pub(crate) fn body_with_source_map(
        &self,
        def_id: DefId,
    ) -> Option<(Arc<Body>, Arc<BodySourceMap>)> {
        let Some(map) = self.current_definition_map(def_id) else {
            self.body_cache.borrow_mut().remove(&def_id);
            return None;
        };
        let definition = map.definition(def_id)?.clone();
        let file_id = definition.file_id();
        let file_revision = self.file_revision(file_id)?;

        if let Some(cached) = self.body_cache.borrow().get(&def_id)
            && cached.file_revision == file_revision
        {
            return Some((cached.body.clone(), cached.source_map.clone()));
        }

        let Some((lowered, source_map)) = self.lower_body(def_id, &definition) else {
            self.body_cache.borrow_mut().remove(&def_id);
            return None;
        };
        let body = self
            .body_cache
            .borrow()
            .get(&def_id)
            .filter(|cached| cached.body.as_ref() == &lowered)
            .map(|cached| cached.body.clone())
            .unwrap_or_else(|| Arc::new(lowered));
        let source_map = Arc::new(source_map);
        self.body_cache.borrow_mut().insert(
            def_id,
            BodyCacheEntry {
                file_revision,
                body: body.clone(),
                source_map: source_map.clone(),
            },
        );
        Some((body, source_map))
    }

    fn lower_body(
        &self,
        def_id: DefId,
        definition: &crate::hir::Definition,
    ) -> Option<(Body, BodySourceMap)> {
        let source_kind = definition.source_kind().item_kind();
        let parse = self.parse(definition.file_id());
        match (definition.kind(), source_kind) {
            (DefKind::Function, ItemSourceKind::Definition)
            | (DefKind::Method, ItemSourceKind::ImplMethod) => parse
                .syntax_node()
                .descendants()
                .filter_map(FnDecl::cast)
                .find(|function| syntax_range(function.syntax()) == definition.range())
                .map(|function| lower_fn_body(def_id, definition.file_id(), &function)),
            (DefKind::Method, ItemSourceKind::TraitDefault) => parse
                .syntax_node()
                .descendants()
                .filter_map(TraitMethod::cast)
                .find(|method| syntax_range(method.syntax()) == definition.range())
                .map(|method| lower_trait_method_body(def_id, definition.file_id(), &method)),
            _ => None,
        }
    }
}

fn syntax_range(node: &rua_syntax::SyntaxNode) -> crate::base::TextRange {
    let range = node.text_range();
    crate::base::TextRange::new(range.start().into(), range.end().into())
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
