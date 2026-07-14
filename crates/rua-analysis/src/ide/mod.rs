//! Snapshot-based IDE API, including `AnalysisHost` and feature queries.
//!
//! Results exposed here remain independent of LSP protocol types.

mod closure_iterator;
mod completion;
mod contract;
mod symbol;

use std::sync::Arc;

use rua_syntax::{Parse, ast::SourceFile};

use crate::{
    BaseDb,
    hir::{
        Body, BodyResolution, BodyScopes, BodySourceId, BodySourceMap, BuiltinMemberId, DefId,
        DefKind, DefMap, Definition, InferenceResult, ItemTree, MemberIndex, MemberResolution,
        MemberTarget, Ty,
        module_resolution::{resolve_module_file, resolve_module_file_in_project_at},
    },
    semantic::Semantics,
    vfs::{Change, FileId, FileKind, SourceRootKind, VfsPath},
};

pub use crate::diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticOrigin, DiagnosticRelated, DiagnosticSeverity,
};
pub use closure_iterator::ClosureParameterInfo;
pub use contract::{
    BuiltinDefinitionTarget, CallHierarchyItem, CompletionInsert, CompletionItem, CompletionKind,
    CompletionRelevance, FileEdit, FilePosition, FileRange, HoverResult, MacroDelimiter,
    NavigationTarget, ProjectFile, ProjectId, ProjectPosition, QueryContext, ReferenceKind,
    ReferenceResult, RenameError, RenameTarget, SemanticToken, SemanticTokenKind,
    SemanticTokenModifiers, SignatureHelpInfo, SourceChange, TextEdit, TextRange,
    TypeHierarchyItem, TypeHint,
};
pub use symbol::{DocumentSymbol, WorkspaceSymbol};

/// Mutable owner of the current analysis inputs.
#[derive(Debug, Default)]
pub struct AnalysisHost {
    db: Arc<BaseDb>,
}

impl AnalysisHost {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply_change(&mut self, change: Change) {
        Arc::make_mut(&mut self.db).apply_change(change);
    }

    pub fn analysis(&self) -> Analysis {
        Analysis {
            db: Arc::clone(&self.db),
        }
    }
}

/// Immutable view of the inputs captured when the snapshot was created.
#[derive(Clone, Debug)]
pub struct Analysis {
    db: Arc<BaseDb>,
}

struct ProjectQueryData {
    def_map: Arc<DefMap>,
    member_index: Arc<MemberIndex>,
}

struct BuiltinMemberAt {
    id: BuiltinMemberId,
    resolution: MemberResolution,
    range: FileRange,
}

struct BuiltinSourceDefinition {
    range: TextRange,
    documentation: Option<String>,
}

fn builtin_source_definition(id: BuiltinMemberId) -> Option<BuiltinSourceDefinition> {
    let source = rua_core::BUILTIN_SOURCES
        .iter()
        .find(|source| source.name == id.source_name())?;
    let parse = rua_syntax::parse(source.text);
    let token = parse
        .syntax_node()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| token.kind() == rua_syntax::SyntaxKind::Ident && token.text() == id.name())?;
    let raw_range = token.text_range();
    let range = TextRange::new(raw_range.start().into(), raw_range.end().into());
    let line_start = source.text[..range.start() as usize]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let mut documentation = Vec::new();
    for line in source.text[..line_start].lines().rev() {
        let Some(line) = line.trim_start().strip_prefix("///") else {
            break;
        };
        documentation.push(line.trim_start().to_string());
    }
    documentation.reverse();
    Some(BuiltinSourceDefinition {
        range,
        documentation: (!documentation.is_empty()).then(|| documentation.join("\n")),
    })
}

fn builtin_hover_text(member: &BuiltinMemberAt) -> String {
    let resolution = &member.resolution;
    if let Some(callable) = resolution.callable() {
        let mut params = callable
            .params()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        if let Some(receiver) = resolution.receiver() {
            params.insert(
                0,
                match receiver {
                    crate::hir::ReceiverKind::Value => "self",
                    crate::hir::ReceiverKind::SharedRef => "&self",
                    crate::hir::ReceiverKind::MutRef => "&mut self",
                }
                .to_string(),
            );
        }
        format!(
            "fn {}({}) -> {}",
            member.id.name(),
            params.join(", "),
            callable.return_ty()
        )
    } else {
        format!("{}: {}", member.id.name(), resolution.ty())
    }
}

fn semantic_query_data(db: &Arc<BaseDb>, position: ProjectPosition) -> Option<ProjectQueryData> {
    if let Some(def_map) = db.project_def_map(position.project_id)
        && def_map.module_for_file(position.position.file_id).is_some()
    {
        let member_index = db.project_member_index(position.project_id)?;
        return Some(ProjectQueryData {
            def_map,
            member_index,
        });
    }

    // A workspace may contain independent Rua files that are not reachable
    // from its selected project root. Keep those files semantically useful
    // by analyzing the current file as a standalone root.
    let def_map = db.def_map(position.position.file_id);
    def_map.module_for_file(position.position.file_id)?;
    let member_index = db.member_index(position.position.file_id);
    Some(ProjectQueryData {
        def_map,
        member_index,
    })
}

impl Analysis {
    fn project_query_data(&self, position: ProjectPosition) -> Option<ProjectQueryData> {
        semantic_query_data(&self.db, position)
    }

    fn builtin_member_at(
        &self,
        position: ProjectPosition,
        query: &ProjectQueryData,
    ) -> Option<BuiltinMemberAt> {
        let owner = completion::innermost_body_owner(
            &query.def_map,
            position.position,
            position.position.offset,
        )?;
        let source_map = self.db.body_source_map(owner.id())?;
        let inference = self.db.infer(owner.id())?;
        for source_id in source_map.ids_at(position.position.file_id, position.position.offset) {
            let BodySourceId::NameRef(name_ref) = source_id else {
                continue;
            };
            let Some(resolution) = inference.member_resolution(name_ref).cloned() else {
                continue;
            };
            let MemberTarget::Builtin(id) = resolution.target() else {
                continue;
            };
            return Some(BuiltinMemberAt {
                id,
                resolution,
                range: source_map.name_ref_range(name_ref)?,
            });
        }
        None
    }

