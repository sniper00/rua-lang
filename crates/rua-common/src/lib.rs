//! Protocol-neutral foundation types shared across the Rua toolchain.
//!
//! This crate merges what were previously four separate crates:
//! - `rua-core`   → `core`   (type identities, diagnostics, builtins)
//! - `rua-lex`    → `lex`    (lossless shared lexer)
//! - `rua-project` → `project` (IO-free project model, source roots)
//! - `rua-resources` → `resources` (embedded standard-library declarations)
//!
//! All public items are re-exported at the crate root so existing code can
//! replace `use rua_core::FileId` with `use rua_common::FileId`.

pub mod core;
pub mod lex;
pub mod project;
pub mod resources;

// Flat re-exports — every public item across all four modules is uniquely named
// (zero collisions verified) so `pub use module::*` is safe.
// The submodules are also `pub` so callers can use explicit paths
// (e.g. `rua_common::lex::TokenKind`) as a disambiguation backstop.
pub use core::*;
pub use lex::*;
pub use project::*;
pub use resources::*;
