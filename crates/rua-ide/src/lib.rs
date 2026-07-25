//! IDE support for Rua: lossless CST, incremental semantic analysis, and LSP server.
//!
//! This crate merges what were previously three separate crates:
//! - `rua-syntax`   → `syntax`   (lossless rowan CST, formatter)
//! - `rua-analysis` → `analysis` (semantic analysis engine, IDE queries)
//! - `rua-lsp`      → `lsp`      (LSP server — requires `lsp` feature)
//!
//! Unlike [`rua_common`]'s flat re-exports, this crate uses module-scoped exports:
//! `syntax`, `analysis`, and `lsp` each define hundreds of public items, and
//! namespace collisions are likely without explicit module boundaries.

pub mod syntax;
pub mod analysis;

#[cfg(feature = "lsp")]
pub mod lsp;
