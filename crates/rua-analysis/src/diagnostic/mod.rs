//! Unified protocol-neutral diagnostics and compiler-oracle reconciliation.

use std::sync::Arc;

use crate::{
    BaseDb,
    base::{FileRange, TextRange},
    hir::{
        Body, BodyResolution, BodySourceMap, Condition, DefKind, DefMap, Expr, InferenceDiagnostic,
        LocalBindingId, LocalResolveResult, LocalUseKind, Statement, TypeMismatchContext,
    },
    semantic::ReferenceIndex,
    vfs::{FileId, FileKind},
};
use rua_syntax::{
    AstNode, Named,
    ast::{Block as SyntaxBlock, Item as SyntaxItem},
};

pub use rua_core::{DiagnosticCode, DiagnosticSeverity};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiagnosticOrigin {
    FastAnalysis,
    Compiler,
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
    Lint,
    Type,
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
        left.range == right.range && left.code == right.code && left.source == right.source
    });
}

/// Suppress cascading noise: type errors on the same line as a parse error are
/// removed to avoid recovery artifacts.  Uses source text for precise line
/// matching instead of a byte-distance heuristic.
pub fn suppress_cascade(diagnostics: &mut Vec<Diagnostic>, text: &str) {
    // Build line-start offsets for line-of-byte-offset lookups.
    let mut line_starts: Vec<usize> = vec![0];
    for (i, &b) in text.as_bytes().iter().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }

    let line_of = |offset: u32| -> usize {
        let o = offset as usize;
        match line_starts.binary_search(&o) {
            Ok(line) => line,
            Err(line) => line.saturating_sub(1),
        }
    };

    let parse_error_lines: Vec<usize> = diagnostics
        .iter()
        .filter(|d| d.source == DiagnosticSource::Parse)
        .map(|d| line_of(d.range.range.start()))
        .collect();

    if parse_error_lines.is_empty() {
        return;
    }

    diagnostics.retain(|d| {
        if d.source == DiagnosticSource::Parse {
            return true;
        }
        !parse_error_lines
            .iter()
            .any(|&line| line == line_of(d.range.range.start()))
    });
}

// ---------------------------------------------------------------------------
// Per-layer diagnostic collection
// ---------------------------------------------------------------------------

/// Shorthand for creating a diagnostic originating from the fast-analysis
/// pipeline (lints, parse errors, type errors).
fn fast_diag(
    file_id: FileId,
    range: impl Into<TextRange>,
    message: impl Into<String>,
) -> Diagnostic {
    Diagnostic::new(
        file_id,
        range.into(),
        message,
        DiagnosticOrigin::FastAnalysis,
    )
    .with_source(DiagnosticSource::Lint)
}

