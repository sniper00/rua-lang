//! Definition and module indices for one workspace entry file.

use std::collections::{BTreeMap, HashSet};

use crate::{
    BaseDb,
    hir::{
        ItemKind, ItemTreeItem, ModuleKind, TextRange, Visibility,
        module_resolution::resolve_module_file,
    },
    vfs::FileId,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleId(u32);

impl ModuleId {
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn index(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DefId(u32);

impl DefId {
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn index(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DefKind {
    Function,
    Struct,
    Enum,
    Trait,
    Module,
    TypeAlias,
}

impl From<ItemKind> for DefKind {
    fn from(kind: ItemKind) -> Self {
        match kind {
            ItemKind::Function => Self::Function,
            ItemKind::Struct => Self::Struct,
            ItemKind::Enum => Self::Enum,
            ItemKind::Trait => Self::Trait,
            ItemKind::Module => Self::Module,
            ItemKind::TypeAlias => Self::TypeAlias,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Definition {
    id: DefId,
    module_id: ModuleId,
    name: String,
    kind: DefKind,
    file_id: FileId,
    range: TextRange,
    name_range: TextRange,
    visibility: Visibility,
    target_module: Option<ModuleId>,
}

impl Definition {
    pub const fn id(&self) -> DefId {
        self.id
    }

    pub const fn module_id(&self) -> ModuleId {
        self.module_id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn kind(&self) -> DefKind {
        self.kind
    }

    pub const fn file_id(&self) -> FileId {
        self.file_id
    }

    pub const fn range(&self) -> TextRange {
        self.range
    }

    pub const fn name_range(&self) -> TextRange {
        self.name_range
    }

    pub const fn visibility(&self) -> Visibility {
        self.visibility
    }

    pub const fn target_module(&self) -> Option<ModuleId> {
        self.target_module
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleData {
    id: ModuleId,
    parent: Option<ModuleId>,
    name: Option<String>,
    file_id: Option<FileId>,
    definitions: BTreeMap<String, Vec<DefId>>,
    children: BTreeMap<String, ModuleId>,
}

impl ModuleData {
    pub const fn id(&self) -> ModuleId {
        self.id
    }

    pub const fn parent(&self) -> Option<ModuleId> {
        self.parent
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub const fn file_id(&self) -> Option<FileId> {
        self.file_id
    }

    pub fn definitions(&self) -> impl Iterator<Item = (&str, &[DefId])> {
        self.definitions
            .iter()
            .map(|(name, definitions)| (name.as_str(), definitions.as_slice()))
    }

    pub fn children(&self) -> impl Iterator<Item = (&str, ModuleId)> + '_ {
        self.children
            .iter()
            .map(|(name, module_id)| (name.as_str(), *module_id))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefMap {
    root: ModuleId,
    modules: Vec<ModuleData>,
    definitions: Vec<Definition>,
}

impl DefMap {
    pub(crate) fn build(db: &BaseDb, root_file: FileId) -> Self {
        let root = ModuleId::new(0);
        let map = Self {
            root,
            modules: vec![ModuleData {
                id: root,
                parent: None,
                name: None,
                file_id: Some(root_file),
                definitions: BTreeMap::new(),
                children: BTreeMap::new(),
            }],
            definitions: Vec::new(),
        };
        let mut builder = DefMapBuilder {
            db,
            map,
            active_files: HashSet::new(),
        };
        builder.lower_file(root, root_file);
        builder.map
    }

    pub const fn root(&self) -> ModuleId {
        self.root
    }

    pub fn module(&self, module_id: ModuleId) -> Option<&ModuleData> {
        self.modules.get(module_id.index() as usize)
    }

    pub fn definition(&self, def_id: DefId) -> Option<&Definition> {
        self.definitions.get(def_id.index() as usize)
    }

    pub fn modules(&self) -> impl Iterator<Item = &ModuleData> {
        self.modules.iter()
    }

    pub fn definitions(&self) -> impl Iterator<Item = &Definition> {
        self.definitions.iter()
    }

    pub fn resolve_name(&self, module_id: ModuleId, name: &str) -> Option<&Definition> {
        let def_id = *self.module(module_id)?.definitions.get(name)?.first()?;
        self.definition(def_id)
    }

    pub fn resolve_path(&self, start: ModuleId, segments: &[&str]) -> Option<&Definition> {
        let (&last, parents) = segments.split_last()?;
        let mut module_id = start;
        for segment in parents {
            module_id = self.resolve_name(module_id, segment)?.target_module()?;
        }
        self.resolve_name(module_id, last)
    }
}

struct DefMapBuilder<'db> {
    db: &'db BaseDb,
    map: DefMap,
    active_files: HashSet<FileId>,
}

impl DefMapBuilder<'_> {
    fn lower_file(&mut self, module_id: ModuleId, file_id: FileId) {
        if !self.active_files.insert(file_id) {
            return;
        }
        let item_tree = self.db.item_tree(file_id);
        self.lower_items(module_id, file_id, item_tree.items());
        self.active_files.remove(&file_id);
    }

    fn lower_items(&mut self, module_id: ModuleId, file_id: FileId, items: &[ItemTreeItem]) {
        for item in items {
            if item.kind() != ItemKind::Module {
                self.add_definition(module_id, file_id, item, None);
                continue;
            }

            let target_file = match item.module_kind() {
                Some(ModuleKind::Inline) => Some(file_id),
                Some(ModuleKind::File) => resolve_module_file(self.db, file_id, item.name()),
                None => None,
            };
            let child_module = self.add_module(module_id, item.name(), target_file);
            self.add_definition(module_id, file_id, item, Some(child_module));

            match item.module_kind() {
                Some(ModuleKind::Inline) => {
                    self.lower_items(child_module, file_id, item.children());
                }
                Some(ModuleKind::File) => {
                    if let Some(target_file) = target_file {
                        self.lower_file(child_module, target_file);
                    }
                }
                None => {}
            }
        }
    }

    fn add_module(&mut self, parent: ModuleId, name: &str, file_id: Option<FileId>) -> ModuleId {
        let module_id = ModuleId::new(self.map.modules.len() as u32);
        self.map.modules.push(ModuleData {
            id: module_id,
            parent: Some(parent),
            name: Some(name.to_string()),
            file_id,
            definitions: BTreeMap::new(),
            children: BTreeMap::new(),
        });
        self.map.modules[parent.index() as usize]
            .children
            .entry(name.to_string())
            .or_insert(module_id);
        module_id
    }

    fn add_definition(
        &mut self,
        module_id: ModuleId,
        file_id: FileId,
        item: &ItemTreeItem,
        target_module: Option<ModuleId>,
    ) -> DefId {
        let def_id = DefId::new(self.map.definitions.len() as u32);
        self.map.definitions.push(Definition {
            id: def_id,
            module_id,
            name: item.name().to_string(),
            kind: item.kind().into(),
            file_id,
            range: item.range(),
            name_range: item.name_range(),
            visibility: item.visibility(),
            target_module,
        });
        self.map.modules[module_id.index() as usize]
            .definitions
            .entry(item.name().to_string())
            .or_default()
            .push(def_id);
        def_id
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        AnalysisHost, Change, DefKind, FileId, FileKind, SourceRootId, SourceRootKind, Visibility,
    };

    fn host_with_module_tree() -> (AnalysisHost, FileId, FileId) {
        let root_id = SourceRootId::new(0);
        let main_id = FileId::new(0);
        let math_id = FileId::new(1);
        let mut change = Change::new();
        change.set_source_root(root_id, SourceRootKind::Workspace);
        change.set_file_with_path(
            main_id,
            root_id,
            FileKind::Source,
            "src/main.rua",
            concat!(
                "pub fn root_fn() {}\n",
                "pub mod inline { pub struct Thing {} fn private_fn() {} }\n",
                "mod math;\n",
            ),
        );
        change.set_file_with_path(
            math_id,
            root_id,
            FileKind::Source,
            "src/math.rua",
            "pub enum Number { One }\n",
        );
        let mut host = AnalysisHost::new();
        host.apply_change(change);
        (host, main_id, math_id)
    }

    #[test]
    fn def_map_builds_inline_and_file_module_tree() {
        let (host, main_id, math_id) = host_with_module_tree();
        let map = host.analysis().def_map(main_id);
        let root = map.root();

        let root_fn = map.resolve_path(root, &["root_fn"]).expect("root function");
        assert_eq!(root_fn.kind(), DefKind::Function);
        assert_eq!(root_fn.visibility(), Visibility::Public);
        assert_eq!(root_fn.file_id(), main_id);

        let inline = map.resolve_name(root, "inline").expect("inline module");
        let inline_module = inline.target_module().expect("inline module target");
        assert_eq!(
            map.module(inline_module).expect("inline data").file_id(),
            Some(main_id)
        );
        assert_eq!(
            map.resolve_path(root, &["inline", "Thing"])
                .expect("inline struct")
                .kind(),
            DefKind::Struct
        );
        assert_eq!(
            map.resolve_path(root, &["inline", "private_fn"])
                .expect("private function")
                .visibility(),
            Visibility::Private
        );

        let math = map.resolve_path(root, &["math"]).expect("file module");
        let math_module = math.target_module().expect("math module target");
        assert_eq!(
            map.module(math_module).expect("math data").file_id(),
            Some(math_id)
        );
        let number = map
            .resolve_path(root, &["math", "Number"])
            .expect("enum in file module");
        assert_eq!(number.kind(), DefKind::Enum);
        assert_eq!(number.file_id(), math_id);
    }

    #[test]
    fn def_map_keeps_unresolved_file_module_node() {
        let root_id = SourceRootId::new(0);
        let main_id = FileId::new(0);
        let mut change = Change::new();
        change.set_source_root(root_id, SourceRootKind::Workspace);
        change.set_file_with_path(
            main_id,
            root_id,
            FileKind::Source,
            "main.rua",
            "pub mod missing;",
        );
        let mut host = AnalysisHost::new();
        host.apply_change(change);

        let map = host.analysis().def_map(main_id);
        let module = map.resolve_name(map.root(), "missing").expect("module def");
        let module_data = map
            .module(module.target_module().expect("module target"))
            .expect("module data");
        assert_eq!(module_data.file_id(), None);
        assert!(module_data.definitions().next().is_none());
    }
}
