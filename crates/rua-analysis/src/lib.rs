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
    AggregateSignature, BinaryOp, Binding, BindingId, BindingKind, Block, Body, BodyId,
    BodySourceId, BodySourceMap, CallableSignature, Condition, DefId, DefKind, DefMap, Definition,
    DefinitionSource, DefinitionSourceKind, Expr, ExprId, GenericParamData, ImplSignature, Import,
    ItemKind, ItemSignature, ItemSourceKind, ItemTree, ItemTreeItem, Literal, LiteralKind,
    MatchArm, MemberId, ModuleData, ModuleId, ModuleKind, NameRef, NameRefId, NameRefKind,
    ParameterData, Pat, PatId, PatternField, ReceiverKind, SignatureFingerprint, Statement,
    StructField, TypeRef, UnaryOp, VariantKind, VariantSignature, Visibility, WherePredicateData,
};
pub use ide::{
    Analysis, AnalysisHost, ClosureParameterInfo, CompletionInsert, CompletionItem, CompletionKind,
    DocumentSymbol, FileEdit, FilePosition, FileRange, HoverResult, MacroDelimiter,
    NavigationTarget, ProjectFile, ProjectId, ProjectPosition, QueryContext, ReferenceKind,
    ReferenceResult, RenameError, RenameTarget, SemanticToken, SemanticTokenKind,
    SemanticTokenModifiers, SourceChange, TextEdit, TextRange, WorkspaceSymbol,
};
pub use semantic::Semantics;
pub use vfs::{
    Change, FileId, FileKind, ProjectData, ProjectRoot, SourceRoot, SourceRootId, SourceRootKind,
    VfsPath,
};
