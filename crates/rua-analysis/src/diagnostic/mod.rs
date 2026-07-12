//! Unified protocol-neutral diagnostics and compiler-oracle reconciliation.

use std::fmt;

use crate::{
    BaseDb,
    base::{FileRange, TextRange},
    hir::{DefKind, InferenceDiagnostic, TypeMismatchContext},
    vfs::FileId,
};

// ---------------------------------------------------------------------------
// DiagnosticCode — stable numeric identifiers
// ---------------------------------------------------------------------------

/// Stable analysis-owned diagnostic identifier with structured numeric codes.
///
/// Ranges:
/// - Parse errors:   0001–0099
/// - Name errors:    0100–0199
/// - Type errors:    0200–0299
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiagnosticCode {
    // Parse errors (0001–0099)
    ParseUnexpectedToken = 1,
    ParseUnterminatedString = 2,
    ParseUnterminatedComment = 3,
    ParseExpectedItem = 4,
    ParseMissingDelimiter = 5,

    // Name resolution errors (0100–0199)
    NameUnresolved = 100,
    NameDuplicateDefinition = 101,
    NamePrivateAccess = 102,
    NameModuleNotFound = 103,
    NameAmbiguousImport = 104,

    // Type errors (0200–0299)
    TypeMismatch = 200,
    TypeExpectedBool = 201,
    TypeNotCallable = 202,
    TypeArgumentCount = 203,
    TypeNotIterable = 204,
    TypeInvalidUnary = 205,
    TypeInvalidBinary = 206,
    TypeUnsatisfiedTraitBound = 207,
    TypeUnknownField = 208,
    TypeUnknownMethod = 209,
    TypeMissingMatchArm = 210,

    // Lint warnings (0300–0399)
    LintUnusedVariable = 300,
    LintRedundantMut = 301,
    LintUnreachableCode = 302,
    LintUnusedFunction = 303,
}

impl DiagnosticCode {
    /// Stable string identifier, e.g. `"E0001"`.
    pub fn error_code(self) -> &'static str {
        match self {
            Self::ParseUnexpectedToken => "E0001",
            Self::ParseUnterminatedString => "E0002",
            Self::ParseUnterminatedComment => "E0003",
            Self::ParseExpectedItem => "E0004",
            Self::ParseMissingDelimiter => "E0005",
            Self::NameUnresolved => "E0100",
            Self::NameDuplicateDefinition => "E0101",
            Self::NamePrivateAccess => "E0102",
            Self::NameModuleNotFound => "E0103",
            Self::NameAmbiguousImport => "E0104",
            Self::TypeMismatch => "E0200",
            Self::TypeExpectedBool => "E0201",
            Self::TypeNotCallable => "E0202",
            Self::TypeArgumentCount => "E0203",
            Self::TypeNotIterable => "E0204",
            Self::TypeInvalidUnary => "E0205",
            Self::TypeInvalidBinary => "E0206",
            Self::TypeUnsatisfiedTraitBound => "E0207",
            Self::TypeUnknownField => "E0208",
            Self::TypeUnknownMethod => "E0209",
            Self::TypeMissingMatchArm => "E0210",
            Self::LintUnusedVariable => "W0300",
            Self::LintRedundantMut => "W0301",
            Self::LintUnreachableCode => "W0302",
            Self::LintUnusedFunction => "W0303",
        }
    }

    pub fn severity(self) -> DiagnosticSeverity {
        match self {
            Self::LintUnusedVariable
            | Self::LintRedundantMut
            | Self::LintUnreachableCode
            | Self::LintUnusedFunction => DiagnosticSeverity::Warning,
            Self::ParseUnexpectedToken
            | Self::ParseUnterminatedString
            | Self::ParseUnterminatedComment
            | Self::ParseExpectedItem
            | Self::ParseMissingDelimiter
            | Self::NameUnresolved
            | Self::NameDuplicateDefinition
            | Self::NamePrivateAccess
            | Self::NameModuleNotFound
            | Self::NameAmbiguousImport
            | Self::TypeMismatch
            | Self::TypeExpectedBool
            | Self::TypeNotCallable
            | Self::TypeArgumentCount
            | Self::TypeNotIterable
            | Self::TypeInvalidUnary
            | Self::TypeInvalidBinary
            | Self::TypeUnsatisfiedTraitBound
            | Self::TypeUnknownField
            | Self::TypeUnknownMethod
            | Self::TypeMissingMatchArm => DiagnosticSeverity::Error,
        }
    }
}

