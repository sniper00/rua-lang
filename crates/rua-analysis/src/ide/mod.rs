//! Snapshot-based IDE API, including `AnalysisHost` and feature queries.
//!
//! Results exposed here remain independent of LSP protocol types.

mod closure_iterator;
mod contract;
mod symbol;

use std::{rc::Rc, sync::Arc};

use rua_syntax::{Parse, ast::SourceFile};

use crate::{
    BaseDb,
    hir::{
        Body, BodyResolution, BodyScopes, BodySourceMap, DefId, DefMap, InferenceResult, ItemTree,
        module_resolution::{resolve_module_file, resolve_module_file_in_project_at},
    },
    semantic::Semantics,
    vfs::{Change, FileId, FileKind, SourceRootKind, VfsPath},
};

pub use crate::diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticOrigin, DiagnosticRelated, DiagnosticSeverity,
};
pub use closure_iterator::ClosureParameterInfo;
pub use contract::{
    CompletionInsert, CompletionItem, CompletionKind, FileEdit, FilePosition, FileRange,
    HoverResult, MacroDelimiter, NavigationTarget, ProjectFile, ProjectId, ProjectPosition,
    QueryContext, ReferenceKind, ReferenceResult, RenameError, RenameTarget, SemanticToken,
    SemanticTokenKind, SemanticTokenModifiers, SourceChange, TextEdit, TextRange,
};
pub use symbol::{DocumentSymbol, WorkspaceSymbol};

/// Mutable owner of the current analysis inputs.
#[derive(Debug, Default)]
pub struct AnalysisHost {
    db: Rc<BaseDb>,
}

impl AnalysisHost {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply_change(&mut self, change: Change) {
        Rc::make_mut(&mut self.db).apply_change(change);
    }

    pub fn analysis(&self) -> Analysis {
        Analysis {
            db: Rc::clone(&self.db),
        }
    }
}

/// Immutable view of the inputs captured when the snapshot was created.
#[derive(Clone, Debug)]
pub struct Analysis {
    db: Rc<BaseDb>,
}

impl Analysis {
    /// Test and integration helper for syntax queries during Phase 2.
    pub fn parse(&self, file_id: FileId) -> Arc<Parse<SourceFile>> {
        self.db.parse(file_id)
    }

    pub fn diagnostics(&self, file_id: FileId) -> Vec<Diagnostic> {
        crate::diagnostic::fast_diagnostics(&self.db, file_id)
    }

    pub fn item_tree(&self, file_id: FileId) -> Arc<ItemTree> {
        self.db.item_tree(file_id)
    }

    pub fn resolve_module(&self, from_file: FileId, name: &str) -> Option<FileId> {
        resolve_module_file(&self.db, from_file, name)
    }

    pub fn resolve_module_in_project(
        &self,
        project_id: ProjectId,
        from_file: FileId,
        name: &str,
    ) -> Option<FileId> {
        let map = self.db.project_def_map(project_id)?;
        let module = map.module(map.module_for_file(from_file)?)?;
        resolve_module_file_in_project_at(
            &self.db,
            project_id,
            from_file,
            module.resolution_directory()?,
            name,
        )
    }

    pub fn file_path(&self, file_id: FileId) -> Option<&VfsPath> {
        self.db.file_path(file_id)
    }

    pub fn def_map(&self, root_file: FileId) -> Arc<DefMap> {
        self.db.def_map(root_file)
    }

    pub fn def_map_for_project(&self, project_id: ProjectId) -> Option<Arc<DefMap>> {
        self.db.project_def_map(project_id)
    }

    pub fn body(&self, def_id: DefId) -> Option<Arc<Body>> {
        self.db.body(def_id)
    }

    pub fn body_source_map(&self, def_id: DefId) -> Option<Arc<BodySourceMap>> {
        self.db.body_source_map(def_id)
    }

    pub fn body_scopes(&self, def_id: DefId) -> Option<Arc<BodyScopes>> {
        self.db.body_scopes(def_id)
    }

    pub fn body_resolution(&self, def_id: DefId) -> Option<Arc<BodyResolution>> {
        self.db.body_resolution(def_id)
    }

    pub fn infer(&self, def_id: DefId) -> Option<Arc<InferenceResult>> {
        self.db.infer(def_id)
    }

    pub fn semantics(&self, root_file: FileId) -> Semantics {
        Semantics::new(Rc::clone(&self.db), self.db.def_map(root_file))
    }

    pub fn semantics_for_project(&self, project_id: ProjectId) -> Option<Semantics> {
        Some(Semantics::new(
            Rc::clone(&self.db),
            self.db.project_def_map(project_id)?,
        ))
    }

    pub fn document_symbols(&self, root_file: FileId, file_id: FileId) -> Vec<DocumentSymbol> {
        symbol::document_symbols(&self.db.def_map(root_file), file_id)
    }

    pub fn workspace_symbols(&self, root_file: FileId, query: &str) -> Vec<WorkspaceSymbol> {
        symbol::workspace_symbols(&self.db.def_map(root_file), query)
    }

    pub fn closure_parameters(&self, file_id: FileId) -> Vec<ClosureParameterInfo> {
        closure_iterator::closure_parameters(&self.db, file_id)
    }

    pub fn semantic_tokens(&self, file_id: FileId) -> Vec<SemanticToken> {
        closure_iterator::semantic_tokens(&self.db, file_id)
    }

