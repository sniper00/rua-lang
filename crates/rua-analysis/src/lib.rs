//! Incremental semantic analysis and IDE queries for Rua.
//!
//! This crate owns the in-memory analysis layer between `rua-syntax` and the
//! LSP transport. The initial skeleton deliberately contains no features; each
//! module marks an ownership boundary for the Phase 2 implementation.

pub mod db_index;
pub mod diagnostic;
pub mod hir;
pub mod ide;
pub mod semantic;
pub mod vfs;
