//! Protocol-neutral source coordinates shared by HIR and IDE queries.

pub use rua_core::TextRange;

use crate::vfs::FileId;

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
