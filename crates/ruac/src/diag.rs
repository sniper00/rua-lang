//! Structured diagnostics with source-file attribution.
//!
//! Checkers accumulate [`Diag`] values (carrying a file id + line taken from the
//! offending span) and format them against the compile-time file registry at the
//! very end. This lets a multi-file build report `path:line: message` instead of
//! a bare line number that is ambiguous once several files are merged into one
//! AST. Single-string compiles use an empty root path and fall back to the old
//! `line: message` form.

use std::ops::Deref;

use rua_core::{DiagnosticCode, DiagnosticSeverity, FileId, StructuredDiagnostic, TextRange};

/// A single diagnostic: which file/span it refers to, plus the message.
///
/// The byte-offset range (`start`..`start+len`) is the **preferred** position
/// carrier — it lets downstream tools (LSP, source-map) use `LineIndex` to
/// convert offsets into precise `(line, UTF-16 column)` LSP positions.
///
/// `line` is kept as a redundant convenience for `ruac`'s own CLI
/// rendering (`render()` / `render_all()`), which does not import `LineIndex`
/// (ruac is rowan-free). When set (non-zero) it must match the actual
/// line of `start`.
///
/// Span-less diagnostics (`bare`) have `start = len = line = file = 0`.
#[derive(Debug, Clone)]
pub struct Diag {
    pub diagnostic: StructuredDiagnostic,
    /// 1-based line number (redundant convenience for CLI rendering).
    pub line: usize,
    pub msg: String,
}

impl Deref for Diag {
    type Target = StructuredDiagnostic;

    fn deref(&self) -> &Self::Target {
        &self.diagnostic
    }
}

impl Diag {
    pub fn new(
        code: DiagnosticCode,
        file: u32,
        start: usize,
        len: usize,
        line: usize,
        msg: String,
    ) -> Self {
        let diagnostic = StructuredDiagnostic::new(
            code,
            Some(FileId::new(file)),
            Some(TextRange::at(start as u32, len as u32)),
        )
        .with_argument("message", &msg)
        .with_argument("line", line.to_string());
        Diag {
            diagnostic,
            line,
            msg,
        }
    }

    /// A diagnostic with no source location (e.g. duplicate-definition errors,
    /// which have no span to point at).
    pub fn bare(code: DiagnosticCode, msg: String) -> Self {
        let diagnostic = StructuredDiagnostic::new(code, None, None).with_argument("message", &msg);
        Diag {
            diagnostic,
            line: 0,
            msg,
        }
    }

    pub fn from_structured(
        diagnostic: StructuredDiagnostic,
        line: usize,
        message: impl Into<String>,
    ) -> Self {
        Self {
            diagnostic,
            line,
            msg: message.into(),
        }
    }

    pub fn file_index(&self) -> Option<u32> {
        self.diagnostic.file.map(FileId::index)
    }

    pub fn start(&self) -> usize {
        self.diagnostic
            .range
            .map_or(0, |range| range.start() as usize)
    }

    pub fn len(&self) -> usize {
        self.diagnostic
            .range
            .map_or(0, |range| range.len() as usize)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub const fn severity(&self) -> DiagnosticSeverity {
        self.diagnostic.code.severity()
    }

    /// Render as `path:line: msg`, degrading gracefully when the file path is
    /// empty (in-memory input) or the line is unknown (`0`).
    pub fn render(&self, files: &[String]) -> String {
        let path = files
            .get(self.file_index().unwrap_or(0) as usize)
            .map(String::as_str)
            .unwrap_or("");
        match (path.is_empty(), self.line) {
            (true, 0) => self.msg.clone(),
            (true, l) => format!("{}: {}", l, self.msg),
            (false, 0) => format!("{}: {}", path, self.msg),
            (false, l) => format!("{}:{}: {}", path, l, self.msg),
        }
    }
}

/// Join diagnostics into a single newline-separated error string.
pub fn render_all(diags: &[Diag], files: &[String]) -> String {
    diags
        .iter()
        .map(|d| d.render(files))
        .collect::<Vec<_>>()
        .join("\n")
}
