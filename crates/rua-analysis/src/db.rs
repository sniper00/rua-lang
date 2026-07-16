//! Root input database and hand-written per-file caches.

use std::{
    cell::RefCell,
    collections::{BTreeSet, HashMap, VecDeque},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
};

use rua_syntax::{
    AstNode, Parse,
    ast::{FnDecl, SourceFile, TraitMethod},
    parse_source_file,
};

use crate::{
    hir::{
        Body, BodyResolution, BodyScopes, BodySourceMap, DefId, DefKind, DefMap, IdentityContext,
        IdentityInterner, IdentityLease, InferenceResult, ItemSourceKind, ItemTree, MemberIndex,
        ModuleId, SignatureFingerprint, StdLibraryIndex,
        body::{lower_chunk_body, lower_fn_body, lower_trait_method_body},
        infer::infer_body,
        standard_library,
    },
    semantic::ReferenceIndex,
    vfs::{
        Change, FileChange, FileId, FileKind, ProjectChange, ProjectData, ProjectId, SourceRoot,
        SourceRootChange, SourceRootId, SourceRootKind, Vfs, VfsPath,
    },
};

/// In-memory analysis inputs and their derived per-file data.
#[derive(Debug)]
pub struct BaseDb {
    session_id: u64,
    vfs: Vfs,
    identity_interner: Arc<Mutex<IdentityInterner>>,
    standard_library: Arc<StdLibraryIndex>,
    item_tree_cache: Mutex<HashMap<FileId, Arc<ItemTree>>>,
    def_map_cache: Mutex<HashMap<DefMapKey, DefMapCacheEntry>>,
    member_index_cache: Mutex<HashMap<DefMapKey, MemberIndexCacheEntry>>,
    reference_index_cache: Mutex<HashMap<DefMapKey, ReferenceIndexCacheEntry>>,
    body_cache: Mutex<HashMap<DefId, BodyCacheEntry>>,
    local_resolution_cache: Mutex<HashMap<DefId, LocalResolutionCacheEntry>>,
    inference_cache: Mutex<HashMap<DefId, InferenceCacheEntry>>,
    query_stats: Mutex<QueryStats>,
}

static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ParseCacheKey {
    session_id: u64,
    file_id: FileId,
    revision: u64,
}

#[derive(Default)]
struct ThreadParseCache {
    entries: HashMap<ParseCacheKey, Arc<Parse<SourceFile>>>,
    insertion_order: VecDeque<ParseCacheKey>,
}

impl ThreadParseCache {
    const CAPACITY: usize = 256;

    fn insert(&mut self, key: ParseCacheKey, parse: Arc<Parse<SourceFile>>) {
        if self.entries.insert(key, parse).is_none() {
            self.insertion_order.push_back(key);
        }
        while self.entries.len() > Self::CAPACITY {
            let Some(oldest) = self.insertion_order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }
}

thread_local! {
    static PARSE_CACHE: RefCell<ThreadParseCache> = RefCell::new(ThreadParseCache::default());
}

impl Default for BaseDb {
    fn default() -> Self {
        Self {
            session_id: NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed),
            vfs: Vfs::default(),
            identity_interner: Arc::new(Mutex::new(IdentityInterner::default())),
            standard_library: Arc::new(
                standard_library()
                    .expect("embedded standard library must be valid")
                    .clone(),
            ),
            item_tree_cache: Mutex::new(HashMap::new()),
            def_map_cache: Mutex::new(HashMap::new()),
            member_index_cache: Mutex::new(HashMap::new()),
            reference_index_cache: Mutex::new(HashMap::new()),
            body_cache: Mutex::new(HashMap::new()),
            local_resolution_cache: Mutex::new(HashMap::new()),
            inference_cache: Mutex::new(HashMap::new()),
            query_stats: Mutex::new(QueryStats::default()),
        }
    }
}

