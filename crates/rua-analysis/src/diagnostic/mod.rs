//! Unified fast-analysis diagnostics and compiler-oracle reconciliation.
//!
//! Diagnostic data remains protocol-neutral; LSP conversion belongs in
//! `rua-lsp`.

/// Protocol-neutral diagnostic result.
///
/// Fields are added when the diagnostics pipeline is implemented. The Phase 2
/// analysis skeleton returns an empty collection.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Diagnostic {}