    pub fn file_kind(&self, file_id: FileId) -> Option<FileKind> {
        self.db.file_kind(file_id)
    }

    pub fn source_root_kind(&self, file_id: FileId) -> Option<SourceRootKind> {
        self.db.source_root_kind(file_id)
    }

    pub fn is_file_read_only(&self, file_id: FileId) -> bool {
        self.db.is_file_read_only(file_id)
    }
}

#[cfg(test)]
mod tests {
    use super::AnalysisHost;
    use crate::vfs::{Change, FileId, FileKind, SourceRootId, SourceRootKind};

    #[test]
    fn analysis_host_applies_changes_and_exposes_parse() {
        let file_id = FileId::new(0);
        let mut change = Change::new();
        change.set_file_text(file_id, "fn main() {}");

        let mut host = AnalysisHost::new();
        host.apply_change(change);
        let analysis = host.analysis();

        assert_eq!(
            analysis.parse(file_id).syntax_node().text().to_string(),
            "fn main() {}"
        );
        assert!(analysis.diagnostics(file_id).is_empty());
    }

    #[test]
    fn analysis_host_snapshots_are_isolated_from_later_changes() {
        let file_id = FileId::new(0);
        let mut initial = Change::new();
        initial.set_file_text(file_id, "fn before() {}");

        let mut host = AnalysisHost::new();
        host.apply_change(initial);
        let before = host.analysis();

        let mut update = Change::new();
        update.set_file_text(file_id, "fn after() {}");
        host.apply_change(update);
        let after = host.analysis();

        assert_eq!(
            before.parse(file_id).syntax_node().text().to_string(),
            "fn before() {}"
        );
        assert_eq!(
            after.parse(file_id).syntax_node().text().to_string(),
            "fn after() {}"
        );
    }

    #[test]
    fn analysis_host_applies_a_change_batch_in_order() {
        let file_id = FileId::new(0);
        let mut change = Change::new();
        change.set_file_text(file_id, "fn first() {}");
        change.remove_file(file_id);
        change.set_file_text(file_id, "fn last() {}");

        let mut host = AnalysisHost::new();
        host.apply_change(change);

        assert_eq!(
            host.analysis()
                .parse(file_id)
                .syntax_node()
                .text()
                .to_string(),
            "fn last() {}"
        );
    }

    #[test]
    fn library_root_declaration_is_a_read_only_analysis_input() {
        let root_id = SourceRootId::new(1);
        let file_id = FileId::new(10);
        let mut change = Change::new();
        change.set_source_root(root_id, SourceRootKind::Library);
        change.set_file(
            file_id,
            root_id,
            FileKind::Declaration,
            "extern \"lua\" { pub fn log(msg: String); }",
        );

        let mut host = AnalysisHost::new();
        host.apply_change(change);
        let analysis = host.analysis();

        assert!(analysis.parse(file_id).errors().is_empty());
        assert_eq!(analysis.file_kind(file_id), Some(FileKind::Declaration));
        assert_eq!(
            analysis.source_root_kind(file_id),
            Some(SourceRootKind::Library)
        );
        assert!(analysis.is_file_read_only(file_id));
    }

    #[test]
    fn library_root_read_only_policy_is_independent_of_file_kind() {
        let cases = [
            (SourceRootKind::Workspace, FileKind::Source, false),
            (SourceRootKind::Library, FileKind::Declaration, true),
            (SourceRootKind::Std, FileKind::Declaration, true),
            (SourceRootKind::Virtual, FileKind::Source, false),
        ];
        let mut change = Change::new();
        for (index, (root_kind, file_kind, _)) in cases.iter().copied().enumerate() {
            let root_id = SourceRootId::new(index as u32);
            let file_id = FileId::new(index as u32);
            change.set_source_root(root_id, root_kind);
            change.set_file(file_id, root_id, file_kind, "");
        }

        let mut host = AnalysisHost::new();
        host.apply_change(change);
        let analysis = host.analysis();

        for (index, (root_kind, file_kind, read_only)) in cases.iter().copied().enumerate() {
            let file_id = FileId::new(index as u32);
            assert_eq!(analysis.source_root_kind(file_id), Some(root_kind));
            assert_eq!(analysis.file_kind(file_id), Some(file_kind));
            assert_eq!(analysis.is_file_read_only(file_id), read_only);
        }
    }

    #[test]
    fn library_root_removal_drops_its_files_but_not_old_snapshots() {
        let root_id = SourceRootId::new(1);
        let file_id = FileId::new(10);
        let mut initial = Change::new();
        initial.set_source_root(root_id, SourceRootKind::Library);
        initial.set_file(file_id, root_id, FileKind::Declaration, "pub fn api();");

        let mut host = AnalysisHost::new();
        host.apply_change(initial);
        let before_removal = host.analysis();

        let mut removal = Change::new();
        removal.remove_source_root(root_id);
        host.apply_change(removal);
        let after_removal = host.analysis();

        assert!(before_removal.is_file_read_only(file_id));
        assert_eq!(
            before_removal.file_kind(file_id),
            Some(FileKind::Declaration)
        );
        assert_eq!(after_removal.file_kind(file_id), None);
        assert_eq!(after_removal.source_root_kind(file_id), None);
    }
}
