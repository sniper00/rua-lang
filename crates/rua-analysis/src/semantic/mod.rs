//! Semantic query facade over VFS inputs, HIR, and incremental indices.
//!
//! Higher layers use this boundary instead of reaching into storage or HIR
//! implementation details directly.

mod reference_index;

use std::sync::Arc;

use rua_syntax::{SyntaxKind, SyntaxToken};

use crate::{
    BaseDb,
    base::FileRange,
    hir::{
        Body, BodyResolution, BodySourceId, BodySourceMap, DefMap, Definition, LocalBindingId,
        LocalResolveResult, LocalUseKind, ModuleId, NameRefKind,
    },
    vfs::FileId,
};

pub use crate::base::FilePosition;
pub use reference_index::{ReferenceIndex, ReferenceOccurrence, ReferenceOccurrenceKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LocalReference {
    range: FileRange,
    kind: LocalUseKind,
}

impl LocalReference {
    pub const fn range(self) -> FileRange {
        self.range
    }

    pub const fn kind(self) -> LocalUseKind {
        self.kind
    }
}

#[derive(Clone, Debug)]
pub struct Semantics {
    db: Arc<BaseDb>,
    def_map: Arc<DefMap>,
}

impl Semantics {
    pub(crate) fn new(db: Arc<BaseDb>, def_map: Arc<DefMap>) -> Self {
        Self { db, def_map }
    }

    pub fn find_def_at(&self, position: FilePosition) -> Option<Definition> {
        self.db.file_text(position.file_id)?;
        let parse = self.db.parse(position.file_id);
        let token = identifier_at_offset(parse.syntax_node(), position.offset)?;

        if previous_significant(&token).is_some_and(|token| token.kind() == SyntaxKind::Dot) {
            return None;
        }

        if let Some(definition) = self.def_map.definitions().find(|definition| {
            definition.file_id() == position.file_id
                && definition.name_range().contains(position.offset)
        }) {
            return Some(definition.clone());
        }

        let local = self.local_at(position);
        if !matches!(local.result, LocalResolveResult::NonLocal) || local.blocks_item_fallback {
            return None;
        }

        let (segments, selected) = path_around(&token);
        let current_module = module_at_position(&self.def_map, position.file_id, position.offset)?;
        resolve_path_segment(&self.def_map, current_module, &segments, selected).cloned()
    }

    pub fn def_map(&self) -> &DefMap {
        &self.def_map
    }

    pub fn resolve_local_at(&self, position: FilePosition) -> LocalResolveResult {
        self.local_at(position).result
    }

    pub fn local_definition(&self, target: LocalBindingId) -> Option<FileRange> {
        self.current_local_data(target)?
            .1
            .binding_range(target.binding())
    }

    pub fn local_definition_at(&self, position: FilePosition) -> Option<FileRange> {
        let LocalResolveResult::Resolved(target) = self.resolve_local_at(position) else {
            return None;
        };
        self.local_definition(target)
    }

    pub fn local_uses(&self, target: LocalBindingId) -> Vec<LocalReference> {
        let (_, source_map, resolution) = match self.current_local_data(target) {
            Some(data) => data,
            None => return Vec::new(),
        };
        let mut uses = resolution
            .uses_for(target)
            .filter_map(|local_use| {
                Some(LocalReference {
                    range: source_map.name_ref_range(local_use.name_ref())?,
                    kind: local_use.kind(),
                })
            })
            .collect::<Vec<_>>();
        uses.sort_by_key(|reference| (reference.range, reference.kind));
        uses.dedup();
        uses
    }

    pub fn local_references(
        &self,
        target: LocalBindingId,
        include_declaration: bool,
    ) -> Vec<FileRange> {
        let mut references = self
            .local_uses(target)
            .into_iter()
            .map(LocalReference::range)
            .collect::<Vec<_>>();
        if include_declaration && let Some(declaration) = self.local_definition(target) {
            references.push(declaration);
        }
        references.sort();
        references.dedup();
        references
    }

    pub fn local_references_at(
        &self,
        position: FilePosition,
        include_declaration: bool,
    ) -> Vec<FileRange> {
        let LocalResolveResult::Resolved(target) = self.resolve_local_at(position) else {
            return Vec::new();
        };
        self.local_references(target, include_declaration)
    }

    fn local_at(&self, position: FilePosition) -> LocalAtResult {
        let Some((body, source_map, resolution)) = self.body_data_at(position) else {
            return LocalAtResult::non_local();
        };
        let ids = source_map.ids_at(position.file_id, position.offset);
        for id in &ids {
            let BodySourceId::Binding(binding) = id else {
                continue;
            };
            if body
                .binding(*binding)
                .is_some_and(|binding| !binding.is_missing())
            {
                return LocalAtResult {
                    result: LocalResolveResult::Resolved(LocalBindingId::new(body.id(), *binding)),
                    blocks_item_fallback: true,
                };
            }
        }

        let mut ambiguous = false;
        let mut blocks_item_fallback = false;
        for id in ids {
            let BodySourceId::NameRef(name_ref) = id else {
                continue;
            };
            match resolution.resolve(name_ref) {
                Some(LocalResolveResult::Resolved(target)) => {
                    return LocalAtResult {
                        result: LocalResolveResult::Resolved(target),
                        blocks_item_fallback: true,
                    };
                }
                Some(LocalResolveResult::Ambiguous) => ambiguous = true,
                Some(LocalResolveResult::NonLocal) | None => {}
            }
            blocks_item_fallback |= body.name_ref(name_ref).is_some_and(|name_ref| {
                matches!(
                    name_ref.kind(),
                    NameRefKind::Method
                        | NameRefKind::Field
                        | NameRefKind::StructField
                        | NameRefKind::PatternField
                        | NameRefKind::Macro
                )
            });
        }
        LocalAtResult {
            result: if ambiguous {
                LocalResolveResult::Ambiguous
            } else {
                LocalResolveResult::NonLocal
            },
            blocks_item_fallback: blocks_item_fallback || ambiguous,
        }
    }

    fn body_data_at(
        &self,
        position: FilePosition,
    ) -> Option<(Arc<Body>, Arc<BodySourceMap>, Arc<BodyResolution>)> {
        let owner = self
            .def_map
            .definitions()
            .filter(|definition| {
                definition.file_id() == position.file_id
                    && definition.range().contains(position.offset)
                    && definition.kind().is_body_owner()
            })
            .min_by_key(|definition| {
                (
                    definition.range().len(),
                    definition.kind() == crate::hir::DefKind::Chunk,
                )
            })?
            .id();
        Some((
            self.db.body(owner)?,
            self.db.body_source_map(owner)?,
            self.db.body_resolution(owner)?,
        ))
    }

    fn current_local_data(
        &self,
        target: LocalBindingId,
    ) -> Option<(Arc<Body>, Arc<BodySourceMap>, Arc<BodyResolution>)> {
        self.def_map.definition(target.owner().owner())?;
        let body = self.db.body(target.owner().owner())?;
        (body.id() == target.owner() && body.binding(target.binding()).is_some()).then_some((
            body,
            self.db.body_source_map(target.owner().owner())?,
            self.db.body_resolution(target.owner().owner())?,
        ))
    }
}

#[derive(Clone, Copy, Debug)]
struct LocalAtResult {
    result: LocalResolveResult,
    blocks_item_fallback: bool,
}

impl LocalAtResult {
    const fn non_local() -> Self {
        Self {
            result: LocalResolveResult::NonLocal,
            blocks_item_fallback: false,
        }
    }
}

fn identifier_at_offset(node: &rua_syntax::SyntaxNode, offset: u32) -> Option<SyntaxToken> {
    let end: u32 = node.text_range().end().into();
    match node.token_at_offset(offset.min(end).into()) {
        rowan::TokenAtOffset::Single(token) if is_path_identifier(&token) => Some(token),
        rowan::TokenAtOffset::Between(left, right) => {
            if is_path_identifier(&right) {
                Some(right)
            } else if is_path_identifier(&left) {
                Some(left)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn is_path_identifier(token: &SyntaxToken) -> bool {
    matches!(token.kind(), SyntaxKind::Ident | SyntaxKind::KwSelf)
}

fn path_around(selected: &SyntaxToken) -> (Vec<String>, usize) {
    let mut first = selected.clone();
    let mut selected_index = 0;
    while let Some(separator) = previous_significant(&first) {
        if separator.kind() != SyntaxKind::ColonColon {
            break;
        }
        let Some(segment) = previous_significant(&separator) else {
            break;
        };
        if !is_path_identifier(&segment) {
            break;
        }
        first = segment;
        selected_index += 1;
    }

    let mut segments = vec![first.text().to_string()];
    let mut current = first;
    while let Some(separator) = next_significant(&current) {
        if separator.kind() != SyntaxKind::ColonColon {
            break;
        }
        let Some(segment) = next_significant(&separator) else {
            break;
        };
        if !is_path_identifier(&segment) {
            break;
        }
        segments.push(segment.text().to_string());
        current = segment;
    }
    (segments, selected_index)
}

fn previous_significant(token: &SyntaxToken) -> Option<SyntaxToken> {
    let mut token = token.prev_token();
    while token.as_ref().is_some_and(|token| token.kind().is_trivia()) {
        token = token.and_then(|token| token.prev_token());
    }
    token
}

fn next_significant(token: &SyntaxToken) -> Option<SyntaxToken> {
    let mut token = token.next_token();
    while token.as_ref().is_some_and(|token| token.kind().is_trivia()) {
        token = token.and_then(|token| token.next_token());
    }
    token
}

fn module_at_position(map: &DefMap, file_id: FileId, offset: u32) -> Option<ModuleId> {
    let mut module_id = map.module_for_file(file_id)?;

    loop {
        let nested = map
            .definitions()
            .filter(|definition| definition.module_id() == module_id)
            .filter_map(|definition| {
                let target = definition.target_module()?;
                let target_data = map.module(target)?;
                (target_data.file_id() == Some(file_id) && definition.range().contains(offset))
                    .then_some((definition.range().len(), target))
            })
            .min_by_key(|(length, _)| *length);
        let Some((_, nested)) = nested else {
            return Some(module_id);
        };
        module_id = nested;
    }
}

fn resolve_path_segment<'map>(
    map: &'map DefMap,
    current_module: ModuleId,
    segments: &[String],
    selected: usize,
) -> Option<&'map Definition> {
    let first = segments.first()?;
    let mut definition = resolve_lexical_name(map, current_module, first)?;
    if selected == 0 {
        return Some(definition);
    }

    for (index, segment) in segments.iter().enumerate().skip(1) {
        definition = if let Some(module_id) = definition.target_module() {
            map.resolve_name(module_id, segment)?
        } else {
            map.resolve_member(definition.id(), segment)?
        };
        if index == selected {
            return Some(definition);
        }
    }
    None
}

fn resolve_lexical_name<'map>(
    map: &'map DefMap,
    mut module_id: ModuleId,
    name: &str,
) -> Option<&'map Definition> {
    loop {
        if let Some(definition) = map.resolve_name(module_id, name) {
            return Some(definition);
        }
        module_id = map.module(module_id)?.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        AnalysisHost, Change, DefKind, FileId, FileKind, FilePosition, SourceRootId, SourceRootKind,
    };

    fn offset_of(source: &str, needle: &str, occurrence: usize) -> u32 {
        source
            .match_indices(needle)
            .nth(occurrence)
            .unwrap_or_else(|| panic!("missing occurrence {occurrence} of {needle:?}"))
            .0 as u32
            + 1
    }

    #[test]
    fn find_def_at_resolves_simple_path_and_module_items() {
        let main_source = concat!(
            "fn helper() {}\n",
            "mod math;\n",
            "fn main() { helper(); math::answer(); }\n",
        );
        let math_source = "pub fn answer() -> i64 { 42 }\n";
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
            main_source,
        );
        change.set_file_with_path(
            math_id,
            root_id,
            FileKind::Source,
            "src/math.rua",
            math_source,
        );
        let mut host = AnalysisHost::new();
        host.apply_change(change);
        let semantics = host.analysis().semantics(main_id);

        let helper = semantics
            .find_def_at(FilePosition::new(
                main_id,
                offset_of(main_source, "helper", 1),
            ))
            .expect("simple name definition");
        assert_eq!(helper.name(), "helper");
        assert_eq!(helper.kind(), DefKind::Function);
        assert_eq!(helper.file_id(), main_id);

        let module_use = semantics
            .find_def_at(FilePosition::new(
                main_id,
                offset_of(main_source, "math", 1),
            ))
            .expect("path module definition");
        assert_eq!(module_use.name(), "math");
        assert_eq!(module_use.kind(), DefKind::Module);

        let answer = semantics
            .find_def_at(FilePosition::new(
                main_id,
                offset_of(main_source, "answer", 0),
            ))
            .expect("path item definition");
        assert_eq!(answer.name(), "answer");
        assert_eq!(answer.file_id(), math_id);

        let module_declaration = semantics
            .find_def_at(FilePosition::new(
                main_id,
                offset_of(main_source, "math", 0),
            ))
            .expect("module declaration definition");
        assert_eq!(module_declaration.id(), module_use.id());
    }

    #[test]
    fn find_def_at_uses_innermost_inline_module_scope() {
        let source = concat!(
            "fn item() {}\n",
            "mod nested { fn item() {} fn call() { item(); } }\n",
        );
        let root_id = SourceRootId::new(0);
        let file_id = FileId::new(0);
        let mut change = Change::new();
        change.set_source_root(root_id, SourceRootKind::Workspace);
        change.set_file_with_path(file_id, root_id, FileKind::Source, "main.rua", source);
        let mut host = AnalysisHost::new();
        host.apply_change(change);

        let definition = host
            .analysis()
            .semantics(file_id)
            .find_def_at(FilePosition::new(file_id, offset_of(source, "item", 2)))
            .expect("nested item definition");

        assert_eq!(
            definition.name_range().start(),
            offset_of(source, "item", 1) - 1
        );
    }

    #[test]
    fn find_def_at_does_not_guess_member_definitions() {
        let source = "fn field() {} fn main() { value.field; }";
        let root_id = SourceRootId::new(0);
        let file_id = FileId::new(0);
        let mut change = Change::new();
        change.set_source_root(root_id, SourceRootKind::Workspace);
        change.set_file_with_path(file_id, root_id, FileKind::Source, "main.rua", source);
        let mut host = AnalysisHost::new();
        host.apply_change(change);

        assert_eq!(
            host.analysis()
                .semantics(file_id)
                .find_def_at(FilePosition::new(file_id, offset_of(source, "field", 1))),
            None
        );
    }
}
