//! Native, error-tolerant type inference over lowered bodies.

use super::{
    BinaryOp, BindingId, BindingKind, Body, BodyId, BodyResolution, CallableRequirement,
    CallableSignature, CallableTy, Condition, DefId, DefKind, DefMap, Definition, Expr, ExprId,
    ItemSignature, LiteralKind, LocalResolveResult, MatchArm, MemberIndex, MemberKind,
    MemberResolution, MemberTarget, NameRefId, Pat, PatId, Statement, StructField, Substitution,
    Ty, TypeRef, UnaryOp, unify,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InferenceSource {
    Expr(ExprId),
    Binding(BindingId),
    Pattern(PatId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TypeMismatchContext {
    Annotation,
    Return,
    Assignment,
    Argument { index: u32 },
    ClosureReturn,
    Branch,
    RangeBound,
    Index,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InferenceDiagnostic {
    TypeMismatch {
        source: InferenceSource,
        expected: Ty,
        actual: Ty,
        context: TypeMismatchContext,
    },
    ExpectedBool {
        expr: ExprId,
        actual: Ty,
    },
    ArgumentCount {
        call: ExprId,
        expected: usize,
        actual: usize,
    },
    NotCallable {
        callee: ExprId,
        actual: Ty,
    },
    NotIterable {
        expr: ExprId,
        actual: Ty,
    },
    InvalidUnary {
        expr: ExprId,
        operand: Ty,
        op: UnaryOp,
    },
    InvalidBinary {
        expr: ExprId,
        lhs: Ty,
        rhs: Ty,
        op: BinaryOp,
    },
    UnsatisfiedTraitBound {
        call: ExprId,
        actual: Ty,
        trait_id: DefId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CallTarget {
    Definition(DefId),
    Closure(ExprId),
    Builtin,
    Unresolved,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallInfo {
    target: CallTarget,
    parameters: Vec<Ty>,
    return_type: Ty,
    substitution: Substitution,
}

impl CallInfo {
    pub const fn target(&self) -> CallTarget {
        self.target
    }

    pub fn parameters(&self) -> &[Ty] {
        &self.parameters
    }

    pub const fn return_type(&self) -> &Ty {
        &self.return_type
    }

    pub const fn substitution(&self) -> &Substitution {
        &self.substitution
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InferenceResult {
    body_id: BodyId,
    expr_types: Vec<Ty>,
    pattern_types: Vec<Ty>,
    binding_types: Vec<Ty>,
    calls: Vec<Option<CallInfo>>,
    member_resolutions: Vec<Option<MemberResolution>>,
    diagnostics: Vec<InferenceDiagnostic>,
}

impl InferenceResult {
    pub const fn body_id(&self) -> BodyId {
        self.body_id
    }

    pub fn type_of_expr(&self, expr: ExprId) -> Option<&Ty> {
        self.expr_types.get(expr.index() as usize)
    }

    pub fn type_of_pattern(&self, pattern: PatId) -> Option<&Ty> {
        self.pattern_types.get(pattern.index() as usize)
    }

    pub fn type_of_binding(&self, binding: BindingId) -> Option<&Ty> {
        self.binding_types.get(binding.index() as usize)
    }

    pub fn call_info(&self, call: ExprId) -> Option<&CallInfo> {
        self.calls.get(call.index() as usize)?.as_ref()
    }

    pub fn member_resolution(&self, name_ref: NameRefId) -> Option<&MemberResolution> {
        self.member_resolutions
            .get(name_ref.index() as usize)?
            .as_ref()
    }

    pub fn diagnostics(&self) -> &[InferenceDiagnostic] {
        &self.diagnostics
    }
}

pub(crate) fn infer_body(
    body: &Body,
    resolution: &BodyResolution,
    def_map: &DefMap,
    member_index: &MemberIndex,
) -> InferenceResult {
    InferenceContext::new(body, resolution, def_map, member_index).infer()
}

struct InferenceContext<'a> {
    body: &'a Body,
    resolution: &'a BodyResolution,
    def_map: &'a DefMap,
    member_index: &'a MemberIndex,
    owner: &'a Definition,
    return_ty: Ty,
    expr_types: Vec<Ty>,
    pattern_types: Vec<Ty>,
    binding_types: Vec<Ty>,
    binding_closures: Vec<Option<ExprId>>,
    calls: Vec<Option<CallInfo>>,
    member_resolutions: Vec<Option<MemberResolution>>,
    diagnostics: Vec<InferenceDiagnostic>,
    closure_returns: Vec<Ty>,
}

impl<'a> InferenceContext<'a> {
    fn new(
        body: &'a Body,
        resolution: &'a BodyResolution,
        def_map: &'a DefMap,
        member_index: &'a MemberIndex,
    ) -> Self {
        let owner = def_map
            .definition(body.owner())
            .expect("body owner must belong to its definition map");
        let return_ty = member_index
            .callable(owner.id())
            .map(|callable| callable.return_ty().clone())
            .unwrap_or(Ty::Unknown);
        Self {
            body,
            resolution,
            def_map,
            member_index,
            owner,
            return_ty,
            expr_types: vec![Ty::Unknown; body.exprs().len()],
            pattern_types: vec![Ty::Unknown; body.patterns().len()],
            binding_types: vec![Ty::Unknown; body.bindings().len()],
            binding_closures: vec![None; body.bindings().len()],
            calls: vec![None; body.exprs().len()],
            member_resolutions: vec![None; body.name_refs().len()],
            diagnostics: Vec::new(),
            closure_returns: Vec::new(),
        }
    }

    fn infer(mut self) -> InferenceResult {
        self.seed_parameters();
        let root = self.body.root_expr();
        let expected = self.return_ty.clone();
        let actual = self.infer_expr(root, Some(&expected));
        self.report_mismatch(
            InferenceSource::Expr(root),
            &expected,
            &actual,
            TypeMismatchContext::Return,
        );
        InferenceResult {
            body_id: self.body.id(),
            expr_types: self.expr_types,
            pattern_types: self.pattern_types,
            binding_types: self.binding_types,
            calls: self.calls,
            member_resolutions: self.member_resolutions,
            diagnostics: self.diagnostics,
        }
    }

    fn seed_parameters(&mut self) {
        let signature = callable_signature(self.owner);
        let mut parameters = signature.into_iter().flat_map(CallableSignature::params);
        for binding in self.body.params() {
            let is_self = self
                .body
                .binding(*binding)
                .is_some_and(|binding| binding.kind() == BindingKind::SelfParameter);
            let signature_parameter = (!is_self).then(|| parameters.next()).flatten();
            let ty = self
                .body
                .binding(*binding)
                .and_then(|binding| binding.type_ref())
                .map(|type_ref| self.lower_type(self.owner, type_ref))
                .or_else(|| {
                    signature_parameter
                        .map(|parameter| self.lower_type(self.owner, parameter.type_ref()))
                })
                .or_else(|| {
                    is_self
                        .then(|| self.member_index.receiver_type(self.owner.id()))
                        .flatten()
                })
                .unwrap_or(Ty::Unknown);
            self.set_binding(*binding, ty);
        }
    }

    fn infer_expr(&mut self, expr_id: ExprId, expected: Option<&Ty>) -> Ty {
        let Some(expr) = self.body.expr(expr_id).cloned() else {
            return Ty::Unknown;
        };
        let ty = match expr {
            Expr::Missing => Ty::Unknown,
            Expr::Literal(literal) => match literal.kind() {
                LiteralKind::Integer => Ty::I64,
                LiteralKind::Float => Ty::F64,
                LiteralKind::String => Ty::STRING,
                LiteralKind::Boolean => Ty::BOOL,
            },
            Expr::Path(path) => {
                let inferred = self.infer_path(&path, expected);
                if inferred.is_unknown()
                    && matches!(expected, Some(Ty::Option(_)))
                    && path_display(self.body, &path) == "None"
                    && self.is_unshadowed_path(&path)
                {
                    let ty = expected.cloned().unwrap_or(Ty::Unknown);
                    self.record_builtin_associated(&path, "None", &ty);
                    ty
                } else {
                    inferred
                }
            }
            Expr::Unary { op, expr } => self.infer_unary(expr_id, op, expr),
            Expr::Binary { op, lhs, rhs } => self.infer_binary(expr_id, op, lhs, rhs),
            Expr::Range { start, end, .. } => {
                let mut diverges = false;
                for bound in [start, end] {
                    let bound_ty = self.infer_expr(bound, Some(&Ty::I64));
                    diverges |= bound_ty.is_never();
                    self.report_mismatch(
                        InferenceSource::Expr(bound),
                        &Ty::I64,
                        &bound_ty,
                        TypeMismatchContext::RangeBound,
                    );
                }
                if diverges {
                    Ty::Never
                } else {
                    Ty::Iterator(Box::new(Ty::I64))
                }
            }
            Expr::Closure {
                params,
                return_type,
                body,
            } => self.infer_closure(expr_id, &params, return_type.as_ref(), body, expected),
            Expr::Assign { target, value } => {
                let target_ty = self.infer_expr(target, None);
                let value_ty = self.infer_expr(value, Some(&target_ty));
                self.report_mismatch(
                    InferenceSource::Expr(value),
                    &target_ty,
                    &value_ty,
                    TypeMismatchContext::Assignment,
                );
                if target_ty.is_never() || value_ty.is_never() {
                    Ty::Never
                } else {
                    Ty::UNIT
                }
            }
            Expr::Try { expr } => match self.infer_expr(expr, expected) {
                Ty::Result(ok, _) => *ok,
                Ty::Never => Ty::Never,
                _ => Ty::Unknown,
            },
            Expr::Call { callee, args } => self.infer_call(expr_id, callee, &args, expected),
            Expr::MethodCall {
                receiver,
                method,
                type_args,
                args,
            } => self.infer_method_call(expr_id, receiver, method, &type_args, &args, expected),
            Expr::Field { base, field } => self.infer_field(base, field),
            Expr::Index { base, index } => {
                let base_ty = self.infer_expr(base, None);
                let index_expected = match &base_ty {
                    Ty::Vec(_) | Ty::Iterator(_) => Ty::I64,
                    Ty::HashMap(key, _) => (**key).clone(),
                    _ => Ty::Unknown,
                };
                let index_ty = self.infer_expr(index, Some(&index_expected));
                self.report_mismatch(
                    InferenceSource::Expr(index),
                    &index_expected,
                    &index_ty,
                    TypeMismatchContext::Index,
                );
                if base_ty.is_never() || index_ty.is_never() {
                    Ty::Never
                } else {
                    match base_ty {
                        Ty::Vec(item) | Ty::Iterator(item) => *item,
                        Ty::HashMap(_, value) => *value,
                        _ => Ty::Unknown,
                    }
                }
            }
            Expr::Paren { expr } => self.infer_expr(expr, expected),
            Expr::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let condition_diverges = self.infer_condition(condition);
                let then_ty = self.infer_expr(then_branch, expected);
                let mut else_fact = None;
                let result = match else_branch {
                    Some(else_branch) => {
                        let else_ty = self.infer_expr(else_branch, expected);
                        let result = then_ty.join(&else_ty);
                        else_fact = Some((else_branch, else_ty));
                        result
                    }
                    None => Ty::UNIT,
                };
                if result.is_unknown()
                    && let Some(expected) = expected
                {
                    self.report_mismatch(
                        InferenceSource::Expr(then_branch),
                        expected,
                        &then_ty,
                        TypeMismatchContext::Branch,
                    );
                    if let Some((else_branch, else_ty)) = else_fact {
                        self.report_mismatch(
                            InferenceSource::Expr(else_branch),
                            expected,
                            &else_ty,
                            TypeMismatchContext::Branch,
                        );
                    }
                }
                if condition_diverges {
                    Ty::Never
                } else {
                    result
                }
            }
            Expr::Match { scrutinee, arms } => {
                let scrutinee_ty = self.infer_expr(scrutinee, None);
                let result = self.infer_match(&arms, &scrutinee_ty, expected);
                if scrutinee_ty.is_never() {
                    Ty::Never
                } else {
                    result
                }
            }
            Expr::StructLiteral { path, fields } => {
                self.infer_struct_literal(&path, &fields, expected)
            }
            Expr::MacroCall { macro_name, args } => self.infer_macro(macro_name, &args, expected),
            Expr::Block(block) => self.infer_block(block.statements(), block.tail(), expected),
        };
        self.set_expr(expr_id, ty.clone());
        ty
    }

    fn infer_block(
        &mut self,
        statements: &[Statement],
        tail: Option<ExprId>,
        expected: Option<&Ty>,
    ) -> Ty {
        let mut diverges = false;
        for statement in statements {
            let statement_diverges = self.infer_statement(statement);
            if !diverges {
                diverges = statement_diverges;
            }
        }
        match tail {
            Some(tail) => {
                let tail_ty = self.infer_expr(tail, expected);
                if diverges { Ty::Never } else { tail_ty }
            }
            None if diverges => Ty::Never,
            None => Ty::UNIT,
        }
    }

    fn infer_statement(&mut self, statement: &Statement) -> bool {
        match statement {
            Statement::Missing => false,
            Statement::Break | Statement::Continue => true,
            Statement::Let {
                binding,
                initializer,
            } => {
                let annotation = self
                    .body
                    .binding(*binding)
                    .and_then(|binding| binding.type_ref())
                    .map(|type_ref| self.lower_type(self.owner, type_ref));
                let actual = self.infer_expr(*initializer, annotation.as_ref());
                if let Some(expected) = annotation.as_ref() {
                    self.report_mismatch(
                        InferenceSource::Expr(*initializer),
                        expected,
                        &actual,
                        TypeMismatchContext::Annotation,
                    );
                }
                let diverges = actual.is_never();
                let binding_ty = annotation.unwrap_or(actual);
                self.set_binding(*binding, binding_ty);
                self.binding_closures[binding.index() as usize] = self.closure_origin(*initializer);
                diverges
            }
            Statement::Expr { expr, .. } => self.infer_expr(*expr, None).is_never(),
            Statement::Return { value } => {
                let actual = value
                    .map(|value| self.infer_expr(value, Some(&self.return_ty.clone())))
                    .unwrap_or(Ty::UNIT);
                self.report_mismatch(
                    value
                        .map(InferenceSource::Expr)
                        .unwrap_or(InferenceSource::Expr(self.body.root_expr())),
                    &self.return_ty.clone(),
                    &actual,
                    TypeMismatchContext::Return,
                );
                if let Some(returned) = self.closure_returns.last_mut() {
                    *returned = returned.join(&actual);
                }
                true
            }
            Statement::While { condition, body } => {
                let diverges = self.infer_condition(*condition);
                self.infer_expr(*body, Some(&Ty::UNIT));
                diverges
            }
            Statement::Loop { body } => {
                self.infer_expr(*body, Some(&Ty::UNIT));
                !self.expr_contains_break(*body)
            }
            Statement::For {
                binding,
                iterable,
                body,
            } => {
                let iterable_ty = self.infer_expr(*iterable, None);
                let (item_ty, diverges) = match iterable_ty.clone() {
                    Ty::Iterator(item) | Ty::Vec(item) => (*item, false),
                    Ty::Never => (Ty::Unknown, true),
                    Ty::Primitive(_) => {
                        self.diagnostics.push(InferenceDiagnostic::NotIterable {
                            expr: *iterable,
                            actual: iterable_ty,
                        });
                        (Ty::Unknown, false)
                    }
                    _ => (Ty::Unknown, false),
                };
                self.set_binding(*binding, item_ty);
                self.infer_expr(*body, Some(&Ty::UNIT));
                diverges
            }
        }
    }

    fn infer_condition(&mut self, condition: Condition) -> bool {
        match condition {
            Condition::Expr(expr) => {
                let actual = self.infer_expr(expr, Some(&Ty::BOOL));
                self.expect_bool(expr, &actual);
                actual.is_never()
            }
            Condition::Let { pattern, scrutinee } => {
                let scrutinee_ty = self.infer_expr(scrutinee, None);
                self.infer_pattern(pattern, &scrutinee_ty);
                scrutinee_ty.is_never()
            }
        }
    }

    fn infer_match(&mut self, arms: &[MatchArm], scrutinee: &Ty, expected: Option<&Ty>) -> Ty {
        let mut result = Ty::Never;
        let mut arm_facts = Vec::with_capacity(arms.len());
        // Each arm has its own unique BindingIds for pattern bindings, so no
        // explicit snapshot/restore is needed — arm bindings are independent.
        for arm in arms {
            for pattern in arm.patterns() {
                self.infer_pattern_with_narrow(*pattern, scrutinee);
            }
            if let Some(guard) = arm.guard() {
                let guard_ty = self.infer_expr(guard, Some(&Ty::BOOL));
                self.expect_bool(guard, &guard_ty);
            }
            let arm_ty = self.infer_expr(arm.body(), expected);
            result = result.join(&arm_ty);
            arm_facts.push((arm.body(), arm_ty));
        }
        if result.is_unknown()
            && let Some(expected) = expected
        {
            for (body, arm_ty) in arm_facts {
                self.report_mismatch(
                    InferenceSource::Expr(body),
                    expected,
                    &arm_ty,
                    TypeMismatchContext::Branch,
                );
            }
        }
        if arms.is_empty() { Ty::Unknown } else { result }
    }

    fn infer_pattern(&mut self, pattern_id: PatId, expected: &Ty) {
        self.infer_pattern_with_narrow(pattern_id, expected);
    }

    /// Like `infer_pattern` but when `expected` is an enum/aggregate type, resolves
    /// the variant path so inner bindings receive narrowed types instead of Unknown.
    fn infer_pattern_with_narrow(&mut self, pattern_id: PatId, expected: &Ty) {
        let Some(pattern) = self.body.pattern(pattern_id).cloned() else {
            return;
        };
        self.set_pattern(pattern_id, expected.clone());
        match &pattern {
            Pat::Binding { binding } => self.set_binding(*binding, expected.clone()),
            Pat::Literal(literal) => {
                let actual = literal_ty(literal.kind());
                self.set_pattern(pattern_id, actual.clone());
                self.report_mismatch(
                    InferenceSource::Pattern(pattern_id),
                    expected,
                    &actual,
                    TypeMismatchContext::Branch,
                );
            }
            Pat::Range { start, end, .. } => {
                let actual = literal_ty(start.kind()).join(&literal_ty(end.kind()));
                self.set_pattern(pattern_id, actual.clone());
                self.report_mismatch(
                    InferenceSource::Pattern(pattern_id),
                    expected,
                    &actual,
                    TypeMismatchContext::Branch,
                );
            }
            Pat::Path(_path) => {
                // For unit-variant patterns like `None`, `Ok`, `Err` — no bindings to
                // narrow, but the pattern type is validated against the scrutinee.
            }
            Pat::TupleVariant { path, subpatterns } => {
                let narrowed = self.resolve_variant_payload(path, expected);
                let narrowed_fields = narrowed.unwrap_or_else(|| {
                    vec![Ty::Unknown; subpatterns.len()]
                });
                for (index, subpattern) in subpatterns.iter().enumerate() {
                    let field_ty = narrowed_fields.get(index).cloned().unwrap_or(Ty::Unknown);
                    self.infer_pattern_with_narrow(*subpattern, &field_ty);
                }
            }
            Pat::StructVariant { path, fields, .. } => {
                let variant_def = self.resolve_variant_def(path, expected);
                for field in fields {
                    let field_name = self
                        .body
                        .name_ref(field.name())
                        .and_then(|name_ref| name_ref.name())
                        .unwrap_or_default();
                    let field_ty = variant_def
                        .and_then(|variant| {
                            self.member_index
                                .resolve_variant_field(variant, expected, field_name)
                        })
                        .map(|resolution| resolution.ty().clone())
                        .unwrap_or(Ty::Unknown);
                    self.infer_pattern_with_narrow(field.pattern(), &field_ty);
                }
            }
            Pat::Missing | Pat::Wildcard => {}
        }
    }

    /// Given a variant path like `["Some"]` and a scrutinee type like `Option<i64>`,
    /// resolve the variant and return its payload field types.
    fn resolve_variant_payload(&self, path: &[NameRefId], scrutinee: &Ty) -> Option<Vec<Ty>> {
        let name = self
            .body
            .name_ref(*path.last()?)
            .and_then(|name_ref| name_ref.name())?;
        // Use resolve_associated_ty which works for both user-defined enums (via
        // DefId) and builtin types like Option/Result.
        let resolution = self.member_index.resolve_associated_ty(scrutinee, name)?;
        if resolution.kind() != MemberKind::Variant {
            return None;
        }
        let result_ty = resolution.ty().clone();
        let substitution = resolution.substitution();
        let payload = substitution.instantiate(&result_ty);
        self.extract_payload_types(&payload)
    }

    /// Extract the inner field types from a variant's payload type, handling
    /// both user-defined aggregate variants and builtin Function-wrapped variants.
    fn extract_payload_types(&self, payload: &Ty) -> Option<Vec<Ty>> {
        match payload {
            Ty::Function(callable) | Ty::Closure(callable) => {
                // Builtin variants like `Some(T)` are modelled as callables.
                // The params are the variant fields; the return type is the enum.
                if callable.params().is_empty() {
                    // Unit variant like `None`.
                    Some(Vec::new())
                } else {
                    Some(callable.params().to_vec())
                }
            }
            Ty::Named(payload_named) => {
                let mut field_types = Vec::new();
                let candidates =
                    self.member_index
                        .field_candidates(&Ty::Named(payload_named.clone()));
                for candidate in candidates {
                    if candidate.kind() == MemberKind::Field {
                        field_types.push(candidate.ty().clone());
                    }
                }
                Some(field_types)
            }
            Ty::Tuple(items) => Some(items.clone()),
            _ => Some(Vec::new()),
        }
    }

    /// Resolve the variant DefId from a pattern path like `["Rect"]` against a
    /// scrutinee type. Returns the variant's DefId for subsequent field lookup.
    fn resolve_variant_def(&self, path: &[NameRefId], scrutinee: &Ty) -> Option<DefId> {
        let name = self
            .body
            .name_ref(*path.last()?)
            .and_then(|name_ref| name_ref.name())?;
        let resolution = self.member_index.resolve_associated_ty(scrutinee, name)?;
        if resolution.kind() != MemberKind::Variant {
            return None;
        }
        match resolution.target() {
            MemberTarget::Definition(def) => Some(def),
            _ => None,
        }
    }

    fn infer_unary(&mut self, expr_id: ExprId, op: UnaryOp, operand: ExprId) -> Ty {
        let operand_ty = self.infer_expr(operand, None);
        if operand_ty.is_never() {
            return Ty::Never;
        }
        match op {
            UnaryOp::Negate if operand_ty.is_numeric() => operand_ty,
            UnaryOp::Negate if matches!(operand_ty, Ty::Named(_)) => Ty::Unknown,
            UnaryOp::Negate if !operand_ty.is_concrete() => Ty::Unknown,
            UnaryOp::Negate => {
                self.diagnostics.push(InferenceDiagnostic::InvalidUnary {
                    expr: expr_id,
                    operand: operand_ty,
                    op,
                });
                Ty::Unknown
            }
            UnaryOp::Not => {
                if operand_ty.is_concrete() && !operand_ty.is_compatible_with(&Ty::BOOL) {
                    self.diagnostics.push(InferenceDiagnostic::InvalidUnary {
                        expr: expr_id,
                        operand: operand_ty,
                        op,
                    });
                }
                Ty::BOOL
            }
            UnaryOp::Missing => Ty::Unknown,
        }
    }

    fn infer_binary(&mut self, expr_id: ExprId, op: BinaryOp, lhs: ExprId, rhs: ExprId) -> Ty {
        let lhs_ty = self.infer_expr(lhs, None);
        let rhs_ty = self.infer_expr(rhs, None);
        if lhs_ty.is_never() || rhs_ty.is_never() {
            return Ty::Never;
        }
        match op {
            BinaryOp::Add
            | BinaryOp::Subtract
            | BinaryOp::Multiply
            | BinaryOp::Divide
            | BinaryOp::Remainder => {
                if op == BinaryOp::Add && lhs_ty == Ty::STRING && rhs_ty == Ty::STRING {
                    return Ty::STRING;
                }
                if matches!(lhs_ty, Ty::Named(_)) || matches!(rhs_ty, Ty::Named(_)) {
                    return Ty::Unknown;
                }
                if lhs_ty.is_numeric() && rhs_ty.is_numeric() {
                    return lhs_ty.join(&rhs_ty);
                }
                if lhs_ty == Ty::BOOL
                    || lhs_ty == Ty::UNIT
                    || rhs_ty == Ty::BOOL
                    || rhs_ty == Ty::UNIT
                {
                    self.diagnostics.push(InferenceDiagnostic::InvalidBinary {
                        expr: expr_id,
                        lhs: lhs_ty,
                        rhs: rhs_ty,
                        op,
                    });
                }
                Ty::Unknown
            }
            BinaryOp::And | BinaryOp::Or => {
                self.expect_bool(lhs, &lhs_ty);
                self.expect_bool(rhs, &rhs_ty);
                Ty::BOOL
            }
            BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessOrEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterOrEqual => {
                // Ordering operators require numeric operands.
                if matches!(
                    op,
                    BinaryOp::Less
                        | BinaryOp::LessOrEqual
                        | BinaryOp::Greater
                        | BinaryOp::GreaterOrEqual
                ) && !lhs_ty.is_unknown()
                    && !rhs_ty.is_unknown()
                    && !lhs_ty.is_never()
                    && !rhs_ty.is_never()
                    && !(lhs_ty.is_numeric() && rhs_ty.is_numeric())
                {
                    self.diagnostics
                        .push(InferenceDiagnostic::InvalidBinary {
                            expr: expr_id,
                            lhs: lhs_ty.clone(),
                            rhs: rhs_ty.clone(),
                            op,
                        });
                }
                Ty::BOOL
            }
            BinaryOp::Missing => Ty::Unknown,
        }
    }

    fn infer_path(&mut self, path: &[NameRefId], expected: Option<&Ty>) -> Ty {
        if let [name_ref] = path {
            match self.resolution.resolve(*name_ref) {
                Some(LocalResolveResult::Resolved(local)) if local.owner() == self.body.id() => {
                    return self
                        .binding_types
                        .get(local.binding().index() as usize)
                        .cloned()
                        .unwrap_or(Ty::Unknown);
                }
                Some(LocalResolveResult::Ambiguous) => return Ty::Unknown,
                Some(LocalResolveResult::NonLocal) | None => {}
                Some(LocalResolveResult::Resolved(_)) => return Ty::Unknown,
            }
        }
        let Some(definition) = self.resolve_definition_path(path) else {
            return self.infer_associated_path(path, expected);
        };
        match definition.kind() {
            DefKind::Function | DefKind::ExternFunction => self
                .member_index
                .callable(definition.id())
                .map(Ty::Function)
                .unwrap_or(Ty::Unknown),
            DefKind::Struct | DefKind::Enum => self
                .member_index
                .type_template(definition.id())
                .map(|template| Substitution::new().instantiate(template))
                .unwrap_or(Ty::Unknown),
            _ => Ty::Unknown,
        }
    }

    fn infer_associated_path(&mut self, path: &[NameRefId], expected: Option<&Ty>) -> Ty {
        let Some((member_ref, owner_path)) = path.split_last() else {
            return Ty::Unknown;
        };
        let Some(owner) = self.resolve_definition_path(owner_path) else {
            return Ty::Unknown;
        };
        if !matches!(owner.kind(), DefKind::Struct | DefKind::Enum) {
            return Ty::Unknown;
        }
        let owner_id = owner.id();
        let Some(name) = self
            .body
            .name_ref(*member_ref)
            .and_then(|name_ref| name_ref.name())
        else {
            return Ty::Unknown;
        };
        let Some(resolution) = self.member_index.resolve_associated(owner_id, name) else {
            return Ty::Unknown;
        };
        let mut ty = resolution.ty().clone();
        if let Some(expected) = expected {
            let mut substitution = resolution.substitution().clone();
            if unify(&ty, expected, &mut substitution).is_match() {
                ty = substitution.instantiate(&ty);
            }
        }
        self.set_member(*member_ref, resolution);
        ty
    }

    fn infer_closure(
        &mut self,
        closure: ExprId,
        params: &[BindingId],
        return_type: Option<&TypeRef>,
        body: ExprId,
        expected: Option<&Ty>,
    ) -> Ty {
        let expected_callable = match expected {
            Some(Ty::Function(callable) | Ty::Closure(callable)) => Some(callable.clone()),
            _ => None,
        };
        let mut param_types = Vec::with_capacity(params.len());
        for (index, binding) in params.iter().enumerate() {
            let ty = self
                .body
                .binding(*binding)
                .and_then(|binding| binding.type_ref())
                .map(|type_ref| self.lower_type(self.owner, type_ref))
                .or_else(|| {
                    expected_callable
                        .as_ref()
                        .and_then(|callable| callable.params().get(index).cloned())
                })
                .unwrap_or(Ty::Unknown);
            self.set_binding(*binding, ty.clone());
            param_types.push(ty);
        }
        let expected_return = return_type
            .map(|return_type| self.lower_type(self.owner, return_type))
            .or_else(|| {
                expected_callable
                    .as_ref()
                    .map(|callable| callable.return_ty().clone())
            });
        let closure_return = expected_return.clone().unwrap_or(Ty::Unknown);
        let outer_return = std::mem::replace(&mut self.return_ty, closure_return);
        self.closure_returns.push(Ty::Never);
        let actual_return = self.infer_expr(body, expected_return.as_ref());
        let explicit_return = self.closure_returns.pop().unwrap_or(Ty::Never);
        self.return_ty = outer_return;
        if let Some(expected_return) = expected_return.as_ref()
            && !actual_return.is_never() {
                self.report_mismatch(
                    InferenceSource::Expr(body),
                    expected_return,
                    &actual_return,
                    TypeMismatchContext::ClosureReturn,
                );
            }
        let inferred_return = explicit_return.join(&actual_return);
        // Prefer the inferred return type when the expected type is Unknown.
        let proclaimed_return = match expected_return {
            Some(ref ty) if !ty.is_unknown() => ty.clone(),
            _ => inferred_return,
        };
        let callable = CallableTy::new(param_types, proclaimed_return);
        let ty = Ty::Closure(callable);
        self.set_expr(closure, ty.clone());
        ty
    }

    fn infer_field(&mut self, base: ExprId, field: NameRefId) -> Ty {
        let receiver = self.infer_expr(base, None);
        if receiver.is_never() {
            return Ty::Never;
        }
        let Some(name) = self
            .body
            .name_ref(field)
            .and_then(|name_ref| name_ref.name())
        else {
            return Ty::Unknown;
        };
        let Some(resolution) = self.member_index.resolve_field(&receiver, name) else {
            return Ty::Unknown;
        };
        let ty = resolution.ty().clone();
        self.set_member(field, resolution);
        ty
    }

    fn infer_struct_literal(
        &mut self,
        path: &[NameRefId],
        fields: &[StructField],
        expected: Option<&Ty>,
    ) -> Ty {
        let Some((template, variant)) = self.struct_literal_target(path) else {
            let mut diverges = false;
            for field in fields {
                diverges |= self.infer_expr(field.value(), None).is_never();
            }
            return if diverges { Ty::Never } else { Ty::Unknown };
        };

        let mut substitution = Substitution::new();
        if let Some(expected) = expected {
            let _ = unify(&template, expected, &mut substitution);
        }
        let mut diverges = false;
        let mut resolved_fields = Vec::new();
        for field in fields {
            let Some(name) = self
                .body
                .name_ref(field.name())
                .and_then(|name_ref| name_ref.name())
            else {
                diverges |= self.infer_expr(field.value(), None).is_never();
                continue;
            };
            let current_ty = substitution.apply(&template);
            let resolution = match variant {
                Some(variant) => {
                    self.member_index
                        .resolve_variant_field(variant, &current_ty, name)
                }
                None => self.member_index.resolve_field(&current_ty, name),
            };
            let expected_field = resolution.as_ref().map(MemberResolution::ty);
            let actual = self.infer_expr(field.value(), expected_field);
            diverges |= actual.is_never();
            if let Some(expected_field) = expected_field {
                let _ = unify(expected_field, &actual, &mut substitution);
                self.report_mismatch(
                    InferenceSource::Expr(field.value()),
                    expected_field,
                    &actual,
                    TypeMismatchContext::Assignment,
                );
                resolved_fields.push((field.name(), name.to_string()));
            }
        }

        let result = substitution.instantiate(&template);
        for (name_ref, name) in resolved_fields {
            let resolution = match variant {
                Some(variant) => self
                    .member_index
                    .resolve_variant_field(variant, &result, &name),
                None => self.member_index.resolve_field(&result, &name),
            };
            if let Some(resolution) = resolution {
                self.set_member(name_ref, resolution);
            }
        }
        if diverges { Ty::Never } else { result }
    }

    fn struct_literal_target(&mut self, path: &[NameRefId]) -> Option<(Ty, Option<DefId>)> {
        if let Some(definition) = self.resolve_definition_path(path)
            && definition.kind() == DefKind::Struct
        {
            return Some((
                self.member_index.type_template(definition.id())?.clone(),
                None,
            ));
        }

        let (member_ref, owner_path) = path.split_last()?;
        let owner_id = {
            let owner = self.resolve_definition_path(owner_path)?;
            (owner.kind() == DefKind::Enum).then_some(owner.id())?
        };
        let name = self.body.name_ref(*member_ref)?.name()?;
        let resolution = self.member_index.resolve_associated(owner_id, name)?;
        if resolution.kind() != MemberKind::Variant {
            return None;
        }
        let MemberTarget::Definition(variant) = resolution.target() else {
            return None;
        };
        let template = resolution.ty().clone();
        if !matches!(template, Ty::Named(_)) {
            return None;
        }
        self.set_member(*member_ref, resolution);
        Some((template, Some(variant)))
    }

    #[allow(clippy::too_many_arguments)]
    fn infer_method_call(
        &mut self,
        call: ExprId,
        receiver: ExprId,
        method: NameRefId,
        type_args: &[TypeRef],
        args: &[ExprId],
        expected: Option<&Ty>,
    ) -> Ty {
        let receiver_ty = self.infer_expr(receiver, None);
        if receiver_ty.is_never() {
            for argument in args {
                self.infer_expr(*argument, None);
            }
            return Ty::Never;
        }
        let Some(name) = self
            .body
            .name_ref(method)
            .and_then(|name_ref| name_ref.name())
        else {
            return self.infer_unresolved_member_call(call, args);
        };
        let resolution =
            self.member_index
                .resolve_method_in(&receiver_ty, name, self.owner.id());
        let Some(resolution) = resolution else {
            // Fallback: Vec -> Iterator conversion methods.
            if let Ty::Vec(item) = &receiver_ty
                && let Some(result) =
                    self.infer_vec_to_iterator(call, item, name, args)
                {
                    return result;
                }
            // Fallback: iterator adapter methods not yet in the member index.
            if let Ty::Iterator(item) = &receiver_ty
                && let Some(result) =
                    self.infer_iterator_adapter(call, item, name, args, expected)
                {
                    return result;
                }
            return self.infer_unresolved_member_call(call, args);
        };
        let Ty::Function(callable) = resolution.ty() else {
            return self.infer_unresolved_member_call(call, args);
        };
        let callable = callable.clone();
        let target = member_call_target(resolution.target());
        let mut substitution = resolution.substitution().clone();
        let requirements = resolution.requirements().to_vec();
        for (generic, type_arg) in resolution.generic_params().iter().zip(type_args) {
            substitution.insert(*generic, self.lower_type(self.owner, type_arg));
        }
        self.set_member(method, resolution);
        self.infer_callable_call(
            call,
            target,
            &callable,
            args,
            expected,
            substitution,
            &requirements,
            false,
        )
    }

    fn infer_unresolved_member_call(&mut self, call: ExprId, args: &[ExprId]) -> Ty {
        let mut diverges = false;
        for argument in args {
            diverges |= self.infer_expr(*argument, None).is_never();
        }
        self.calls[call.index() as usize] = Some(CallInfo {
            target: CallTarget::Unresolved,
            parameters: Vec::new(),
            return_type: Ty::Unknown,
            substitution: Substitution::new(),
        });
        if diverges { Ty::Never } else { Ty::Unknown }
    }

    /// Handle `.iter()` and `.into_iter()` on `Vec<T>` returning `Iterator<T>`.
    fn infer_vec_to_iterator(
        &mut self,
        call: ExprId,
        item: &Ty,
        method_name: &str,
        args: &[ExprId],
    ) -> Option<Ty> {
        match method_name {
            "iter" | "into_iter" if args.is_empty() => {
                let result = Ty::Iterator(Box::new(item.clone()));
                self.calls[call.index() as usize] = Some(CallInfo {
                    target: CallTarget::Builtin,
                    parameters: Vec::new(),
                    return_type: result.clone(),
                    substitution: Substitution::new(),
                });
                Some(result)
            }
            _ => None,
        }
    }

    /// Infer iterator adapter method calls like `.map()`, `.filter()`, `.collect()`.
    /// These are not yet in the MemberIndex; we handle them inline until 4B.6's
    /// builtin metadata is complete enough.
    fn infer_iterator_adapter(
        &mut self,
        call: ExprId,
        item: &Ty,
        method_name: &str,
        args: &[ExprId],
        _expected: Option<&Ty>,
    ) -> Option<Ty> {
        let result = match method_name {
            // Consumers — return a non-iterator value.
            "count" => {
                if let [arg] = args {
                    self.infer_expr(*arg, None);
                }
                Ty::I64
            }
            "any" | "all" => {
                if let [predicate] = args {
                    let pred_closure_ty = Ty::Closure(CallableTy::new(
                        vec![item.clone()],
                        Ty::BOOL,
                    ));
                    self.infer_expr(*predicate, Some(&pred_closure_ty));
                }
                Ty::BOOL
            }
            "find" => {
                if let [predicate] = args {
                    let pred_closure_ty = Ty::Closure(CallableTy::new(
                        vec![item.clone()],
                        Ty::BOOL,
                    ));
                    self.infer_expr(*predicate, Some(&pred_closure_ty));
                }
                Ty::Option(Box::new(item.clone()))
            }
            "fold" => {
                match args {
                    [init, closure] => {
                        let init_ty = self.infer_expr(*init, None);
                        let fold_closure_ty = Ty::Closure(CallableTy::new(
                            vec![init_ty.clone(), item.clone()],
                            init_ty.clone(),
                        ));
                        self.infer_expr(*closure, Some(&fold_closure_ty));
                        init_ty
                    }
                    _ => {
                        for arg in args {
                            self.infer_expr(*arg, None);
                        }
                        return Some(Ty::Unknown);
                    }
                }
            }
            // Collectors / terminal adapters.
            "collect" => {
                for arg in args {
                    self.infer_expr(*arg, None);
                }
                Ty::Vec(Box::new(item.clone()))
            }
            // Adapters that preserve the item type.
            "filter" | "take" | "skip" => {
                if let [arg] = args {
                    let pred_closure_ty = Ty::Closure(CallableTy::new(
                        vec![item.clone()],
                        if method_name == "filter" { Ty::BOOL } else { Ty::Unknown },
                    ));
                    self.infer_expr(*arg, Some(&pred_closure_ty));
                }
                Ty::Iterator(Box::new(item.clone()))
            }
            // map: iterator item becomes the closure's return type.
            "map" => {
                if let [closure] = args {
                    // Infer the closure with expected type: |T| -> ?
                    let map_closure_ty = Ty::Closure(CallableTy::new(
                        vec![item.clone()],
                        Ty::Unknown,
                    ));
                    let closure_ty = self.infer_expr(*closure, Some(&map_closure_ty));
                    // Extract the closure's return type as the new item type.
                    let new_item = match &closure_ty {
                        Ty::Closure(callable) | Ty::Function(callable) => {
                            callable.return_ty().clone()
                        }
                        _ => Ty::Unknown,
                    };
                    Ty::Iterator(Box::new(new_item))
                } else {
                    Ty::Iterator(Box::new(item.clone()))
                }
            }
            // filter_map: extract Option<U>'s U as new item type.
            "filter_map" => {
                if let [closure] = args {
                    let map_closure_ty = Ty::Closure(CallableTy::new(
                        vec![item.clone()],
                        Ty::Unknown,
                    ));
                    let closure_ty = self.infer_expr(*closure, Some(&map_closure_ty));
                    let new_item = match &closure_ty {
                        Ty::Closure(callable) | Ty::Function(callable) => {
                            match callable.return_ty() {
                                Ty::Option(inner) => (**inner).clone(),
                                _ => Ty::Unknown,
                            }
                        }
                        _ => Ty::Unknown,
                    };
                    Ty::Iterator(Box::new(new_item))
                } else {
                    Ty::Iterator(Box::new(item.clone()))
                }
            }
            // enumerate: pair items with index. Takes no arguments.
            "enumerate" if args.is_empty() => {
                Ty::Iterator(Box::new(Ty::Tuple(vec![Ty::I64, item.clone()])))
            }
            _ => return None,
        };

        let diverges = result.is_never();
        self.calls[call.index() as usize] = Some(CallInfo {
            target: CallTarget::Builtin,
            parameters: Vec::new(),
            return_type: result.clone(),
            substitution: Substitution::new(),
        });
        Some(if diverges { Ty::Never } else { result })
    }

    fn infer_call(
        &mut self,
        call: ExprId,
        callee: ExprId,
        args: &[ExprId],
        expected: Option<&Ty>,
    ) -> Ty {
        if let Some(info) = self.infer_builtin_call(call, callee, args, expected) {
            let result = info.return_type.clone();
            self.calls[call.index() as usize] = Some(info);
            return result;
        }

        let callee_ty = self.infer_expr(callee, None);
        let closure = self.closure_target(callee);
        let (target, callable) = match callee_ty {
            Ty::Function(callable) => (
                callable
                    .target()
                    .map(CallTarget::Definition)
                    .unwrap_or(CallTarget::Unresolved),
                Some(callable),
            ),
            Ty::Closure(callable) => (
                closure
                    .map(CallTarget::Closure)
                    .unwrap_or(CallTarget::Unresolved),
                Some(callable),
            ),
            Ty::Never => {
                for argument in args {
                    self.infer_expr(*argument, None);
                }
                self.calls[call.index() as usize] = Some(CallInfo {
                    target: CallTarget::Unresolved,
                    parameters: Vec::new(),
                    return_type: Ty::Never,
                    substitution: Substitution::new(),
                });
                return Ty::Never;
            }
            Ty::Unknown | Ty::GenericParam(_) => (CallTarget::Unresolved, None),
            actual => {
                for argument in args {
                    self.infer_expr(*argument, None);
                }
                self.diagnostics
                    .push(InferenceDiagnostic::NotCallable { callee, actual });
                self.calls[call.index() as usize] = Some(CallInfo {
                    target: CallTarget::Unresolved,
                    parameters: Vec::new(),
                    return_type: Ty::Unknown,
                    substitution: Substitution::new(),
                });
                return Ty::Unknown;
            }
        };

        let Some(callable) = callable else {
            let mut diverges = false;
            for argument in args {
                diverges |= self.infer_expr(*argument, None).is_never();
            }
            self.calls[call.index() as usize] = Some(CallInfo {
                target,
                parameters: Vec::new(),
                return_type: Ty::Unknown,
                substitution: Substitution::new(),
            });
            return if diverges { Ty::Never } else { Ty::Unknown };
        };

        let variadic = callable
            .target()
            .and_then(|target| self.def_map.definition(target))
            .and_then(callable_signature)
            .is_some_and(CallableSignature::is_variadic);
        let requirements = match target {
            CallTarget::Definition(definition) => {
                self.member_index.callable_requirements(definition).to_vec()
            }
            CallTarget::Closure(_) | CallTarget::Builtin | CallTarget::Unresolved => Vec::new(),
        };
        self.infer_callable_call(
            call,
            target,
            &callable,
            args,
            expected,
            Substitution::new(),
            &requirements,
            variadic,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn infer_callable_call(
        &mut self,
        call: ExprId,
        target: CallTarget,
        callable: &CallableTy,
        args: &[ExprId],
        expected: Option<&Ty>,
        mut substitution: Substitution,
        requirements: &[CallableRequirement],
        variadic: bool,
    ) -> Ty {
        if (!variadic && args.len() != callable.params().len())
            || (variadic && args.len() < callable.params().len())
        {
            self.diagnostics.push(InferenceDiagnostic::ArgumentCount {
                call,
                expected: callable.params().len(),
                actual: args.len(),
            });
        }

        let mut diverges = false;
        for (index, argument) in args.iter().enumerate() {
            let parameter = callable.params().get(index);
            let instantiated = parameter.map(|parameter| substitution.apply(parameter));
            let actual = self.infer_expr(*argument, instantiated.as_ref());
            diverges |= actual.is_never();
            if let Some(parameter) = parameter {
                let result = unify(parameter, &actual, &mut substitution);
                self.report_mismatch(
                    InferenceSource::Expr(*argument),
                    instantiated.as_ref().unwrap_or(parameter),
                    &actual,
                    TypeMismatchContext::Argument {
                        index: u32::try_from(index).unwrap_or(u32::MAX),
                    },
                );
                if !result.is_match() {
                    invalidate_generics(parameter, &mut substitution);
                } else {
                    refine_generic_bindings(parameter, &actual, &mut substitution);
                }
            }
        }
        if let Some(expected) = expected {
            let _ = unify(callable.return_ty(), expected, &mut substitution);
        }
        self.report_unsatisfied_bounds(call, &substitution, requirements);
        let parameters = callable
            .params()
            .iter()
            .map(|parameter| substitution.instantiate(parameter))
            .collect::<Vec<_>>();
        let return_type = substitution.instantiate(callable.return_ty());
        self.calls[call.index() as usize] = Some(CallInfo {
            target,
            parameters,
            return_type: return_type.clone(),
            substitution,
        });
        if diverges { Ty::Never } else { return_type }
    }

    fn report_unsatisfied_bounds(
        &mut self,
        call: ExprId,
        substitution: &Substitution,
        requirements: &[CallableRequirement],
    ) {
        let mut failures = std::collections::BTreeSet::new();
        for (generic, actual) in substitution.iter() {
            if !matches!(actual, Ty::Named(_)) || !actual.is_concrete() {
                continue;
            }
            for bound in self.member_index.bounds(generic) {
                if !self.member_index.implements_trait(actual, bound.trait_id()) {
                    failures.insert((actual.clone(), bound.trait_id()));
                }
            }
        }
        for (target, bound) in requirements {
            let actual = substitution.apply(target);
            if !matches!(actual, Ty::Named(_)) || !actual.is_concrete() {
                continue;
            }
            if !self
                .member_index
                .implements_trait(&actual, bound.trait_id())
            {
                failures.insert((actual, bound.trait_id()));
            }
        }
        self.diagnostics
            .extend(failures.into_iter().map(|(actual, trait_id)| {
                InferenceDiagnostic::UnsatisfiedTraitBound {
                    call,
                    actual,
                    trait_id,
                }
            }));
    }

    fn infer_builtin_call(
        &mut self,
        call: ExprId,
        callee: ExprId,
        args: &[ExprId],
        expected: Option<&Ty>,
    ) -> Option<CallInfo> {
        let path = match self.body.expr(callee)? {
            Expr::Path(path) => path.clone(),
            _ => return None,
        };
        if let [name_ref] = path.as_slice()
            && !matches!(
                self.resolution.resolve(*name_ref),
                Some(LocalResolveResult::NonLocal) | None
            )
        {
            return None;
        }
        if self.resolve_definition_path(&path).is_some() || self.path_starts_with_definition(&path)
        {
            return None;
        }

        let name = path_display(self.body, &path);
        let expected_arity = match name.as_str() {
            "Some" | "Ok" | "Err" => 1,
            "Vec::new" | "HashMap::new" => 0,
            _ => return None,
        };
        if args.len() != expected_arity {
            for argument in args {
                self.infer_expr(*argument, None);
            }
            self.diagnostics.push(InferenceDiagnostic::ArgumentCount {
                call,
                expected: expected_arity,
                actual: args.len(),
            });
            let owner_ty = builtin_constructor_owner(&name, expected);
            self.record_builtin_associated(&path, &name, &owner_ty);
            self.set_expr(callee, Ty::Unknown);
            return Some(CallInfo {
                target: CallTarget::Builtin,
                parameters: Vec::new(),
                return_type: Ty::Unknown,
                substitution: Substitution::new(),
            });
        }

        let (parameters, return_type) = match (name.as_str(), args) {
            ("Some", [argument]) => {
                let expected_item = match expected {
                    Some(Ty::Option(item)) => Some((**item).clone()),
                    _ => None,
                };
                let actual = self.infer_expr(*argument, expected_item.as_ref());
                self.report_argument_mismatch(*argument, expected_item.as_ref(), &actual);
                let item = prefer_expected_if_unknown(actual, expected_item);
                (vec![item.clone()], Ty::Option(Box::new(item)))
            }
            ("Ok", [argument]) => {
                let expected_parts = match expected {
                    Some(Ty::Result(ok, error)) => Some(((**ok).clone(), (**error).clone())),
                    _ => None,
                };
                let expected_ok = expected_parts.as_ref().map(|(ok, _)| ok);
                let actual = self.infer_expr(*argument, expected_ok);
                self.report_argument_mismatch(*argument, expected_ok, &actual);
                let (expected_ok, error) = expected_parts.unwrap_or((Ty::Unknown, Ty::Unknown));
                let ok = prefer_expected_if_unknown(actual, Some(expected_ok));
                (vec![ok.clone()], Ty::Result(Box::new(ok), Box::new(error)))
            }
            ("Err", [argument]) => {
                let expected_parts = match expected {
                    Some(Ty::Result(ok, error)) => Some(((**ok).clone(), (**error).clone())),
                    _ => None,
                };
                let expected_error = expected_parts.as_ref().map(|(_, error)| error);
                let actual = self.infer_expr(*argument, expected_error);
                self.report_argument_mismatch(*argument, expected_error, &actual);
                let (ok, expected_error) = expected_parts.unwrap_or((Ty::Unknown, Ty::Unknown));
                let error = prefer_expected_if_unknown(actual, Some(expected_error));
                (
                    vec![error.clone()],
                    Ty::Result(Box::new(ok), Box::new(error)),
                )
            }
            ("Vec::new", []) => (Vec::new(), Ty::Vec(Box::new(Ty::Unknown))),
            ("HashMap::new", []) => (
                Vec::new(),
                Ty::HashMap(Box::new(Ty::Unknown), Box::new(Ty::Unknown)),
            ),
            _ => return None,
        };
        let return_type = if parameters.iter().any(Ty::is_never) {
            Ty::Never
        } else {
            return_type
        };
        self.record_builtin_associated(&path, &name, &return_type);
        self.set_expr(callee, Ty::Unknown);
        Some(CallInfo {
            target: CallTarget::Builtin,
            parameters,
            return_type,
            substitution: Substitution::new(),
        })
    }

    fn record_builtin_associated(&mut self, path: &[NameRefId], name: &str, owner_ty: &Ty) {
        let member_name = name.rsplit("::").next().unwrap_or(name);
        let Some(resolution) = self
            .member_index
            .resolve_associated_ty(owner_ty, member_name)
        else {
            return;
        };
        if let Some(name_ref) = path.last() {
            self.set_member(*name_ref, resolution);
        }
    }

    fn infer_macro(&mut self, name_ref: NameRefId, args: &[ExprId], expected: Option<&Ty>) -> Ty {
        let name = self
            .body
            .name_ref(name_ref)
            .and_then(|name_ref| name_ref.name())
            .unwrap_or_default();
        match name {
            "vec" => {
                let expected_item = match expected {
                    Some(Ty::Vec(item)) => Some((**item).clone()),
                    _ => None,
                };
                let mut item = Ty::Never;
                let mut diverges = false;
                for argument in args {
                    let actual = self.infer_expr(*argument, expected_item.as_ref());
                    self.report_argument_mismatch(*argument, expected_item.as_ref(), &actual);
                    diverges |= actual.is_never();
                    let item_ty = if actual.is_unknown() {
                        expected_item.clone().unwrap_or(Ty::Unknown)
                    } else {
                        actual
                    };
                    item = item.join(&item_ty);
                }
                if diverges {
                    Ty::Never
                } else if args.is_empty() {
                    Ty::Vec(Box::new(expected_item.unwrap_or(Ty::Unknown)))
                } else {
                    Ty::Vec(Box::new(item))
                }
            }
            "format" => {
                let mut diverges = false;
                for argument in args {
                    diverges |= self.infer_expr(*argument, None).is_never();
                }
                if diverges { Ty::Never } else { Ty::STRING }
            }
            "panic" => {
                for argument in args {
                    self.infer_expr(*argument, None);
                }
                Ty::Never
            }
            "print" | "println" => {
                let mut diverges = false;
                for argument in args {
                    diverges |= self.infer_expr(*argument, None).is_never();
                }
                if diverges { Ty::Never } else { Ty::UNIT }
            }
            _ => {
                let mut diverges = false;
                for argument in args {
                    diverges |= self.infer_expr(*argument, None).is_never();
                }
                if diverges { Ty::Never } else { Ty::Unknown }
            }
        }
    }

    fn resolve_definition_path(&self, path: &[NameRefId]) -> Option<&Definition> {
        let names = path
            .iter()
            .map(|name_ref| self.body.name_ref(*name_ref)?.name())
            .collect::<Option<Vec<_>>>()?;
        if names.first() == Some(&"self") {
            self.def_map
                .resolve_path_unique(self.owner.module_id(), names.get(1..)?)
        } else {
            self.def_map
                .resolve_path_lexical_unique(self.owner.module_id(), &names)
        }
    }

    fn is_unshadowed_path(&self, path: &[NameRefId]) -> bool {
        if let [name_ref] = path
            && !matches!(
                self.resolution.resolve(*name_ref),
                Some(LocalResolveResult::NonLocal) | None
            )
        {
            return false;
        }
        self.resolve_definition_path(path).is_none() && !self.path_starts_with_definition(path)
    }

    fn path_starts_with_definition(&self, path: &[NameRefId]) -> bool {
        let Some(name) = path
            .first()
            .and_then(|name_ref| self.body.name_ref(*name_ref))
            .and_then(|name_ref| name_ref.name())
        else {
            return false;
        };
        self.def_map
            .name_is_defined_lexically(self.owner.module_id(), name)
    }

    fn lower_type(&self, definition: &Definition, type_ref: &TypeRef) -> Ty {
        self.member_index.lower_type(definition.id(), type_ref)
    }

    fn expect_bool(&mut self, expr: ExprId, actual: &Ty) {
        if actual.is_concrete() && !actual.is_compatible_with(&Ty::BOOL) {
            self.diagnostics.push(InferenceDiagnostic::ExpectedBool {
                expr,
                actual: actual.clone(),
            });
        }
    }

    fn report_mismatch(
        &mut self,
        source: InferenceSource,
        expected: &Ty,
        actual: &Ty,
        context: TypeMismatchContext,
    ) {
        if expected.is_concrete() && actual.is_concrete() && !expected.is_compatible_with(actual) {
            self.diagnostics.push(InferenceDiagnostic::TypeMismatch {
                source,
                expected: expected.clone(),
                actual: actual.clone(),
                context,
            });
        }
    }

    fn report_argument_mismatch(&mut self, argument: ExprId, expected: Option<&Ty>, actual: &Ty) {
        if let Some(expected) = expected {
            self.report_mismatch(
                InferenceSource::Expr(argument),
                expected,
                actual,
                TypeMismatchContext::Argument { index: 0 },
            );
        }
    }

    fn closure_origin(&self, expr: ExprId) -> Option<ExprId> {
        match self.body.expr(expr)? {
            Expr::Closure { .. } => Some(expr),
            Expr::Paren { expr } => self.closure_origin(*expr),
            _ => None,
        }
    }

    fn expr_contains_break(&self, expr_id: ExprId) -> bool {
        let Some(expr) = self.body.expr(expr_id) else {
            return false;
        };
        match expr {
            Expr::Missing | Expr::Literal(_) | Expr::Path(_) | Expr::Closure { .. } => false,
            Expr::Unary { expr, .. } | Expr::Try { expr } | Expr::Paren { expr } => {
                self.expr_contains_break(*expr)
            }
            Expr::Binary { lhs, rhs, .. }
            | Expr::Range {
                start: lhs,
                end: rhs,
                ..
            }
            | Expr::Assign {
                target: lhs,
                value: rhs,
            }
            | Expr::Index {
                base: lhs,
                index: rhs,
            } => self.expr_contains_break(*lhs) || self.expr_contains_break(*rhs),
            Expr::Call { callee, args } => {
                self.expr_contains_break(*callee)
                    || args.iter().any(|arg| self.expr_contains_break(*arg))
            }
            Expr::MethodCall { receiver, args, .. } => {
                self.expr_contains_break(*receiver)
                    || args.iter().any(|arg| self.expr_contains_break(*arg))
            }
            Expr::Field { base, .. } => self.expr_contains_break(*base),
            Expr::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.condition_contains_break(*condition)
                    || self.expr_contains_break(*then_branch)
                    || else_branch.is_some_and(|branch| self.expr_contains_break(branch))
            }
            Expr::Match { scrutinee, arms } => {
                self.expr_contains_break(*scrutinee)
                    || arms.iter().any(|arm| {
                        arm.guard()
                            .is_some_and(|guard| self.expr_contains_break(guard))
                            || self.expr_contains_break(arm.body())
                    })
            }
            Expr::StructLiteral { fields, .. } => fields
                .iter()
                .any(|field| self.expr_contains_break(field.value())),
            Expr::MacroCall { args, .. } => args.iter().any(|arg| self.expr_contains_break(*arg)),
            Expr::Block(block) => {
                block
                    .statements()
                    .iter()
                    .any(|statement| self.statement_contains_break(statement))
                    || block
                        .tail()
                        .is_some_and(|tail| self.expr_contains_break(tail))
            }
        }
    }

    fn condition_contains_break(&self, condition: Condition) -> bool {
        match condition {
            Condition::Expr(expr) => self.expr_contains_break(expr),
            Condition::Let { scrutinee, .. } => self.expr_contains_break(scrutinee),
        }
    }

    fn statement_contains_break(&self, statement: &Statement) -> bool {
        match statement {
            Statement::Break => true,
            Statement::Expr { expr, .. } => self.expr_contains_break(*expr),
            Statement::Let { initializer, .. } => self.expr_contains_break(*initializer),
            Statement::Return { value } => {
                value.is_some_and(|value| self.expr_contains_break(value))
            }
            Statement::Missing
            | Statement::While { .. }
            | Statement::Loop { .. }
            | Statement::For { .. }
            | Statement::Continue => false,
        }
    }

    fn closure_target(&self, callee: ExprId) -> Option<ExprId> {
        match self.body.expr(callee)? {
            Expr::Closure { .. } => Some(callee),
            Expr::Paren { expr } => self.closure_target(*expr),
            Expr::Path(path) if path.len() == 1 => {
                let LocalResolveResult::Resolved(local) = self.resolution.resolve(path[0])? else {
                    return None;
                };
                self.binding_closures
                    .get(local.binding().index() as usize)
                    .copied()
                    .flatten()
            }
            _ => None,
        }
    }

    fn set_expr(&mut self, expr: ExprId, ty: Ty) {
        if let Some(slot) = self.expr_types.get_mut(expr.index() as usize) {
            *slot = ty;
        }
    }

    fn set_pattern(&mut self, pattern: PatId, ty: Ty) {
        if let Some(slot) = self.pattern_types.get_mut(pattern.index() as usize) {
            *slot = ty;
        }
    }

    fn set_binding(&mut self, binding: BindingId, ty: Ty) {
        if let Some(slot) = self.binding_types.get_mut(binding.index() as usize) {
            *slot = ty;
        }
    }

    fn set_member(&mut self, name_ref: NameRefId, resolution: MemberResolution) {
        if let Some(slot) = self.member_resolutions.get_mut(name_ref.index() as usize) {
            *slot = Some(resolution);
        }
    }
}

fn member_call_target(target: MemberTarget) -> CallTarget {
    match target {
        MemberTarget::Definition(definition) => CallTarget::Definition(definition),
        MemberTarget::Builtin(_) => CallTarget::Builtin,
    }
}

fn callable_signature(definition: &Definition) -> Option<&CallableSignature> {
    match definition.signature() {
        ItemSignature::Callable(signature) => Some(signature),
        _ => None,
    }
}

fn path_display(body: &Body, path: &[NameRefId]) -> String {
    path.iter()
        .filter_map(|name_ref| body.name_ref(*name_ref)?.name())
        .collect::<Vec<_>>()
        .join("::")
}

fn builtin_constructor_owner(name: &str, expected: Option<&Ty>) -> Ty {
    match name {
        "Some" => match expected {
            Some(Ty::Option(_)) => expected.cloned().unwrap_or(Ty::Unknown),
            _ => Ty::Option(Box::new(Ty::Unknown)),
        },
        "Ok" | "Err" => match expected {
            Some(Ty::Result(_, _)) => expected.cloned().unwrap_or(Ty::Unknown),
            _ => Ty::Result(Box::new(Ty::Unknown), Box::new(Ty::Unknown)),
        },
        "Vec::new" => Ty::Vec(Box::new(Ty::Unknown)),
        "HashMap::new" => Ty::HashMap(Box::new(Ty::Unknown), Box::new(Ty::Unknown)),
        _ => Ty::Unknown,
    }
}

fn literal_ty(kind: LiteralKind) -> Ty {
    match kind {
        LiteralKind::Integer => Ty::I64,
        LiteralKind::Float => Ty::F64,
        LiteralKind::String => Ty::STRING,
        LiteralKind::Boolean => Ty::BOOL,
    }
}

fn prefer_expected_if_unknown(actual: Ty, expected: Option<Ty>) -> Ty {
    if actual.is_unknown() {
        expected.unwrap_or(Ty::Unknown)
    } else {
        actual
    }
}

fn invalidate_generics(ty: &Ty, substitution: &mut Substitution) {
    match ty {
        Ty::GenericParam(parameter) => {
            substitution.insert(parameter.id(), Ty::Unknown);
        }
        Ty::Named(named) => {
            for argument in named.args() {
                invalidate_generics(argument, substitution);
            }
        }
        Ty::Tuple(items) => {
            for item in items {
                invalidate_generics(item, substitution);
            }
        }
        Ty::Function(callable) | Ty::Closure(callable) => {
            for parameter in callable.params() {
                invalidate_generics(parameter, substitution);
            }
            invalidate_generics(callable.return_ty(), substitution);
        }
        Ty::Vec(item) | Ty::Option(item) | Ty::Iterator(item) => {
            invalidate_generics(item, substitution);
        }
        Ty::HashMap(key, value) | Ty::Result(key, value) => {
            invalidate_generics(key, substitution);
            invalidate_generics(value, substitution);
        }
        Ty::Primitive(_) | Ty::Unknown | Ty::Never => {}
    }
}

fn refine_generic_bindings(expected: &Ty, actual: &Ty, substitution: &mut Substitution) {
    match (expected, actual) {
        (Ty::GenericParam(parameter), actual) if !actual.is_unknown() && !actual.is_never() => {
            if let Some(current) = substitution.get(parameter.id()).cloned() {
                substitution.insert(parameter.id(), current.join(actual));
            }
        }
        (Ty::Named(expected), Ty::Named(actual))
            if expected.definition() == actual.definition() =>
        {
            for (expected, actual) in expected.args().iter().zip(actual.args()) {
                refine_generic_bindings(expected, actual, substitution);
            }
        }
        (Ty::Tuple(expected), Ty::Tuple(actual)) => {
            for (expected, actual) in expected.iter().zip(actual) {
                refine_generic_bindings(expected, actual, substitution);
            }
        }
        (Ty::Function(expected), Ty::Function(actual))
        | (Ty::Function(expected), Ty::Closure(actual))
        | (Ty::Closure(expected), Ty::Function(actual))
        | (Ty::Closure(expected), Ty::Closure(actual)) => {
            for (expected, actual) in expected.params().iter().zip(actual.params()) {
                refine_generic_bindings(expected, actual, substitution);
            }
            refine_generic_bindings(expected.return_ty(), actual.return_ty(), substitution);
        }
        (Ty::Vec(expected), Ty::Vec(actual))
        | (Ty::Option(expected), Ty::Option(actual))
        | (Ty::Iterator(expected), Ty::Iterator(actual)) => {
            refine_generic_bindings(expected, actual, substitution);
        }
        (Ty::HashMap(expected_key, expected_value), Ty::HashMap(actual_key, actual_value))
        | (Ty::Result(expected_key, expected_value), Ty::Result(actual_key, actual_value)) => {
            refine_generic_bindings(expected_key, actual_key, substitution);
            refine_generic_bindings(expected_value, actual_value, substitution);
        }
        _ => {}
    }
}
