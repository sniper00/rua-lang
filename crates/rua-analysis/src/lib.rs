//! Incremental semantic analysis and IDE queries for Rua.
//!
//! This crate owns the in-memory analysis layer between `rua-syntax` and the
//! LSP transport. Each module marks an ownership boundary for the incremental
//! implementation.

mod base;

pub mod db;
pub mod db_index;
pub mod diagnostic;
pub mod hir;
pub mod ide;
pub mod semantic;
pub mod vfs;

pub use db::BaseDb;
pub use diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticOrigin, DiagnosticRelated, DiagnosticSeverity,
    normalize_diagnostics, reconcile_diagnostics,
};
pub use hir::{
    DefId, DefKind, DefMap, Definition, Import, ItemKind, ItemTree, ItemTreeItem, ModuleData,
    ModuleId, ModuleKind, Visibility,
};
pub use ide::{
    Analysis, AnalysisHost, ClosureParameterInfo, CompletionInsert, CompletionItem, CompletionKind,
    DocumentSymbol, FileEdit, FilePosition, FileRange, HoverResult, MacroDelimiter,
    NavigationTarget, ProjectFile, ProjectId, ProjectPosition, QueryContext, ReferenceKind,
    ReferenceResult, RenameError, RenameTarget, SemanticToken, SemanticTokenKind,
    SemanticTokenModifiers, SourceChange, TextEdit, TextRange, WorkspaceSymbol,
};
pub use semantic::Semantics;
pub use vfs::{Change, FileId, FileKind, SourceRoot, SourceRootId, SourceRootKind, VfsPath};