impl fmt::Display for DiagnosticCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.error_code())
    }
}

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiagnosticOrigin {
    FastAnalysis,
    Compiler,
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

/// Which analysis layer produced this diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiagnosticSource {
    Parse,
    Name,
    Type,
    Structural,
}

// ---------------------------------------------------------------------------
// Diagnostic
// ---------------------------------------------------------------------------

/// Protocol-neutral diagnostic result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    range: FileRange,
    message: String,
    code: Option<DiagnosticCode>,
    severity: DiagnosticSeverity,
    related: Vec<DiagnosticRelated>,
    origin: DiagnosticOrigin,
    source: DiagnosticSource,
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
            source: DiagnosticSource::Parse,
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

    pub const fn with_source(mut self, source: DiagnosticSource) -> Self {
        self.source = source;
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

    pub fn code(&self) -> Option<DiagnosticCode> {
        self.code
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

    pub const fn source(&self) -> DiagnosticSource {
        self.source
    }
}

// ---------------------------------------------------------------------------
// Normalization and suppression
// ---------------------------------------------------------------------------

pub fn normalize_diagnostics(diagnostics: &mut Vec<Diagnostic>) {
    diagnostics.sort_by(|left, right| {
        (
            left.range,
            left.severity,
            left.code,
            left.source,
            &left.message,
            &left.related,
            left.origin,
        )
            .cmp(&(
                right.range,
                right.severity,
                right.code,
                right.source,
                &right.message,
                &right.related,
                right.origin,
            ))
    });
    diagnostics.dedup_by(|left, right| {
        left.range == right.range
            && left.code == right.code
            && left.source == right.source
    });
}

/// Suppress cascading noise: type errors on the same line as a parse error are
/// downgraded or removed to avoid recovery artifacts.
pub fn suppress_cascade(diagnostics: &mut Vec<Diagnostic>) {
    let parse_error_lines: Vec<u32> = diagnostics
        .iter()
        .filter(|d| d.source == DiagnosticSource::Parse)
        .map(|d| d.range.range.start())
        .collect();

    if parse_error_lines.is_empty() {
        return;
    }

    diagnostics.retain(|d| {
        if d.source == DiagnosticSource::Parse {
            return true;
        }
        // Keep type/name diagnostics that are not on parse-error lines.
        let start = d.range.range.start();
        !parse_error_lines.iter().any(|line| {
            // Approximate: same offset region (within 100 bytes).
            let diff = if start > *line { start - line } else { line - start };
            diff < 100
        })
    });
}

// ---------------------------------------------------------------------------
// Per-layer diagnostic collection
// ---------------------------------------------------------------------------

pub(crate) fn fast_diagnostics(db: &BaseDb, file_id: FileId) -> Vec<Diagnostic> {
    let Some(text) = db.file_text(file_id) else {
        return Vec::new();
    };
    let parse_diagnostics: Vec<Diagnostic> = db
        .parse(file_id)
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
            .with_code(parse_error_code(&error.message))
            .with_source(DiagnosticSource::Parse)
        })
        .collect();

    let mut diagnostics = parse_diagnostics;

    // Type diagnostics from inference.
    let def_map = db.def_map(file_id);
    for definition in def_map.definitions() {
        if definition.file_id() != file_id {
            continue;
        }
        if !matches!(definition.kind(), DefKind::Function | DefKind::Method) {
            continue;
        }
        if let Some(source_map) = db.body_source_map(definition.id())
            && let Some(inference) = db.infer(definition.id()) {
                for inf_diag in inference.diagnostics() {
                    if let Some(diag) =
                        convert_inference_diagnostic(file_id, inf_diag, &source_map)
                    {
                        diagnostics.push(diag);
                    }
                }
            }

        // Lint: unused variables.
        if let Some(body) = db.body(definition.id())
            && let Some(source_map) = db.body_source_map(definition.id())
            && let Some(resolution) = db.body_resolution(definition.id())
        {
            for (binding_id, binding) in body.bindings() {
                // Skip wildcards and unnamed bindings.
                if binding.name().is_none_or(|n| n.starts_with('_')) {
                    continue;
                }
                // Check if any name ref resolves to this binding.
                let is_used = body.name_refs().any(|(name_ref_id, _nr)| {
                    matches!(
                        resolution.resolve(name_ref_id),
                        Some(crate::hir::LocalResolveResult::Resolved(lid))
                            if lid.binding() == binding_id
                    )
                });
                if !is_used
                    && let Some(fr) = source_map.binding_range(binding_id)
                {
                    let name = binding.name().unwrap_or("?");
                    diagnostics.push(
                        Diagnostic::new(
                            file_id,
                            fr.range,
                            format!("unused variable `{name}`"),
                            DiagnosticOrigin::FastAnalysis,
                        )
                        .with_code(DiagnosticCode::LintUnusedVariable),
                    );
                }
            }

            // Redundant mut: binding is mutable but has no write uses.
            for (binding_id, binding) in body.bindings() {
                if !binding.is_mutable() {
                    continue;
                }
                if binding.name().is_none_or(|n| n.starts_with('_')) {
                    continue;
                }
                let lid = crate::hir::LocalBindingId::new(body.id(), binding_id);
                // Direct writes: `binding = value`
                let has_write = resolution
                    .uses_for(lid)
                    .any(|u| u.kind() == crate::hir::LocalUseKind::Write);
                // Field/index writes: `binding.field = value` or `binding[i] = value`
                // These require `mut` on the binding even though the binding
                // itself isn't reassigned — the mutation goes through it.
                let has_field_write = has_write
                    || body.exprs().any(|(_eid, expr)| {
                        let mut current = match expr {
                            crate::hir::Expr::Assign { target, .. } => *target,
                            _ => return false,
                        };
                        // Walk through nested field/index exprs to find the
                        // root path: e.g. self.x.y = v → Field(Field(Path(self), x), y)
                        let name_ref = loop {
                            match body.expr(current) {
                                Some(crate::hir::Expr::Field { base, .. })
                                | Some(crate::hir::Expr::Index { base, .. }) => current = *base,
                                Some(crate::hir::Expr::Path(path)) if path.len() == 1 => {
                                    break Some(path[0]);
                                }
                                _ => break None,
                            }
                        };
                        let Some(nr) = name_ref else { return false };
                        matches!(
                            resolution.resolve(nr),
                            Some(crate::hir::LocalResolveResult::Resolved(lid))
                                if lid.binding() == binding_id
                        )
                    });
                if !has_field_write
                    && let Some(fr) = source_map.binding_range(binding_id)
                {
                    diagnostics.push(
                        Diagnostic::new(
                            file_id,
                            fr.range,
                            format!(
                                "redundant `mut` — `{}` is never assigned",
                                binding.name().unwrap_or("?")
                            ),
                            DiagnosticOrigin::FastAnalysis,
                        )
                        .with_code(DiagnosticCode::LintRedundantMut),
                    );
                }
            }
        }
    }

    // Cross-file lint: unused functions. Skip if there's only one function
    // in the project (likely a test/entry point).
    let total_defs = def_map
        .definitions()
        .filter(|d| matches!(d.kind(), DefKind::Function | DefKind::Method))
        .count();
    if total_defs > 1 {
        for definition in def_map.definitions() {
        if definition.kind() != DefKind::Function {
            continue;
        }
        if matches!(
            definition.visibility(),
            crate::hir::Visibility::Public
        ) || definition.name() == "main" {
            continue;
        }
        // Check if any other body references this function by name.
        let name = definition.name();
        let is_referenced = def_map.definitions().any(|d| {
            if !matches!(d.kind(), DefKind::Function | DefKind::Method) {
                return false;
            }
            let Some(body) = db.body(d.id()) else { return false };
            body.name_refs()
                .any(|(_nrid, nr)| nr.name() == Some(name))
        });
        if !is_referenced {
            diagnostics.push(
                Diagnostic::new(
                    definition.file_id(),
                    definition.name_range(),
                    format!("unused function `{name}`"),
                    DiagnosticOrigin::FastAnalysis,
                )
                .with_code(DiagnosticCode::LintUnusedFunction),
            );
        }
        }
    }

    // Per-file lint: unreachable code after return/break/continue.
    if let Some(ref text) = db.file_text(file_id) {
        for (line_idx, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            // Find semicolons after return/break/continue on the same line.
            for keyword in &["return", "break", "continue"] {
                if let Some(pos) = trimmed.find(keyword) {
                    let after = trimmed[pos + keyword.len()..].trim();
                    if let Some(semi_pos) = after.find(';') {
                        let rest = after[semi_pos + 1..].trim();
                        if !rest.is_empty() && !rest.starts_with("//") {
                            let line_offset =
                                text.lines().take(line_idx).map(|l| l.len() + 1).sum::<usize>();
                            let byte_offset = line_offset + pos
                                + keyword.len()
                                + after[..semi_pos].len();
                            let end_offset =
                                byte_offset + 1 + rest.len() + (line.len() - trimmed.len());
                            diagnostics.push(
                                Diagnostic::new(
                                    file_id,
                                    TextRange::new(
                                        byte_offset as u32,
                                        end_offset.min(text.len()) as u32,
                                    ),
                                    format!("unreachable code after `{keyword}`"),
                                    DiagnosticOrigin::FastAnalysis,
                                )
                                .with_code(DiagnosticCode::LintUnreachableCode),
                            );
                        }
                        break; // one diag per line
                    }
                }
            }
        }
    }

    normalize_diagnostics(&mut diagnostics);
    suppress_cascade(&mut diagnostics);
    diagnostics
}