    /// Test and integration helper for syntax queries during Phase 2.
    pub fn parse(&self, file_id: FileId) -> Arc<Parse<SourceFile>> {
        self.db.parse(file_id)
    }

    pub fn file_text(&self, file_id: FileId) -> Option<Arc<str>> {
        self.db.file_text(file_id)
    }

    pub fn file_revision(&self, file_id: FileId) -> Option<u64> {
        self.db.file_revision(file_id)
    }

    #[doc(hidden)]
    pub fn query_stats(&self) -> crate::QueryStats {
        self.db.query_stats()
    }

    #[doc(hidden)]
    pub fn cache_sizes(&self) -> crate::CacheSizes {
        self.db.cache_sizes()
    }

    pub fn diagnostics(&self, file_id: FileId) -> Vec<Diagnostic> {
        crate::diagnostic::fast_diagnostics(&self.db, self.db.def_map(file_id), file_id)
    }

    pub fn diagnostics_in_project(&self, file: ProjectFile) -> Vec<Diagnostic> {
        let Some(def_map) = self.db.project_def_map(file.project_id) else {
            return Vec::new();
        };
        if def_map.module_for_file(file.file_id).is_none() {
            return Vec::new();
        }
        crate::diagnostic::fast_diagnostics(&self.db, def_map, file.file_id)
    }

    pub fn item_tree(&self, file_id: FileId) -> Arc<ItemTree> {
        self.db.item_tree(file_id)
    }

    pub fn resolve_module(&self, from_file: FileId, name: &str) -> Option<FileId> {
        resolve_module_file(&self.db, from_file, name)
    }

    pub fn resolve_module_in_project(
        &self,
        project_id: ProjectId,
        from_file: FileId,
        name: &str,
    ) -> Option<FileId> {
        let map = self.db.project_def_map(project_id)?;
        let module = map.module(map.module_for_file(from_file)?)?;
        resolve_module_file_in_project_at(
            &self.db,
            project_id,
            from_file,
            module.resolution_directory()?,
            name,
        )
    }

    pub fn file_path(&self, file_id: FileId) -> Option<&VfsPath> {
        self.db.file_path(file_id)
    }

    pub fn def_map(&self, root_file: FileId) -> Arc<DefMap> {
        self.db.def_map(root_file)
    }

    pub fn def_map_for_project(&self, project_id: ProjectId) -> Option<Arc<DefMap>> {
        self.db.project_def_map(project_id)
    }

    pub fn member_index(&self, root_file: FileId) -> Arc<MemberIndex> {
        self.db.member_index(root_file)
    }

    pub fn member_index_for_project(&self, project_id: ProjectId) -> Option<Arc<MemberIndex>> {
        self.db.project_member_index(project_id)
    }

    pub fn reference_index_for_project(
        &self,
        project_id: ProjectId,
    ) -> Option<Arc<crate::semantic::ReferenceIndex>> {
        self.db.project_reference_index(project_id)
    }

    pub fn body(&self, def_id: DefId) -> Option<Arc<Body>> {
        self.db.body(def_id)
    }

    pub fn body_source_map(&self, def_id: DefId) -> Option<Arc<BodySourceMap>> {
        self.db.body_source_map(def_id)
    }

    pub fn body_scopes(&self, def_id: DefId) -> Option<Arc<BodyScopes>> {
        self.db.body_scopes(def_id)
    }

    pub fn body_resolution(&self, def_id: DefId) -> Option<Arc<BodyResolution>> {
        self.db.body_resolution(def_id)
    }

    pub fn infer(&self, def_id: DefId) -> Option<Arc<InferenceResult>> {
        self.db.infer(def_id)
    }

    pub fn semantics(&self, root_file: FileId) -> Semantics {
        Semantics::new(Arc::clone(&self.db), self.db.def_map(root_file))
    }

    pub fn semantics_for_project(&self, project_id: ProjectId) -> Option<Semantics> {
        Some(Semantics::new(
            Arc::clone(&self.db),
            self.db.project_def_map(project_id)?,
        ))
    }

    pub fn document_symbols(&self, root_file: FileId, file_id: FileId) -> Vec<DocumentSymbol> {
        symbol::document_symbols(&self.db.def_map(root_file), file_id)
    }

    pub fn document_symbols_in_project(&self, file: ProjectFile) -> Vec<DocumentSymbol> {
        self.db
            .project_def_map(file.project_id)
            .filter(|map| map.module_for_file(file.file_id).is_some())
            .map_or_else(Vec::new, |map| symbol::document_symbols(&map, file.file_id))
    }

    pub fn workspace_symbols(&self, root_file: FileId, query: &str) -> Vec<WorkspaceSymbol> {
        symbol::workspace_symbols(&self.db.def_map(root_file), query)
    }

    pub fn workspace_symbols_in_project(
        &self,
        context: QueryContext,
        query: &str,
    ) -> Vec<WorkspaceSymbol> {
        self.db
            .project_def_map(context.project_id())
            .map_or_else(Vec::new, |map| symbol::workspace_symbols(&map, query))
    }

    pub fn closure_parameters(&self, file_id: FileId) -> Vec<ClosureParameterInfo> {
        closure_iterator::closure_parameters(&self.db, &self.db.def_map(file_id), file_id)
    }

    pub fn closure_parameters_in_project(&self, file: ProjectFile) -> Vec<ClosureParameterInfo> {
        self.db
            .project_def_map(file.project_id)
            .filter(|map| map.module_for_file(file.file_id).is_some())
            .map_or_else(Vec::new, |map| {
                closure_iterator::closure_parameters(&self.db, &map, file.file_id)
            })
    }

    pub fn semantic_tokens(&self, file_id: FileId) -> Vec<SemanticToken> {
        closure_iterator::semantic_tokens(&self.db, self.db.def_map(file_id), file_id)
    }

    pub fn semantic_tokens_in_project(&self, file: ProjectFile) -> Vec<SemanticToken> {
        self.db
            .project_def_map(file.project_id)
            .filter(|map| map.module_for_file(file.file_id).is_some())
            .map_or_else(Vec::new, |map| {
                closure_iterator::semantic_tokens(&self.db, map, file.file_id)
            })
    }

