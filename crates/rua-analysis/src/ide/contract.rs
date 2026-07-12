//! Protocol-neutral input and result types for IDE queries.

use std::{cmp::Reverse, collections::BTreeSet, fmt};

use crate::vfs::FileId;

pub use crate::base::{FilePosition, FileRange, TextRange};
pub use crate::vfs::ProjectId;

/// A file interpreted in one project's ordered dependency context.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectFile {
    pub project_id: ProjectId,
    pub file_id: FileId,
}

impl ProjectFile {
    pub const fn new(project_id: ProjectId, file_id: FileId) -> Self {
        Self {
            project_id,
            file_id,
        }
    }
}

/// Context required by queries whose answer depends on project/root priority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QueryContext {
    project_id: ProjectId,
}

impl QueryContext {
    pub const fn new(project_id: ProjectId) -> Self {
        Self { project_id }
    }

    pub const fn project_id(self) -> ProjectId {
        self.project_id
    }

    pub const fn file(self, file_id: FileId) -> ProjectFile {
        ProjectFile::new(self.project_id, file_id)
    }

    pub const fn position(self, position: FilePosition) -> ProjectPosition {
        ProjectPosition::new(self.project_id, position)
    }
}

/// Cursor position interpreted in one project context.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectPosition {
    pub project_id: ProjectId,
    pub position: FilePosition,
}

impl ProjectPosition {
    pub const fn new(project_id: ProjectId, position: FilePosition) -> Self {
        Self {
            project_id,
            position,
        }
    }

    pub const fn at(project_id: ProjectId, file_id: FileId, offset: u32) -> Self {
        Self::new(project_id, FilePosition::new(file_id, offset))
    }

    pub const fn project_file(self) -> ProjectFile {
        ProjectFile::new(self.project_id, self.position.file_id)
    }
}

/// Source target for go-to-definition and related navigation queries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NavigationTarget {
    full_range: FileRange,
    focus_range: Option<TextRange>,
}

impl NavigationTarget {
    pub const fn new(full_range: FileRange, focus_range: Option<TextRange>) -> Self {
        if let Some(focus_range) = focus_range {
            assert!(
                full_range.range.contains_range(focus_range),
                "navigation focus must be inside the full range"
            );
        }
        Self {
            full_range,
            focus_range,
        }
    }

    pub const fn full_range(self) -> FileRange {
        self.full_range
    }

    pub const fn focus_range(self) -> Option<TextRange> {
        self.focus_range
    }

    pub const fn target_range(self) -> FileRange {
        FileRange::new(
            self.full_range.file_id,
            match self.focus_range {
                Some(range) => range,
                None => self.full_range.range,
            },
        )
    }

    pub fn normalize(targets: &mut Vec<Self>) {
        targets.sort_by_key(|target| target.target_range());
        targets.dedup_by_key(|target| target.target_range());
    }
}

/// Stable display data for hover. Markdown wrapping belongs to the LSP adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HoverResult {
    range: FileRange,
    signature: String,
    documentation: Option<String>,
}

impl HoverResult {
    pub fn new(range: FileRange, signature: impl Into<String>) -> Self {
        Self {
            range,
            signature: signature.into(),
            documentation: None,
        }
    }

    pub fn with_documentation(mut self, documentation: impl Into<String>) -> Self {
        self.documentation = Some(documentation.into());
        self
    }

    pub const fn range(&self) -> FileRange {
        self.range
    }

    pub fn signature(&self) -> &str {
        &self.signature
    }

