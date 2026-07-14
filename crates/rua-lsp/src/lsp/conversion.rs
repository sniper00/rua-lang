use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use lsp_types::{Position, Range, Uri};
use rua_analysis::{Analysis, FileId, TextRange};
use rua_syntax::LineIndex;

struct CachedLineIndex {
    revision: u64,
    source: Arc<str>,
    index: Arc<LineIndex>,
    last_used: u64,
}

#[derive(Default)]
pub(super) struct LineIndexCache {
    clock: u64,
    entries: HashMap<FileId, CachedLineIndex>,
}

impl LineIndexCache {
    const CAPACITY: usize = 512;

    pub(super) fn get(
        &mut self,
        analysis: &Analysis,
        file_id: FileId,
    ) -> Option<(Arc<str>, Arc<LineIndex>)> {
        let revision = analysis.file_revision(file_id)?;
        self.clock = self.clock.wrapping_add(1);
        let now = self.clock;
        if let Some(entry) = self.entries.get_mut(&file_id)
            && entry.revision == revision
        {
            entry.last_used = now;
            return Some((Arc::clone(&entry.source), Arc::clone(&entry.index)));
        }

        let source = analysis.file_text(file_id)?;
        let index = Arc::new(LineIndex::new(&source));
        self.clock = self.clock.wrapping_add(1);
        self.entries.insert(
            file_id,
            CachedLineIndex {
                revision,
                source: Arc::clone(&source),
                index: Arc::clone(&index),
                last_used: self.clock,
            },
        );
        if self.entries.len() > Self::CAPACITY
            && let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(file_id, _)| *file_id)
        {
            self.entries.remove(&oldest);
        }
        Some((source, index))
    }
}

pub(super) fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    let url = url::Url::parse(uri.as_str()).ok()?;
    if url.scheme() != "file" || url.query().is_some() || url.fragment().is_some() {
        return None;
    }
    url.to_file_path().ok()
}

pub(super) fn normalize_physical_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| {
        let Some(parent) = path.parent() else {
            return path.to_path_buf();
        };
        std::fs::canonicalize(parent)
            .map(|canonical| {
                path.file_name()
                    .map_or(canonical.clone(), |name| canonical.join(name))
            })
            .unwrap_or_else(|_| path.to_path_buf())
    })
}

pub(super) fn path_to_uri(path: &Path) -> Option<Uri> {
    let path = normalize_physical_path(path);
    url::Url::from_file_path(path).ok()?.as_str().parse().ok()
}

pub(super) fn range_from_bytes(range: TextRange, line_index: &LineIndex, source: &str) -> Range {
    let start = (range.start() as usize).min(source.len());
    let end = (range.end() as usize).min(source.len());
    let (start_line, start_column) = line_index.line_col(start, source);
    let (end_line, end_column) = line_index.line_col(end, source);
    Range {
        start: Position::new(start_line as u32, start_column as u32),
        end: Position::new(end_line as u32, end_column as u32),
    }
}

pub(super) fn find_import_insertion_point(source: &str) -> usize {
    let mut last_import_end = 0usize;
    let mut position = 0usize;
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("use ") || trimmed.starts_with("mod ") {
            last_import_end = position + line.len() + 1;
        }
        position += line.len() + 1;
    }
    if last_import_end > 0 {
        return last_import_end.min(source.len());
    }

    position = 0;
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            last_import_end = position + line.len() + 1;
            position += line.len() + 1;
        } else {
            break;
        }
    }
    last_import_end.min(source.len())
}