    /// Resolve the callable type, parameter names, and arguments for a
    /// call or method-call expression.  Returns `None` when the callee
    /// type cannot be resolved.
    fn resolve_call_target(
        &self,
        body: &crate::hir::Body,
        inference: &std::sync::Arc<crate::hir::InferenceResult>,
        def_map: &crate::hir::DefMap,
        member_index: &crate::hir::MemberIndex,
        expr: &crate::hir::Expr,
    ) -> Option<(
        crate::hir::CallableTy,
        Vec<Option<String>>,
        Vec<crate::hir::ExprId>,
    )> {
        match expr {
            crate::hir::Expr::Call { callee, args } => {
                let callee_ty = inference.type_of_expr(*callee)?.clone();
                let (callable, names) = match &callee_ty {
                    Ty::Function(c) | Ty::Closure(c) => {
                        let names = resolve_callee_param_names(body, def_map, *callee);
                        (c.clone(), names)
                    }
                    _ => return None,
                };
                Some((callable, names, args.clone()))
            }
            crate::hir::Expr::MethodCall {
                receiver,
                method,
                args,
                ..
            } => {
                let receiver_ty = inference.type_of_expr(*receiver)?;
                let method_name = body.name_ref(*method)?.name()?.to_string();
                let resolution = member_index.resolve_method(receiver_ty, &method_name)?;
                let callable = resolution.callable()?.clone();
                let names = match resolution.target() {
                    crate::hir::MemberTarget::Definition(def_id) => def_map
                        .definition(def_id)
                        .and_then(|def| {
                            if let crate::hir::ItemSignature::Callable(sig) = def.signature() {
                                Some(
                                    sig.params()
                                        .iter()
                                        .map(|p| p.name().map(|n| n.to_string()))
                                        .collect(),
                                )
                            } else {
                                None
                            }
                        })
                        .unwrap_or_default(),
                    _ => vec![],
                };
                Some((callable, names, args.clone()))
            }
            _ => None,
        }
    }

    /// Signature help at a cursor position (inside a call expression).
    pub fn signature_help(&self, position: ProjectPosition) -> Option<SignatureHelpInfo> {
        let text = self.db.file_text(position.position.file_id)?;
        let offset = position.position.offset.min(text.len() as u32);
        let query = self.project_query_data(position)?;
        let ctx = completion::find_containing_body_data(
            &self.db,
            &query.def_map,
            position.position,
            offset,
        )?;
        let body = &ctx.body;
        let source_map = &ctx.source_map;
        let inference = ctx.inference.as_ref()?;

        // Find the innermost Call or MethodCall containing the cursor.
        let mut best_expr_id: Option<crate::hir::ExprId> = None;
        let mut best_len = u32::MAX;
        for (expr_id, expr) in body.exprs() {
            let range = source_map.expr_range(expr_id)?;
            if range.range.contains(offset)
                && matches!(
                    expr,
                    crate::hir::Expr::Call { .. } | crate::hir::Expr::MethodCall { .. }
                )
            {
                let len = range.range.len();
                if len < best_len {
                    best_len = len;
                    best_expr_id = Some(expr_id);
                }
            }
        }

        let expr_id = best_expr_id?;
        let expr = body.expr(expr_id)?;

        let (callable, param_names, args) =
            self.resolve_call_target(body, inference, &query.def_map, &query.member_index, expr)?;

        // Build parameter display: use original names when available.
        let param_types: Vec<String> = callable
            .params()
            .iter()
            .enumerate()
            .map(|(i, ty)| match param_names.get(i).and_then(|n| n.clone()) {
                Some(name) => format!("{name}: {ty}"),
                None => ty.to_string(),
            })
            .collect();
        let ret = callable.return_ty().to_string();

        // Active parameter: count fully-completed args before cursor.
        let mut active_param = 0u32;
        for arg_id in &args {
            if let Some(r) = source_map.expr_range(*arg_id) {
                if r.range.end() <= offset {
                    active_param += 1;
                } else if r.range.contains(offset) {
                    break; // cursor inside this arg — it is active
                }
            }
        }
        let max_param = param_types.len().saturating_sub(1) as u32;

        let label = format!("fn({}) -> {}", param_types.join(", "), ret);
        Some(SignatureHelpInfo {
            label,
            parameters: param_types,
            active_parameter: active_param.min(max_param),
        })
    }

    /// Completion candidates at a cursor position.
    pub fn completions(&self, position: ProjectPosition) -> Vec<CompletionItem> {
        completion::completions(&self.db, position)
    }

    // ------------------------------------------------------------------
    // Navigation and hover
    // ------------------------------------------------------------------

    /// Inferred type hints for bindings in one project-aware file.
    pub fn inlay_hints(&self, file: ProjectFile) -> Vec<TypeHint> {
        let Some(query) = semantic_query_data(
            &self.db,
            ProjectPosition::at(file.project_id, file.file_id, 0),
        ) else {
            return Vec::new();
        };
        let mut hints = Vec::new();

        for definition in query.def_map.definitions().filter(|definition| {
            definition.file_id() == file.file_id && definition.kind().is_body_owner()
        }) {
            let Some(body) = self.db.body(definition.id()) else {
                continue;
            };
            let Some(source_map) = self.db.body_source_map(definition.id()) else {
                continue;
            };
            let Some(inference) = self.db.infer(definition.id()) else {
                continue;
            };

            for (binding_id, binding) in body.bindings() {
                if binding.name().is_none()
                    || binding.type_ref().is_some()
                    || matches!(
                        binding.kind(),
                        crate::hir::BindingKind::Parameter
                            | crate::hir::BindingKind::ClosureParameter
                            | crate::hir::BindingKind::SelfParameter
                    )
                {
                    continue;
                }
                let Some(ty) = inference.type_of_binding(binding_id) else {
                    continue;
                };
                if ty.is_unknown() || ty.is_never() {
                    continue;
                }
                let Some(range) = source_map.binding_range(binding_id) else {
                    continue;
                };
                if range.file_id != file.file_id {
                    continue;
                }

                let mut hint = TypeHint::new(
                    FilePosition::new(file.file_id, range.range.end()),
                    ty.to_string(),
                );
                if let Ty::Named(named) = ty
                    && let Some(target) = query.def_map.definition(named.definition())
                {
                    hint = hint.with_target(FileRange::new(target.file_id(), target.name_range()));
                }
                hints.push(hint);
            }
        }

        hints.sort();
        hints.dedup();
        hints
    }

