//! Unified protocol-neutral diagnostics and compiler-oracle reconciliation.

use std::fmt;

use crate::{
    BaseDb,
    base::{FileRange, TextRange},
    vfs::FileId,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiagnosticOrigin {
    FastAnalysis,
    Compiler,
}

/// Stable analysis-owned diagnostic identifier.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DiagnosticCode(String);

impl DiagnosticCode {
    pub fn new(code: impl Into<String>) -> Self {
        Self(code.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DiagnosticCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DiagnosticRelated {
    range: FileRange,
    message: String,
}

impl DiagnosticRelated {
    pub fn new(range: FileRange, message: impl Into<String>) -> Self {
        Self {
            range,
            message: message.into(),
        }
    }

    pub const fn range(&self) -> FileRange {
        self.range
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Protocol-neutral diagnostic result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    range: FileRange,
    message: String,
    code: Option<DiagnosticCode>,
    severity: DiagnosticSeverity,
    related: Vec<DiagnosticRelated>,
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
            range: FileRange::new(file_id, range),
            message: message.into(),
            code: None,
            severity: DiagnosticSeverity::Error,
            related: Vec::new(),
            origin,
        }
    }

    pub fn with_code(mut self, code: DiagnosticCode) -> Self {
        self.code = Some(code);
        self
    }

    pub const fn with_severity(mut self, severity: DiagnosticSeverity) -> Self {
        self.severity = severity;
        self
    }

    pub fn with_related(mut self, related: impl IntoIterator<Item = DiagnosticRelated>) -> Self {
        self.related = related.into_iter().collect();
        self.related.sort();
        self.related.dedup();
        self
    }

    pub const fn file_id(&self) -> FileId {
        self.range.file_id
    }

    pub const fn range(&self) -> TextRange {
        self.range.range
    }

    pub const fn file_range(&self) -> FileRange {
        self.range
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn code(&self) -> Option<&DiagnosticCode> {
        self.code.as_ref()
    }

    pub const fn severity(&self) -> DiagnosticSeverity {
        self.severity
    }

    pub fn related(&self) -> &[DiagnosticRelated] {
        &self.related
    }

    pub const fn origin(&self) -> DiagnosticOrigin {
        self.origin
    }
}

pub fn normalize_diagnostics(diagnostics: &mut Vec<Diagnostic>) {
    diagnostics.sort_by(|left, right| {
        (
            left.range,
            left.severity,
            &left.code,
            &left.message,
            &left.related,
            left.origin,
        )
            .cmp(&(
                right.range,
                right.severity,
                &right.code,
                &right.message,
                &right.related,
                right.origin,
            ))
    });
    diagnostics.dedup();
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
