//! Lexical scopes and body-local name resolution.
//!
//! This layer depends only on semantic [`Body`] data. It deliberately leaves
//! item paths and members as [`LocalResolveResult::NonLocal`] for the semantic
//! facade to resolve with a project-specific definition map.

use std::{collections::HashSet, ops::Index};

use super::{
    BindingId, Block, Body, BodyId, Condition, Expr, ExprId, MatchArm, NameRefId, Pat, PatId,
    Statement,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeId(u32);

impl ScopeId {
    fn new(index: usize) -> Self {
        Self(u32::try_from(index).expect("body scope space exhausted"))
    }

    pub const fn index(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScopeKind {
    Root,
    Block { expr: ExprId },
    AfterLet { binding: BindingId },
    Closure { expr: ExprId },
    ForBody { binding: BindingId },
    IfLetBody { pattern: PatId },
    WhileLetBody { pattern: PatId },
    MatchArm,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeData {
    parent: Option<ScopeId>,
    children: Vec<ScopeId>,
    kind: ScopeKind,
    bindings: Vec<BindingId>,
    ambiguous_names: Vec<String>,
}

impl ScopeData {
    pub const fn parent(&self) -> Option<ScopeId> {
        self.parent
    }

    pub fn children(&self) -> &[ScopeId] {
        &self.children
    }

    pub const fn kind(&self) -> ScopeKind {
        self.kind
    }

    pub fn bindings(&self) -> &[BindingId] {
        &self.bindings
    }

    pub fn ambiguous_names(&self) -> &[String] {
        &self.ambiguous_names
    }

    pub fn is_name_ambiguous(&self, name: &str) -> bool {
        self.ambiguous_names
            .iter()
            .any(|ambiguous| ambiguous == name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LocalCandidate {
    name_ref: NameRefId,
    scope: ScopeId,
    kind: LocalUseKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodyScopes {
    body_id: BodyId,
    root: ScopeId,
    scopes: Vec<ScopeData>,
    expr_scopes: Vec<Option<ScopeId>>,
    pat_scopes: Vec<Option<ScopeId>>,
    binding_scopes: Vec<Option<ScopeId>>,
    name_ref_scopes: Vec<Option<ScopeId>>,
    local_candidates: Vec<LocalCandidate>,
}

impl BodyScopes {
    pub const fn body_id(&self) -> BodyId {
        self.body_id
    }

    pub const fn root(&self) -> ScopeId {
        self.root
    }

    pub fn scope(&self, id: ScopeId) -> Option<&ScopeData> {
        self.scopes.get(id.index() as usize)
    }

    pub fn scopes(&self) -> impl ExactSizeIterator<Item = (ScopeId, &ScopeData)> {
        self.scopes
            .iter()
            .enumerate()
            .map(|(index, scope)| (ScopeId::new(index), scope))
    }

    pub fn scope_for_expr(&self, id: ExprId) -> Option<ScopeId> {
        self.expr_scopes.get(id.index() as usize).copied().flatten()
    }

    pub fn scope_for_pattern(&self, id: PatId) -> Option<ScopeId> {
        self.pat_scopes.get(id.index() as usize).copied().flatten()
    }

    pub fn scope_for_binding(&self, id: BindingId) -> Option<ScopeId> {
        self.binding_scopes
            .get(id.index() as usize)
            .copied()
            .flatten()
    }

    pub fn scope_for_name_ref(&self, id: NameRefId) -> Option<ScopeId> {
        self.name_ref_scopes
            .get(id.index() as usize)
            .copied()
            .flatten()
    }

    pub(crate) fn build(body: &Body) -> Self {
        ScopeBuilder::new(body).build()
    }
}

impl Index<ScopeId> for BodyScopes {
    type Output = ScopeData;

    fn index(&self, id: ScopeId) -> &Self::Output {
        &self.scopes[id.index() as usize]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LocalBindingId {
    owner: BodyId,
    binding: BindingId,
}

impl LocalBindingId {
    pub const fn new(owner: BodyId, binding: BindingId) -> Self {
        Self { owner, binding }
    }

    pub const fn owner(self) -> BodyId {
        self.owner
    }

    pub const fn binding(self) -> BindingId {
        self.binding
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LocalResolveResult {
    Resolved(LocalBindingId),
    NonLocal,
    Ambiguous,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LocalUseKind {
    Read,
    Write,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalUse {
    name_ref: NameRefId,
    target: LocalBindingId,
    kind: LocalUseKind,
    captured_by: Vec<ExprId>,
}

impl LocalUse {
    pub const fn name_ref(&self) -> NameRefId {
        self.name_ref
    }

    pub const fn target(&self) -> LocalBindingId {
        self.target
    }

    pub const fn kind(&self) -> LocalUseKind {
        self.kind
    }

    /// Closure expressions crossed from the binding to this use, outermost first.
    pub fn captured_by(&self) -> &[ExprId] {
        &self.captured_by
    }

    pub const fn is_capture(&self) -> bool {
        !self.captured_by.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LocalCapture {
    closure: ExprId,
    binding: LocalBindingId,
    first_use: NameRefId,
}

impl LocalCapture {
    pub const fn closure(self) -> ExprId {
        self.closure
    }

    pub const fn binding(self) -> LocalBindingId {
        self.binding
    }

    pub const fn first_use(self) -> NameRefId {
        self.first_use
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodyResolution {
    body_id: BodyId,
    results: Vec<LocalResolveResult>,
    uses: Vec<LocalUse>,
    captures: Vec<LocalCapture>,
}

impl BodyResolution {
    pub const fn body_id(&self) -> BodyId {
        self.body_id
    }

    pub fn resolve(&self, name_ref: NameRefId) -> Option<LocalResolveResult> {
        self.results.get(name_ref.index() as usize).copied()
    }

    /// Resolved local uses in semantic source order.
    pub fn uses(&self) -> &[LocalUse] {
        &self.uses
    }

    pub fn uses_for(&self, binding: LocalBindingId) -> impl Iterator<Item = &LocalUse> {
        self.uses
            .iter()
            .filter(move |local_use| local_use.target == binding)
    }

    /// Unique `(closure, binding)` captures, ordered by their first source use.
    pub fn captures(&self) -> &[LocalCapture] {
        &self.captures
    }

    pub fn captures_for(&self, closure: ExprId) -> impl Iterator<Item = LocalCapture> + '_ {
        self.captures
            .iter()
            .copied()
            .filter(move |capture| capture.closure == closure)
    }

    pub(crate) fn resolve_body(body: &Body, scopes: &BodyScopes) -> Self {
        LocalResolver::new(body, scopes).resolve()
    }
}

struct ScopeBuilder<'body> {
    body: &'body Body,
    scopes: BodyScopes,
}

impl<'body> ScopeBuilder<'body> {
    fn new(body: &'body Body) -> Self {
        let expr_count = body.exprs().len();
        let pat_count = body.patterns().len();
        let binding_count = body.bindings().len();
        let name_ref_count = body.name_refs().len();
        let root = ScopeId::new(0);
        Self {
            body,
            scopes: BodyScopes {
                body_id: body.id(),
                root,
                scopes: vec![ScopeData {
                    parent: None,
                    children: Vec::new(),
                    kind: ScopeKind::Root,
                    bindings: Vec::new(),
                    ambiguous_names: Vec::new(),
                }],
                expr_scopes: vec![None; expr_count],
                pat_scopes: vec![None; pat_count],
                binding_scopes: vec![None; binding_count],
                name_ref_scopes: vec![None; name_ref_count],
                local_candidates: Vec::new(),
            },
        }
    }

    fn build(mut self) -> BodyScopes {
        let root = self.scopes.root;
        self.add_bindings(root, self.body.params().iter().copied(), false);
        self.visit_expr(self.body.root_expr(), root, LocalUseKind::Read);
        self.scopes
    }

    fn alloc_scope(
        &mut self,
        parent: ScopeId,
        kind: ScopeKind,
        bindings: impl IntoIterator<Item = BindingId>,
        poison_bindings: bool,
    ) -> ScopeId {
        let id = ScopeId::new(self.scopes.scopes.len());
        self.scopes.scopes.push(ScopeData {
            parent: Some(parent),
            children: Vec::new(),
            kind,
            bindings: Vec::new(),
            ambiguous_names: Vec::new(),
        });
        self.scopes.scopes[parent.index() as usize]
            .children
            .push(id);
        self.add_bindings(id, bindings, poison_bindings);
        id
    }

    fn add_bindings(
        &mut self,
        scope: ScopeId,
        bindings: impl IntoIterator<Item = BindingId>,
        poison_bindings: bool,
    ) {
        let valid = bindings
            .into_iter()
            .filter(|binding| {
                self.body
                    .binding(*binding)
                    .is_some_and(|binding| !binding.is_missing())
            })
            .collect::<Vec<_>>();

        let mut ambiguous = Vec::new();
        for (index, binding) in valid.iter().enumerate() {
            let Some(name) = self.body[*binding].name() else {
                continue;
            };
            let duplicate = valid[..index]
                .iter()
                .any(|previous| self.body[*previous].name() == Some(name));
            if (poison_bindings || duplicate)
                && !ambiguous.iter().any(|candidate| candidate == name)
            {
                ambiguous.push(name.to_string());
            }
        }

        let data = &mut self.scopes.scopes[scope.index() as usize];
        data.bindings.extend(valid.iter().copied());
        data.ambiguous_names.extend(ambiguous);
        for binding in valid {
            self.scopes.binding_scopes[binding.index() as usize] = Some(scope);
        }
    }

    fn visit_expr(&mut self, expr_id: ExprId, scope: ScopeId, use_kind: LocalUseKind) {
        let Some(expr) = self.body.expr(expr_id) else {
            return;
        };
        if matches!(expr, Expr::Block(_)) {
            let Expr::Block(block) = expr else {
                return;
            };
            let block_scope =
                self.alloc_scope(scope, ScopeKind::Block { expr: expr_id }, [], false);
            self.set_expr_scope(expr_id, block_scope);
            self.visit_block(block, block_scope);
            return;
        }

        self.set_expr_scope(expr_id, scope);
        match expr {
            Expr::Missing | Expr::Literal(_) => {}
            Expr::Path(path) => self.visit_expr_path(path, scope, use_kind),
            Expr::Unary { expr, .. } | Expr::Try { expr } => {
                self.visit_expr(*expr, scope, LocalUseKind::Read);
            }
            Expr::Paren { expr } => self.visit_expr(*expr, scope, use_kind),
            Expr::Binary { lhs, rhs, .. } => {
                self.visit_expr(*lhs, scope, LocalUseKind::Read);
                self.visit_expr(*rhs, scope, LocalUseKind::Read);
            }
            Expr::Range { start, end, .. } => {
                self.visit_expr(*start, scope, LocalUseKind::Read);
                self.visit_expr(*end, scope, LocalUseKind::Read);
            }
            Expr::Closure { params, body, .. } => {
                let closure_scope = self.alloc_scope(
                    scope,
                    ScopeKind::Closure { expr: expr_id },
                    params.iter().copied(),
                    false,
                );
                self.visit_expr(*body, closure_scope, LocalUseKind::Read);
            }
            Expr::Assign { target, value } => {
                if self.is_local_assignment_target(*target) {
                    self.visit_expr(*target, scope, LocalUseKind::Write);
                } else {
                    self.visit_expr(*target, scope, LocalUseKind::Read);
                }
                self.visit_expr(*value, scope, LocalUseKind::Read);
            }
            Expr::Call { callee, args } => {
                self.visit_expr(*callee, scope, LocalUseKind::Read);
                for argument in args {
                    self.visit_expr(*argument, scope, LocalUseKind::Read);
                }
            }
            Expr::MethodCall {
                receiver,
                method,
                args,
                ..
            } => {
                self.visit_expr(*receiver, scope, LocalUseKind::Read);
                self.visit_non_local(*method, scope);
                for argument in args {
                    self.visit_expr(*argument, scope, LocalUseKind::Read);
                }
            }
            Expr::Field { base, field } => {
                self.visit_expr(*base, scope, LocalUseKind::Read);
                self.visit_non_local(*field, scope);
            }
            Expr::Index { base, index } => {
                self.visit_expr(*base, scope, LocalUseKind::Read);
                self.visit_expr(*index, scope, LocalUseKind::Read);
            }
            Expr::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let then_scope = match condition {
                    Condition::Expr(condition) => {
                        self.visit_expr(*condition, scope, LocalUseKind::Read);
                        scope
                    }
                    Condition::Let { pattern, scrutinee } => {
                        let bindings = self.visit_pattern(*pattern, scope);
                        self.visit_expr(*scrutinee, scope, LocalUseKind::Read);
                        self.alloc_scope(
                            scope,
                            ScopeKind::IfLetBody { pattern: *pattern },
                            bindings,
                            false,
                        )
                    }
                };
                self.visit_expr(*then_branch, then_scope, LocalUseKind::Read);
                if let Some(else_branch) = else_branch {
                    self.visit_expr(*else_branch, scope, LocalUseKind::Read);
                }
            }
            Expr::Match { scrutinee, arms } => {
                self.visit_expr(*scrutinee, scope, LocalUseKind::Read);
                for arm in arms {
                    self.visit_match_arm(arm, scope);
                }
            }
            Expr::StructLiteral { path, fields } => {
                for segment in path {
                    self.visit_non_local(*segment, scope);
                }
                for field in fields {
                    self.visit_non_local(field.name(), scope);
                    self.visit_expr(field.value(), scope, LocalUseKind::Read);
                }
            }
            Expr::MacroCall { macro_name, args } => {
                self.visit_non_local(*macro_name, scope);
                for argument in args {
                    self.visit_expr(*argument, scope, LocalUseKind::Read);
                }
            }
            Expr::Block(_) => {}
        }
    }

    fn visit_block(&mut self, block: &Block, initial_scope: ScopeId) {
        let mut scope = initial_scope;
        for statement in block.statements() {
            scope = self.visit_statement(statement, scope);
        }
        if let Some(tail) = block.tail() {
            self.visit_expr(tail, scope, LocalUseKind::Read);
        }
    }

    fn visit_statement(&mut self, statement: &Statement, scope: ScopeId) -> ScopeId {
        match statement {
            Statement::Missing | Statement::Break | Statement::Continue => scope,
            Statement::Let {
                binding,
                initializer,
            } => {
                self.visit_expr(*initializer, scope, LocalUseKind::Read);
                self.alloc_scope(
                    scope,
                    ScopeKind::AfterLet { binding: *binding },
                    [*binding],
                    false,
                )
            }
            Statement::Expr { expr, .. } => {
                self.visit_expr(*expr, scope, LocalUseKind::Read);
                scope
            }
            Statement::Return { value } => {
                if let Some(value) = value {
                    self.visit_expr(*value, scope, LocalUseKind::Read);
                }
                scope
            }
            Statement::While { condition, body } => {
                let body_scope = match condition {
                    Condition::Expr(condition) => {
                        self.visit_expr(*condition, scope, LocalUseKind::Read);
                        scope
                    }
                    Condition::Let { pattern, scrutinee } => {
                        let bindings = self.visit_pattern(*pattern, scope);
                        self.visit_expr(*scrutinee, scope, LocalUseKind::Read);
                        self.alloc_scope(
                            scope,
                            ScopeKind::WhileLetBody { pattern: *pattern },
                            bindings,
                            false,
                        )
                    }
                };
                self.visit_expr(*body, body_scope, LocalUseKind::Read);
                scope
            }
            Statement::Loop { body } => {
                self.visit_expr(*body, scope, LocalUseKind::Read);
                scope
            }
            Statement::For {
                binding,
                iterable,
                body,
            } => {
                self.visit_expr(*iterable, scope, LocalUseKind::Read);
                let body_scope = self.alloc_scope(
                    scope,
                    ScopeKind::ForBody { binding: *binding },
                    [*binding],
                    false,
                );
                self.visit_expr(*body, body_scope, LocalUseKind::Read);
                scope
            }
        }
    }

    fn visit_match_arm(&mut self, arm: &MatchArm, scope: ScopeId) {
        let mut bindings = Vec::new();
        for pattern in arm.patterns() {
            bindings.extend(self.visit_pattern(*pattern, scope));
        }
        let poison = arm.patterns().len() > 1 && !bindings.is_empty();
        let arm_scope = self.alloc_scope(scope, ScopeKind::MatchArm, bindings, poison);
        if let Some(guard) = arm.guard() {
            self.visit_expr(guard, arm_scope, LocalUseKind::Read);
        }
        self.visit_expr(arm.body(), arm_scope, LocalUseKind::Read);
    }

    fn visit_pattern(&mut self, pattern_id: PatId, scope: ScopeId) -> Vec<BindingId> {
        self.set_pat_scope(pattern_id, scope);
        let Some(pattern) = self.body.pattern(pattern_id) else {
            return Vec::new();
        };
        match pattern {
            Pat::Missing | Pat::Wildcard | Pat::Literal(_) | Pat::Range { .. } => Vec::new(),
            Pat::Binding { binding } => vec![*binding],
            Pat::Path(path) => {
                for segment in path {
                    self.visit_non_local(*segment, scope);
                }
                Vec::new()
            }
            Pat::TupleVariant { path, subpatterns } => {
                for segment in path {
                    self.visit_non_local(*segment, scope);
                }
                let mut bindings = Vec::new();
                for subpattern in subpatterns {
                    bindings.extend(self.visit_pattern(*subpattern, scope));
                }
                bindings
            }
            Pat::StructVariant { path, fields, .. } => {
                for segment in path {
                    self.visit_non_local(*segment, scope);
                }
                let mut bindings = Vec::new();
                for field in fields {
                    self.visit_non_local(field.name(), scope);
                    bindings.extend(self.visit_pattern(field.pattern(), scope));
                }
                bindings
            }
        }
    }

    fn visit_expr_path(&mut self, path: &[NameRefId], scope: ScopeId, kind: LocalUseKind) {
        if let [name_ref] = path {
            self.set_name_ref_scope(*name_ref, scope);
            self.scopes.local_candidates.push(LocalCandidate {
                name_ref: *name_ref,
                scope,
                kind,
            });
        } else {
            for name_ref in path {
                self.visit_non_local(*name_ref, scope);
            }
        }
    }

    fn is_local_assignment_target(&self, expr_id: ExprId) -> bool {
        match self.body.expr(expr_id) {
            Some(Expr::Path(path)) => path.len() == 1,
            Some(Expr::Paren { expr }) => self.is_local_assignment_target(*expr),
            _ => false,
        }
    }

    fn visit_non_local(&mut self, name_ref: NameRefId, scope: ScopeId) {
        self.set_name_ref_scope(name_ref, scope);
    }

    fn set_expr_scope(&mut self, expr: ExprId, scope: ScopeId) {
        if let Some(slot) = self.scopes.expr_scopes.get_mut(expr.index() as usize) {
            *slot = Some(scope);
        }
    }

    fn set_pat_scope(&mut self, pattern: PatId, scope: ScopeId) {
        if let Some(slot) = self.scopes.pat_scopes.get_mut(pattern.index() as usize) {
            *slot = Some(scope);
        }
    }

    fn set_name_ref_scope(&mut self, name_ref: NameRefId, scope: ScopeId) {
        if let Some(slot) = self
            .scopes
            .name_ref_scopes
            .get_mut(name_ref.index() as usize)
        {
            *slot = Some(scope);
        }
    }
}

struct LocalResolver<'a> {
    body: &'a Body,
    scopes: &'a BodyScopes,
    results: Vec<LocalResolveResult>,
    uses: Vec<LocalUse>,
    captures: Vec<LocalCapture>,
    seen_captures: HashSet<(ExprId, BindingId)>,
}

impl<'a> LocalResolver<'a> {
    fn new(body: &'a Body, scopes: &'a BodyScopes) -> Self {
        Self {
            body,
            scopes,
            results: vec![LocalResolveResult::NonLocal; body.name_refs().len()],
            uses: Vec::new(),
            captures: Vec::new(),
            seen_captures: HashSet::new(),
        }
    }

    fn resolve(mut self) -> BodyResolution {
        if self.body.id() != self.scopes.body_id() {
            return BodyResolution {
                body_id: self.body.id(),
                results: self.results,
                uses: self.uses,
                captures: self.captures,
            };
        }

        for candidate in &self.scopes.local_candidates {
            let Some(name) = self
                .body
                .name_ref(candidate.name_ref)
                .and_then(|name_ref| name_ref.name())
            else {
                continue;
            };
            let result = self.lookup(candidate.scope, name);
            self.results[candidate.name_ref.index() as usize] = result;
            if let LocalResolveResult::Resolved(target) = result {
                let mut captured_by = self.crossed_closures(
                    candidate.scope,
                    self.scopes.scope_for_binding(target.binding()),
                );
                captured_by.reverse();
                for closure in &captured_by {
                    if self.seen_captures.insert((*closure, target.binding())) {
                        self.captures.push(LocalCapture {
                            closure: *closure,
                            binding: target,
                            first_use: candidate.name_ref,
                        });
                    }
                }
                self.uses.push(LocalUse {
                    name_ref: candidate.name_ref,
                    target,
                    kind: candidate.kind,
                    captured_by,
                });
            }
        }

        BodyResolution {
            body_id: self.body.id(),
            results: self.results,
            uses: self.uses,
            captures: self.captures,
        }
    }

    fn lookup(&self, mut scope: ScopeId, name: &str) -> LocalResolveResult {
        loop {
            let Some(data) = self.scopes.scope(scope) else {
                return LocalResolveResult::NonLocal;
            };
            if data.is_name_ambiguous(name) {
                return LocalResolveResult::Ambiguous;
            }
            let mut matching = data.bindings().iter().filter(|binding| {
                self.body
                    .binding(**binding)
                    .and_then(|binding| binding.name())
                    == Some(name)
            });
            let Some(binding) = matching.next().copied() else {
                let Some(parent) = data.parent() else {
                    return LocalResolveResult::NonLocal;
                };
                scope = parent;
                continue;
            };
            if matching.next().is_some() {
                return LocalResolveResult::Ambiguous;
            }
            return LocalResolveResult::Resolved(LocalBindingId::new(self.body.id(), binding));
        }
    }

    fn crossed_closures(
        &self,
        mut use_scope: ScopeId,
        binding_scope: Option<ScopeId>,
    ) -> Vec<ExprId> {
        let Some(binding_scope) = binding_scope else {
            return Vec::new();
        };
        let mut closures = Vec::new();
        while use_scope != binding_scope {
            let Some(data) = self.scopes.scope(use_scope) else {
                break;
            };
            if let ScopeKind::Closure { expr } = data.kind() {
                closures.push(expr);
            }
            let Some(parent) = data.parent() else {
                break;
            };
            use_scope = parent;
        }
        closures
    }
}
