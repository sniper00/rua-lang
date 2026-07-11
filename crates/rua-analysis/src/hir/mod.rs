//! IDE-oriented HIR, definition maps, body lowering, and type inference.
//!
//! Compiler AST and type-checker types do not cross into this module; parity
//! with `ruac` is maintained through conformance tests.

mod def_map;
mod item_tree;
pub(crate) mod module_resolution;

pub use crate::base::TextRange;
pub use def_map::{
    DefId, DefKind, DefMap, Definition, DefinitionSource, DefinitionSourceKind, MemberId,
    ModuleData, ModuleId,
};
pub(crate) use def_map::{IdentityContext, IdentityInterner};
pub use item_tree::{
    AggregateSignature, CallableSignature, GenericParamData, ImplSignature, Import, ItemKind,
    ItemSignature, ItemSourceKind, ItemTree, ItemTreeItem, ModuleKind, ParameterData, ReceiverKind,
    SignatureFingerprint, TypeRef, VariantKind, VariantSignature, Visibility, WherePredicateData,
};
pub use module_resolution::module_file_candidates;
