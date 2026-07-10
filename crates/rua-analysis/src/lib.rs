//! Incremental semantic analysis and IDE queries for Rua.
//!
//! This crate owns the in-memory analysis layer between `rua-syntax` and the
//! LSP transport. Each module marks an ownership boundary for the incremental
//! implementation.

pub mod db;
pub mod db_index;
pub mod diagnostic;
pub mod hir;
pub mod ide;
pub mod semantic;
pub mod vfs;

pub use db::BaseDb;
pub use diagnostic::Diagnostic;
pub use ide::{Analysis, AnalysisHost};
pub use vfs::{Change, FileId, FileKind, SourceRoot, SourceRootId, SourceRootKind};