impl Clone for BaseDb {
    fn clone(&self) -> Self {
        Self {
            session_id: self.session_id,
            vfs: self.vfs.clone(),
            identity_interner: Arc::clone(&self.identity_interner),
            standard_library: Arc::clone(&self.standard_library),
            item_tree_cache: Mutex::new(lock(&self.item_tree_cache).clone()),
            def_map_cache: Mutex::new(lock(&self.def_map_cache).clone()),
            member_index_cache: Mutex::new(lock(&self.member_index_cache).clone()),
            reference_index_cache: Mutex::new(lock(&self.reference_index_cache).clone()),
            body_cache: Mutex::new(lock(&self.body_cache).clone()),
            local_resolution_cache: Mutex::new(lock(&self.local_resolution_cache).clone()),
            inference_cache: Mutex::new(lock(&self.inference_cache).clone()),
            query_stats: Mutex::new(*lock(&self.query_stats)),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn get_mut<T>(mutex: &mut Mutex<T>) -> &mut T {
    mutex
        .get_mut()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueryStats {
    pub parse: u64,
    pub item_tree: u64,
    pub def_map: u64,
    pub body: u64,
    pub body_resolution: u64,
    pub inference: u64,
    pub member_index: u64,
    pub reference_index: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheSizes {
    pub parse: usize,
    pub item_tree: usize,
    pub def_map: usize,
    pub body: usize,
    pub body_resolution: usize,
    pub inference: usize,
    pub member_index: usize,
    pub reference_index: usize,
    pub definition_identities: usize,
    pub module_identities: usize,
}

#[derive(Clone, Debug)]
struct DefMapCacheEntry {
    map: Arc<DefMap>,
    dependencies: Vec<DeclarationDependency>,
}

#[derive(Clone, Debug)]
struct DeclarationDependency {
    file_id: FileId,
    kind: FileKind,
    fingerprint: SignatureFingerprint,
    item_tree: Arc<ItemTree>,
}

#[derive(Clone, Debug)]
struct BodyCacheEntry {
    file_revision: u64,
    body: Arc<Body>,
    source_map: Arc<BodySourceMap>,
}

#[derive(Clone, Debug)]
struct LocalResolutionCacheEntry {
    body: Arc<Body>,
    scopes: Arc<BodyScopes>,
    resolution: Arc<BodyResolution>,
}

#[derive(Clone, Debug)]
struct InferenceCacheEntry {
    body: Arc<Body>,
    resolution: Arc<BodyResolution>,
    def_map: Arc<DefMap>,
    member_index: Arc<MemberIndex>,
    result: Arc<InferenceResult>,
}

#[derive(Clone, Debug)]
struct MemberIndexCacheEntry {
    def_map: Arc<DefMap>,
    index: Arc<MemberIndex>,
}

#[derive(Clone, Debug)]
struct ReferenceIndexCacheEntry {
    def_map: Arc<DefMap>,
    index: Arc<ReferenceIndex>,
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

    pub fn set_standard_library(&mut self, library: Arc<StdLibraryIndex>) {
        if Arc::ptr_eq(&self.standard_library, &library) {
            return;
        }
        self.standard_library = library;
        get_mut(&mut self.member_index_cache).clear();
        get_mut(&mut self.inference_cache).clear();
    }

    pub fn standard_library(&self) -> Arc<StdLibraryIndex> {
        Arc::clone(&self.standard_library)
    }

    pub fn set_file_text(&mut self, file_id: FileId, text: impl Into<Arc<str>>) {
        let text = text.into();
        if self.file_text(file_id).as_deref() == Some(text.as_ref()) {
            return;
        }
        self.vfs.set_file_text(file_id, text);
        self.invalidate_file_text(file_id);
    }

    pub fn remove_file(&mut self, file_id: FileId) {
        self.vfs.remove_file(file_id);
        self.invalidate_file_structure(file_id);
        self.remove_semantic_caches_for_file(file_id);
    }

    pub fn apply_change(&mut self, change: Change) {
        for project_change in change.project_changes() {
            if let ProjectChange::Remove { project_id } = project_change {
                self.remove_semantic_caches_for_context(IdentityContext::Project(*project_id));
            }
        }
        let topology_changed =
            change
                .source_root_changes()
                .iter()
                .any(|root_change| match root_change {
                    SourceRootChange::Set {
                        source_root_id,
                        kind,
                    } => self
                        .vfs
                        .source_root(*source_root_id)
                        .is_none_or(|root| root.kind() != *kind),
                    SourceRootChange::Remove { source_root_id } => {
                        self.vfs.source_root(*source_root_id).is_some()
                    }
                })
                || change
                    .project_changes()
                    .iter()
                    .any(|project_change| match project_change {
                        ProjectChange::Set { project_id, data } => {
                            self.vfs.project(*project_id) != Some(data)
                        }
                        ProjectChange::Remove { project_id } => {
                            self.vfs.project(*project_id).is_some()
                        }
                    });
        if topology_changed {
            self.clear_project_caches();
        }
        for source_root_change in change.source_root_changes() {
            let SourceRootChange::Remove { source_root_id } = source_root_change else {
                continue;
            };
            if let Some(source_root) = self.vfs.source_root(*source_root_id) {
                let file_ids: Vec<_> = source_root.files().collect();
                for file_id in file_ids {
                    self.invalidate_file_structure(file_id);
                    self.remove_semantic_caches_for_file(file_id);
                }
            }
        }
        for file_change in change.file_changes() {
            let file_id = file_change.file_id();
            match file_change {
                FileChange::SetText { text, .. } => {
                    if self.file_text(file_id).as_deref() != Some(text.as_ref()) {
                        self.invalidate_file_text(file_id);
                    }
                }
                FileChange::SetFile {
                    source_root_id,
                    kind,
                    path,
                    text,
                    ..
                } => {
                    let metadata_changed = self.source_root_id(file_id) != Some(*source_root_id)
                        || self.file_kind(file_id) != Some(*kind)
                        || self.file_path(file_id) != path.as_ref();
                    let text_changed = self.file_text(file_id).as_deref() != Some(text.as_ref());
                    if metadata_changed {
                        self.invalidate_file_structure(file_id);
                    } else if text_changed {
                        self.invalidate_file_text(file_id);
                    }
                }
                FileChange::Remove { .. } => {
                    self.invalidate_file_structure(file_id);
                    self.remove_semantic_caches_for_file(file_id);
                }
            }
        }
        self.vfs.apply_change(change);
    }

    pub fn file_text(&self, file_id: FileId) -> Option<Arc<str>> {
        self.vfs.file_text(file_id)
    }

    pub fn file_revision(&self, file_id: FileId) -> Option<u64> {
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

    pub fn query_stats(&self) -> QueryStats {
        *lock(&self.query_stats)
    }

    pub fn cache_sizes(&self) -> CacheSizes {
        let parse = PARSE_CACHE.with(|cache| {
            cache
                .borrow()
                .entries
                .keys()
                .filter(|key| key.session_id == self.session_id)
                .count()
        });
        let (definition_identities, module_identities) =
            lock(&self.identity_interner).active_sizes();
        CacheSizes {
            parse,
            item_tree: lock(&self.item_tree_cache).len(),
            def_map: lock(&self.def_map_cache).len(),
            body: lock(&self.body_cache).len(),
            body_resolution: lock(&self.local_resolution_cache).len(),
            inference: lock(&self.inference_cache).len(),
            member_index: lock(&self.member_index_cache).len(),
            reference_index: lock(&self.reference_index_cache).len(),
            definition_identities,
            module_identities,
        }
    }

    pub fn file_path(&self, file_id: FileId) -> Option<&VfsPath> {
        self.vfs.file_path(file_id)
    }

    fn invalidate_file_text(&mut self, file_id: FileId) {
        get_mut(&mut self.item_tree_cache).remove(&file_id);
        get_mut(&mut self.reference_index_cache).clear();
    }

    fn invalidate_file_structure(&mut self, file_id: FileId) {
        self.invalidate_file_text(file_id);
        self.clear_project_caches();
    }

    fn clear_project_caches(&mut self) {
        get_mut(&mut self.def_map_cache).clear();
        get_mut(&mut self.member_index_cache).clear();
        get_mut(&mut self.reference_index_cache).clear();
    }

    fn remove_semantic_caches_for_file(&mut self, file_id: FileId) {
        let definitions = {
            let interner = lock(&self.identity_interner);
            get_mut(&mut self.body_cache)
                .keys()
                .chain(get_mut(&mut self.local_resolution_cache).keys())
                .chain(get_mut(&mut self.inference_cache).keys())
                .copied()
                .filter(|definition| {
                    interner
                        .definition_location(*definition)
                        .is_some_and(|(_, owner_file)| owner_file == file_id)
                })
                .collect::<BTreeSet<_>>()
        };
        get_mut(&mut self.body_cache).retain(|definition, _| !definitions.contains(definition));
        get_mut(&mut self.local_resolution_cache)
            .retain(|definition, _| !definitions.contains(definition));
        get_mut(&mut self.inference_cache)
            .retain(|definition, _| !definitions.contains(definition));
    }

    fn remove_semantic_caches_for_context(&mut self, context: IdentityContext) {
        let definitions = {
            let interner = lock(&self.identity_interner);
            get_mut(&mut self.body_cache)
                .keys()
                .chain(get_mut(&mut self.local_resolution_cache).keys())
                .chain(get_mut(&mut self.inference_cache).keys())
                .copied()
                .filter(|definition| {
                    interner
                        .definition_location(*definition)
                        .is_some_and(|(owner_context, _)| owner_context == context)
                })
                .collect::<BTreeSet<_>>()
        };
        get_mut(&mut self.body_cache).retain(|definition, _| !definitions.contains(definition));
        get_mut(&mut self.local_resolution_cache)
            .retain(|definition, _| !definitions.contains(definition));
        get_mut(&mut self.inference_cache)
            .retain(|definition, _| !definitions.contains(definition));
    }

    // Rowan red nodes are not Send. Each worker therefore keeps a bounded
    // thread-local cache keyed by immutable database revision.
    #[allow(clippy::arc_with_non_send_sync)]
    pub fn parse(&self, file_id: FileId) -> Arc<Parse<SourceFile>> {
        let key = ParseCacheKey {
            session_id: self.session_id,
            file_id,
            revision: self.file_revision(file_id).unwrap_or(0),
        };
        if let Some(parse) = PARSE_CACHE.with(|cache| cache.borrow().entries.get(&key).cloned()) {
            return parse;
        }

        let text = self
            .file_text(file_id)
            .unwrap_or_else(|| panic!("cannot parse unknown file {file_id:?}"));
        lock(&self.query_stats).parse += 1;
        let parse = Arc::new(parse_source_file(&text));
        PARSE_CACHE.with(|cache| cache.borrow_mut().insert(key, parse.clone()));
        parse
    }

    pub fn item_tree(&self, file_id: FileId) -> Arc<ItemTree> {
        if let Some(item_tree) = lock(&self.item_tree_cache).get(&file_id).cloned() {
            return item_tree;
        }

        let parse = self.parse(file_id);
        lock(&self.query_stats).item_tree += 1;
        let item_tree = Arc::new(ItemTree::lower(parse.tree()));
        lock(&self.item_tree_cache).insert(file_id, item_tree.clone());
        item_tree
    }

    pub(crate) fn intern_definition(
        &self,
        context: IdentityContext,
        file_id: FileId,
        structural_path: &str,
    ) -> DefId {
        lock(&self.identity_interner).intern_definition(context, file_id, structural_path)
    }

    pub(crate) fn lease_identities(
        &self,
        definitions: Vec<DefId>,
        modules: Vec<ModuleId>,
    ) -> Arc<IdentityLease> {
        Arc::new(IdentityLease::new(
            &self.identity_interner,
            definitions,
            modules,
        ))
    }

    pub(crate) fn intern_root_module(
        &self,
        context: IdentityContext,
        root_file: FileId,
    ) -> ModuleId {
        lock(&self.identity_interner).intern_root_module(context, root_file)
    }

    pub(crate) fn intern_child_module(
        &self,
        context: IdentityContext,
        parent: ModuleId,
        name: &str,
        occurrence: u32,
    ) -> ModuleId {
        lock(&self.identity_interner).intern_child_module(context, parent, name, occurrence)
    }

    fn definition_context(&self, def_id: DefId) -> Option<(IdentityContext, FileId)> {
        // Building a DefMap may intern more definitions, so the shared
        // interner borrow must not be held while the map is queried.
        lock(&self.identity_interner).definition_location(def_id)
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
        if let Some(def_map) = self.cached_def_map(key) {
            return def_map;
        }

        lock(&self.query_stats).def_map += 1;
        let def_map = Arc::new(DefMap::build(self, root_file));
        self.cache_def_map(key, def_map.clone());
        def_map
    }

    pub fn project_def_map(&self, project_id: ProjectId) -> Option<Arc<DefMap>> {
        let key = DefMapKey::Project(project_id);
        if let Some(def_map) = self.cached_def_map(key) {
            return Some(def_map);
        }

        lock(&self.query_stats).def_map += 1;
        let def_map = Arc::new(DefMap::build_project(self, project_id)?);
        self.cache_def_map(key, def_map.clone());
        Some(def_map)
    }

    fn cached_def_map(&self, key: DefMapKey) -> Option<Arc<DefMap>> {
        let cached = lock(&self.def_map_cache).get(&key).cloned()?;
        let current = cached.dependencies.iter().all(|dependency| {
            if self.file_text(dependency.file_id).is_none() {
                return false;
            }
            let current_tree = self.item_tree(dependency.file_id);
            self.file_kind(dependency.file_id) == Some(dependency.kind)
                && current_tree.declaration_fingerprint() == dependency.fingerprint
                && current_tree.as_ref() == dependency.item_tree.as_ref()
        });
        if current {
            return Some(cached.map);
        }
        lock(&self.def_map_cache).remove(&key);
        lock(&self.member_index_cache).remove(&key);
        lock(&self.reference_index_cache).remove(&key);
        None
    }

    fn cache_def_map(&self, key: DefMapKey, map: Arc<DefMap>) {
        let mut files = BTreeSet::new();
        files.extend(map.modules().filter_map(|module| module.file_id()));
        files.extend(map.definitions().map(|definition| definition.file_id()));
        let dependencies = files
            .into_iter()
            .filter_map(|file_id| {
                let item_tree = self.item_tree(file_id);
                Some(DeclarationDependency {
                    file_id,
                    kind: self.file_kind(file_id)?,
                    fingerprint: item_tree.declaration_fingerprint(),
                    item_tree,
                })
            })
            .collect();
        lock(&self.def_map_cache).insert(key, DefMapCacheEntry { map, dependencies });
    }

    pub fn member_index(&self, root_file: FileId) -> Arc<MemberIndex> {
        let key = DefMapKey::Implicit(root_file);
        let def_map = self.def_map(root_file);
        self.member_index_for_map(key, def_map)
    }

    pub fn project_member_index(&self, project_id: ProjectId) -> Option<Arc<MemberIndex>> {
        let key = DefMapKey::Project(project_id);
        let def_map = self.project_def_map(project_id)?;
        Some(self.member_index_for_map(key, def_map))
    }

    pub(crate) fn project_reference_index(
        self: &Arc<Self>,
        project_id: ProjectId,
    ) -> Option<Arc<ReferenceIndex>> {
        self.project_reference_index_cancellable(project_id, &mut || false)
    }

    pub(crate) fn project_reference_index_cancellable(
        self: &Arc<Self>,
        project_id: ProjectId,
        is_cancelled: &mut impl FnMut() -> bool,
    ) -> Option<Arc<ReferenceIndex>> {
        let key = DefMapKey::Project(project_id);
        let def_map = self.project_def_map(project_id)?;
        if let Some(cached) = lock(&self.reference_index_cache).get(&key)
            && Arc::ptr_eq(&cached.def_map, &def_map)
        {
            return Some(cached.index.clone());
        }
        lock(&self.query_stats).reference_index += 1;
        let index = Arc::new(ReferenceIndex::build_cancellable(
            Arc::clone(self),
            def_map.clone(),
            is_cancelled,
        )?);
        lock(&self.reference_index_cache).insert(
            key,
            ReferenceIndexCacheEntry {
                def_map,
                index: index.clone(),
            },
        );
        Some(index)
    }

    fn member_index_for_map(&self, key: DefMapKey, def_map: Arc<DefMap>) -> Arc<MemberIndex> {
        if let Some(cached) = lock(&self.member_index_cache).get(&key)
            && Arc::ptr_eq(&cached.def_map, &def_map)
        {
            return cached.index.clone();
        }
        lock(&self.query_stats).member_index += 1;
        let index = Arc::new(MemberIndex::build_shared(
            def_map.clone(),
            Arc::clone(&self.standard_library),
        ));
        lock(&self.member_index_cache).insert(
            key,
            MemberIndexCacheEntry {
                def_map,
                index: index.clone(),
            },
        );
        index
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

    pub fn body_scopes(&self, def_id: DefId) -> Option<Arc<BodyScopes>> {
        self.local_resolution(def_id).map(|(scopes, _)| scopes)
    }

    pub fn body_resolution(&self, def_id: DefId) -> Option<Arc<BodyResolution>> {
        self.local_resolution(def_id)
            .map(|(_, resolution)| resolution)
    }

    pub fn infer(&self, def_id: DefId) -> Option<Arc<InferenceResult>> {
        let Some((context, _)) = self.definition_context(def_id) else {
            lock(&self.inference_cache).remove(&def_id);
            return None;
        };
        let Some(def_map) = self.current_definition_map(def_id) else {
            lock(&self.inference_cache).remove(&def_id);
            return None;
        };
        let Some(body) = self.body(def_id) else {
            lock(&self.inference_cache).remove(&def_id);
            return None;
        };
        let Some(resolution) = self.body_resolution(def_id) else {
            lock(&self.inference_cache).remove(&def_id);
            return None;
        };
        let key = match context {
            IdentityContext::Implicit(root_file) => DefMapKey::Implicit(root_file),
            IdentityContext::Project(project_id) => DefMapKey::Project(project_id),
        };
        let member_index = self.member_index_for_map(key, def_map.clone());
        if let Some(cached) = lock(&self.inference_cache).get(&def_id)
            && Arc::ptr_eq(&cached.body, &body)
            && Arc::ptr_eq(&cached.resolution, &resolution)
            && Arc::ptr_eq(&cached.def_map, &def_map)
            && Arc::ptr_eq(&cached.member_index, &member_index)
        {
            return Some(cached.result.clone());
        }

        lock(&self.query_stats).inference += 1;
        let result = Arc::new(infer_body(&body, &resolution, &def_map, &member_index));
        lock(&self.inference_cache).insert(
            def_id,
            InferenceCacheEntry {
                body,
                resolution,
                def_map,
                member_index,
                result: result.clone(),
            },
        );
        Some(result)
    }

    fn local_resolution(&self, def_id: DefId) -> Option<(Arc<BodyScopes>, Arc<BodyResolution>)> {
        let Some(body) = self.body(def_id) else {
            lock(&self.local_resolution_cache).remove(&def_id);
            return None;
        };
        if let Some(cached) = lock(&self.local_resolution_cache).get(&def_id)
            && Arc::ptr_eq(&cached.body, &body)
        {
            return Some((cached.scopes.clone(), cached.resolution.clone()));
        }

        lock(&self.query_stats).body_resolution += 1;
        let scopes = Arc::new(BodyScopes::build(&body));
        let resolution = Arc::new(BodyResolution::resolve_body(&body, &scopes));
        lock(&self.local_resolution_cache).insert(
            def_id,
            LocalResolutionCacheEntry {
                body,
                scopes: scopes.clone(),
                resolution: resolution.clone(),
            },
        );
        Some((scopes, resolution))
    }

    pub(crate) fn body_with_source_map(
        &self,
        def_id: DefId,
    ) -> Option<(Arc<Body>, Arc<BodySourceMap>)> {
        let Some(map) = self.current_definition_map(def_id) else {
            lock(&self.body_cache).remove(&def_id);
            return None;
        };
        let definition = map.definition(def_id)?.clone();
        let file_id = definition.file_id();
        let file_revision = self.file_revision(file_id)?;

        if let Some(cached) = lock(&self.body_cache).get(&def_id)
            && cached.file_revision == file_revision
        {
            return Some((cached.body.clone(), cached.source_map.clone()));
        }

        lock(&self.query_stats).body += 1;
        let Some((lowered, source_map)) = self.lower_body(def_id, &definition) else {
            lock(&self.body_cache).remove(&def_id);
            return None;
        };
        let body = lock(&self.body_cache)
            .get(&def_id)
            .filter(|cached| cached.body.as_ref() == &lowered)
            .map(|cached| cached.body.clone())
            .unwrap_or_else(|| Arc::new(lowered));
        let source_map = Arc::new(source_map);
        lock(&self.body_cache).insert(
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
            (DefKind::Chunk, ItemSourceKind::SyntheticFileChunk) => Some(lower_chunk_body(
                def_id,
                definition.file_id(),
                parse.tree().syntax(),
            )),
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
    use crate::vfs::{
        Change, FileId, FileKind, ProjectData, ProjectId, ProjectRoot, SourceRootId, SourceRootKind,
    };

    fn project_db(source: &str) -> (BaseDb, FileId, ProjectId) {
        let root = SourceRootId::new(0);
        let file = FileId::new(0);
        let project = ProjectId::new(0);
        let mut change = Change::new();
        change.set_source_root(root, SourceRootKind::Workspace);
        change.set_file_with_path(file, root, FileKind::Source, "src/main.rua", source);
        change.set_project(
            project,
            ProjectData::new(file, [ProjectRoot::at_root(root)], []),
        );
        let mut db = BaseDb::new();
        db.apply_change(change);
        (db, file, project)
    }

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
        initial.set_file_with_path(main_id, root_id, FileKind::Source, "src/main.rua", "");
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

    #[test]
    fn private_body_edit_reuses_project_declarations_and_member_index() {
        let (mut db, file, project) = project_db("fn work() -> i64 { 1 }");
        let map_before = db.project_def_map(project).unwrap();
        let member_before = db.project_member_index(project).unwrap();
        let definition = map_before
            .definitions()
            .find(|definition| definition.name() == "work")
            .unwrap()
            .id();
        let inference_before = db.infer(definition).unwrap();
        let stats_before = db.query_stats();

        let mut change = Change::new();
        change.set_file_text(file, "fn work() -> i64 { 2 }");
        db.apply_change(change);

        let map_after = db.project_def_map(project).unwrap();
        let member_after = db.project_member_index(project).unwrap();
        let inference_after = db.infer(definition).unwrap();
        let stats_after = db.query_stats();
        assert!(Arc::ptr_eq(&map_before, &map_after));
        assert!(Arc::ptr_eq(&member_before, &member_after));
        assert!(!Arc::ptr_eq(&inference_before, &inference_after));
        assert_eq!(stats_after.def_map, stats_before.def_map);
        assert_eq!(stats_after.member_index, stats_before.member_index);
        assert_eq!(stats_after.inference, stats_before.inference + 1);
    }

    #[test]
    fn public_signature_edit_rebuilds_project_declarations_and_member_index() {
        let (mut db, file, project) = project_db("fn work() -> i64 { 1 }");
        let map_before = db.project_def_map(project).unwrap();
        let member_before = db.project_member_index(project).unwrap();
        let stats_before = db.query_stats();

        let mut change = Change::new();
        change.set_file_text(file, "fn work(value: i64) -> i64 { value }");
        db.apply_change(change);

        let map_after = db.project_def_map(project).unwrap();
        let member_after = db.project_member_index(project).unwrap();
        let stats_after = db.query_stats();
        assert!(!Arc::ptr_eq(&map_before, &map_after));
        assert!(!Arc::ptr_eq(&member_before, &member_after));
        assert_eq!(stats_after.def_map, stats_before.def_map + 1);
        assert_eq!(stats_after.member_index, stats_before.member_index + 1);
    }

    #[test]
    fn body_edit_invalidates_reference_index_without_rebuilding_def_map() {
        let (db, file, project) =
            project_db("fn first() {} fn other() {} fn caller() { first(); }");
        let mut db = Arc::new(db);
        let map_before = db.project_def_map(project).unwrap();
        let references_before = db.project_reference_index(project).unwrap();
        let stats_before = db.query_stats();

        let mut change = Change::new();
        change.set_file_text(file, "fn first() {} fn other() {} fn caller() { other(); }");
        Arc::make_mut(&mut db).apply_change(change);

        let map_after = db.project_def_map(project).unwrap();
        let references_after = db.project_reference_index(project).unwrap();
        let stats_after = db.query_stats();
        assert!(Arc::ptr_eq(&map_before, &map_after));
        assert!(!Arc::ptr_eq(&references_before, &references_after));
        assert_eq!(stats_after.def_map, stats_before.def_map);
        assert_eq!(
            stats_after.reference_index,
            stats_before.reference_index + 1
        );
    }

    #[test]
    fn cancelled_reference_index_is_not_cached() {
        let (db, _file, project) = project_db(
            "fn target() {} fn one() { target(); } fn two() { target(); } fn three() { target(); }",
        );
        let db = Arc::new(db);
        let mut checkpoints = 0;
        let cancelled = db.project_reference_index_cancellable(project, &mut || {
            checkpoints += 1;
            checkpoints >= 2
        });

        assert!(cancelled.is_none());
        assert_eq!(db.cache_sizes().reference_index, 0);
        assert!(db.project_reference_index(project).is_some());
        assert_eq!(db.cache_sizes().reference_index, 1);
    }

    #[test]
    fn removing_project_reclaims_project_semantic_caches() {
        let (mut db, _file, project) = project_db("fn work() -> i64 { 1 }");
        let map = db.project_def_map(project).unwrap();
        let definition = map
            .definitions()
            .find(|definition| definition.name() == "work")
            .unwrap()
            .id();
        db.project_member_index(project).unwrap();
        db.infer(definition).unwrap();
        assert!(db.cache_sizes().body > 0);

        let mut change = Change::new();
        change.remove_project(project);
        db.apply_change(change);

        let sizes = db.cache_sizes();
        assert_eq!(sizes.def_map, 0);
        assert_eq!(sizes.member_index, 0);
        assert_eq!(sizes.reference_index, 0);
        assert_eq!(sizes.body, 0);
        assert_eq!(sizes.body_resolution, 0);
        assert_eq!(sizes.inference, 0);
        assert!(
            sizes.definition_identities > 0,
            "live DefMap keeps identities valid"
        );
        drop(map);
        let reclaimed = db.cache_sizes();
        assert_eq!(reclaimed.definition_identities, 0);
        assert_eq!(reclaimed.module_identities, 0);
    }

    #[test]
    fn repeated_project_add_remove_reuses_identity_capacity() {
        let (mut db, _file, project) = project_db("fn work() -> i64 { 1 }");
        let project_data = db.project(project).unwrap().clone();
        let mut upper_bound = None;

        for _ in 0..64 {
            let map = db.project_def_map(project).unwrap();
            let sizes = db.cache_sizes();
            let current = (sizes.definition_identities, sizes.module_identities);
            if let Some(expected) = upper_bound {
                assert_eq!(current, expected);
            } else {
                upper_bound = Some(current);
            }

            let mut remove = Change::new();
            remove.remove_project(project);
            db.apply_change(remove);
            drop(map);
            let reclaimed = db.cache_sizes();
            assert_eq!(reclaimed.definition_identities, 0);
            assert_eq!(reclaimed.module_identities, 0);

            let mut add = Change::new();
            add.set_project(project, project_data.clone());
            db.apply_change(add);
        }
    }
}
