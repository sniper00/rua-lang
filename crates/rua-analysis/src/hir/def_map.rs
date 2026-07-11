//! Stable definition and module indices for one explicit project context.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::{
    BaseDb,
    base::{FileRange, TextRange},
    hir::{
        Import, ItemKind, ItemSignature, ItemSourceKind, ItemTreeItem, ModuleKind,
        SignatureFingerprint, Visibility,
        module_resolution::{resolve_module_file_at, resolve_module_file_in_project_at},
    },
    vfs::{FileId, FileKind, ProjectId, VfsPath},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleId(u32);

impl ModuleId {
    pub(crate) const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn index(self) -> u32 {
        self.0
    }
}

/// Session-stable definition identity. Values are append-only and never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DefId(u32);

impl DefId {
    pub(crate) const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn index(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MemberId(DefId);

impl MemberId {
    pub const fn def_id(self) -> DefId {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DefLoc {
    context: IdentityContext,
    file_id: FileId,
    structural_path: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum IdentityContext {
    Implicit(FileId),
    Project(ProjectId),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ModuleLoc {
    Root {
        context: IdentityContext,
        root_file: FileId,
    },
    Child {
        context: IdentityContext,
        parent: ModuleId,
        name: String,
        occurrence: u32,
    },
}

/// Shared by all COW snapshots belonging to one AnalysisHost session.
#[derive(Debug, Default)]
pub(crate) struct IdentityInterner {
    definitions: HashMap<DefLoc, DefId>,
    definition_locations: Vec<DefLoc>,
    modules: HashMap<ModuleLoc, ModuleId>,
}

impl IdentityInterner {
    pub(crate) fn intern_definition(
        &mut self,
        context: IdentityContext,
        file_id: FileId,
        structural_path: &str,
    ) -> DefId {
        let location = DefLoc {
            context,
            file_id,
            structural_path: structural_path.to_string(),
        };
        if let Some(id) = self.definitions.get(&location) {
            return *id;
        }
        let raw =
            u32::try_from(self.definitions.len()).expect("definition identity space exhausted");
        let id = DefId::new(raw);
        self.definitions.insert(location.clone(), id);
        self.definition_locations.push(location);
        id
    }

    pub(crate) fn definition_location(&self, id: DefId) -> Option<(IdentityContext, FileId)> {
        self.definition_locations
            .get(id.index() as usize)
            .map(|location| (location.context, location.file_id))
    }

    pub(crate) fn intern_root_module(
        &mut self,
        context: IdentityContext,
        root_file: FileId,
    ) -> ModuleId {
        self.intern_module(ModuleLoc::Root { context, root_file })
    }

    pub(crate) fn intern_child_module(
        &mut self,
        context: IdentityContext,
        parent: ModuleId,
        name: &str,
        occurrence: u32,
    ) -> ModuleId {
        self.intern_module(ModuleLoc::Child {
            context,
            parent,
            name: name.to_string(),
            occurrence,
        })
    }

    fn intern_module(&mut self, location: ModuleLoc) -> ModuleId {
        if let Some(id) = self.modules.get(&location) {
            return *id;
        }
        let raw = u32::try_from(self.modules.len()).expect("module identity space exhausted");
        let id = ModuleId::new(raw);
        self.modules.insert(location, id);
        id
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DefKind {
    Function,
    Struct,
    Field,
    Enum,
    Variant,
    Trait,
    Impl,
    Method,
    ExternFunction,
    Module,
    TypeAlias,
}

impl DefKind {
    pub const fn is_member(self) -> bool {
        matches!(self, Self::Field | Self::Variant | Self::Method)
    }

    const fn binds_in_module(self) -> bool {
        !matches!(
            self,
            Self::Field | Self::Variant | Self::Impl | Self::Method
        )
    }

    const fn path_tag(self) -> &'static str {
        match self {
            Self::Function => "fn",
            Self::Struct => "struct",
            Self::Field => "field",
            Self::Enum => "enum",
            Self::Variant => "variant",
            Self::Trait => "trait",
            Self::Impl => "impl",
            Self::Method => "method",
            Self::ExternFunction => "extern_fn",
            Self::Module => "module",
            Self::TypeAlias => "type",
        }
    }
}

impl From<ItemKind> for DefKind {
    fn from(kind: ItemKind) -> Self {
        match kind {
            ItemKind::Function => Self::Function,
            ItemKind::Struct => Self::Struct,
            ItemKind::Field => Self::Field,
            ItemKind::Enum => Self::Enum,
            ItemKind::Variant => Self::Variant,
            ItemKind::Trait => Self::Trait,
            ItemKind::Impl => Self::Impl,
            ItemKind::Method => Self::Method,
            ItemKind::ExternFunction => Self::ExternFunction,
            ItemKind::Module => Self::Module,
            ItemKind::TypeAlias => Self::TypeAlias,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DefinitionSourceKind {
    file_kind: FileKind,
    item_kind: ItemSourceKind,
}

impl DefinitionSourceKind {
    pub const fn file_kind(self) -> FileKind {
        self.file_kind
    }

    pub const fn item_kind(self) -> ItemSourceKind {
        self.item_kind
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DefinitionSource {
    full_range: FileRange,
    name_range: FileRange,
}

impl DefinitionSource {
    pub const fn full_range(self) -> FileRange {
        self.full_range
    }

    pub const fn name_range(self) -> FileRange {
        self.name_range
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Definition {
    id: DefId,
    module_id: ModuleId,
    owner: Option<DefId>,
    name: String,
    kind: DefKind,
    source: DefinitionSource,
    source_kind: DefinitionSourceKind,
    visibility: Visibility,
    signature: ItemSignature,
    signature_fingerprint: SignatureFingerprint,
    target_module: Option<ModuleId>,
}

impl Definition {
    pub const fn id(&self) -> DefId {
        self.id
    }

    pub const fn member_id(&self) -> Option<MemberId> {
        if self.kind.is_member() {
            Some(MemberId(self.id))
        } else {
            None
        }
    }

    pub const fn module_id(&self) -> ModuleId {
        self.module_id
    }

    pub const fn owner(&self) -> Option<DefId> {
        self.owner
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn kind(&self) -> DefKind {
        self.kind
    }

    pub const fn file_id(&self) -> FileId {
        self.source.full_range.file_id
    }

    pub const fn range(&self) -> TextRange {
        self.source.full_range.range
    }

    pub const fn name_range(&self) -> TextRange {
        self.source.name_range.range
    }

    pub const fn source(&self) -> DefinitionSource {
        self.source
    }

    pub const fn source_kind(&self) -> DefinitionSourceKind {
        self.source_kind
    }

    pub const fn visibility(&self) -> Visibility {
        self.visibility
    }

    pub fn signature(&self) -> &ItemSignature {
        &self.signature
    }

    pub const fn signature_fingerprint(&self) -> SignatureFingerprint {
        self.signature_fingerprint
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
    resolution_directory: Option<VfsPath>,
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

    pub fn resolution_directory(&self) -> Option<&VfsPath> {
        self.resolution_directory.as_ref()
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
    project_id: Option<ProjectId>,
    root_file: FileId,
    root: ModuleId,
    modules: Vec<ModuleData>,
    module_slots: HashMap<ModuleId, usize>,
    definitions: Vec<Definition>,
    definition_slots: HashMap<DefId, usize>,
    members: BTreeMap<DefId, Vec<DefId>>,
    source_map: BTreeMap<DefId, DefinitionSource>,
}

impl DefMap {
    pub(crate) fn build(db: &BaseDb, root_file: FileId) -> Self {
        Self::build_inner(db, None, root_file)
    }

    pub(crate) fn build_project(db: &BaseDb, project_id: ProjectId) -> Option<Self> {
        let project = db.project(project_id)?;
        let root_file = project.root_file();
        let root_id = db.source_root_id(root_file)?;
        let root_path = db.file_path(root_file)?;
        if db.file_text(root_file).is_none()
            || !project.workspace_roots().iter().any(|root| {
                root.source_root_id() == root_id
                    && root_path.strip_prefix(root.logical_base()).is_some()
            })
        {
            return None;
        }
        Some(Self::build_inner(db, Some(project_id), root_file))
    }

    fn build_inner(db: &BaseDb, project_id: Option<ProjectId>, root_file: FileId) -> Self {
        let context = project_id.map_or(
            IdentityContext::Implicit(root_file),
            IdentityContext::Project,
        );
        let root = db.intern_root_module(context, root_file);
        let map = Self {
            project_id,
            root_file,
            root,
            modules: vec![ModuleData {
                id: root,
                parent: None,
                name: None,
                file_id: Some(root_file),
                resolution_directory: match project_id {
                    Some(project_id) => {
                        crate::hir::module_resolution::project_file_logical_directory(
                            db, project_id, root_file,
                        )
                    }
                    None => db.file_path(root_file).and_then(VfsPath::parent),
                },
                definitions: BTreeMap::new(),
                children: BTreeMap::new(),
            }],
            module_slots: HashMap::from([(root, 0)]),
            definitions: Vec::new(),
            definition_slots: HashMap::new(),
            members: BTreeMap::new(),
            source_map: BTreeMap::new(),
        };
        let mut builder = DefMapBuilder {
            db,
            project_id,
            context,
            map,
            active_files: HashSet::new(),
            lowered_files: HashSet::new(),
            occurrences: HashMap::new(),
            module_occurrences: HashMap::new(),
            pending_imports: Vec::new(),
        };
        builder.lower_file(root, root_file);
        builder.resolve_imports();
        builder.map
    }

    pub const fn project_id(&self) -> Option<ProjectId> {
        self.project_id
    }

    pub const fn root_file(&self) -> FileId {
        self.root_file
    }

    pub const fn root(&self) -> ModuleId {
        self.root
    }

    pub fn module(&self, module_id: ModuleId) -> Option<&ModuleData> {
        self.module_slots
            .get(&module_id)
            .and_then(|slot| self.modules.get(*slot))
    }

    fn module_mut(&mut self, module_id: ModuleId) -> Option<&mut ModuleData> {
        let slot = *self.module_slots.get(&module_id)?;
        self.modules.get_mut(slot)
    }

    pub fn definition(&self, def_id: DefId) -> Option<&Definition> {
        self.definition_slots
            .get(&def_id)
            .and_then(|slot| self.definitions.get(*slot))
    }

    pub fn definition_source(&self, def_id: DefId) -> Option<DefinitionSource> {
        self.source_map.get(&def_id).copied()
    }

    pub fn modules(&self) -> impl Iterator<Item = &ModuleData> {
        self.modules.iter()
    }

    pub fn definitions(&self) -> impl Iterator<Item = &Definition> {
        self.definitions.iter()
    }

    pub fn member_ids(&self, owner: DefId) -> &[DefId] {
        self.members.get(&owner).map_or(&[], Vec::as_slice)
    }

    pub fn members(&self, owner: DefId) -> impl Iterator<Item = &Definition> {
        self.member_ids(owner)
            .iter()
            .filter_map(|id| self.definition(*id))
    }

    pub fn resolve_member(&self, owner: DefId, name: &str) -> Option<&Definition> {
        self.members(owner).find(|member| member.name() == name)
    }

    pub fn module_for_file(&self, file_id: FileId) -> Option<ModuleId> {
        self.modules()
            .find(|module| {
                module.file_id() == Some(file_id)
                    && !self.definitions().any(|definition| {
                        definition.target_module() == Some(module.id())
                            && definition.file_id() == file_id
                    })
            })
            .map(ModuleData::id)
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

    pub fn resolve_path_lexical(
        &self,
        mut start: ModuleId,
        segments: &[&str],
    ) -> Option<&Definition> {
        let (&first, remaining) = segments.split_first()?;
        let mut definition = loop {
            if let Some(definition) = self.resolve_name(start, first) {
                break definition;
            }
            start = self.module(start)?.parent()?;
        };
        for segment in remaining {
            definition = self.resolve_name(definition.target_module()?, segment)?;
        }
        Some(definition)
    }
}

struct DefMapBuilder<'db> {
    db: &'db BaseDb,
    project_id: Option<ProjectId>,
    context: IdentityContext,
    map: DefMap,
    active_files: HashSet<FileId>,
    lowered_files: HashSet<FileId>,
    occurrences: HashMap<(FileId, String), u32>,
    module_occurrences: HashMap<(ModuleId, String), u32>,
    pending_imports: Vec<(ModuleId, Import)>,
}

impl DefMapBuilder<'_> {
    fn lower_file(&mut self, module_id: ModuleId, file_id: FileId) {
        if !self.active_files.insert(file_id) || !self.lowered_files.insert(file_id) {
            return;
        }
        let item_tree = self.db.item_tree(file_id);
        self.lower_items(module_id, file_id, item_tree.items(), "");
        self.pending_imports.extend(
            item_tree
                .imports()
                .iter()
                .cloned()
                .map(|import| (module_id, import)),
        );
        self.active_files.remove(&file_id);
    }

    fn lower_items(
        &mut self,
        module_id: ModuleId,
        file_id: FileId,
        items: &[ItemTreeItem],
        scope_path: &str,
    ) {
        for item in items {
            if item.kind() != ItemKind::Module {
                let (definition, path) =
                    self.add_definition(module_id, file_id, item, None, None, scope_path);
                self.lower_members(module_id, file_id, definition, &path, item.children());
                continue;
            }

            let target_file = match item.module_kind() {
                Some(ModuleKind::Inline) => Some(file_id),
                Some(ModuleKind::File) => self.resolve_module(module_id, file_id, item.name()),
                None => None,
            };
            let child_directory = self
                .map
                .module(module_id)
                .and_then(ModuleData::resolution_directory)
                .map(|directory| directory.join(item.name()));
            let child_module =
                self.add_module(module_id, item.name(), target_file, child_directory);
            let (module_definition, path) = self.add_definition(
                module_id,
                file_id,
                item,
                Some(child_module),
                None,
                scope_path,
            );

            match item.module_kind() {
                Some(ModuleKind::Inline) => {
                    self.lower_items(child_module, file_id, item.children(), &path);
                    self.pending_imports.extend(
                        item.imports()
                            .iter()
                            .cloned()
                            .map(|import| (child_module, import)),
                    );
                    let _ = module_definition;
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

    fn lower_members(
        &mut self,
        module_id: ModuleId,
        file_id: FileId,
        owner: DefId,
        owner_path: &str,
        members: &[ItemTreeItem],
    ) {
        for member in members {
            let (member_id, path) =
                self.add_definition(module_id, file_id, member, None, Some(owner), owner_path);
            self.lower_members(module_id, file_id, member_id, &path, member.children());
        }
    }

    fn resolve_module(&self, module_id: ModuleId, file_id: FileId, name: &str) -> Option<FileId> {
        let directory = self.map.module(module_id)?.resolution_directory()?;
        match self.project_id {
            Some(project_id) => {
                resolve_module_file_in_project_at(self.db, project_id, file_id, directory, name)
            }
            None => resolve_module_file_at(self.db, file_id, directory, name),
        }
    }

    fn add_module(
        &mut self,
        parent: ModuleId,
        name: &str,
        file_id: Option<FileId>,
        resolution_directory: Option<VfsPath>,
    ) -> ModuleId {
        let occurrence = self
            .module_occurrences
            .entry((parent, name.to_string()))
            .or_default();
        let module_id = self
            .db
            .intern_child_module(self.context, parent, name, *occurrence);
        *occurrence += 1;
        let slot = self.map.modules.len();
        self.map.modules.push(ModuleData {
            id: module_id,
            parent: Some(parent),
            name: Some(name.to_string()),
            file_id,
            resolution_directory,
            definitions: BTreeMap::new(),
            children: BTreeMap::new(),
        });
        self.map.module_slots.insert(module_id, slot);
        self.map
            .module_mut(parent)
            .expect("parent module belongs to this DefMap")
            .children
            .entry(name.to_string())
            .or_insert(module_id);
        module_id
    }

    #[allow(clippy::too_many_arguments)]
    fn add_definition(
        &mut self,
        module_id: ModuleId,
        file_id: FileId,
        item: &ItemTreeItem,
        target_module: Option<ModuleId>,
        owner: Option<DefId>,
        scope_path: &str,
    ) -> (DefId, String) {
        let kind = DefKind::from(item.kind());
        let base_path = if scope_path.is_empty() {
            format!("{}:{}", kind.path_tag(), item.name())
        } else {
            format!("{scope_path}/{}:{}", kind.path_tag(), item.name())
        };
        let occurrence = self
            .occurrences
            .entry((file_id, base_path.clone()))
            .or_default();
        let structural_path = format!("{base_path}#{occurrence}");
        *occurrence += 1;
        let def_id = self
            .db
            .intern_definition(self.context, file_id, &structural_path);
        let file_kind = self.db.file_kind(file_id).unwrap_or(FileKind::Source);
        let source = DefinitionSource {
            full_range: FileRange::new(file_id, item.range()),
            name_range: FileRange::new(file_id, item.name_range()),
        };
        let definition = Definition {
            id: def_id,
            module_id,
            owner,
            name: item.name().to_string(),
            kind,
            source,
            source_kind: DefinitionSourceKind {
                file_kind,
                item_kind: item.source_kind(),
            },
            visibility: item.visibility(),
            signature: item.signature().clone(),
            signature_fingerprint: item.signature_fingerprint().with_file_kind(file_kind),
            target_module,
        };
        let slot = self.map.definitions.len();
        let previous = self.map.definition_slots.insert(def_id, slot);
        assert!(
            previous.is_none(),
            "duplicate definition identity in one DefMap"
        );
        self.map.definitions.push(definition);
        self.map.source_map.insert(def_id, source);
        if let Some(owner) = owner {
            self.map.members.entry(owner).or_default().push(def_id);
        } else if kind.binds_in_module() {
            self.map
                .module_mut(module_id)
                .expect("definition module belongs to this DefMap")
                .definitions
                .entry(item.name().to_string())
                .or_default()
                .push(def_id);
        }
        (def_id, structural_path)
    }

    fn resolve_imports(&mut self) {
        let bindings: Vec<_> = self
            .pending_imports
            .iter()
            .filter_map(|(module_id, import)| {
                let segments: Vec<_> = import.path().iter().map(String::as_str).collect();
                let definition = self.map.resolve_path_lexical(*module_id, &segments)?;
                Some((
                    *module_id,
                    import.binding_name()?.to_string(),
                    definition.id(),
                ))
            })
            .collect();
        for (module_id, name, def_id) in bindings {
            self.map
                .module_mut(module_id)
                .expect("import module belongs to this DefMap")
                .definitions
                .entry(name)
                .or_default()
                .push(def_id);
        }
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

        let math = map.resolve_path(root, &["math"]).expect("file module");
        let math_module = math.target_module().expect("math module target");
        assert_eq!(
            map.module(math_module).expect("math data").file_id(),
            Some(math_id)
        );
        assert_eq!(
            map.resolve_path(root, &["math", "Number"])
                .expect("enum in file module")
                .kind(),
            DefKind::Enum
        );
    }

    #[test]
    fn def_map_lowers_members_with_owner_and_source() {
        let root_id = SourceRootId::new(0);
        let file_id = FileId::new(0);
        let mut change = Change::new();
        change.set_source_root(root_id, SourceRootKind::Workspace);
        change.set_file_with_path(
            file_id,
            root_id,
            FileKind::Source,
            "main.rua",
            "struct Point { x: i64 } enum State { Ready } impl Point { fn x(&self) -> i64 { self.x } }",
        );
        let mut host = AnalysisHost::new();
        host.apply_change(change);
        let map = host.analysis().def_map(file_id);

        let point = map.resolve_name(map.root(), "Point").expect("Point");
        let field = map.resolve_member(point.id(), "x").expect("field");
        assert_eq!(field.kind(), DefKind::Field);
        assert_eq!(field.owner(), Some(point.id()));
        assert_eq!(map.definition_source(field.id()), Some(field.source()));
        assert!(field.member_id().is_some());

        let state = map.resolve_name(map.root(), "State").expect("State");
        assert_eq!(
            map.resolve_member(state.id(), "Ready")
                .expect("variant")
                .kind(),
            DefKind::Variant
        );
        assert!(
            map.definitions()
                .any(|definition| definition.kind() == DefKind::Impl)
        );
        assert!(
            map.definitions()
                .any(|definition| definition.kind() == DefKind::Method)
        );
    }
}
