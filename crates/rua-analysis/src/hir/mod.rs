//! IDE-oriented HIR, definition maps, body lowering, and type inference.
//!
//! Compiler AST and type-checker types do not cross into this module; parity
//! with `ruac` is maintained through conformance tests.

pub mod body;
mod cfg;
mod def_map;
pub mod infer;
mod item_tree;
mod member;
pub mod scope;
mod std_library;
pub mod ty;

pub use crate::base::TextRange;
pub use body::{
    BinaryOp, Binding, BindingId, BindingKind, Block, Body, BodyId, BodySourceId, BodySourceMap,
    Condition, Expr, ExprId, Literal, LiteralKind, MatchArm, NameRef, NameRefId, NameRefKind, Pat,
    PatId, PatternField, Statement, StructField, UnaryOp,
};
pub use cfg::{ControlFlowGraph, StatementId};
pub use def_map::{
    DefId, DefKind, DefMap, Definition, DefinitionSource, DefinitionSourceKind, MemberId,
    ModuleData, ModuleId, ResolveStrategy,
};
pub(crate) use def_map::{IdentityContext, IdentityInterner, IdentityLease};
pub use infer::{
    CallInfo, CallTarget, InferenceDiagnostic, InferenceResult, InferenceSource,
    TypeMismatchContext,
};
pub use item_tree::{
    AggregateSignature, CallableSignature, GenericParamData, ImplSignature, Import, ItemKind,
    ItemSignature, ItemSourceKind, ItemTree, ItemTreeItem, ParameterData, ReceiverKind,
    SignatureFingerprint, TypeRef, VariantKind, VariantSignature, Visibility, WherePredicateData,
};
pub(crate) use member::CallableRequirement;
pub use member::{
    BuiltinType, ImplementationData, MemberCandidate, MemberIndex, MemberKind, MemberOrigin,
    MemberResolution, MemberTarget, TraitBound,
};
pub use scope::{
    BodyResolution, BodyScopes, CaptureKind, LocalBindingId, LocalCapture, LocalResolveResult,
    LocalUse, LocalUseKind, ScopeData, ScopeId, ScopeKind,
};
pub use std_library::{
    StdFunction, StdLibraryIndex, StdMember, StdMemberKind, StdType, standard_library,
};
pub use ty::{
    CallableTy, GenericParamId, GenericParamTy, NamedTy, NamedTypeResolver, PrimitiveTy,
    Substitution, Ty, TypeLoweringContext, UnifyResult, unify,
};
