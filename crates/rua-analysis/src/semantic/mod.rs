//! Semantic query facade over VFS inputs, HIR, and incremental indices.
//!
//! Higher layers use this boundary instead of reaching into storage or HIR
//! implementation details directly.

use std::{rc::Rc, sync::Arc};

use rua_syntax::{SyntaxKind, SyntaxToken};

use crate::{
    BaseDb,
    hir::{DefMap, Definition, ModuleId},
    vfs::FileId,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FilePosition {
    pub file_id: FileId,
    pub offset: u32,
}

impl FilePosition {
    pub const fn new(file_id: FileId, offset: u32) -> Self {
        Self { file_id, offset }
    }
}

#[derive(Clone, Debug)]
pub struct Semantics {
    db: Rc<BaseDb>,
    def_map: Arc<DefMap>,
}

impl Semantics {
    pub(crate) fn new(db: Rc<BaseDb>, def_map: Arc<DefMap>) -> Self {
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

        let (segments, selected) = path_around(&token);
        let current_module = module_at_position(&self.def_map, position.file_id, position.offset)?;
        resolve_path_segment(&self.def_map, current_module, &segments, selected).cloned()
    }

    pub fn def_map(&self) -> &DefMap {
        &self.def_map
    }
}

fn identifier_at_offset(node: &rua_syntax::SyntaxNode, offset: u32) -> Option<SyntaxToken> {
    let end: u32 = node.text_range().end().into();
    match node.token_at_offset(offset.min(end).into()) {
        rowan::TokenAtOffset::Single(token) if token.kind() == SyntaxKind::Ident => Some(token),
        rowan::TokenAtOffset::Between(left, right) => {
            if right.kind() == SyntaxKind::Ident {
                Some(right)
            } else if left.kind() == SyntaxKind::Ident {
                Some(left)
            } else {
                None
            }
        }
        _ => None,
    }
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
        if segment.kind() != SyntaxKind::Ident {
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
        if segment.kind() != SyntaxKind::Ident {
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
    let mut module_id = map
        .modules()
        .find(|module| {
            module.file_id() == Some(file_id)
                && !map.definitions().any(|definition| {
                    definition.target_module() == Some(module.id())
                        && definition.file_id() == file_id
                })
        })?
        .id();

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
        let module_id = definition.target_module()?;
        definition = map.resolve_name(module_id, segment)?;
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