fn convert_inference_diagnostic(
    file_id: FileId,
    inf_diag: &InferenceDiagnostic,
    source_map: &crate::hir::BodySourceMap,
) -> Option<Diagnostic> {
    let (code, message, range) = match inf_diag {
        InferenceDiagnostic::TypeMismatch {
            source,
            expected,
            actual,
            context,
        } => {
            let range = inference_source_range(file_id, *source, source_map)?;
            let ctx_str = mismatch_context_label(*context);
            (
                DiagnosticCode::TypeMismatch,
                format!("type mismatch: expected `{expected}`, found `{actual}`{ctx_str}"),
                range,
            )
        }
        InferenceDiagnostic::ExpectedBool { expr, actual } => {
            let range = expr_range(file_id, *expr, source_map)?;
            (
                DiagnosticCode::TypeExpectedBool,
                format!("expected `bool`, found `{actual}`"),
                range,
            )
        }
        InferenceDiagnostic::ArgumentCount {
            call,
            expected,
            actual,
        } => {
            let range = expr_range(file_id, *call, source_map)?;
            (
                DiagnosticCode::TypeArgumentCount,
                format!("expected {expected} arguments, found {actual}"),
                range,
            )
        }
        InferenceDiagnostic::NotCallable { callee, actual } => {
            let range = expr_range(file_id, *callee, source_map)?;
            (
                DiagnosticCode::TypeNotCallable,
                format!("`{actual}` is not callable"),
                range,
            )
        }
        InferenceDiagnostic::NotIterable { expr, actual } => {
            let range = expr_range(file_id, *expr, source_map)?;
            (
                DiagnosticCode::TypeNotIterable,
                format!("`{actual}` is not iterable"),
                range,
            )
        }
        InferenceDiagnostic::InvalidUnary { expr, operand, op } => {
            let range = expr_range(file_id, *expr, source_map)?;
            (
                DiagnosticCode::TypeInvalidUnary,
                format!("cannot apply unary `{op:?}` to `{operand}`"),
                range,
            )
        }
        InferenceDiagnostic::InvalidBinary { expr, lhs, rhs, op } => {
            let range = expr_range(file_id, *expr, source_map)?;
            (
                DiagnosticCode::TypeInvalidBinary,
                format!("cannot apply binary `{op:?}` to `{lhs}` and `{rhs}`"),
                range,
            )
        }
        InferenceDiagnostic::UnsatisfiedTraitBound {
            call,
            actual,
            trait_id: _,
        } => {
            let range = expr_range(file_id, *call, source_map)?;
            (
                DiagnosticCode::TypeUnsatisfiedTraitBound,
                format!("`{actual}` does not satisfy required trait bound"),
                range,
            )
        }
    };
    Some(
        Diagnostic::new(file_id, range, message, DiagnosticOrigin::FastAnalysis)
            .with_code(code)
            .with_source(DiagnosticSource::Type),
    )
}