    /// Type and signature information at a cursor position.
    pub fn hover(&self, position: ProjectPosition) -> Option<HoverResult> {
        let query = self.project_query_data(position)?;
        let semantics = Semantics::new(Arc::clone(&self.db), query.def_map.clone());

        if let Some(hover) = self.builtin_macro_hover(position) {
            return Some(hover);
        }

        if let Some(member) = self.builtin_member_at(position, &query) {
            let mut hover = HoverResult::new(member.range, builtin_hover_text(&member));
            if let Some(documentation) =
                builtin_source_definition(member.id).and_then(|source| source.documentation)
            {
                hover = hover.with_documentation(documentation);
            }
            return Some(hover);
        }

        // 1. Try member access hover first (field/method after `.`).
        let member = self.member_hover(position, &query);
        if let Some(hover) = member {
            return Some(hover);
        }

        // 2. Try item/definition hover.
        let def = semantics.find_def_at(position.position);
        if let Some(definition) = def {
            let signature = item_hover_text(&definition, &query.member_index);
            let mut hover = HoverResult::new(
                FileRange::new(definition.file_id(), definition.name_range()),
                signature,
            );
            if let Some(documentation) = definition.documentation() {
                hover = hover.with_documentation(documentation);
            }
            return Some(hover);
        }

        // 3. Try local binding hover.
        if let crate::hir::LocalResolveResult::Resolved(target) =
            semantics.resolve_local_at(position.position)
        {
            let owner_def = target.owner().owner();
            if let Some(inference) = self.db.infer(owner_def) {
                let ty = inference
                    .type_of_binding(target.binding())
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "?".to_string());
                if let Some(body) = self.db.body(owner_def) {
                    let name = body
                        .binding(target.binding())
                        .and_then(|b| b.name())
                        .unwrap_or("?");
                    if let Some(source_map) = self.db.body_source_map(owner_def)
                        && let Some(file_range) = source_map.binding_range(target.binding())
                    {
                        return Some(HoverResult::new(file_range, format!("let {name}: {ty}")));
                    }
                }
            }
        }

