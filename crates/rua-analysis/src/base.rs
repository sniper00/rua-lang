//! Protocol-neutral source coordinates shared by HIR and IDE queries.

use crate::vfs::FileId;

/// Half-open UTF-8 byte range in a source file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TextRange {
    start: u32,
    end: u32,
}

impl TextRange {
    pub const fn new(start: u32, end: u32) -> Self {
        assert!(start <= end, "text range start must not exceed end");
        Self { start, end }
    }

    pub const fn start(self) -> u32 {
        self.start
    }

    pub const fn end(self) -> u32 {
        self.end
    }

    pub const fn len(self) -> u32 {
        self.end - self.start
    }

    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    pub const fn contains(self, offset: u32) -> bool {
        self.start <= offset && offset < self.end
    }

    pub const fn contains_range(self, other: Self) -> bool {
        self.start <= other.start && other.end <= self.end
    }
}

/// Byte position in a single file. Semantic cross-file queries also require a project context.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FilePosition {
    pub file_id: FileId,
    pub offset: u32,
}

impl FilePosition {
    pub const fn new(file_id: FileId, offset: u32) -> Self {
        Self { file_id, offset }
    }
}

/// Byte range paired with its source file identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileRange {
    pub file_id: FileId,
    pub range: TextRange,
}

impl FileRange {
    pub const fn new(file_id: FileId, range: TextRange) -> Self {
        Self { file_id, range }
    }
}
