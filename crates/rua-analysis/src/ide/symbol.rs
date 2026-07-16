//! Protocol-neutral document and workspace symbol projections.

use crate::{
    hir::{DefKind, DefMap, Definition, ModuleId, TextRange},
    vfs::FileId,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentSymbol {
    name: String,
    kind: DefKind,
    range: TextRange,
    selection_range: TextRange,
    children: Vec<DocumentSymbol>,
}

impl DocumentSymbol {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn kind(&self) -> DefKind {
        self.kind
    }

    pub const fn range(&self) -> TextRange {
        self.range
    }

    pub const fn selection_range(&self) -> TextRange {
        self.selection_range
    }

    pub fn children(&self) -> &[DocumentSymbol] {
        &self.children
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceSymbol {
    name: String,
    kind: DefKind,
    file_id: FileId,
    range: TextRange,
    selection_range: TextRange,
    container_name: Option<String>,
}

impl WorkspaceSymbol {
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

    pub const fn selection_range(&self) -> TextRange {
        self.selection_range
    }

    pub fn container_name(&self) -> Option<&str> {
        self.container_name.as_deref()
    }
}

pub(crate) fn document_symbols(map: &DefMap, file_id: FileId) -> Vec<DocumentSymbol> {
    let Some(module_id) = map.module_for_file(file_id) else {
        return Vec::new();
    };
    document_symbols_in_module(map, module_id, file_id)
}

fn document_symbols_in_module(
    map: &DefMap,
    module_id: ModuleId,
    file_id: FileId,
) -> Vec<DocumentSymbol> {
    map.definitions()
        .filter(|definition| {
            definition.module_id() == module_id
                && definition.file_id() == file_id
                && definition.owner().is_none()
                && !matches!(definition.kind(), DefKind::Impl | DefKind::Chunk)
        })
        .map(|definition| {
            let children = definition
                .target_module()
                .filter(|target| {
                    map.module(*target)
                        .is_some_and(|module| module.file_id() == Some(file_id))
                })
                .map(|target| document_symbols_in_module(map, target, file_id))
                .unwrap_or_default();
            DocumentSymbol {
                name: definition.name().to_string(),
                kind: definition.kind(),
                range: definition.range(),
                selection_range: definition.name_range(),
                children,
            }
        })
        .collect()
}

pub(crate) fn workspace_symbols(map: &DefMap, query: &str) -> Vec<WorkspaceSymbol> {
    let query = query.to_lowercase();
    map.definitions()
        .filter(|definition| definition.owner().is_none())
        .filter(|definition| !matches!(definition.kind(), DefKind::Impl | DefKind::Chunk))
        .filter(|definition| query.is_empty() || definition.name().to_lowercase().contains(&query))
        .map(|definition| workspace_symbol(map, definition))
        .collect()
}

fn workspace_symbol(map: &DefMap, definition: &Definition) -> WorkspaceSymbol {
    WorkspaceSymbol {
        name: definition.name().to_string(),
        kind: definition.kind(),
        file_id: definition.file_id(),
        range: definition.range(),
        selection_range: definition.name_range(),
        container_name: map.module_path(definition.module_id()),
    }
}

#[cfg(test)]
mod tests {
    use crate::{AnalysisHost, Change, DefKind, FileId, FileKind, SourceRootId, SourceRootKind};

    fn analysis_with_symbols() -> (crate::Analysis, FileId, FileId, &'static str) {
        let main_source = "pub fn root_fn() {}\n";
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
            "pub enum Number { One }\n",
        );
        let mut host = AnalysisHost::new();
        host.apply_change(change);
        (host.analysis(), main_id, math_id, main_source)
    }

    #[test]
    fn document_symbols_do_not_cross_path_module_files() {
        let (analysis, main_id, math_id, main_source) = analysis_with_symbols();

        let main = analysis.document_symbols(main_id, main_id);
        assert_eq!(
            main.iter().map(|symbol| symbol.name()).collect::<Vec<_>>(),
            ["root_fn"]
        );
        for symbol in &main {
            let range = symbol.selection_range();
            assert_eq!(
                &main_source[range.start() as usize..range.end() as usize],
                symbol.name()
            );
        }

        let math = analysis.document_symbols(main_id, math_id);
        assert_eq!(math.len(), 1);
        assert_eq!(math[0].name(), "Number");
        assert_eq!(math[0].kind(), DefKind::Enum);
    }

    #[test]
    fn workspace_symbols_are_flat_filterable_and_include_containers() {
        let (analysis, main_id, math_id, _) = analysis_with_symbols();

        let symbols = analysis.workspace_symbols(main_id, "");
        assert_eq!(
            symbols
                .iter()
                .map(|symbol| symbol.name())
                .collect::<Vec<_>>(),
            ["root_fn", "math", "Number"]
        );
        let number = symbols
            .iter()
            .find(|symbol| symbol.name() == "Number")
            .expect("file module enum symbol");
        assert_eq!(number.container_name(), Some("math"));
        assert_eq!(number.file_id(), math_id);

        let filtered = analysis.workspace_symbols(main_id, "number");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name(), "Number");
    }
}