pub(crate) fn fast_diagnostics(
    db: &Arc<BaseDb>,
    def_map: Arc<DefMap>,
    file_id: FileId,
) -> Vec<Diagnostic> {
    let Some(text) = db.file_text(file_id) else {
        return Vec::new();
    };
    let parse_diagnostics: Vec<Diagnostic> = db
        .parse(file_id)
        .errors()
        .iter()
        .map(|error| {
            let offset = error.offset.min(text.len()) as u32;
            fast_diag(
                file_id,
                TextRange::new(offset, offset),
                format!("parse error: {}", error.message),
            )
            .with_code(error.code)
            .with_source(DiagnosticSource::Parse)
        })
        .collect();

    let mut diagnostics = parse_diagnostics;
    if db.file_kind(file_id) == Some(FileKind::Declaration) {
        diagnostics.extend(declaration_file_diagnostics(db, file_id));
    }

    // Type diagnostics from inference.
    for definition in def_map.definitions() {
        if definition.file_id() != file_id {
            continue;
        }
        if !definition.kind().is_body_owner() {
            continue;
        }
        if let Some(source_map) = db.body_source_map(definition.id())
            && let Some(inference) = db.infer(definition.id())
        {
            for inf_diag in inference.diagnostics() {
                if let Some(diag) = convert_inference_diagnostic(file_id, inf_diag, &source_map) {
                    diagnostics.push(diag);
                }
            }
        }

        // Lint: unused variables and redundant-mut.
        if let Some(body) = db.body(definition.id())
            && let Some(source_map) = db.body_source_map(definition.id())
            && let Some(resolution) = db.body_resolution(definition.id())
        {
            for (binding_id, binding) in body.bindings() {
                // Skip wildcards, unnamed bindings, and the implicit
                // `self` receiver (which is semantically "used" by the
                // method contract even if never read explicitly).
                if binding
                    .name()
                    .is_none_or(|n| n.starts_with('_') || n == "self")
                {
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
                if !is_used && let Some(fr) = source_map.binding_range(binding_id) {
                    let name = binding.name().unwrap_or("?");
                    diagnostics.push(
                        fast_diag(file_id, fr.range, format!("unused variable `{name}`"))
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
                // &mut self method calls: `p.translate(…)` where translate takes
                // &mut self.  The name-ref to `p` is a Read in local use tracking,
                // but the method borrows p mutably, so `mut` is required.
                let has_mut_method_call = has_field_write
                    || (db.infer(definition.id()).is_some_and(|inference| {
                        body.exprs().any(|(_eid, expr)| {
                            let (receiver, method) = match expr {
                                crate::hir::Expr::MethodCall {
                                    receiver, method, ..
                                } => (*receiver, *method),
                                _ => return false,
                            };
                            // Check that the receiver path resolves to our binding.
                            let receiver_path = match body.expr(receiver) {
                                Some(crate::hir::Expr::Path(path)) if path.len() == 1 => path[0],
                                _ => return false,
                            };
                            if !matches!(
                                resolution.resolve(receiver_path),
                                Some(crate::hir::LocalResolveResult::Resolved(lid))
                                    if lid.binding() == binding_id
                            ) {
                                return false;
                            }
                            // Resolve the method to see if it takes &mut self.
                            let Some(receiver_ty) = inference.type_of_expr(receiver) else {
                                return false;
                            };
                            let Some(ref_info) = body.name_ref(method) else {
                                return false;
                            };
                            let Some(method_name) = ref_info.name() else {
                                return false;
                            };
                            let member_index = db.member_index(file_id);
                            let Some(method_res) =
                                member_index.resolve_method(receiver_ty, method_name)
                            else {
                                return false;
                            };
                            method_res.receiver() == Some(crate::hir::ReceiverKind::MutRef)
                        })
                    }));
                if !has_mut_method_call && let Some(fr) = source_map.binding_range(binding_id) {
                    diagnostics.push(
                        fast_diag(
                            file_id,
                            fr.range,
                            format!(
                                "redundant `mut` — `{}` is never assigned",
                                binding.name().unwrap_or("?")
                            ),
                        )
                        .with_code(DiagnosticCode::LintRedundantMut),
                    );
                }
            }

            add_control_flow_lints(file_id, &body, &source_map, &resolution, &mut diagnostics);
        }
    }

    let reference_index =
        ReferenceIndex::build_cancellable(Arc::clone(db), Arc::clone(&def_map), &mut || false)
            .unwrap_or_default();

    // Cross-file lint: unused private functions. A recursive call is owned by
    // the function itself and therefore does not make that function reachable.
    for definition in def_map.definitions() {
        if definition.kind() != DefKind::Function {
            continue;
        }
        if matches!(definition.visibility(), crate::hir::Visibility::Public) {
            continue;
        }
        let name = definition.name();
        let is_used = reference_index
            .occurrences(definition.id())
            .iter()
            .any(|occurrence| occurrence.owner() != definition.id());
        if !is_used {
            diagnostics.push(
                fast_diag(
                    definition.file_id(),
                    definition.name_range(),
                    format!("unused function `{name}`"),
                )
                .with_code(DiagnosticCode::LintUnusedFunction),
            );
        }
    }

    normalize_diagnostics(&mut diagnostics);
    suppress_cascade(&mut diagnostics, &text);
    diagnostics
}

fn declaration_file_diagnostics(db: &Arc<BaseDb>, file_id: FileId) -> Vec<Diagnostic> {
    let parsed = db.parse(file_id);
    let tree = &parsed.tree;
    let mut diagnostics = Vec::new();
    if let Some(statement) = tree.stmts().next() {
        diagnostics.push(invalid_declaration_diag(
            file_id,
            syntax_node_range(statement.syntax()),
            "declaration files cannot contain top-level executable statements",
        ));
    }
    collect_declaration_item_diagnostics(file_id, tree.items(), &mut diagnostics);
    diagnostics
}

fn collect_declaration_item_diagnostics(
    file_id: FileId,
    items: impl Iterator<Item = SyntaxItem>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for item in items {
        match item {
            SyntaxItem::Fn(function) => {
                if function.body().is_some_and(syntax_block_has_body) {
                    diagnostics.push(invalid_declaration_diag(
                        file_id,
                        named_range(&function),
                        format!(
                            "function `{}` in a declaration file must have an empty body",
                            function.name().map_or_else(
                                || "<missing>".to_string(),
                                |name| name.text().to_string(),
                            )
                        ),
                    ));
                }
            }
            SyntaxItem::Impl(implementation) => {
                for method in implementation.methods() {
                    if method.body().is_some_and(syntax_block_has_body) {
                        diagnostics.push(invalid_declaration_diag(
                            file_id,
                            named_range(&method),
                            format!(
                                "method `{}` in a declaration file must have an empty body",
                                method.name().map_or_else(
                                    || "<missing>".to_string(),
                                    |name| name.text().to_string(),
                                )
                            ),
                        ));
                    }
                }
            }
            SyntaxItem::Trait(trait_decl) => {
                for method in trait_decl.methods() {
                    if method.default_body().is_some_and(syntax_block_has_body) {
                        diagnostics.push(invalid_declaration_diag(
                            file_id,
                            named_range(&method),
                            format!(
                                "trait method `{}` in a declaration file must have an empty body",
                                method.name().map_or_else(
                                    || "<missing>".to_string(),
                                    |name| name.text().to_string(),
                                )
                            ),
                        ));
                    }
                }
            }
            SyntaxItem::Annotation(_)
            | SyntaxItem::Struct(_)
            | SyntaxItem::Enum(_)
            | SyntaxItem::Extern(_)
            | SyntaxItem::Use(_) => {}
        }
    }
}

fn syntax_block_has_body(block: SyntaxBlock) -> bool {
    block.stmts().next().is_some()
}

fn named_range(item: &impl Named) -> TextRange {
    item.name()
        .map(|name| rowan_range(name.text_range()))
        .unwrap_or_else(|| syntax_node_range(item.syntax()))
}

fn syntax_node_range(node: &rua_syntax::SyntaxNode) -> TextRange {
    rowan_range(node.text_range())
}

fn rowan_range(range: rowan::TextRange) -> TextRange {
    TextRange::new(range.start().into(), range.end().into())
}

fn invalid_declaration_diag(
    file_id: FileId,
    range: TextRange,
    message: impl Into<String>,
) -> Diagnostic {
    fast_diag(file_id, range, message)
        .with_code(DiagnosticCode::NameInvalidDeclaration)
        .with_source(DiagnosticSource::Parse)
}

fn add_control_flow_lints(
    file_id: FileId,
    body: &Body,
    source_map: &BodySourceMap,
    resolution: &BodyResolution,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let cfg = crate::hir::ControlFlowGraph::build(body);
    let mut unreachable = cfg
        .unreachable_statements()
        .filter_map(|statement_id| {
            let statement = statement_by_id(body, statement_id)?;
            Some((statement_range(statement, source_map)?, statement_id))
        })
        .collect::<Vec<_>>();
    unreachable.sort_by_key(|(range, _)| (range.start(), range.end()));
    for (range, _) in unreachable {
        diagnostics.push(
            fast_diag(file_id, range, "unreachable code")
                .with_code(DiagnosticCode::LintUnreachableCode),
        );
    }

    for loop_body in cfg.infinite_loops() {
        if let Some(range) = source_map.expr_range(loop_body) {
            diagnostics.push(
                fast_diag(
                    file_id,
                    range.range,
                    "`loop` without an exit may run forever",
                )
                .with_code(DiagnosticCode::LintInfiniteLoop),
            );
        }
    }

    for (_, expression) in body.exprs() {
        let Expr::Block(block) = expression else {
            continue;
        };
        for statement in block.statements() {
            let Statement::While {
                condition: Condition::Let { scrutinee, .. },
                body: loop_body,
            } = statement
            else {
                continue;
            };
            let Some(binding) = local_binding_for_expr(body, resolution, *scrutinee) else {
                continue;
            };
            let Some(loop_range) = source_map.expr_range(*loop_body) else {
                continue;
            };
            let updated = resolution.uses_for(binding).any(|local_use| {
                local_use.kind() == LocalUseKind::Write
                    && source_map
                        .name_ref_range(local_use.name_ref())
                        .is_some_and(|range| {
                            range.file_id == loop_range.file_id
                                && loop_range.range.contains_range(range.range)
                        })
            });
            if !updated && let Some(range) = source_map.expr_range(*scrutinee) {
                diagnostics.push(
                    fast_diag(
                        file_id,
                        range.range,
                        "`while let` scrutinee is never updated in the loop body",
                    )
                    .with_code(DiagnosticCode::LintInfiniteLoop),
                );
            }
        }
    }
}

fn statement_by_id(body: &Body, statement: crate::hir::StatementId) -> Option<&Statement> {
    let Expr::Block(block) = body.expr(statement.block())? else {
        return None;
    };
    block.statements().get(statement.index())
}

fn local_binding_for_expr(
    body: &Body,
    resolution: &BodyResolution,
    expression: crate::hir::ExprId,
) -> Option<LocalBindingId> {
    let Expr::Path(path) = body.expr(expression)? else {
        return None;
    };
    let name_ref = path.last().copied()?;
    match resolution.resolve(name_ref)? {
        LocalResolveResult::Resolved(binding) => Some(binding),
        LocalResolveResult::Ambiguous | LocalResolveResult::NonLocal => None,
    }
}

fn statement_range(statement: &Statement, source_map: &BodySourceMap) -> Option<TextRange> {
    let range = match statement {
        Statement::Let { binding, .. } | Statement::For { binding, .. } => {
            source_map.binding_range(*binding)?
        }
        Statement::Expr { expr, .. } => source_map.expr_range(*expr)?,
        Statement::Return { value: Some(expr) } => source_map.expr_range(*expr)?,
        Statement::While { body, .. } | Statement::Loop { body } => source_map.expr_range(*body)?,
        Statement::Return { value: None }
        | Statement::Missing
        | Statement::Break { value: None }
        | Statement::Continue => return None,
        Statement::Break { value: Some(expr) } => source_map.expr_range(*expr)?,
    };
    Some(range.range)
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
            let range = expr_range(*expr, source_map)?;
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
            let range = expr_range(*call, source_map)?;
            (
                DiagnosticCode::TypeArgumentCount,
                format!("expected {expected} arguments, found {actual}"),
                range,
            )
        }
        InferenceDiagnostic::NotCallable { callee, actual } => {
            let range = expr_range(*callee, source_map)?;
            (
                DiagnosticCode::TypeNotCallable,
                format!("`{actual}` is not callable"),
                range,
            )
        }
        InferenceDiagnostic::NotIterable { expr, actual } => {
            let range = expr_range(*expr, source_map)?;
            (
                DiagnosticCode::TypeNotIterable,
                format!("`{actual}` is not iterable"),
                range,
            )
        }
        InferenceDiagnostic::InvalidUnary { expr, operand, op } => {
            let range = expr_range(*expr, source_map)?;
            (
                DiagnosticCode::TypeInvalidUnary,
                format!("cannot apply unary `{op:?}` to `{operand}`"),
                range,
            )
        }
        InferenceDiagnostic::InvalidBinary { expr, lhs, rhs, op } => {
            let range = expr_range(*expr, source_map)?;
            (
                DiagnosticCode::TypeInvalidBinary,
                format!(
                    "cannot apply binary `{}` to `{lhs}` and `{rhs}`",
                    op.symbol()
                ),
                range,
            )
        }
        InferenceDiagnostic::InvalidTry { expr, found } => {
            let range = expr_range(*expr, source_map)?;
            (
                DiagnosticCode::TypeInvalidTry,
                format!("`?` operator requires `Result` or `Option`, found `{found}`"),
                range,
            )
        }
        InferenceDiagnostic::InvalidBreakValue { expr } => {
            let range = expr_range(*expr, source_map)?;
            (
                DiagnosticCode::TypeInvalidBreak,
                "`break` with a value is only allowed in `loop`".to_string(),
                range,
            )
        }
        InferenceDiagnostic::InvalidOptionalChain { expr, found } => {
            let range = expr_range(*expr, source_map)?;
            (
                DiagnosticCode::TypeInvalidOptionalChain,
                format!("optional chaining requires `Option`, found `{found}`"),
                range,
            )
        }
        InferenceDiagnostic::UnsatisfiedTraitBound {
            call,
            actual,
            trait_id: _,
        } => {
            let range = expr_range(*call, source_map)?;
            (
                DiagnosticCode::TypeUnsatisfiedTraitBound,
                format!("`{actual}` does not satisfy required trait bound"),
                range,
            )
        }
        InferenceDiagnostic::ImmutableAssignment {
            target,
            binding: _,
            name,
        } => {
            let range = expr_range(*target, source_map)?;
            (
                DiagnosticCode::TypeImmutableAssignment,
                format!("cannot assign to immutable binding `{name}`"),
                range,
            )
        }
    };
    Some(
        fast_diag(file_id, range, message)
            .with_code(code)
            .with_source(DiagnosticSource::Type),
    )
}

fn inference_source_range(
    _file_id: FileId,
    source: crate::hir::InferenceSource,
    source_map: &crate::hir::BodySourceMap,
) -> Option<TextRange> {
    match source {
        crate::hir::InferenceSource::Expr(expr) => expr_range(expr, source_map),
        crate::hir::InferenceSource::Binding(binding) => {
            source_map.binding_range(binding).map(|fr| fr.range)
        }
        crate::hir::InferenceSource::Pattern(pat) => source_map.pat_range(pat).map(|fr| fr.range),
    }
}

fn expr_range(
    expr: crate::hir::ExprId,
    source_map: &crate::hir::BodySourceMap,
) -> Option<TextRange> {
    source_map.expr_range(expr).map(|fr| fr.range)
}

fn mismatch_context_label(context: TypeMismatchContext) -> std::borrow::Cow<'static, str> {
    match context {
        TypeMismatchContext::Annotation => " in let annotation".into(),
        TypeMismatchContext::Return => " in return position".into(),
        TypeMismatchContext::Assignment => " in assignment".into(),
        TypeMismatchContext::Argument { index } => {
            std::borrow::Cow::Owned(format!(" in argument {}", index + 1))
        }
        TypeMismatchContext::ClosureReturn => " in closure return".into(),
        TypeMismatchContext::Branch => " in branch".into(),
        TypeMismatchContext::RangeBound => " in range bound".into(),
        TypeMismatchContext::Index => " in index".into(),
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
