//! Shared test harness for rua-lsp integration tests.
//!
//! Provides a minimal `TestServer` that mirrors the production `Server` without
//! LSP protocol overhead — tests call the `Analysis` API directly via
//! `snapshot()`.
#![allow(dead_code)]

use std::path::PathBuf;

use lsp_types::Uri;

use rua_analysis::{AnalysisHost, Change, FileId, ProjectId, ProjectPosition};
use rua_syntax::LineIndex;

// ---------------------------------------------------------------------------
// TestServer
// ---------------------------------------------------------------------------

pub struct TestServer {
    host: AnalysisHost,
    file_ids: std::collections::HashMap<PathBuf, (Uri, FileId)>,
    open_buffers: std::collections::HashMap<FileId, (Uri, String)>,
    next_file_id: u32,
}

#[allow(dead_code)]
impl TestServer {
    pub fn new() -> Self {
        Self {
            host: AnalysisHost::new(),
            file_ids: std::collections::HashMap::new(),
            open_buffers: std::collections::HashMap::new(),
            next_file_id: 0,
        }
    }

    pub fn doc_key(uri: &Uri) -> PathBuf {
        let s = uri.as_str();
        s.strip_prefix("file://")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(s))
    }

    pub fn file_id_for_uri(&self, uri: &Uri) -> Option<FileId> {
        self.file_ids.get(&Self::doc_key(uri)).map(|(_, id)| *id)
    }

    pub fn ensure_file_id(&mut self, uri: &Uri) -> FileId {
        let key = Self::doc_key(uri);
        if let Some((_, id)) = self.file_ids.get(&key) {
            return *id;
        }
        let id = FileId::new(self.next_file_id);
        self.next_file_id += 1;
        self.file_ids.insert(key, (uri.clone(), id));
        id
    }

    pub fn open(&mut self, uri: &Uri, text: &str) -> FileId {
        let file_id = self.ensure_file_id(uri);
        let mut change = Change::new();
        change.set_file_text(file_id, text);
        self.host.apply_change(change);
        self.open_buffers
            .insert(file_id, (uri.clone(), text.to_string()));
        file_id
    }

    pub fn change(&mut self, uri: &Uri, text: &str) {
        let file_id = self.ensure_file_id(uri);
        let mut change = Change::new();
        change.set_file_text(file_id, text);
        self.host.apply_change(change);
        self.open_buffers
            .insert(file_id, (uri.clone(), text.to_string()));
    }

    pub fn close(&mut self, uri: &Uri) {
        let key = Self::doc_key(uri);
        if let Some((_, file_id)) = self.file_ids.remove(&key) {
            self.open_buffers.remove(&file_id);
            let mut change = Change::new();
            change.remove_file(file_id);
            self.host.apply_change(change);
        }
    }

    pub fn snapshot(&self) -> rua_analysis::Analysis {
        self.host.analysis()
    }

    /// Convert (line, col) to a `ProjectPosition`.
    pub fn pp(&self, uri: &Uri, line: u32, col: u32) -> Option<ProjectPosition> {
        let file_id = self.file_id_for_uri(uri)?;
        let analysis = self.host.analysis();
        let source = analysis.parse(file_id).syntax_node().text().to_string();
        let li = LineIndex::new(&source);
        let offset = li.offset(line as usize, col as usize, &source);
        Some(ProjectPosition::at(ProjectId::new(0), file_id, offset as u32))
    }

    /// Get a `ProjectPosition` at a specific byte offset.
    pub fn pp_at_offset(&self, uri: &Uri, offset: usize) -> Option<ProjectPosition> {
        let file_id = self.file_id_for_uri(uri)?;
        Some(ProjectPosition::at(
            ProjectId::new(0),
            file_id,
            offset as u32,
        ))
    }

    /// Get source text for a file.
    pub fn source(&self, uri: &Uri) -> Option<String> {
        let file_id = self.file_id_for_uri(uri)?;
        let analysis = self.host.analysis();
        Some(analysis.parse(file_id).syntax_node().text().to_string())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub fn uri(path: &str) -> Uri {
    format!("file://{path}").parse().expect("valid URI")
}

/// Find the byte offset of the first occurrence of `needle` in `haystack`.
pub fn find_offset(haystack: &str, needle: &str) -> usize {
    haystack.find(needle).expect("needle not found in source")
}

/// Find the byte offset of the nth occurrence (0-indexed) of `needle`.
pub fn find_nth_offset(haystack: &str, needle: &str, n: usize) -> usize {
    let mut remaining = haystack;
    let mut total = 0;
    for _ in 0..=n {
        let pos = remaining.find(needle).expect("needle not found in source");
        let abs = total + pos;
        remaining = &haystack[abs + needle.len()..];
        total = abs + needle.len();
        if remaining.is_empty() && remaining.find(needle).is_none() {
            return abs;
        }
    }
    haystack
        .match_indices(needle)
        .nth(n)
        .expect("needle not found")
        .0
}

/// Get the (line, col) for a byte offset.
pub fn offset_to_line_col(source: &str, offset: usize) -> (u32, u32) {
    let li = LineIndex::new(source);
    let (line, col) = li.line_col(offset, source);
    (line as u32, col as u32)
}

/// Find the byte offset of a `$0` marker in the source.
///
/// The marker `$0` is stripped from the source before it's passed to the
/// analysis, and the byte offset of the marker is returned. This allows
/// writing ergonomic cursor-position tests:
///
/// ```ignore
/// let source = "fn main() { let x$0 = 1; }";
/// let offset = marker_offset(source);
/// assert_eq!(source[..offset], "fn main() { let x");
/// ```
pub fn marker_offset(source_with_marker: &str) -> usize {
    source_with_marker.find("$0").expect("source must contain $0 marker")
}

/// Remove the `$0` marker from source and return the clean source.
pub fn strip_marker(source: &str) -> String {
    source.replace("$0", "")
}

/// Find offset and return clean source + offset in one call.
pub fn extract_marker(source: &str) -> (String, usize) {
    let offset = marker_offset(source);
    (strip_marker(source), offset)
}