    pub fn documentation(&self) -> Option<&str> {
        self.documentation.as_deref()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CompletionKind {
    Keyword,
    Variable,
    Parameter,
    Function,
    Method,
    Field,
    Struct,
    Enum,
    Variant,
    Trait,
    Impl,
    Module,
    TypeAlias,
    BuiltinType,
    Macro,
}

/// Semantic insertion intent. The adapter chooses the concrete snippet syntax.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CompletionInsert {
    Plain(String),
    /// Snippet with placeholders (`$1`, `$0`, etc.).
    Snippet(String),
    Call {
        callee: String,
        /// Parameter names/types for snippet placeholders (e.g. `["x: i64", "y: i64"]`).
        params: Vec<String>,
    },
    MacroCall {
        name: String,
        delimiter: MacroDelimiter,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MacroDelimiter {
    Parentheses,
    Brackets,
    Braces,
}

/// Orthogonal relevance score for a completion item, modelled after
/// rust-analyzer's `CompletionRelevance`.  Each dimension contributes to the
/// final `score()` so the ranking is transparent and new categories have a
/// clear place to land.
///
/// Previously these were 14 hardcoded magic integers scattered across
/// `scope_completions()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[derive(Default)]
pub struct CompletionRelevance {
    pub base: u8,
    pub exact_type_match: bool,
    pub type_name_match: bool,
    pub is_local: bool,
    pub is_from_this_crate: bool,
    pub is_deprecated: bool,
}

impl CompletionRelevance {
    // ── Category constructors (one per completion source) ──────────────

    const DEFAULT: Self = Self {
        base: 0, exact_type_match: false, type_name_match: false,
        is_local: false, is_from_this_crate: false, is_deprecated: false,
    };

    /// Construct a relevance with just a base score, for tests.
    pub const fn from_score(base: u8) -> Self {
        Self { base, ..Self::DEFAULT }
    }

    pub const fn keyword() -> Self { Self { base: 50, ..Self::DEFAULT } }
    pub const fn snippet() -> Self { Self { base: 51, ..Self::DEFAULT } }
    pub const fn builtin_type() -> Self { Self { base: 40, ..Self::DEFAULT } }
    pub const fn builtin_type_pos() -> Self { Self { base: 90, ..Self::DEFAULT } }
    pub const fn local(usage: u8) -> Self {
        let extra = if usage > 5 { 5 } else { usage };
        Self { base: 95 + extra, is_local: true, ..Self::DEFAULT }
    }
    pub const fn self_keyword() -> Self { Self { base: 96, is_local: true, ..Self::DEFAULT } }
    pub const fn member() -> Self { Self { base: 90, ..Self::DEFAULT } }
    pub const fn same_module() -> Self { Self { base: 85, is_from_this_crate: true, ..Self::DEFAULT } }
    pub const fn cross_module() -> Self { Self { base: 75, ..Self::DEFAULT } }
    pub const fn postfix() -> Self { Self { base: 85, ..Self::DEFAULT } }
    pub const fn match_variant() -> Self { Self { base: 93, ..Self::DEFAULT } }
    pub const fn struct_field() -> Self { Self { base: 93, ..Self::DEFAULT } }
    pub const fn iflet_variant() -> Self { Self { base: 94, ..Self::DEFAULT } }
    pub const fn path_member() -> Self { Self { base: 80, ..Self::DEFAULT } }
    pub const fn path_variant() -> Self { Self { base: 85, ..Self::DEFAULT } }
    pub const fn builtin_const() -> Self { Self { base: 35, ..Self::DEFAULT } }
    pub const fn builtin_macro() -> Self { Self { base: 20, ..Self::DEFAULT } }
    pub const fn arithmetic_num() -> Self { Self { base: 88, ..Self::DEFAULT } }

    // ── Modifiers ──────────────────────────────────────────────────────

    pub const fn with_exact_type_match(mut self, matches: bool) -> Self {
        self.exact_type_match = matches;
        self
    }

    pub const fn with_type_name_match(mut self, matches: bool) -> Self {
        self.type_name_match = matches;
        self
    }

    pub const fn with_deprecated(mut self, deprecated: bool) -> Self {
        self.is_deprecated = deprecated;
        self
    }

    /// Resolve all dimensions into a comparable integer score.
    pub fn score(&self) -> u16 {
        let mut s = self.base as u16;
        if self.exact_type_match { s += 10; }
        if self.type_name_match { s += 5; }
        if self.is_local { s += 2; }
        if self.is_from_this_crate { s += 3; }
        if self.is_deprecated { s = s.saturating_sub(20); }
        s
    }

    /// Return a copy with the base score increased by `boost` (saturating at 255).
    pub fn with_boost(self, boost: u16) -> Self {
        Self { base: self.base.saturating_add(boost as u8), ..self }
    }
}


#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionItem {
    label: String,
    kind: CompletionKind,
    detail: Option<String>,
    documentation: Option<String>,
    lookup: Option<String>,
    insert: Option<CompletionInsert>,
    replacement_range: Option<TextRange>,
    target: Option<FileRange>,
    relevance: CompletionRelevance,
    deprecated: bool,
    /// If set, the LSP client should add this import statement.
    import_path: Option<String>,
}

impl CompletionItem {
    pub fn new(label: impl Into<String>, kind: CompletionKind) -> Self {
        Self {
            label: label.into(),
            kind,
            detail: None,
            documentation: None,
            lookup: None,
            insert: None,
            replacement_range: None,
            target: None,
            relevance: CompletionRelevance::default(),
            deprecated: false,
            import_path: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn with_documentation(mut self, documentation: impl Into<String>) -> Self {
        self.documentation = Some(documentation.into());
        self
    }

    pub fn with_lookup(mut self, lookup: impl Into<String>) -> Self {
        self.lookup = Some(lookup.into());
        self
    }

    pub fn with_insert(mut self, insert: CompletionInsert) -> Self {
        self.insert = Some(insert);
        self
    }

    pub const fn with_replacement_range(mut self, range: TextRange) -> Self {
        self.replacement_range = Some(range);
        self
    }

    pub const fn with_target(mut self, target: FileRange) -> Self {
        self.target = Some(target);
        self
    }

    pub const fn with_relevance(mut self, relevance: CompletionRelevance) -> Self {
        self.relevance = relevance;
        self
    }

    pub const fn deprecated(mut self, deprecated: bool) -> Self {
        self.deprecated = deprecated;
        self
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub const fn kind(&self) -> CompletionKind {
        self.kind
    }

    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    pub fn documentation(&self) -> Option<&str> {
        self.documentation.as_deref()
    }

    pub fn lookup(&self) -> Option<&str> {
        self.lookup.as_deref()
    }

    pub fn insert(&self) -> Option<&CompletionInsert> {
        self.insert.as_ref()
    }

    pub const fn replacement_range(&self) -> Option<TextRange> {
        self.replacement_range
    }

    pub const fn target(&self) -> Option<FileRange> {
        self.target
    }

    pub fn relevance(&self) -> u16 {
        self.relevance.score()
    }

    pub fn relevance_raw(&self) -> CompletionRelevance {
        self.relevance
    }

    pub const fn is_deprecated(&self) -> bool {
        self.deprecated
    }

    pub fn import_path(&self) -> Option<&str> {
        self.import_path.as_deref()
    }

    pub fn with_import_path(mut self, path: impl Into<String>) -> Self {
        self.import_path = Some(path.into());
        self
    }

    pub fn normalize(items: &mut Vec<Self>) {
        items.sort_by(|left, right| {
            Reverse(left.relevance.score())
                .cmp(&Reverse(right.relevance.score()))
                .then_with(|| left.label.cmp(&right.label))
                .then_with(|| left.kind.cmp(&right.kind))
                .then_with(|| left.detail.cmp(&right.detail))
                .then_with(|| left.insert.cmp(&right.insert))
                .then_with(|| left.replacement_range.cmp(&right.replacement_range))
                .then_with(|| Reverse(left.target.is_some()).cmp(&Reverse(right.target.is_some())))
                .then_with(|| left.target.cmp(&right.target))
                .then_with(|| Reverse(left.lookup.is_some()).cmp(&Reverse(right.lookup.is_some())))
                .then_with(|| left.lookup.cmp(&right.lookup))
                .then_with(|| {
                    Reverse(left.documentation.is_some())
                        .cmp(&Reverse(right.documentation.is_some()))
                })
                .then_with(|| left.documentation.cmp(&right.documentation))
                .then_with(|| left.deprecated.cmp(&right.deprecated))
        });

        let mut identities = BTreeSet::new();
        items.retain(|item| {
            identities.insert((
                item.label.clone(),
                item.kind,
                item.detail.clone(),
                item.insert.clone(),
                item.replacement_range,
            ))
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReferenceKind {
    Declaration,
    Read,
    Write,
}

impl ReferenceKind {
    const fn priority(self) -> u8 {
        match self {
            Self::Declaration => 0,
            Self::Write => 1,
            Self::Read => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReferenceResult {
    range: FileRange,
    kind: ReferenceKind,
}

impl ReferenceResult {
    pub const fn new(range: FileRange, kind: ReferenceKind) -> Self {
        Self { range, kind }
    }

    pub const fn range(self) -> FileRange {
        self.range
    }

    pub const fn kind(self) -> ReferenceKind {
        self.kind
    }

    pub fn normalize(references: &mut Vec<Self>) {
        references.sort_by_key(|reference| (reference.range, reference.kind.priority()));
        references.dedup_by_key(|reference| reference.range);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TextEdit {
    range: TextRange,
    new_text: String,
}

impl TextEdit {
    pub fn new(range: TextRange, new_text: impl Into<String>) -> Self {
        Self {
            range,
            new_text: new_text.into(),
        }
    }

    pub const fn range(&self) -> TextRange {
        self.range
    }

    pub fn new_text(&self) -> &str {
        &self.new_text
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEdit {
    file_id: FileId,
    edits: Vec<TextEdit>,
}

impl FileEdit {
    pub const fn file_id(&self) -> FileId {
        self.file_id
    }

    pub fn edits(&self) -> &[TextEdit] {
        &self.edits
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceChange {
    file_edits: Vec<FileEdit>,
}

impl SourceChange {
    pub fn from_edits(
        edits: impl IntoIterator<Item = (FileId, TextEdit)>,
        mut is_read_only: impl FnMut(FileId) -> bool,
    ) -> Result<Self, RenameError> {
        let mut edits = edits.into_iter().collect::<Vec<_>>();
        edits.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
        edits.dedup();

        let mut file_edits: Vec<FileEdit> = Vec::new();
        for (file_id, edit) in edits {
            if is_read_only(file_id) {
                return Err(RenameError::ReadOnly {
                    range: FileRange::new(file_id, edit.range),
                });
            }
            let group = match file_edits.last_mut() {
                Some(group) if group.file_id == file_id => group,
                _ => {
                    file_edits.push(FileEdit {
                        file_id,
                        edits: Vec::new(),
                    });
                    file_edits.last_mut().expect("file edit group was inserted")
                }
            };
            if let Some(previous) = group.edits.last()
                && ranges_overlap(previous.range, edit.range)
            {
                return Err(RenameError::ConflictingEdits {
                    first: FileRange::new(file_id, previous.range),
                    second: FileRange::new(file_id, edit.range),
                });
            }
            group.edits.push(edit);
        }
        Ok(Self { file_edits })
    }

    pub fn file_edits(&self) -> &[FileEdit] {
        &self.file_edits
    }

    pub fn is_empty(&self) -> bool {
        self.file_edits.is_empty()
    }
}

fn ranges_overlap(left: TextRange, right: TextRange) -> bool {
    if left.is_empty() && right.is_empty() {
        left.start() == right.start()
    } else if left.is_empty() {
        right.start() <= left.start() && left.start() < right.end()
    } else if right.is_empty() {
        left.start() <= right.start() && right.start() < left.end()
    } else {
        left.start() < right.end() && right.start() < left.end()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenameTarget {
    range: FileRange,
    placeholder: String,
}

impl RenameTarget {
    pub fn new(range: FileRange, placeholder: impl Into<String>) -> Self {
        Self {
            range,
            placeholder: placeholder.into(),
        }
    }

    pub const fn range(&self) -> FileRange {
        self.range
    }

    pub fn placeholder(&self) -> &str {
        &self.placeholder
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenameError {
    NoTarget,
    InvalidName { name: String },
    ReadOnly { range: FileRange },
    UnsupportedTarget,
    IncompleteReferenceSet,
    ConflictingEdits { first: FileRange, second: FileRange },
}

impl fmt::Display for RenameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoTarget => formatter.write_str("no rename target at this position"),
            Self::InvalidName { name } => write!(formatter, "invalid Rua identifier {name:?}"),
            Self::ReadOnly { .. } => formatter.write_str("rename would edit a read-only file"),
            Self::UnsupportedTarget => formatter.write_str("this symbol cannot be renamed"),
            Self::IncompleteReferenceSet => {
                formatter.write_str("rename cannot prove a complete reference set")
            }
            Self::ConflictingEdits { .. } => {
                formatter.write_str("rename produced conflicting edits")
            }
        }
    }
}

impl std::error::Error for RenameError {}

/// Protocol-neutral item for call hierarchy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallHierarchyItem {
    pub name: String,
    pub kind: crate::hir::DefKind,
    pub file_id: crate::vfs::FileId,
    pub range: TextRange,
}

/// Protocol-neutral item for type hierarchy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeHierarchyItem {
    pub name: String,
    pub kind: crate::hir::DefKind,
    pub file_id: crate::vfs::FileId,
    pub range: TextRange,
}

/// Protocol-neutral result for the `textDocument/signatureHelp` query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureHelpInfo {
    /// Full signature label, e.g. "fn(dx: i64, dy: i64) -> ()"
    pub label: String,
    /// Individual parameter type strings, e.g. ["i64", "i64"]
    pub parameters: Vec<String>,
    /// 0-based index of the active parameter.
    pub active_parameter: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SemanticTokenModifiers(u8);

impl SemanticTokenModifiers {
    pub const NONE: Self = Self(0);
    pub const DECLARATION: Self = Self(1 << 0);
    pub const READ_ONLY: Self = Self(1 << 1);
    pub const STATIC: Self = Self(1 << 2);
    pub const DEFAULT_LIBRARY: Self = Self(1 << 3);
    pub const UNUSED: Self = Self(1 << 4);
    pub const MUTABLE: Self = Self(1 << 5);

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn bits(self) -> u32 {
        self.0 as u32
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticTokenKind {
    Namespace,
    Type,
    Struct,
    Enum,
    Trait,
    Function,
    Method,
    Property,
    EnumMember,
    Variable,
    Parameter,
    Macro,
    Keyword,
    String,
    Number,
    Comment,
    Operator,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SemanticToken {
    range: FileRange,
    kind: SemanticTokenKind,
    modifiers: SemanticTokenModifiers,
}

impl SemanticToken {
    pub const fn new(
        range: FileRange,
        kind: SemanticTokenKind,
        modifiers: SemanticTokenModifiers,
    ) -> Self {
        assert!(!range.range.is_empty(), "semantic tokens must not be empty");
        Self {
            range,
            kind,
            modifiers,
        }
    }

    pub const fn file_range(self) -> FileRange {
        self.range
    }

    pub const fn file_id(self) -> FileId {
        self.range.file_id
    }

    pub const fn range(self) -> TextRange {
        self.range.range
    }

    pub const fn kind(self) -> SemanticTokenKind {
        self.kind
    }

    pub const fn modifiers(self) -> SemanticTokenModifiers {
        self.modifiers
    }

    pub const fn is_declaration(self) -> bool {
        self.modifiers.contains(SemanticTokenModifiers::DECLARATION)
    }

    pub fn normalize(tokens: &mut Vec<Self>) {
        tokens.sort_by_key(|token| (token.range, token.kind, token.modifiers));
        let mut merged: Vec<Self> = Vec::with_capacity(tokens.len());
        for token in tokens.drain(..) {
            if let Some(previous) = merged.last_mut()
                && (previous.range, previous.kind) == (token.range, token.kind)
            {
                previous.modifiers = previous.modifiers.union(token.modifiers);
            } else {
                merged.push(token);
            }
        }
        *tokens = merged;
    }
}
