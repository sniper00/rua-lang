//! IDE-oriented HIR, definition maps, body lowering, and type inference.
//!
//! Compiler AST and type-checker types do not cross into this module; parity
//! with `ruac` is maintained through conformance tests.

mod item_tree;
pub(crate) mod module_resolution;

pub use item_tree::{ItemKind, ItemTree, ItemTreeItem, TextRange, Visibility};
pub use module_resolution::module_file_candidates;