fn inference_source_range(
    file_id: FileId,
    source: crate::hir::InferenceSource,
    source_map: &crate::hir::BodySourceMap,
) -> Option<TextRange> {
    match source {
        crate::hir::InferenceSource::Expr(expr) => expr_range(file_id, expr, source_map),
        crate::hir::InferenceSource::Binding(binding) => source_map
            .binding_range(binding)
            .map(|fr| fr.range),
        crate::hir::InferenceSource::Pattern(pat) => {
            source_map.pat_range(pat).map(|fr| fr.range)
        }
    }
}

fn expr_range(
    _file_id: FileId,
    expr: crate::hir::ExprId,
    source_map: &crate::hir::BodySourceMap,
) -> Option<TextRange> {
    source_map.expr_range(expr).map(|fr| fr.range)
}

fn mismatch_context_label(context: TypeMismatchContext) -> &'static str {
    match context {
        TypeMismatchContext::Annotation => " in let annotation",
        TypeMismatchContext::Return => " in return position",
        TypeMismatchContext::Assignment => " in assignment",
        TypeMismatchContext::Argument { .. } => " in argument",
        TypeMismatchContext::ClosureReturn => " in closure return",
        TypeMismatchContext::Branch => " in branch",
        TypeMismatchContext::RangeBound => " in range bound",
        TypeMismatchContext::Index => " in index",
    }
}

fn parse_error_code(message: &str) -> DiagnosticCode {
    if message.contains("unterminated") {
        DiagnosticCode::ParseUnterminatedString
    } else if message.contains("expected") {
        DiagnosticCode::ParseExpectedItem
    } else if message.contains("unexpected") {
        DiagnosticCode::ParseUnexpectedToken
    } else {
        DiagnosticCode::ParseExpectedItem
    }
}

// ---------------------------------------------------------------------------
// Compiler reconciliation (parity-test only, not production hot path)
// ---------------------------------------------------------------------------

/// Reconcile speculative fast diagnostics with the authoritative compiler
/// result. Compiler diagnostics take priority for same-location diagnostics.
pub fn reconcile_diagnostics(fast: Vec<Diagnostic>, compiler: Vec<Diagnostic>) -> Vec<Diagnostic> {
    if compiler.is_empty() {
        return fast;
    }
    // Merge: compiler diagnostics override fast diagnostics at the same location.
    let mut result: Vec<Diagnostic> = fast
        .into_iter()
        .filter(|f| {
            !compiler
                .iter()
                .any(|c| c.range == f.range && c.code == f.code)
        })
        .collect();
    result.extend(compiler);
    normalize_diagnostics(&mut result);
    result
}
