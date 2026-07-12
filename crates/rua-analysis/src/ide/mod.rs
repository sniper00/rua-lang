//! Snapshot-based IDE API, including `AnalysisHost` and feature queries.
//!
//! Results exposed here remain independent of LSP protocol types.

mod closure_iterator;
mod completion;
mod contract;
mod symbol;

use std::{rc::Rc, sync::Arc};

use rua_syntax::{Parse, ast::SourceFile};

use crate::{
    BaseDb,
    hir::{
        Body, BodyResolution, BodyScopes, BodySourceMap, DefId, DefKind, DefMap,
        Definition, InferenceResult, ItemTree, MemberIndex, Ty,
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
    CallHierarchyItem, CompletionInsert, CompletionItem, CompletionKind, FileEdit, FilePosition,
    FileRange, HoverResult, MacroDelimiter, NavigationTarget, ProjectFile, ProjectId,
    ProjectPosition, QueryContext, ReferenceKind, ReferenceResult, RenameError, RenameTarget,
    SemanticToken, SemanticTokenKind, SemanticTokenModifiers, SignatureHelpInfo, SourceChange,
    TextEdit, TextRange, TypeHierarchyItem,
};
pub use symbol::{DocumentSymbol, WorkspaceSymbol};

/// Mutable owner of the current analysis inputs.
#[derive(Debug, Default)]
pub struct AnalysisHost {
    db: Rc<BaseDb>,
}

impl AnalysisHost {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply_change(&mut self, change: Change) {
        Rc::make_mut(&mut self.db).apply_change(change);
    }

    pub fn analysis(&self) -> Analysis {
        Analysis {
            db: Rc::clone(&self.db),
        }
    }
}

/// Immutable view of the inputs captured when the snapshot was created.
#[derive(Clone, Debug)]
pub struct Analysis {
    db: Rc<BaseDb>,
}

impl Analysis {
    /// Test and integration helper for syntax queries during Phase 2.
    pub fn parse(&self, file_id: FileId) -> Arc<Parse<SourceFile>> {
        self.db.parse(file_id)
    }

    pub fn diagnostics(&self, file_id: FileId) -> Vec<Diagnostic> {
        crate::diagnostic::fast_diagnostics(&self.db, file_id)
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
        Semantics::new(Rc::clone(&self.db), self.db.def_map(root_file))
    }

    pub fn semantics_for_project(&self, project_id: ProjectId) -> Option<Semantics> {
        Some(Semantics::new(
            Rc::clone(&self.db),
            self.db.project_def_map(project_id)?,
        ))
    }

    pub fn document_symbols(&self, root_file: FileId, file_id: FileId) -> Vec<DocumentSymbol> {
        symbol::document_symbols(&self.db.def_map(root_file), file_id)
    }

    pub fn workspace_symbols(&self, root_file: FileId, query: &str) -> Vec<WorkspaceSymbol> {
        symbol::workspace_symbols(&self.db.def_map(root_file), query)
    }

    pub fn closure_parameters(&self, file_id: FileId) -> Vec<ClosureParameterInfo> {
        closure_iterator::closure_parameters(&self.db, file_id)
    }

    pub fn semantic_tokens(&self, file_id: FileId) -> Vec<SemanticToken> {
        closure_iterator::semantic_tokens(&self.db, file_id)
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
                let resolution =
                    member_index.resolve_method(receiver_ty, &method_name)?;
                let callable = resolution.callable()?.clone();
                let names = match resolution.target() {
                    crate::hir::MemberTarget::Definition(def_id) => def_map
                        .definition(def_id)
                        .and_then(|def| {
                            if let crate::hir::ItemSignature::Callable(sig) =
                                def.signature()
                            {
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
        let def_map = self.db.def_map(position.position.file_id);
        let member_index = self.db.member_index(position.position.file_id);
        let (body, source_map, inference) =
            completion::find_containing_body_data(&self.db, &def_map, position.position, offset)?;

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

        let (callable, param_names, args) = self.resolve_call_target(
            &body, &inference, &def_map, &member_index, expr,
        )?;

        // Build parameter display: use original names when available.
        let param_types: Vec<String> = callable
            .params()
            .iter()
            .enumerate()
            .map(|(i, ty)| {
                match param_names.get(i).and_then(|n| n.clone()) {
                    Some(name) => format!("{name}: {ty}"),
                    None => ty.to_string(),
                }
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
        completion::completions(&self.db, position.position)
    }

    // ------------------------------------------------------------------
    // Navigation and hover
    // ------------------------------------------------------------------

    /// Type and signature information at a cursor position.
    pub fn hover(&self, position: ProjectPosition) -> Option<HoverResult> {
        let def_map = self.db.def_map(position.position.file_id);
        let semantics = Semantics::new(Rc::clone(&self.db), def_map);

        // 1. Try member access hover first (field/method after `.`).
        let member = self.member_hover(position);
        if let Some(hover) = member {
            return Some(hover);
        }

        // 2. Try item/definition hover.
        let def = semantics.find_def_at(position.position);
        if let Some(definition) = def {
            let root_file = position.position.file_id;
            let signature = item_hover_text(&definition, &self.db, root_file);
            return Some(HoverResult::new(
                FileRange::new(definition.file_id(), definition.name_range()),
                signature,
            ));
        }

        // 3. Try local binding hover.
        if let crate::hir::LocalResolveResult::Resolved(target) = semantics.resolve_local_at(position.position) {
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
                        && let Some(file_range) = source_map.binding_range(target.binding()) {
                            return Some(HoverResult::new(
                                file_range,
                                format!("let {name}: {ty}"),
                            ));
                        }
                }
            }
        }

        None
    }

    /// Hover for `receiver.field` or `receiver.method()`.
    fn member_hover(&self, position: ProjectPosition) -> Option<HoverResult> {
        let file_id = position.position.file_id;
        let (receiver_ty, field_name, token_range) = self.resolve_dot_access(position)?;
        let member_index = self.db.member_index(file_id);

        // Try field resolution
        if let Some(resolution) = member_index.resolve_field(&receiver_ty, &field_name) {
            return Some(HoverResult::new(
                token_range,
                format!("{}: {}", field_name, resolution.ty()),
            ));
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
            return Some(HoverResult::new(
                token_range,
                format!("fn {}({pts}) -> {ret}", field_name),
            ));
        }

        None
    }

    /// Navigate to the definition of the symbol at a cursor position.
    pub fn goto_definition(&self, position: ProjectPosition) -> Option<NavigationTarget> {
        let def_map = self.db.def_map(position.position.file_id);
        let semantics = Semantics::new(Rc::clone(&self.db), def_map);

        // 1. Try member access — resolve field/method to its definition.
        if let Some(target) = self.member_goto_definition(position) {
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

    /// Goto definition for `receiver.field` or `receiver.method()`.
    fn member_goto_definition(
        &self,
        position: ProjectPosition,
    ) -> Option<NavigationTarget> {
        let (receiver_ty, field_name, _token_range) = self.resolve_dot_access(position)?;
        let file_id = position.position.file_id;
        let def_map = self.db.def_map(file_id);
        let member_index = self.db.member_index(file_id);

        // Resolve field or method to its definition. Short-circuit:
        // only resolve the method if the field lookup returned nothing.
        let field = member_index.resolve_field(&receiver_ty, &field_name);
        let resolution = field.or_else(|| {
            member_index.resolve_method(&receiver_ty, &field_name)
        })?;

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

        // Primary path: inference-based receiver type.
        let def_map = self.db.def_map(position.position.file_id);
        let receiver_ty = completion::infer_dot_receiver(
            &self.db, &def_map, position.position, offset,
        )
        .or_else(|| {
            // Fallback: scan syntax tree for the receiver identifier,
            // then look it up by name across all bodies that contain
            // the cursor position.
            let prev = completion::previous_significant(&token)?;
            let before_dot = completion::previous_significant(&prev)?;
            if before_dot.kind() != rua_syntax::SyntaxKind::Ident {
                return None;
            }
            let receiver_name = before_dot.text().to_string();
            // Only search the body that contains the cursor.
            let (body, _source_map, inference) = completion::find_containing_body_data(
                &self.db, &def_map, position.position, offset,
            )?;
            for (bid, binding) in body.bindings() {
                if binding.name() == Some(&receiver_name) {
                    return inference.type_of_binding(bid).cloned();
                }
            }
            None
        })?;

        Some((receiver_ty, field_name, token_range))
    }

    /// Go to implementation(s) of a trait method.
    pub fn goto_implementation(
        &self,
        position: ProjectPosition,
    ) -> Vec<NavigationTarget> {
        let def_map = self.db.def_map(position.position.file_id);
        let semantics = Semantics::new(Rc::clone(&self.db), def_map.clone());
        let Some(definition) = semantics.find_def_at(position.position) else {
            return Vec::new();
        };
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
        let trait_name = owner_def.name();

        let mut targets = Vec::new();
        for def in def_map.definitions() {
            if def.kind() != DefKind::Impl {
                continue;
            }
            // Check if this impl is for our trait (by matching trait_ref name).
            let sig = def.signature();
            let trait_match = match sig {
                crate::hir::ItemSignature::Impl(s) => s
                    .trait_ref()
                    .as_ref()
                    .is_some_and(|tr| tr.name_matches(trait_name)),
                _ => false,
            };
            if !trait_match {
                continue;
            }
            // Look for a method with the same name in this impl's children.
            for method_def in def_map.members(def.id()) {
                if method_def.name() == method_name {
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
        let def_map = self.db.def_map(position.position.file_id);
        let semantics = Semantics::new(Rc::clone(&self.db), def_map);

        // 1. Try local references.
        let local_refs = semantics.local_references_at(position.position, include_declaration);
        if !local_refs.is_empty() {
            let mut results: Vec<ReferenceResult> = local_refs
                .into_iter()
                .map(|range| {
                    let kind = if let Some(def_range) = semantics.local_definition_at(position.position)
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
            return results;
        }

        // 2. Try item-level references (cross-file).
        let def_map = self.db.def_map(position.position.file_id);
        let semantics = Semantics::new(Rc::clone(&self.db), def_map.clone());
        if let Some(definition) = semantics.find_def_at(position.position) {
            let target_name = definition.name().to_string();
            let target_id = definition.id();
            let target_file = definition.file_id();
            let mut results: Vec<ReferenceResult> = Vec::new();

            // Scan all function/method bodies across the project.
            for def in def_map.definitions() {
                if !matches!(def.kind(), DefKind::Function | DefKind::Method) {
                    continue;
                }
                let Some(body) = self.db.body(def.id()) else { continue };
                let Some(source_map) = self.db.body_source_map(def.id()) else {
                    continue;
                };
                let Some(resolution) = self.db.body_resolution(def.id()) else {
                    continue;
                };
                // Check name refs for matching text.
                for (nrid, nr) in body.name_refs() {
                    if nr.name() != Some(&target_name) {
                        continue;
                    }
                    // Try to resolve this name ref to an item.
                    let is_item_ref = matches!(
                        resolution.resolve(nrid),
                        Some(crate::hir::LocalResolveResult::NonLocal)
                    );
                    if !is_item_ref {
                        continue;
                    }
                    // Verify it points to our target by checking the definition
                    // at the name ref's position in its file.
                    let Some(fr) = source_map.name_ref_range(nrid) else {
                        continue;
                    };
                    let ref_file = def.file_id();
                    let ref_def_map = if ref_file == target_file {
                        def_map.clone()
                    } else {
                        self.db.def_map(ref_file)
                    };
                    let ref_semantics =
                        Semantics::new(Rc::clone(&self.db), ref_def_map);
                    if let Some(ref_def) = ref_semantics.find_def_at(
                        crate::FilePosition::new(ref_file, fr.range.start()),
                    )
                        && ref_def.id() == target_id
                    {
                        results.push(ReferenceResult::new(fr, ReferenceKind::Read));
                    }
                }
            }

            // Include the declaration.
            if include_declaration {
                results.push(ReferenceResult::new(
                    FileRange::new(target_file, definition.name_range()),
                    ReferenceKind::Declaration,
                ));
            }

            ReferenceResult::normalize(&mut results);
            if !results.is_empty() {
                return results;
            }
        }

        Vec::new()
    }

    /// Check whether the symbol at the cursor can be renamed.
    pub fn prepare_rename(&self, position: ProjectPosition) -> Option<RenameTarget> {
        if self.is_file_read_only(position.position.file_id) {
            return None;
        }
        let def_map = self.db.def_map(position.position.file_id);
        let semantics = Semantics::new(Rc::clone(&self.db), def_map);

        // Local binding rename.
        if let crate::hir::LocalResolveResult::Resolved(target) =
            semantics.resolve_local_at(position.position)
        {
            let owner_def = target.owner().owner();
            if let Some(body) = self.db.body(owner_def)
                && let Some(source_map) = self.db.body_source_map(owner_def)
                    && let Some(file_range) = source_map.binding_range(target.binding()) {
                        let name = body
                            .binding(target.binding())
                            .and_then(|b| b.name())
                            .unwrap_or("?");
                        return Some(RenameTarget::new(file_range, name));
                    }
        }

        // Item rename.
        if let Some(definition) = semantics.find_def_at(position.position) {
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
            refs.iter().map(|r| (r.range().file_id, TextEdit::new(r.range().range, new_name))),
            |file_id| self.is_file_read_only(file_id),
        )
    }

    // -- call hierarchy --------------------------------------------------

    /// Find the function/method definition at the cursor for call hierarchy.
    pub fn call_hierarchy_prepare(
        &self,
        position: ProjectPosition,
    ) -> Option<CallHierarchyItem> {
        let def_map = self.db.def_map(position.position.file_id);
        let semantics = Semantics::new(Rc::clone(&self.db), def_map);
        let definition = semantics.find_def_at(position.position)?;
        if !matches!(
            definition.kind(),
            DefKind::Function | DefKind::Method
        ) {
            return None;
        }
        Some(CallHierarchyItem {
            name: definition.name().to_string(),
            kind: definition.kind(),
            file_id: definition.file_id(),
            range: definition.name_range(),
        })
    }

    /// Find all callers of a function/method.
    pub fn call_hierarchy_incoming(
        &self,
        item: &CallHierarchyItem,
    ) -> Vec<CallHierarchyItem> {
        let def_map = self.db.def_map(item.file_id);
        let mut callers = Vec::new();
        for def in def_map.definitions() {
            if !matches!(def.kind(), DefKind::Function | DefKind::Method) {
                continue;
            }
            let Some(body) = self.db.body(def.id()) else { continue };
            let Some(_source_map) = self.db.body_source_map(def.id()) else {
                continue;
            };
            // Check if this body contains a call to the target.
            let has_call = body.exprs().any(|(_eid, expr)| match expr {
                crate::hir::Expr::Call { callee, .. } => {
                    // Check if callee name matches
                    if let crate::hir::Expr::Path(path) =
                        body.expr(*callee).unwrap_or(&crate::hir::Expr::Missing)
                    {
                        path.last()
                            .and_then(|nrid| body.name_ref(*nrid))
                            .and_then(|nr| nr.name())
                            == Some(&item.name)
                    } else {
                        false
                    }
                }
                crate::hir::Expr::MethodCall { method, .. } => {
                    body.name_ref(*method)
                        .and_then(|nr| nr.name())
                        == Some(&item.name)
                }
                _ => false,
            });
            if has_call {
                callers.push(CallHierarchyItem {
                    name: def.name().to_string(),
                    kind: def.kind(),
                    file_id: def.file_id(),
                    range: def.name_range(),
                });
            }
        }
        callers
    }

    /// Find all functions/methods called by this one.
    pub fn call_hierarchy_outgoing(
        &self,
        item: &CallHierarchyItem,
    ) -> Vec<CallHierarchyItem> {
        let def_map = self.db.def_map(item.file_id);
        let target_id = match def_map
            .definitions()
            .find(|d| {
                d.name() == item.name
                    && d.file_id() == item.file_id
                    && matches!(d.kind(), DefKind::Function | DefKind::Method)
            })
            .map(|d| d.id())
        {
            Some(id) => id,
            None => return Vec::new(),
        };
        let Some(body) = self.db.body(target_id) else {
            return Vec::new();
        };
        let mut callees = Vec::new();
        for (_eid, expr) in body.exprs() {
            let name = match expr {
                crate::hir::Expr::Call { callee, .. } => {
                    if let crate::hir::Expr::Path(path) = body.expr(*callee).unwrap_or(&crate::hir::Expr::Missing) {
                        path.last()
                            .and_then(|nrid| body.name_ref(*nrid))
                            .and_then(|nr| nr.name())
                            .map(|n| n.to_string())
                    } else {
                        None
                    }
                }
                crate::hir::Expr::MethodCall { method, .. } => {
                    body.name_ref(*method).and_then(|nr| nr.name().map(|n| n.to_string()))
                }
                _ => None,
            };
            if let Some(name) = name
                && let Some(target_def) = def_map
                    .definitions()
                    .find(|d| {
                        d.name() == name
                            && matches!(d.kind(), DefKind::Function | DefKind::Method)
                    })
            {
                callees.push(CallHierarchyItem {
                    name: target_def.name().to_string(),
                    kind: target_def.kind(),
                    file_id: target_def.file_id(),
                    range: target_def.name_range(),
                });
            }
        }
        callees
    }

    // -- type hierarchy ---------------------------------------------------

    /// Find the type definition at the cursor for type hierarchy.
    pub fn type_hierarchy_prepare(
        &self,
        position: ProjectPosition,
    ) -> Option<TypeHierarchyItem> {
        let def_map = self.db.def_map(position.position.file_id);
        let semantics = Semantics::new(Rc::clone(&self.db), def_map);
        let definition = semantics.find_def_at(position.position)?;
        if !matches!(
            definition.kind(),
            DefKind::Struct | DefKind::Enum | DefKind::Trait | DefKind::Impl
        ) {
            return None;
        }
        Some(TypeHierarchyItem {
            name: definition.name().to_string(),
            kind: definition.kind(),
            file_id: definition.file_id(),
            range: definition.name_range(),
        })
    }

    /// Find supertypes (traits implemented) for a type.
    pub fn type_hierarchy_supertypes(
        &self,
        item: &TypeHierarchyItem,
    ) -> Vec<TypeHierarchyItem> {
        let def_map = self.db.def_map(item.file_id);
        let mut result = Vec::new();
        // Find all impl blocks whose target type matches this item.
        for def in def_map.definitions() {
            if def.kind() != DefKind::Impl {
                continue;
            }
            let sig = def.signature();
            if let crate::hir::ItemSignature::Impl(s) = sig {
                // Check if the impl is for our type (by matching target type name)
                if s.target_type().name_matches(&item.name)
                {
                    // Add the trait being implemented
                    if let Some(trait_ref) = s.trait_ref()
                        && let Some(trait_name) = trait_ref.syntax()
                        && let Some(trait_def) = def_map.definitions().find(|d| {
                            d.kind() == DefKind::Trait && d.name() == trait_name
                        })
                    {
                        result.push(TypeHierarchyItem {
                            name: trait_def.name().to_string(),
                            kind: DefKind::Trait,
                            file_id: trait_def.file_id(),
                            range: trait_def.name_range(),
                        });
                    }
                }
            }
        }
        result
    }

    /// Find subtypes (implementors) of a trait.
    pub fn type_hierarchy_subtypes(
        &self,
        item: &TypeHierarchyItem,
    ) -> Vec<TypeHierarchyItem> {
        let def_map = self.db.def_map(item.file_id);
        let mut result = Vec::new();
        for def in def_map.definitions() {
            if def.kind() != DefKind::Impl {
                continue;
            }
            let sig = def.signature();
            if let crate::hir::ItemSignature::Impl(s) = sig
                && let Some(trait_ref) = s.trait_ref()
                    && let Some(trait_name) = trait_ref.syntax()
                    && trait_name == item.name
                {
                    // Add the implementing type
                    if let Some(type_name) = s.target_type().syntax()
                        && let Some(type_def) = def_map.definitions().find(|d| {
                            matches!(
                                d.kind(),
                                DefKind::Struct | DefKind::Enum
                            ) && d.name() == type_name
                        }) {
                            result.push(TypeHierarchyItem {
                                name: type_def.name().to_string(),
                                kind: type_def.kind(),
                                file_id: type_def.file_id(),
                                range: type_def.name_range(),
                            });
                        }
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

fn item_hover_text(definition: &Definition, db: &BaseDb, root_file: FileId) -> String {
    let def_map = db.def_map(root_file);
    // Delegate to the shared signature formatter in the completion module.
    // For Impl blocks (which definition_signature returns None for), show a
    // simple label.
    if definition.kind() == DefKind::Impl {
        return format!("impl {}", definition.name());
    }
    completion::definition_signature(db, &def_map, definition)
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
    use super::AnalysisHost;
    use crate::vfs::{Change, FileId, FileKind, SourceRootId, SourceRootKind};

    #[test]
    fn analysis_host_applies_changes_and_exposes_parse() {
        let file_id = FileId::new(0);
        let mut change = Change::new();
        change.set_file_text(file_id, "fn main() {}");

        let mut host = AnalysisHost::new();
        host.apply_change(change);
        let analysis = host.analysis();

        assert_eq!(
            analysis.parse(file_id).syntax_node().text().to_string(),
            "fn main() {}"
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