        None
    }

    fn builtin_macro_hover(&self, position: ProjectPosition) -> Option<HoverResult> {
        let file_id = position.position.file_id;
        let text = self.db.file_text(file_id)?;
        let bytes = text.as_bytes();
        let offset = (position.position.offset as usize).min(bytes.len());
        let mut start = offset;
        while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
            start -= 1;
        }
        let mut end = offset;
        while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
            end += 1;
        }
        if start == end || bytes.get(end) != Some(&b'!') {
            return None;
        }
        let spec = rua_core::builtin_macro(&text[start..end])?;
        Some(
            HoverResult::new(
                FileRange::new(file_id, TextRange::new(start as u32, end as u32)),
                spec.signature,
            )
            .with_documentation(spec.documentation),
        )
    }

    /// Hover for `receiver.field` or `receiver.method()`.
    fn member_hover(
        &self,
        position: ProjectPosition,
        query: &ProjectQueryData,
    ) -> Option<HoverResult> {
        let (receiver_ty, field_name, token_range) =
            self.resolve_dot_access(position, &query.def_map)?;
        let member_index = &query.member_index;

        // Try field resolution
        if let Some(resolution) = member_index.resolve_field(&receiver_ty, &field_name) {
            let mut hover =
                HoverResult::new(token_range, format!("{}: {}", field_name, resolution.ty()));
            if let crate::hir::MemberTarget::Definition(id) = resolution.target()
                && let Some(definition) = query.def_map.definition(id)
                && let Some(documentation) = definition.documentation()
            {
                hover = hover.with_documentation(documentation);
            }
            return Some(hover);
        }

        // Try method resolution
        if let Some(resolution) = member_index.resolve_method(&receiver_ty, &field_name) {
            let callable = resolution.callable();
            let params: Vec<String> = callable
                .map(|c| c.params().iter().map(|t| t.to_string()).collect())
                .unwrap_or_default();
            let ret = callable
                .map(|c| c.return_ty().to_string())
                .unwrap_or_else(|| "?".to_string());
            let receiver_str = match resolution.receiver() {
                Some(crate::hir::ReceiverKind::Value) => "self",
                Some(crate::hir::ReceiverKind::SharedRef) => "&self",
                Some(crate::hir::ReceiverKind::MutRef) => "&mut self",
                None => "",
            };
            let pts = if receiver_str.is_empty() {
                params.join(", ")
            } else if params.is_empty() {
                receiver_str.to_string()
            } else {
                format!("{}, {}", receiver_str, params.join(", "))
            };
            let mut hover =
                HoverResult::new(token_range, format!("fn {}({pts}) -> {ret}", field_name));
            if let crate::hir::MemberTarget::Definition(id) = resolution.target()
                && let Some(definition) = query.def_map.definition(id)
                && let Some(documentation) = definition.documentation()
            {
                hover = hover.with_documentation(documentation);
            }
            return Some(hover);
        }

        None
    }

    /// Navigate to the definition of the symbol at a cursor position.
    pub fn goto_definition(&self, position: ProjectPosition) -> Option<NavigationTarget> {
        let query = self.project_query_data(position)?;
        let semantics = Semantics::new(Arc::clone(&self.db), query.def_map.clone());

        // 1. Try member access — resolve field/method to its definition.
        if let Some(target) = self.member_goto_definition(position, &query) {
            return Some(target);
        }

        // 2. Try item definition.
        if let Some(definition) = semantics.find_def_at(position.position) {
            return Some(NavigationTarget::new(
                FileRange::new(definition.file_id(), definition.name_range()),
                None,
            ));
        }

        // 3. Try local definition.
        if let Some(local_range) = semantics.local_definition_at(position.position) {
            return Some(NavigationTarget::new(local_range, None));
        }

        None
    }

    /// Navigate to a member declaration in the configured builtin sysroot.
    pub fn goto_builtin_definition(
        &self,
        position: ProjectPosition,
    ) -> Option<BuiltinDefinitionTarget> {
        let query = self.project_query_data(position)?;
        let member = self.builtin_member_at(position, &query)?;
        let source = builtin_source_definition(member.id)?;
        Some(BuiltinDefinitionTarget::new(
            member.id.source_name(),
            source.range,
        ))
    }

    /// Goto definition for `receiver.field` or `receiver.method()`.
    fn member_goto_definition(
        &self,
        position: ProjectPosition,
        query: &ProjectQueryData,
    ) -> Option<NavigationTarget> {
        let (receiver_ty, field_name, _token_range) =
            self.resolve_dot_access(position, &query.def_map)?;
        let def_map = &query.def_map;
        let member_index = &query.member_index;

        // Resolve field or method to its definition. Short-circuit:
        // only resolve the method if the field lookup returned nothing.
        let field = member_index.resolve_field(&receiver_ty, &field_name);
        let resolution =
            field.or_else(|| member_index.resolve_method(&receiver_ty, &field_name))?;

        let def_id = match resolution.target() {
            crate::hir::MemberTarget::Definition(id) => id,
            _ => return None,
        };
        let definition = def_map.definition(def_id)?;
        Some(NavigationTarget::new(
            FileRange::new(definition.file_id(), definition.name_range()),
            None,
        ))
    }

    /// Shared preamble for member hover and goto-def: find the token
    /// after `.`, extract the field/method name, and infer the receiver
    /// type.  Returns `(receiver_ty, field_name, token_range)` on success.
    fn resolve_dot_access(
        &self,
        position: ProjectPosition,
        def_map: &DefMap,
    ) -> Option<(Ty, String, FileRange)> {
        let file_id = position.position.file_id;
        let text = self.db.file_text(file_id)?;
        let parse = self.db.parse(file_id);
        let root = parse.syntax_node();
        let offset = position.position.offset.min(text.len() as u32);
        let token = completion::token_at_offset(root, offset)?;

        // Compute the token range for hover highlighting.
        let token_range = {
            let tr = token.text_range();
            FileRange::new(file_id, TextRange::new(tr.start().into(), tr.end().into()))
        };

        // Must be preceded by `.`
        if completion::previous_significant(&token)
            .is_none_or(|t| t.kind() != rua_syntax::SyntaxKind::Dot)
        {
            return None;
        }
        let field_name = if token.kind() == rua_syntax::SyntaxKind::Ident {
            token.text().to_string()
        } else {
            return None;
        };

        let receiver_ty =
            completion::infer_dot_receiver(&self.db, def_map, position.position, offset)?;

        Some((receiver_ty, field_name, token_range))
    }

    fn definition_at(
        &self,
        position: ProjectPosition,
        query: &ProjectQueryData,
    ) -> Option<Definition> {
        if let Some((receiver_ty, name, _)) = self.resolve_dot_access(position, &query.def_map) {
            let resolution = query
                .member_index
                .resolve_field(&receiver_ty, &name)
                .or_else(|| query.member_index.resolve_method(&receiver_ty, &name))?;
            let crate::hir::MemberTarget::Definition(def_id) = resolution.target() else {
                return None;
            };
            return query.def_map.definition(def_id).cloned();
        }
        Semantics::new(Arc::clone(&self.db), query.def_map.clone()).find_def_at(position.position)
    }

    /// Go to implementation(s) of a trait method.
    pub fn goto_implementation(&self, position: ProjectPosition) -> Vec<NavigationTarget> {
        let Some(query) = self.project_query_data(position) else {
            return Vec::new();
        };
        let Some(definition) = self.definition_at(position, &query) else {
            return Vec::new();
        };
        let def_map = query.def_map.clone();
        // Only for methods owned by a trait.
        if definition.kind() != DefKind::Method {
            return Vec::new();
        }
        let Some(owner_id) = definition.owner() else {
            return Vec::new();
        };
        let Some(owner_def) = def_map.definition(owner_id) else {
            return Vec::new();
        };
        if owner_def.kind() != DefKind::Trait {
            return Vec::new();
        }
        let method_name = definition.name();

        let mut targets = Vec::new();
        for implementation in query.member_index.implementations() {
            if implementation.trait_definition() != Some(owner_id) {
                continue;
            }
            for method in implementation.methods() {
                if let Some(method_def) = def_map.definition(*method)
                    && method_def.name() == method_name
                {
                    targets.push(NavigationTarget::new(
                        FileRange::new(method_def.file_id(), method_def.name_range()),
                        None,
                    ));
                }
            }
        }
        NavigationTarget::normalize(&mut targets);
        targets
    }

    /// Find all references to the symbol at a cursor position.
    pub fn references(
        &self,
        position: ProjectPosition,
        include_declaration: bool,
    ) -> Vec<ReferenceResult> {
        self.references_cancellable(position, include_declaration, || false)
            .unwrap_or_default()
    }

    /// Cancellable form used by adapters that run large project queries on a
    /// worker. `None` means cancellation; an empty `Some` is a completed query.
    pub fn references_cancellable(
        &self,
        position: ProjectPosition,
        include_declaration: bool,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Option<Vec<ReferenceResult>> {
        if is_cancelled() {
            return None;
        }
        let Some(query) = self.project_query_data(position) else {
            return Some(Vec::new());
        };
        let def_map = query.def_map.clone();
        let semantics = Semantics::new(Arc::clone(&self.db), def_map.clone());

        // 1. Try local references.
        let local_refs = semantics.local_references_at(position.position, include_declaration);
        if !local_refs.is_empty() {
            let mut results: Vec<ReferenceResult> = local_refs
                .into_iter()
                .map(|range| {
                    let kind = if let Some(def_range) =
                        semantics.local_definition_at(position.position)
                        && range == def_range
                    {
                        ReferenceKind::Declaration
                    } else {
                        ReferenceKind::Read
                    };
                    ReferenceResult::new(range, kind)
                })
                .collect();
            ReferenceResult::normalize(&mut results);
            return Some(results);
        }

        // 2. Try item-level references (cross-file).
        if let Some(definition) = self.definition_at(position, &query) {
            let target_id = definition.id();
            let target_file = definition.file_id();
            let Some(index) = self
                .db
                .project_reference_index_cancellable(position.project_id, &mut is_cancelled)
            else {
                return if is_cancelled() {
                    None
                } else {
                    Some(Vec::new())
                };
            };
            let mut results: Vec<ReferenceResult> = index
                .occurrences(target_id)
                .iter()
                .map(|occurrence| {
                    let kind = match occurrence.kind() {
                        crate::semantic::ReferenceOccurrenceKind::Write => ReferenceKind::Write,
                        crate::semantic::ReferenceOccurrenceKind::Read
                        | crate::semantic::ReferenceOccurrenceKind::Call => ReferenceKind::Read,
                    };
                    ReferenceResult::new(occurrence.range(), kind)
                })
                .collect();

            // Include the declaration.
            if include_declaration {
                results.push(ReferenceResult::new(
                    FileRange::new(target_file, definition.name_range()),
                    ReferenceKind::Declaration,
                ));
            }

            ReferenceResult::normalize(&mut results);
            if !results.is_empty() {
                return Some(results);
            }
        }

        Some(Vec::new())
    }

    /// Check whether the symbol at the cursor can be renamed.
    pub fn prepare_rename(&self, position: ProjectPosition) -> Option<RenameTarget> {
        if self.is_file_read_only(position.position.file_id) {
            return None;
        }
        let query = self.project_query_data(position)?;
        let semantics = Semantics::new(Arc::clone(&self.db), query.def_map.clone());

        // Local binding rename.
        if let crate::hir::LocalResolveResult::Resolved(target) =
            semantics.resolve_local_at(position.position)
        {
            let owner_def = target.owner().owner();
            if let Some(body) = self.db.body(owner_def)
                && let Some(source_map) = self.db.body_source_map(owner_def)
                && let Some(file_range) = source_map.binding_range(target.binding())
            {
                let name = body
                    .binding(target.binding())
                    .and_then(|b| b.name())
                    .unwrap_or("?");
                return Some(RenameTarget::new(file_range, name));
            }
        }

        // Item rename.
        if let Some(definition) = self.definition_at(position, &query) {
            if self.is_file_read_only(definition.file_id()) {
                return None;
            }
            return Some(RenameTarget::new(
                FileRange::new(definition.file_id(), definition.name_range()),
                definition.name(),
            ));
        }

        None
    }

    /// Rename the symbol at the cursor.
    pub fn rename(
        &self,
        position: ProjectPosition,
        new_name: &str,
    ) -> Result<SourceChange, RenameError> {
        if !is_valid_identifier(new_name) {
            return Err(RenameError::InvalidName {
                name: new_name.to_string(),
            });
        }

        let refs = self.references(position, true);
        if refs.is_empty() {
            return Err(RenameError::NoTarget);
        }

        SourceChange::from_edits(
            refs.iter()
                .map(|r| (r.range().file_id, TextEdit::new(r.range().range, new_name))),
            |file_id| self.is_file_read_only(file_id),
        )
    }

    // -- call hierarchy --------------------------------------------------

    /// Find the function/method definition at the cursor for call hierarchy.
    pub fn call_hierarchy_prepare(&self, position: ProjectPosition) -> Option<CallHierarchyItem> {
        let query = self.project_query_data(position)?;
        let definition = self.definition_at(position, &query)?;
        if !matches!(definition.kind(), DefKind::Function | DefKind::Method) {
            return None;
        }
        Some(CallHierarchyItem {
            project_id: position.project_id,
            target: definition.id(),
            name: definition.name().to_string(),
            kind: definition.kind(),
            file_id: definition.file_id(),
            range: definition.name_range(),
            call_sites: Vec::new(),
        })
    }

    /// Find all callers of a function/method.
    pub fn call_hierarchy_incoming(&self, item: &CallHierarchyItem) -> Vec<CallHierarchyItem> {
        let Some(def_map) = self.db.project_def_map(item.project_id) else {
            return Vec::new();
        };
        let Some(target) = def_map.definition(item.target) else {
            return Vec::new();
        };
        let Some(index) = self.db.project_reference_index(item.project_id) else {
            return Vec::new();
        };
        let mut callers = std::collections::BTreeMap::<DefId, Vec<FileRange>>::new();
        for occurrence in index.occurrences(target.id()).iter().filter(|occurrence| {
            occurrence.kind() == crate::semantic::ReferenceOccurrenceKind::Call
        }) {
            callers
                .entry(occurrence.owner())
                .or_default()
                .push(occurrence.range());
        }
        callers
            .into_iter()
            .filter_map(|(caller, mut call_sites)| {
                let definition = def_map.definition(caller)?;
                call_sites.sort();
                call_sites.dedup();
                Some(CallHierarchyItem {
                    project_id: item.project_id,
                    target: definition.id(),
                    name: definition.name().to_string(),
                    kind: definition.kind(),
                    file_id: definition.file_id(),
                    range: definition.name_range(),
                    call_sites,
                })
            })
            .collect()
    }

    /// Find all functions/methods called by this one.
    pub fn call_hierarchy_outgoing(&self, item: &CallHierarchyItem) -> Vec<CallHierarchyItem> {
        let Some(def_map) = self.db.project_def_map(item.project_id) else {
            return Vec::new();
        };
        let Some(owner) = def_map.definition(item.target) else {
            return Vec::new();
        };
        let Some(index) = self.db.project_reference_index(item.project_id) else {
            return Vec::new();
        };
        let mut callees = std::collections::BTreeMap::<DefId, Vec<FileRange>>::new();
        for occurrence in index
            .occurrences_in(owner.id())
            .iter()
            .filter(|occurrence| {
                occurrence.kind() == crate::semantic::ReferenceOccurrenceKind::Call
            })
        {
            callees
                .entry(occurrence.target())
                .or_default()
                .push(occurrence.range());
        }
        callees
            .into_iter()
            .filter_map(|(callee, mut call_sites)| {
                let definition = def_map.definition(callee)?;
                call_sites.sort();
                call_sites.dedup();
                Some(CallHierarchyItem {
                    project_id: item.project_id,
                    target: definition.id(),
                    name: definition.name().to_string(),
                    kind: definition.kind(),
                    file_id: definition.file_id(),
                    range: definition.name_range(),
                    call_sites,
                })
            })
            .collect()
    }

    // -- type hierarchy ---------------------------------------------------

    /// Find the type definition at the cursor for type hierarchy.
    pub fn type_hierarchy_prepare(&self, position: ProjectPosition) -> Option<TypeHierarchyItem> {
        let query = self.project_query_data(position)?;
        let semantics = Semantics::new(Arc::clone(&self.db), query.def_map);
        let definition = semantics.find_def_at(position.position)?;
        if !matches!(
            definition.kind(),
            DefKind::Struct | DefKind::Enum | DefKind::Trait | DefKind::Impl
        ) {
            return None;
        }
        Some(TypeHierarchyItem {
            project_id: position.project_id,
            target: definition.id(),
            name: definition.name().to_string(),
            kind: definition.kind(),
            file_id: definition.file_id(),
            range: definition.name_range(),
        })
    }

    /// Find supertypes (traits implemented) for a type.
    pub fn type_hierarchy_supertypes(&self, item: &TypeHierarchyItem) -> Vec<TypeHierarchyItem> {
        let Some(def_map) = self.db.project_def_map(item.project_id) else {
            return Vec::new();
        };
        let Some(member_index) = self.db.project_member_index(item.project_id) else {
            return Vec::new();
        };
        let target = if def_map
            .definition(item.target)
            .is_some_and(|definition| definition.kind() == DefKind::Impl)
        {
            member_index
                .implementation(item.target)
                .and_then(|implementation| match implementation.target_ty() {
                    Ty::Named(named) => Some(named.definition()),
                    _ => None,
                })
        } else {
            Some(item.target)
        };
        let Some(target) = target else {
            return Vec::new();
        };
        let mut result = Vec::new();
        for implementation in member_index.implementations() {
            if !matches!(implementation.target_ty(), Ty::Named(named) if named.definition() == target)
            {
                continue;
            }
            if let Some(trait_id) = implementation.trait_definition()
                && let Some(trait_def) = def_map.definition(trait_id)
            {
                result.push(TypeHierarchyItem {
                    project_id: item.project_id,
                    target: trait_def.id(),
                    name: trait_def.name().to_string(),
                    kind: DefKind::Trait,
                    file_id: trait_def.file_id(),
                    range: trait_def.name_range(),
                });
            }
        }
        result
    }

    /// Find subtypes (implementors) of a trait.
    pub fn type_hierarchy_subtypes(&self, item: &TypeHierarchyItem) -> Vec<TypeHierarchyItem> {
        let Some(def_map) = self.db.project_def_map(item.project_id) else {
            return Vec::new();
        };
        let Some(member_index) = self.db.project_member_index(item.project_id) else {
            return Vec::new();
        };
        let mut result = Vec::new();
        for implementation in member_index.implementations() {
            if implementation.trait_definition() != Some(item.target) {
                continue;
            }
            let Ty::Named(named) = implementation.target_ty() else {
                continue;
            };
            if let Some(type_def) = def_map.definition(named.definition()) {
                result.push(TypeHierarchyItem {
                    project_id: item.project_id,
                    target: type_def.id(),
                    name: type_def.name().to_string(),
                    kind: type_def.kind(),
                    file_id: type_def.file_id(),
                    range: type_def.name_range(),
                });
            }
        }
        result
    }

    pub fn file_kind(&self, file_id: FileId) -> Option<FileKind> {
        self.db.file_kind(file_id)
    }

    pub fn source_root_kind(&self, file_id: FileId) -> Option<SourceRootKind> {
        self.db.source_root_kind(file_id)
    }

    pub fn is_file_read_only(&self, file_id: FileId) -> bool {
        self.db.is_file_read_only(file_id)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn item_hover_text(definition: &Definition, member_index: &MemberIndex) -> String {
    // Delegate to the shared signature formatter in the completion module.
    // For Impl blocks (which definition_signature returns None for), show a
    // simple label.
    if definition.kind() == DefKind::Impl {
        return format!("impl {}", definition.name());
    }
    completion::definition_signature(member_index, definition)
        .unwrap_or_else(|| definition.name().to_string())
}

fn is_valid_identifier(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let bytes = name.as_bytes();
    if !bytes[0].is_ascii_alphabetic() && bytes[0] != b'_' {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'_')
}

/// Resolve parameter names for a direct call by looking up the callee
/// path's definition in the def_map and extracting its signature params.
fn resolve_callee_param_names(
    body: &crate::hir::Body,
    def_map: &crate::hir::DefMap,
    callee: crate::hir::ExprId,
) -> Vec<Option<String>> {
    let Some(crate::hir::Expr::Path(path)) = body.expr(callee) else {
        return vec![];
    };
    let [nr] = &path[..] else {
        return vec![];
    };
    let Some(ref_info) = body.name_ref(*nr) else {
        return vec![];
    };
    let Some(nr_name) = ref_info.name() else {
        return vec![];
    };
    def_map
        .resolve_name(def_map.root(), nr_name)
        .and_then(|def| {
            if let crate::hir::ItemSignature::Callable(sig) = def.signature() {
                Some(
                    sig.params()
                        .iter()
                        .map(|p| p.name().map(|n| n.to_string()))
                        .collect(),
                )
            } else {
                None
            }
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{Analysis, AnalysisHost};
    use crate::vfs::{Change, FileId, FileKind, SourceRootId, SourceRootKind};

    #[test]
    fn analysis_host_applies_changes_and_exposes_parse() {
        let file_id = FileId::new(0);
        let mut change = Change::new();
        change.set_file_text(file_id, "fn main() {}\nmain();");

        let mut host = AnalysisHost::new();
        host.apply_change(change);
        let analysis = host.analysis();

        assert_eq!(
            analysis.parse(file_id).syntax_node().text().to_string(),
            "fn main() {}\nmain();"
        );
        assert!(analysis.diagnostics(file_id).is_empty());
    }

    #[test]
    fn analysis_host_snapshots_are_isolated_from_later_changes() {
        let file_id = FileId::new(0);
        let mut initial = Change::new();
        initial.set_file_text(file_id, "fn before() {}");

        let mut host = AnalysisHost::new();
        host.apply_change(initial);
        let before = host.analysis();

        let mut update = Change::new();
        update.set_file_text(file_id, "fn after() {}");
        host.apply_change(update);
        let after = host.analysis();

        assert_eq!(
            before.parse(file_id).syntax_node().text().to_string(),
            "fn before() {}"
        );
        assert_eq!(
            after.parse(file_id).syntax_node().text().to_string(),
            "fn after() {}"
        );
    }

    #[test]
    fn analysis_snapshot_is_send_sync_and_queries_on_a_worker() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Analysis>();

        let file_id = FileId::new(0);
        let mut change = Change::new();
        change.set_file_text(file_id, "fn worker() -> i64 { 42 }");
        let mut host = AnalysisHost::new();
        host.apply_change(change);
        let analysis = host.analysis();

        let source =
            std::thread::spawn(move || analysis.parse(file_id).syntax_node().text().to_string())
                .join()
                .expect("analysis worker must not panic");
        assert_eq!(source, "fn worker() -> i64 { 42 }");
    }

    #[test]
    fn analysis_host_applies_a_change_batch_in_order() {
        let file_id = FileId::new(0);
        let mut change = Change::new();
        change.set_file_text(file_id, "fn first() {}");
        change.remove_file(file_id);
        change.set_file_text(file_id, "fn last() {}");

        let mut host = AnalysisHost::new();
        host.apply_change(change);

        assert_eq!(
            host.analysis()
                .parse(file_id)
                .syntax_node()
                .text()
                .to_string(),
            "fn last() {}"
        );
    }

    #[test]
    fn library_root_declaration_is_a_read_only_analysis_input() {
        let root_id = SourceRootId::new(1);
        let file_id = FileId::new(10);
        let mut change = Change::new();
        change.set_source_root(root_id, SourceRootKind::Library);
        change.set_file(
            file_id,
            root_id,
            FileKind::Declaration,
            "extern \"lua\" { pub fn log(msg: String); }",
        );

        let mut host = AnalysisHost::new();
        host.apply_change(change);
        let analysis = host.analysis();

        assert!(analysis.parse(file_id).errors().is_empty());
        assert_eq!(analysis.file_kind(file_id), Some(FileKind::Declaration));
        assert_eq!(
            analysis.source_root_kind(file_id),
            Some(SourceRootKind::Library)
        );
        assert!(analysis.is_file_read_only(file_id));
    }

    #[test]
    fn declaration_files_reject_executable_bodies_and_top_level_statements() {
        let root_id = SourceRootId::new(1);
        let file_id = FileId::new(10);
        let source = r#"
            struct Value {}
            fn implemented() { let value = 1; }
            impl Value { fn method(&self) { let value = 1; } }
            trait Contract { fn defaulted(&self) { let value = 1; } }
            mod nested { let value = 1; }
            let top_level = 1;
        "#;
        let mut change = Change::new();
        change.set_source_root(root_id, SourceRootKind::Library);
        change.set_file(file_id, root_id, FileKind::Declaration, source);

        let mut host = AnalysisHost::new();
        host.apply_change(change);
        let diagnostics = host.analysis().diagnostics(file_id);
        let invalid = diagnostics
            .iter()
            .filter(|diagnostic| {
                diagnostic.code() == Some(rua_core::DiagnosticCode::NameInvalidDeclaration)
            })
            .collect::<Vec<_>>();

        assert_eq!(invalid.len(), 5, "{invalid:#?}");
        for diagnostic in invalid {
            assert!(!diagnostic.range().is_empty(), "{diagnostic:#?}");
        }
    }

    #[test]
    fn library_root_read_only_policy_is_independent_of_file_kind() {
        let cases = [
            (SourceRootKind::Workspace, FileKind::Source, false),
            (SourceRootKind::Library, FileKind::Declaration, true),
            (SourceRootKind::Std, FileKind::Declaration, true),
            (SourceRootKind::Virtual, FileKind::Source, false),
        ];
        let mut change = Change::new();
        for (index, (root_kind, file_kind, _)) in cases.iter().copied().enumerate() {
            let root_id = SourceRootId::new(index as u32);
            let file_id = FileId::new(index as u32);
            change.set_source_root(root_id, root_kind);
            change.set_file(file_id, root_id, file_kind, "");
        }

        let mut host = AnalysisHost::new();
        host.apply_change(change);
        let analysis = host.analysis();

        for (index, (root_kind, file_kind, read_only)) in cases.iter().copied().enumerate() {
            let file_id = FileId::new(index as u32);
            assert_eq!(analysis.source_root_kind(file_id), Some(root_kind));
            assert_eq!(analysis.file_kind(file_id), Some(file_kind));
            assert_eq!(analysis.is_file_read_only(file_id), read_only);
        }
    }

    #[test]
    fn library_root_removal_drops_its_files_but_not_old_snapshots() {
        let root_id = SourceRootId::new(1);
        let file_id = FileId::new(10);
        let mut initial = Change::new();
        initial.set_source_root(root_id, SourceRootKind::Library);
        initial.set_file(file_id, root_id, FileKind::Declaration, "pub fn api();");

        let mut host = AnalysisHost::new();
        host.apply_change(initial);
        let before_removal = host.analysis();

        let mut removal = Change::new();
        removal.remove_source_root(root_id);
        host.apply_change(removal);
        let after_removal = host.analysis();

        assert!(before_removal.is_file_read_only(file_id));
        assert_eq!(
            before_removal.file_kind(file_id),
            Some(FileKind::Declaration)
        );
        assert_eq!(after_removal.file_kind(file_id), None);
        assert_eq!(after_removal.source_root_kind(file_id), None);
    }
}
