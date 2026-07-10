//! Structured diagnostics with source-file attribution.
//!
//! Checkers accumulate [`Diag`] values (carrying a file id + line taken from the
//! offending span) and format them against the compile-time file registry at the
//! very end. This lets a multi-file build report `path:line: message` instead of
//! a bare line number that is ambiguous once several files are merged into one
//! AST. Single-string compiles use an empty root path and fall back to the old
//! `line: message` form.

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
    pub file: u32,
    /// Byte offset of the start of the diagnostic range in the source.
    pub start: usize,
    /// Byte length of the diagnostic range (0 for point / unknown).
    pub len: usize,
    /// 1-based line number (redundant convenience for CLI rendering).
    pub line: usize,
    pub msg: String,
}

impl Diag {
    pub fn new(file: u32, start: usize, len: usize, line: usize, msg: String) -> Self {
        Diag {
            file,
            start,
            len,
            line,
            msg,
        }
    }

    /// A diagnostic with no source location (e.g. duplicate-definition errors,
    /// which have no span to point at).
    pub fn bare(msg: String) -> Self {
        Diag {
            file: 0,
            start: 0,
            len: 0,
            line: 0,
            msg,
        }
    }

    /// Render as `path:line: msg`, degrading gracefully when the file path is
    /// empty (in-memory input) or the line is unknown (`0`).
    pub fn render(&self, files: &[String]) -> String {
        let path = files
            .get(self.file as usize)
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
