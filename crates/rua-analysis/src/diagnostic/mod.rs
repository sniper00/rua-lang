//! Unified fast-analysis diagnostics and compiler-oracle reconciliation.

use crate::{BaseDb, FileId, TextRange};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiagnosticOrigin {
    FastAnalysis,
    Compiler,
}

/// Protocol-neutral diagnostic result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    file_id: FileId,
    range: TextRange,
    message: String,
    origin: DiagnosticOrigin,
}

impl Diagnostic {
    pub fn new(
        file_id: FileId,
        range: TextRange,
        message: impl Into<String>,
        origin: DiagnosticOrigin,
    ) -> Self {
        Self {
            file_id,
            range,
            message: message.into(),
            origin,
        }
    }

    pub fn file_id(&self) -> FileId {
        self.file_id
    }

    pub fn range(&self) -> TextRange {
        self.range
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn origin(&self) -> DiagnosticOrigin {
        self.origin
    }
}

pub(crate) fn fast_diagnostics(db: &BaseDb, file_id: FileId) -> Vec<Diagnostic> {
    let Some(text) = db.file_text(file_id) else {
        return Vec::new();
    };
    db.parse(file_id)
        .errors()
        .iter()
        .map(|error| {
            let offset = error.offset.min(text.len()) as u32;
            Diagnostic::new(
                file_id,
                TextRange::new(offset, offset),
                format!("parse error: {}", error.message),
                DiagnosticOrigin::FastAnalysis,
            )
        })
        .collect()
}

/// Reconcile speculative fast diagnostics with the authoritative compiler
/// result. Once the compiler has findings for a version, they replace the fast
/// set so clients never see duplicate or contradictory messages.
pub fn reconcile_diagnostics(fast: Vec<Diagnostic>, compiler: Vec<Diagnostic>) -> Vec<Diagnostic> {
    if compiler.is_empty() { fast } else { compiler }
}
