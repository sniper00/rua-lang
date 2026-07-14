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

pub use db::{BaseDb, CacheSizes, QueryStats};
pub use diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticOrigin, DiagnosticRelated, DiagnosticSeverity,
    normalize_diagnostics, reconcile_diagnostics,
};
pub use hir::{
    AggregateSignature, BinaryOp, Binding, BindingId, BindingKind, Block, Body, BodyId,
    BodyResolution, BodyScopes, BodySourceId, BodySourceMap, BuiltinMemberId, BuiltinType,
    CallInfo, CallTarget, CallableSignature, CallableTy, CaptureKind, Condition, DefId, DefKind,
    DefMap, Definition, DefinitionSource, DefinitionSourceKind, Expr, ExprId, GenericParamData,
    GenericParamId, GenericParamTy, ImplSignature, ImplementationData, Import, InferenceDiagnostic,
    InferenceResult, InferenceSource, ItemKind, ItemSignature, ItemSourceKind, ItemTree,
    ItemTreeItem, Literal, LiteralKind, LocalBindingId, LocalCapture, LocalResolveResult, LocalUse,
    LocalUseKind, MatchArm, MemberCandidate, MemberId, MemberIndex, MemberKind, MemberOrigin,
    MemberResolution, MemberTarget, ModuleData, ModuleId, ModuleKind, NameRef, NameRefId,
    NameRefKind, NamedTy, NamedTypeResolver, ParameterData, Pat, PatId, PatternField, PrimitiveTy,
    ReceiverKind, ScopeData, ScopeId, ScopeKind, SignatureFingerprint, Statement, StructField,
    Substitution, TraitBound, Ty, TypeLoweringContext, TypeMismatchContext, TypeRef, UnaryOp,
    UnifyResult, VariantKind, VariantSignature, Visibility, WherePredicateData, unify,
};
pub use ide::{
    Analysis, AnalysisHost, BuiltinDefinitionTarget, CallHierarchyItem, ClosureParameterInfo,
    CompletionInsert, CompletionItem, CompletionKind, CompletionRelevance, DocumentSymbol,
    FileEdit, FilePosition, FileRange, HoverResult, MacroDelimiter, NavigationTarget, ProjectFile,
    ProjectId, ProjectPosition, QueryContext, ReferenceKind, ReferenceResult, RenameError,
    RenameTarget, SemanticToken, SemanticTokenKind, SemanticTokenModifiers, SignatureHelpInfo,
    SourceChange, TextEdit, TextRange, TypeHierarchyItem, TypeHint, WorkspaceSymbol,
};
pub use semantic::{LocalReference, Semantics};
pub use vfs::{
    Change, FileId, FileKind, ProjectData, ProjectRoot, SourceRoot, SourceRootId, SourceRootKind,
    VfsPath,
};
