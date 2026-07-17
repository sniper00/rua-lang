//! Conservative bidirectional-ish type checker.
//!
//! This pass runs after the structural checker (`check.rs`). It infers a type
//! for every expression, falling back to `Ty::Unknown` whenever it cannot be
//! certain (extern symbols, generics, collection elements, methods, ...).
//!
//! Guiding rule: **zero false positives**. An error is only reported when both
//! sides of a constraint are *concretely* known and definitely incompatible, so
//! any `Unknown` silently satisfies every constraint. This keeps the checker
//! useful (it catches `if 1`, `fn f() -> bool { 1 }`, wrong arity, ...) without
//! ever rejecting a program that would actually run.

use crate::ast::*;
use crate::diag::Diag;
use crate::token::SourceRange;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IterSourceKind {
    ExclusiveRange,
    InclusiveRange,
    Vec,
    VecIter,
    VecIntoIter,
    StringChars,
    StringSplit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IterAdapterKind {
    Map,
    Filter,
    FilterMap,
    Enumerate,
    Take,
    Skip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IterConsumerKind {
    For,
    CollectVec,
    Fold,
    Count,
    Any,
    All,
    Find,
    Next,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IterSource {
    pub kind: IterSourceKind,
    pub range: SourceRange,
    pub item_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IterAdapter {
    pub kind: IterAdapterKind,
    pub range: SourceRange,
    pub input_type: String,
    pub output_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IterPlan {
    pub source: IterSource,
    pub adapters: Vec<IterAdapter>,
    pub consumer: IterConsumerKind,
    pub consumer_range: SourceRange,
    pub item_type: String,
    pub output_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IterDraft {
    source: IterSource,
    adapters: Vec<IterAdapter>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum Ty {
    I64,
    F64,
    Bool,
    Str,
    Unit,
    Never,
    /// A user struct or enum, keyed by its declaration identity.
    Named {
        def: crate::hir::DefId,
        name: String,
    },
    /// A user trait object. It stays non-concrete for compatibility checks but
    /// carries identity so method calls use Rua's metatable dispatch.
    Trait {
        def: crate::hir::DefId,
        name: String,
    },
    /// `Vec<T>` / `[T]`.
    Vec(Box<Ty>),
    /// `Option<T>` (represented at runtime as pure nil, but typed here).
    Option(Box<Ty>),
    /// `Result<T, E>`.
    Result(Box<Ty>, Box<Ty>),
    /// `HashMap<K, V>`.
    Map(Box<Ty>, Box<Ty>),
    /// A lazy iterator item type. Step 4A.5 attaches the corresponding IterPlan;
    /// this slot already supplies closure context without materializing values.
    Iter(Box<Ty>, Box<IterDraft>),
    /// `(A, B, ...)`, currently introduced by `enumerate()`.
    Tuple(Vec<Ty>),
    /// A Phase 4A closure signature. Unknown parameter/return slots preserve
    /// the checker's zero-false-positive behavior until context proves them.
    Closure(Vec<Ty>, Box<Ty>),
    /// A generic type parameter in scope (e.g. `T`). Behaves like `Unknown` for
    /// compatibility (never a mismatch), but carries its name so method calls can
    /// be resolved through the parameter's trait bounds.
    Generic {
        id: GenericParamId,
        name: String,
    },
    /// Unknown / any — unifies with everything, suppresses all errors.
    Unknown,
}

impl Ty {
    fn is_numeric(&self) -> bool {
        matches!(self, Ty::I64 | Ty::F64)
    }
    /// Concrete = we are sure what it is (so a mismatch is a real error). A
    /// generic parameter is *not* concrete: it stands for an unknown instantiation.
    fn is_concrete(&self) -> bool {
        !matches!(self, Ty::Unknown | Ty::Generic { .. } | Ty::Trait { .. })
    }
    fn name(&self) -> String {
        match self {
            Ty::I64 => "i64".into(),
            Ty::F64 => "f64".into(),
            Ty::Bool => "bool".into(),
            Ty::Str => "String".into(),
            Ty::Unit => "()".into(),
            Ty::Never => "!".into(),
            Ty::Named { name, .. } => name.clone(),
            Ty::Trait { name, .. } => format!("dyn {name}"),
            Ty::Vec(t) => format!("Vec<{}>", t.name()),
            Ty::Option(t) => format!("Option<{}>", t.name()),
            Ty::Result(t, e) => format!("Result<{}, {}>", t.name(), e.name()),
            Ty::Map(k, v) => format!("HashMap<{}, {}>", k.name(), v.name()),
            Ty::Iter(item, _) => format!("Iterator<{}>", item.name()),
            Ty::Tuple(items) => format!(
                "({})",
                items.iter().map(Ty::name).collect::<Vec<_>>().join(", ")
            ),
            Ty::Closure(params, ret) => format!(
                "fn({}) -> {}",
                params.iter().map(Ty::name).collect::<Vec<_>>().join(", "),
                ret.name()
            ),
            Ty::Generic { name, .. } => name.clone(),
            Ty::Unknown => "?".into(),
        }
    }
}

/// Two types are compatible unless both are concrete and genuinely different.
/// Numeric types are mutually compatible (Lua unifies numbers; we stay lenient).
/// Parameterized types recurse on their element types.
fn compatible(a: &Ty, b: &Ty) -> bool {
    if matches!(a, Ty::Never) || matches!(b, Ty::Never) {
        return true;
    }
    if !a.is_concrete() || !b.is_concrete() {
        return true;
    }
    if a.is_numeric() && b.is_numeric() {
        return true;
    }
    match (a, b) {
        (Ty::Vec(x), Ty::Vec(y)) => compatible(x, y),
        (Ty::Option(x), Ty::Option(y)) => compatible(x, y),
        (Ty::Result(x1, e1), Ty::Result(x2, e2)) => compatible(x1, x2) && compatible(e1, e2),
        (Ty::Map(k1, v1), Ty::Map(k2, v2)) => compatible(k1, k2) && compatible(v1, v2),
        (Ty::Iter(x, _), Ty::Iter(y, _)) => compatible(x, y),
        (Ty::Tuple(x), Ty::Tuple(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| compatible(a, b))
        }
        (Ty::Closure(p1, r1), Ty::Closure(p2, r2)) => {
            p1.len() == p2.len()
                && p1.iter().zip(p2).all(|(x, y)| compatible(x, y))
                && compatible(r1, r2)
        }
        _ => a == b,
    }
}

/// Collect every method call `name.method(args)` reachable from a statement
/// (recursing into nested blocks/branches), used by empty-collection element
/// inference. Records `(method, args)` for calls whose receiver is the bare
/// variable `name`.
fn collect_calls_on_stmt<'a>(name: &str, s: &'a Stmt, out: &mut Vec<(&'a str, &'a [Expr])>) {
    match s {
        Stmt::Let { init, .. } => collect_calls_on_expr(name, init, out),
        Stmt::Expr(e) => collect_calls_on_expr(name, e, out),
        Stmt::Return(Some(e)) => collect_calls_on_expr(name, e, out),
        Stmt::While { cond, body } => {
            collect_calls_on_expr(name, cond, out);
            collect_calls_on_block(name, body, out);
        }
        Stmt::Loop { body } => collect_calls_on_block(name, body, out),
        Stmt::For { iter, body, .. } => {
            collect_calls_on_expr(name, iter, out);
            collect_calls_on_block(name, body, out);
        }
        Stmt::WhileLet { expr, body, .. } => {
            collect_calls_on_expr(name, expr, out);
            collect_calls_on_block(name, body, out);
        }
        Stmt::Break(value) => {
            if let Some(value) = value {
                collect_calls_on_expr(name, value, out);
            }
        }
        Stmt::Return(None) | Stmt::Continue => {}
    }
}

fn collect_calls_on_block<'a>(name: &str, b: &'a Block, out: &mut Vec<(&'a str, &'a [Expr])>) {
    for s in &b.stmts {
        collect_calls_on_stmt(name, s, out);
    }
    if let Some(t) = &b.tail {
        collect_calls_on_expr(name, t, out);
    }
}

fn collect_calls_on_expr<'a>(name: &str, e: &'a Expr, out: &mut Vec<(&'a str, &'a [Expr])>) {
    match &e.kind {
        ExprKind::VecLit(elements) => {
            for element in elements {
                collect_calls_on_expr(name, element, out);
            }
        }
        ExprKind::Closure { body, .. } => match body {
            ClosureBody::Expr(expr) => collect_calls_on_expr(name, expr, out),
            ClosureBody::Block(block) => collect_calls_on_block(name, block, out),
        },
        ExprKind::MethodCall {
            recv, method, args, ..
        } => {
            if let ExprKind::Path(segs) = &recv.kind
                && segs.len() == 1
                && segs[0] == name
            {
                out.push((method.as_str(), args.as_slice()));
            }
            collect_calls_on_expr(name, recv, out);
            for a in args {
                collect_calls_on_expr(name, a, out);
            }
        }
        ExprKind::Unary { expr, .. } => collect_calls_on_expr(name, expr, out),
        ExprKind::Loop(body) => collect_calls_on_block(name, body, out),
        ExprKind::Binary { lhs, rhs, .. } => {
            collect_calls_on_expr(name, lhs, out);
            collect_calls_on_expr(name, rhs, out);
        }
        ExprKind::Call { callee, args } => {
            collect_calls_on_expr(name, callee, out);
            for a in args {
                collect_calls_on_expr(name, a, out);
            }
        }
        ExprKind::Field { base, .. } => collect_calls_on_expr(name, base, out),
        ExprKind::StructLit { fields, .. } => {
            for (_, f) in fields {
                collect_calls_on_expr(name, f, out);
            }
        }
        ExprKind::MapLit(entries) => {
            for (key, value) in entries {
                collect_calls_on_expr(name, key, out);
                collect_calls_on_expr(name, value, out);
            }
        }
        ExprKind::Try { expr } => collect_calls_on_expr(name, expr, out),
        ExprKind::Match { scrut, arms } => {
            collect_calls_on_expr(name, scrut, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_calls_on_expr(name, g, out);
                }
                collect_calls_on_expr(name, &arm.body, out);
            }
        }
        ExprKind::Range { start, end, .. } => {
            collect_calls_on_expr(name, start, out);
            collect_calls_on_expr(name, end, out);
        }
        ExprKind::Index { base, index } => {
            collect_calls_on_expr(name, base, out);
            collect_calls_on_expr(name, index, out);
        }
        ExprKind::If {
            cond,
            then_block,
            else_block,
        } => {
            collect_calls_on_expr(name, cond, out);
            collect_calls_on_block(name, then_block, out);
            if let Some(eb) = else_block {
                collect_calls_on_else(name, eb, out);
            }
        }
        ExprKind::IfLet {
            expr,
            then_block,
            else_block,
            ..
        } => {
            collect_calls_on_expr(name, expr, out);
            collect_calls_on_block(name, then_block, out);
            if let Some(eb) = else_block {
                collect_calls_on_else(name, eb, out);
            }
        }
        ExprKind::Block(b) => collect_calls_on_block(name, b, out),
        ExprKind::Assign { target, value, .. } => {
            collect_calls_on_expr(name, target, out);
            collect_calls_on_expr(name, value, out);
        }
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Bool(_)
        | ExprKind::Path(_) => {}
    }
}

fn collect_calls_on_else<'a>(name: &str, eb: &'a ElseBranch, out: &mut Vec<(&'a str, &'a [Expr])>) {
    match eb {
        ElseBranch::Block(b) => collect_calls_on_block(name, b, out),
        ElseBranch::If(e) => collect_calls_on_expr(name, e, out),
    }
}

#[derive(Default)]
struct ClosureUsage<'a> {
    calls: Vec<&'a [Expr]>,
    escapes: bool,
}

#[derive(Clone, Copy)]
struct ClosureContext<'a> {
    expected: &'a [Ty],
    report_unknown_params: bool,
    allow_mutable_capture: bool,
}

fn collect_closure_usage_stmt<'a>(name: &str, stmt: &'a Stmt, usage: &mut ClosureUsage<'a>) {
    match stmt {
        Stmt::Let { init, .. } | Stmt::Expr(init) | Stmt::Return(Some(init)) => {
            collect_closure_usage_expr(name, init, usage)
        }
        Stmt::While { cond, body } => {
            collect_closure_usage_expr(name, cond, usage);
            collect_closure_usage_block(name, body, usage);
        }
        Stmt::Loop { body } => collect_closure_usage_block(name, body, usage),
        Stmt::For { iter, body, .. } => {
            collect_closure_usage_expr(name, iter, usage);
            collect_closure_usage_block(name, body, usage);
        }
        Stmt::WhileLet { expr, body, .. } => {
            collect_closure_usage_expr(name, expr, usage);
            collect_closure_usage_block(name, body, usage);
        }
        Stmt::Break(value) => {
            if let Some(value) = value {
                collect_closure_usage_expr(name, value, usage);
            }
        }
        Stmt::Return(None) | Stmt::Continue => {}
    }
}

fn collect_closure_usage_block<'a>(name: &str, block: &'a Block, usage: &mut ClosureUsage<'a>) {
    for stmt in &block.stmts {
        collect_closure_usage_stmt(name, stmt, usage);
    }
    if let Some(tail) = &block.tail {
        collect_closure_usage_expr(name, tail, usage);
    }
}

fn collect_closure_usage_expr<'a>(name: &str, expr: &'a Expr, usage: &mut ClosureUsage<'a>) {
    match &expr.kind {
        ExprKind::VecLit(elements) => {
            for element in elements {
                collect_closure_usage_expr(name, element, usage);
            }
        }
        ExprKind::Path(segments) => {
            if segments.len() == 1 && segments[0] == name {
                usage.escapes = true;
            }
        }
        ExprKind::Call { callee, args } => {
            if matches!(&callee.kind, ExprKind::Path(segments) if segments.len() == 1 && segments[0] == name)
            {
                usage.calls.push(args);
            } else {
                collect_closure_usage_expr(name, callee, usage);
            }
            for arg in args {
                collect_closure_usage_expr(name, arg, usage);
            }
        }
        ExprKind::Closure { body, .. } => match body {
            ClosureBody::Expr(body) => collect_closure_usage_expr(name, body, usage),
            ClosureBody::Block(body) => collect_closure_usage_block(name, body, usage),
        },
        ExprKind::Loop(body) => collect_closure_usage_block(name, body, usage),
        ExprKind::Unary { expr, .. } | ExprKind::Try { expr } => {
            collect_closure_usage_expr(name, expr, usage)
        }
        ExprKind::Binary { lhs, rhs, .. }
        | ExprKind::Range {
            start: lhs,
            end: rhs,
            ..
        }
        | ExprKind::Index {
            base: lhs,
            index: rhs,
        }
        | ExprKind::Assign {
            target: lhs,
            value: rhs,
            ..
        } => {
            collect_closure_usage_expr(name, lhs, usage);
            collect_closure_usage_expr(name, rhs, usage);
        }
        ExprKind::MethodCall { recv, args, .. } => {
            collect_closure_usage_expr(name, recv, usage);
            for arg in args {
                collect_closure_usage_expr(name, arg, usage);
            }
        }
        ExprKind::Field { base, .. } => collect_closure_usage_expr(name, base, usage),
        ExprKind::StructLit { fields, .. } => {
            for (_, field) in fields {
                collect_closure_usage_expr(name, field, usage);
            }
        }
        ExprKind::MapLit(entries) => {
            for (key, value) in entries {
                collect_closure_usage_expr(name, key, usage);
                collect_closure_usage_expr(name, value, usage);
            }
        }
        ExprKind::Match { scrut, arms } => {
            collect_closure_usage_expr(name, scrut, usage);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_closure_usage_expr(name, guard, usage);
                }
                collect_closure_usage_expr(name, &arm.body, usage);
            }
        }
        ExprKind::If {
            cond,
            then_block,
            else_block,
        } => {
            collect_closure_usage_expr(name, cond, usage);
            collect_closure_usage_block(name, then_block, usage);
            if let Some(branch) = else_block {
                collect_closure_usage_else(name, branch, usage);
            }
        }
        ExprKind::IfLet {
            expr,
            then_block,
            else_block,
            ..
        } => {
            collect_closure_usage_expr(name, expr, usage);
            collect_closure_usage_block(name, then_block, usage);
            if let Some(branch) = else_block {
                collect_closure_usage_else(name, branch, usage);
            }
        }
        ExprKind::Block(block) => collect_closure_usage_block(name, block, usage),
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Bool(_) => {}
    }
}

fn collect_closure_usage_else<'a>(
    name: &str,
    branch: &'a ElseBranch,
    usage: &mut ClosureUsage<'a>,
) {
    match branch {
        ElseBranch::Block(block) => collect_closure_usage_block(name, block, usage),
        ElseBranch::If(expr) => collect_closure_usage_expr(name, expr, usage),
    }
}

/// Join two types into their least-informative common type. If incompatible,
/// or if either is unknown, the result is `Unknown`.
fn join(a: &Ty, b: &Ty) -> Ty {
    if !compatible(a, b) {
        return Ty::Unknown;
    }
    match (a, b) {
        (Ty::Never, ty) | (ty, Ty::Never) => ty.clone(),
        (Ty::Unknown, _) => b.clone(),
        (_, Ty::Unknown) => a.clone(),
        (Ty::F64, _) | (_, Ty::F64) if a.is_numeric() && b.is_numeric() => Ty::F64,
        _ => a.clone(),
    }
}

/// Infer bindings for generic parameters by structurally matching a declared
/// parameter type against a concrete argument type. Returns false when the
/// same parameter was already bound to an incompatible concrete type.
fn unify_generic(param: &Ty, arg: &Ty, subst: &mut HashMap<GenericParamId, Ty>) -> bool {
    match (param, arg) {
        (Ty::Generic { id, .. }, a) if a.is_concrete() => {
            if let Some(current) = subst.get_mut(id) {
                if !compatible(current, a) {
                    return false;
                }
                *current = join(current, a);
            } else {
                subst.insert(*id, a.clone());
            }
            true
        }
        (Ty::Vec(p), Ty::Vec(a)) => unify_generic(p, a, subst),
        (Ty::Option(p), Ty::Option(a)) => unify_generic(p, a, subst),
        (Ty::Result(p1, e1), Ty::Result(p2, e2)) => {
            let first = unify_generic(p1, p2, subst);
            let second = unify_generic(e1, e2, subst);
            first && second
        }
        (Ty::Map(k1, v1), Ty::Map(k2, v2)) => {
            let key = unify_generic(k1, k2, subst);
            let value = unify_generic(v1, v2, subst);
            key && value
        }
        (Ty::Iter(p, _), Ty::Iter(a, _)) => unify_generic(p, a, subst),
        (Ty::Tuple(p), Ty::Tuple(a)) if p.len() == a.len() => {
            let mut compatible = true;
            for (p, a) in p.iter().zip(a) {
                compatible &= unify_generic(p, a, subst);
            }
            compatible
        }
        (Ty::Closure(p1, r1), Ty::Closure(p2, r2)) if p1.len() == p2.len() => {
            let mut compatible = true;
            for (p, a) in p1.iter().zip(p2) {
                compatible &= unify_generic(p, a, subst);
            }
            compatible && unify_generic(r1, r2, subst)
        }
        _ => true,
    }
}

/// Replace generic parameters in `ty` with their inferred bindings; unbound
/// generics become `Unknown` (they carry no meaning outside the callee).
fn subst_ty(ty: &Ty, subst: &HashMap<GenericParamId, Ty>) -> Ty {
    match ty {
        Ty::Generic { id, .. } => subst.get(id).cloned().unwrap_or(Ty::Unknown),
        Ty::Vec(t) => Ty::Vec(Box::new(subst_ty(t, subst))),
        Ty::Option(t) => Ty::Option(Box::new(subst_ty(t, subst))),
        Ty::Result(t, e) => Ty::Result(Box::new(subst_ty(t, subst)), Box::new(subst_ty(e, subst))),
        Ty::Map(k, v) => Ty::Map(Box::new(subst_ty(k, subst)), Box::new(subst_ty(v, subst))),
        Ty::Iter(item, draft) => Ty::Iter(Box::new(subst_ty(item, subst)), draft.clone()),
        Ty::Tuple(items) => Ty::Tuple(items.iter().map(|item| subst_ty(item, subst)).collect()),
        Ty::Closure(params, ret) => Ty::Closure(
            params.iter().map(|param| subst_ty(param, subst)).collect(),
            Box::new(subst_ty(ret, subst)),
        ),
        other => other.clone(),
    }
}

#[derive(Clone)]
struct FnSig {
    params: Vec<Ty>,
    ret: Ty,
    variadic: bool,
    /// Generic parameters (with bounds) declared on this function, used to check
    /// bound satisfaction at call sites. Empty for non-generic fns and for
    /// methods/trait signatures (where call-site checking is not yet done).
    generics: Vec<GenericParam>,
    generic_bounds: HashMap<GenericParamId, Vec<crate::hir::TraitTarget>>,
}

/// Type-derived facts the backend needs: the sets of `/` and `%` expressions
/// whose operands are both `i64`, so codegen can emit truncating integer helpers
/// (the `number` export from `rua_std`) that match Rust rather than Lua's
/// floored `//`/`%`.
/// All expression facts are keyed by parser-owned `ExprId`; source ranges are
/// used only for diagnostics.
#[derive(Debug, Default)]
pub struct TypeInfo {
    int_divs: std::collections::HashSet<ExprId>,
    int_rems: std::collections::HashSet<ExprId>,
    /// Method-call expressions whose receiver is a `String` and whose method is
    /// a recognized std string method, so codegen routes them through its configured module.
    str_methods: std::collections::HashSet<ExprId>,
    /// `+` expressions whose operands are both `String`, so codegen emits Lua
    /// string concatenation (`..`) instead of arithmetic.
    str_concats: std::collections::HashSet<ExprId>,
    /// Method-call expressions where the receiver is `Option<T>` and the method
    /// is `map`, so codegen inlines the closure instead of emitting `:map()`.
    option_maps: std::collections::HashSet<ExprId>,
    /// Standard method declaration selected for each call. Codegen resolves its
    /// runtime module from the declaration file recorded by `std.toml`.
    standard_methods: HashMap<ExprId, crate::hir::DefId>,
    /// Unary/binary expressions proven to use primitive Lua operators rather
    /// than user metamethods.
    pure_operators: std::collections::HashSet<ExprId>,
    user_methods: HashMap<ExprId, UserMethodDispatch>,
    result_tries: std::collections::HashSet<ExprId>,
    /// First closure encountered during type checking. The compiler entry point
    /// uses this as a temporary backend gate until fused closure codegen lands.
    first_closure: Option<SourceRange>,
    iter_plans: HashMap<ExprId, IterPlan>,
    contains: HashMap<ExprId, ContainsKind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContainsKind {
    Vec,
    Map,
    String,
    Iter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UserMethodDispatch {
    Static(crate::hir::DefId),
    Dynamic,
}

impl TypeInfo {
    pub fn is_int_div(&self, expression: ExprId) -> bool {
        self.int_divs.contains(&expression)
    }

    pub fn is_int_rem(&self, expression: ExprId) -> bool {
        self.int_rems.contains(&expression)
    }

    pub fn is_str_method(&self, expression: ExprId) -> bool {
        self.str_methods.contains(&expression)
    }

    pub fn is_str_concat(&self, expression: ExprId) -> bool {
        self.str_concats.contains(&expression)
    }

    pub fn is_option_map(&self, expression: ExprId) -> bool {
        self.option_maps.contains(&expression)
    }

    pub fn standard_method(&self, expression: ExprId) -> Option<crate::hir::DefId> {
        self.standard_methods.get(&expression).copied()
    }

    pub fn is_pure_operator(&self, expression: ExprId) -> bool {
        self.pure_operators.contains(&expression)
    }

    pub(crate) fn user_method(&self, expression: ExprId) -> Option<UserMethodDispatch> {
        self.user_methods.get(&expression).copied()
    }

    pub fn is_result_try(&self, expression: ExprId) -> bool {
        self.result_tries.contains(&expression)
    }

    pub fn first_closure(&self) -> Option<SourceRange> {
        self.first_closure
    }

    pub fn iter_plan(&self, expression: ExprId) -> Option<&IterPlan> {
        self.iter_plans.get(&expression)
    }

    pub fn iter_plans(&self) -> impl Iterator<Item = &IterPlan> {
        self.iter_plans.values()
    }

    pub(crate) fn contains_kind(&self, expression: ExprId) -> Option<ContainsKind> {
        self.contains.get(&expression).copied()
    }

    pub fn pending_iter_codegen(&self) -> Option<SourceRange> {
        self.iter_plans
            .values()
            .filter(|plan| {
                !plan.adapters.is_empty()
                    || plan.consumer != IterConsumerKind::For
                    || !matches!(
                        plan.source.kind,
                        IterSourceKind::ExclusiveRange
                            | IterSourceKind::InclusiveRange
                            | IterSourceKind::Vec
                    )
            })
            .map(|plan| plan.consumer_range)
            .min_by_key(|range| (range.file, range.start))
    }
}

/// Resolved payload of a user enum variant, used to type the bindings a
/// `match` / `if let` arm introduces (`Shape::Circle(r)`, `Msg::Move { x, y }`).
#[derive(Debug, Clone)]
enum VariantPayload {
    /// Tuple variant element types, positionally aligned with the pattern.
    Tuple(Vec<Ty>),
    /// Struct variant field types, keyed by field name.
    Struct(Vec<(String, Ty)>),
}

/// Payload element types for a built-in refutable pattern (`Some(x)`, `Ok(v)`,
/// `Err(e)`) matched against scrutinee type `ty`. Returns one type per pattern
/// element, or `None` when the path isn't a recognized built-in (the caller
/// then binds the elements as `Unknown`).
fn builtin_payload(builtin: rua_core::BuiltinId, ty: &Ty) -> Option<Vec<Ty>> {
    match (builtin, ty) {
        (rua_core::BuiltinId::VariantOptionSome, Ty::Option(inner)) => {
            Some(vec![(**inner).clone()])
        }
        (rua_core::BuiltinId::VariantResultOk, Ty::Result(ok, _)) => Some(vec![(**ok).clone()]),
        (rua_core::BuiltinId::VariantResultErr, Ty::Result(_, err)) => Some(vec![(**err).clone()]),
        _ => None,
    }
}

/// Combine impl-level and method-level generic parameters for a method body.
fn merge_generics(outer: &[GenericParam], inner: &[GenericParam]) -> Vec<GenericParam> {
    let mut v = outer.to_vec();
    v.extend(inner.iter().cloned());
    v
}

/// Run type-checking and return every diagnostic. The returned vec is suitable
/// for LSP consumption (byte-offset spans are preserved from `Expr`).
pub fn collect_diags(prog: &Program) -> Vec<Diag> {
    let hir = crate::hir::resolve(prog);
    collect_diags_resolved(prog, &hir)
}

pub fn collect_diags_resolved(prog: &Program, hir: &crate::hir::ResolvedHir) -> Vec<Diag> {
    let mut tc = Tc::new(prog, hir);
    tc.run(prog);
    tc.errs
}

pub fn check(prog: &Program) -> Result<TypeInfo, Vec<Diag>> {
    let hir = crate::hir::resolve(prog);
    check_resolved(prog, &hir)
}

pub fn check_resolved(
    prog: &Program,
    hir: &crate::hir::ResolvedHir,
) -> Result<TypeInfo, Vec<Diag>> {
    check_resolved_diagnostics(prog, hir)
}

pub fn check_resolved_diagnostics(
    prog: &Program,
    hir: &crate::hir::ResolvedHir,
) -> Result<TypeInfo, Vec<Diag>> {
    let mut tc = Tc::new(prog, hir);
    tc.run(prog);
    if tc.errs.is_empty() {
        Ok(TypeInfo {
            int_divs: tc.int_divs,
            int_rems: tc.int_rems,
            str_methods: tc.str_methods,
            str_concats: tc.str_concats,
            option_maps: tc.option_maps,
            standard_methods: tc.standard_methods,
            pure_operators: tc.pure_operators,
            user_methods: tc.user_methods,
            result_tries: tc.result_tries,
            first_closure: tc.first_closure,
            iter_plans: tc.iter_plans,
            contains: tc.contains,
        })
    } else {
        Err(tc.errs)
    }
}

struct Tc {
    hir: crate::hir::ResolvedHir,
    /// Resolved function declaration -> signature. Calls consume the target
    /// selected by name resolution; the type checker never reconstructs it from
    /// source path strings.
    fn_sigs: HashMap<crate::hir::DefId, FnSig>,
    /// Resolved struct declaration -> fields.
    struct_defs: HashMap<crate::hir::DefId, Vec<(String, Ty, SourceRange)>>,
    /// Resolved enum variant declaration -> payload.
    variant_payloads: HashMap<crate::hir::DefId, VariantPayload>,
    /// Resolved impl/trait method declaration -> callable signature.
    method_sigs: HashMap<crate::hir::DefId, FnSig>,
    /// Generic parameters in scope for the function being checked: name -> the
    /// trait names it is bounded by. Set on entry to each `check_fn`.
    gen_bounds: HashMap<GenericParamId, Vec<crate::hir::TraitTarget>>,
    scopes: Vec<HashMap<String, Ty>>,
    mutable_scopes: Vec<std::collections::HashSet<String>>,
    /// Scope-count boundaries for nested closures. Assignments resolving below
    /// the innermost boundary mutate an enclosing capture.
    closure_boundaries: Vec<usize>,
    closure_mutable_capture_allowed: Vec<bool>,
    /// Explicit return expression types for the currently inferred closure.
    closure_returns: Vec<Vec<Ty>>,
    /// Innermost loop first. `Some` collects values for a `loop` expression;
    /// `None` marks `while`/`for`, which only accept a bare `break`.
    loop_breaks: Vec<Option<Vec<(Ty, SourceRange)>>>,
    errs: Vec<Diag>,
    /// Every `i64 / i64` division expression.
    int_divs: std::collections::HashSet<ExprId>,
    /// Every `i64 % i64` remainder expression.
    int_rems: std::collections::HashSet<ExprId>,
    /// Recognized `String` method calls.
    str_methods: std::collections::HashSet<ExprId>,
    /// `String + String` concatenations.
    str_concats: std::collections::HashSet<ExprId>,
    /// `Option::map` calls that need inline codegen.
    option_maps: std::collections::HashSet<ExprId>,
    standard_methods: HashMap<ExprId, crate::hir::DefId>,
    pure_operators: std::collections::HashSet<ExprId>,
    user_methods: HashMap<ExprId, UserMethodDispatch>,
    result_tries: std::collections::HashSet<ExprId>,
    first_closure: Option<SourceRange>,
    iter_plans: HashMap<ExprId, IterPlan>,
    contains: HashMap<ExprId, ContainsKind>,
}

impl Tc {
    fn new(prog: &Program, hir: &crate::hir::ResolvedHir) -> Tc {
        let mut tc = Tc {
            hir: hir.clone(),
            fn_sigs: HashMap::new(),
            struct_defs: HashMap::new(),
            variant_payloads: HashMap::new(),
            method_sigs: HashMap::new(),
            gen_bounds: HashMap::new(),
            scopes: Vec::new(),
            mutable_scopes: Vec::new(),
            closure_boundaries: Vec::new(),
            closure_mutable_capture_allowed: Vec::new(),
            closure_returns: Vec::new(),
            loop_breaks: Vec::new(),
            errs: Vec::new(),
            int_divs: std::collections::HashSet::new(),
            int_rems: std::collections::HashSet::new(),
            str_methods: std::collections::HashSet::new(),
            str_concats: std::collections::HashSet::new(),
            option_maps: std::collections::HashSet::new(),
            standard_methods: HashMap::new(),
            pure_operators: std::collections::HashSet::new(),
            user_methods: HashMap::new(),
            result_tries: std::collections::HashSet::new(),
            first_closure: None,
            iter_plans: HashMap::new(),
            contains: HashMap::new(),
        };
        tc.collect_identity_declarations(&prog.items, hir.root);
        for (inherited, origin) in hir.method_origins.iter() {
            if let Some(signature) = tc.method_sigs.get(origin).cloned() {
                tc.fn_sigs.insert(*inherited, signature.clone());
                tc.method_sigs.insert(*inherited, signature);
            }
        }
        tc
    }

    fn sig_of(&self, params: &[Param], ret: Option<&Type>) -> FnSig {
        FnSig {
            params: params.iter().map(|p| self.ty_of(&p.ty)).collect(),
            ret: ret.map(|t| self.ty_of(t)).unwrap_or(Ty::Unit),
            variadic: false,
            generics: Vec::new(),
            generic_bounds: HashMap::new(),
        }
    }

    fn resolved_target(&self, expression: &Expr) -> Option<crate::hir::ResolvedTarget> {
        self.hir.expression_targets.get(&expression.id).copied()
    }

    fn definition_for_target(
        &self,
        target: crate::hir::ResolvedTarget,
    ) -> Option<&crate::hir::DefData> {
        match target {
            crate::hir::ResolvedTarget::Item(definition) => Some(self.hir.definition(definition)),
            crate::hir::ResolvedTarget::Extern(extern_id) => {
                self.hir.definitions.iter().find(|definition| {
                    matches!(
                        definition.kind,
                        crate::hir::DefKind::ExternFunction { extern_id: candidate }
                            if candidate == extern_id
                    )
                })
            }
            _ => None,
        }
    }

    fn definition_key(&self, definition: &crate::hir::DefData) -> String {
        let owner = match definition.kind {
            crate::hir::DefKind::Method { owner } | crate::hir::DefKind::TraitMethod { owner } => {
                Some(owner)
            }
            _ => None,
        };
        let module = owner
            .map(|owner| self.hir.definition(owner).module)
            .unwrap_or(definition.module);
        let mut segments = self.hir.module(module).path.segments().to_vec();
        if let Some(owner) = owner {
            segments.push(self.hir.definition(owner).name.clone());
        }
        segments.push(definition.name.clone());
        segments.join("::")
    }

    fn resolved_enum_variant_ids(
        &self,
        target: crate::hir::ResolvedTarget,
    ) -> Option<(crate::hir::DefId, crate::hir::DefId)> {
        let definition = self.definition_for_target(target)?;
        let crate::hir::DefKind::EnumVariant { owner, .. } = definition.kind else {
            return None;
        };
        Some((owner, definition.id))
    }

    fn named_type(&self, definition: crate::hir::DefId) -> Ty {
        Ty::Named {
            def: definition,
            name: self.hir.definition(definition).name.clone(),
        }
    }

    fn resolved_pattern_variant(&self, id: PatternId) -> Option<crate::hir::ResolvedTarget> {
        self.hir.pattern_targets.get(&id).copied()
    }

    fn collect_identity_declarations(&mut self, items: &[Item], module: crate::hir::ModuleId) {
        for (item_index, it) in items.iter().enumerate() {
            match it {
                Item::Annotation(_) => {}
                Item::Fn(f) => {
                    self.set_gen_bounds(&f.generics);
                    let mut sig = self.sig_of(&f.params, f.ret.as_ref());
                    sig.generic_bounds = self.gen_bounds.clone();
                    self.gen_bounds.clear();
                    sig.generics = f.generics.clone();
                    if let Some(&definition) = self.hir.module(module).scope.values.get(&f.name) {
                        self.fn_sigs.insert(definition, sig);
                    }
                }
                Item::Struct(structure) => {
                    self.set_gen_bounds(&structure.generics);
                    let fields = structure
                        .fields
                        .iter()
                        .map(|field| (field.name.clone(), self.ty_of(&field.ty), field.name_span))
                        .collect();
                    self.gen_bounds.clear();
                    if let Some(&definition) =
                        self.hir.module(module).scope.types.get(&structure.name)
                    {
                        self.struct_defs.insert(definition, fields);
                    }
                }
                Item::Enum(enumeration) => {
                    self.set_gen_bounds(&enumeration.generics);
                    let owner = self
                        .hir
                        .module(module)
                        .scope
                        .types
                        .get(&enumeration.name)
                        .copied();
                    if let Some(owner) = owner {
                        for variant in &enumeration.variants {
                            let Some(&definition) =
                                self.hir.enum_variants.get(&(owner, variant.name.clone()))
                            else {
                                continue;
                            };
                            let payload = match &variant.kind {
                                VariantKind::Unit => continue,
                                VariantKind::Tuple(types) => VariantPayload::Tuple(
                                    types.iter().map(|ty| self.ty_of(ty)).collect(),
                                ),
                                VariantKind::Struct(fields) => VariantPayload::Struct(
                                    fields
                                        .iter()
                                        .map(|field| (field.name.clone(), self.ty_of(&field.ty)))
                                        .collect(),
                                ),
                            };
                            self.variant_payloads.insert(definition, payload);
                        }
                    }
                    self.gen_bounds.clear();
                }
                Item::Extern(b) => {
                    for ef in &b.fns {
                        self.set_gen_bounds(&ef.generics);
                        let mut sig = self.sig_of(&ef.params, ef.ret.as_ref());
                        sig.variadic = ef.variadic;
                        sig.generic_bounds = self.gen_bounds.clone();
                        sig.generics = ef.generics.clone();
                        self.gen_bounds.clear();
                        if let Some(&definition) =
                            self.hir.module(module).scope.values.get(&ef.name)
                        {
                            self.fn_sigs.insert(definition, sig);
                        }
                    }
                }
                Item::Impl(implementation) => {
                    if let Some(owner) = self
                        .hir
                        .impl_targets
                        .get(&(module, item_index))
                        .map(|target| target.owner)
                    {
                        for method in &implementation.methods {
                            let Some(&definition) =
                                self.hir.associated_items.get(&(owner, method.name.clone()))
                            else {
                                continue;
                            };
                            let generics =
                                merge_generics(&implementation.generics, &method.generics);
                            self.set_gen_bounds(&generics);
                            let mut signature = self.sig_of(&method.params, method.ret.as_ref());
                            signature.generic_bounds = self.gen_bounds.clone();
                            self.gen_bounds.clear();
                            signature.generics = generics;
                            self.fn_sigs.insert(definition, signature.clone());
                            self.method_sigs.insert(definition, signature);
                        }
                    }
                }
                Item::Trait(trait_decl) => {
                    let Some(&owner) = self.hir.module(module).scope.types.get(&trait_decl.name)
                    else {
                        continue;
                    };
                    for method in &trait_decl.methods {
                        let Some(&definition) =
                            self.hir.trait_items.get(&(owner, method.name.clone()))
                        else {
                            continue;
                        };
                        let generics = merge_generics(&trait_decl.generics, &method.generics);
                        self.set_gen_bounds(&generics);
                        let mut signature = self.sig_of(&method.params, method.ret.as_ref());
                        signature.generic_bounds = self.gen_bounds.clone();
                        self.gen_bounds.clear();
                        signature.generics = method.generics.clone();
                        self.fn_sigs.insert(definition, signature.clone());
                        self.method_sigs.insert(definition, signature);
                    }
                }
                Item::Mod(m) => {
                    if let Some(&child) = self.hir.module(module).scope.modules.get(&m.name) {
                        self.collect_identity_declarations(&m.items, child);
                    }
                }
                Item::Use(_) => {}
            }
        }
    }

    /// Install `GenericParamId -> bounds` for the current generic scope.
    /// generic scope (used by `ty_of` and bound-based method resolution).
    fn set_gen_bounds(&mut self, generics: &[GenericParam]) {
        self.gen_bounds = generics
            .iter()
            .map(|g| {
                (
                    g.id,
                    g.bounds
                        .iter()
                        .filter_map(|bound| self.hir.trait_ref_targets.get(&bound.id).copied())
                        .collect(),
                )
            })
            .collect();
    }

    fn ty_of(&self, t: &Type) -> Ty {
        match t {
            Type::Unit => Ty::Unit,
            Type::Never => Ty::Never,
            Type::Ref { inner, .. } => self.ty_of(inner),
            Type::Function { params, ret } => Ty::Closure(
                params.iter().map(|param| self.ty_of(param)).collect(),
                Box::new(self.ty_of(ret)),
            ),
            Type::Tuple(items) => Ty::Tuple(items.iter().map(|item| self.ty_of(item)).collect()),
            Type::Path { id, name, args } => {
                let arg = |i: usize| args.get(i).map(|t| self.ty_of(t)).unwrap_or(Ty::Unknown);
                match self.hir.type_targets.get(id).copied() {
                    Some(crate::hir::TypeTarget::Primitive(crate::hir::PrimitiveType::I64)) => {
                        Ty::I64
                    }
                    Some(crate::hir::TypeTarget::Primitive(crate::hir::PrimitiveType::F64)) => {
                        Ty::F64
                    }
                    Some(crate::hir::TypeTarget::Primitive(crate::hir::PrimitiveType::Bool)) => {
                        Ty::Bool
                    }
                    Some(crate::hir::TypeTarget::Primitive(crate::hir::PrimitiveType::Box)) => {
                        arg(0)
                    }
                    Some(crate::hir::TypeTarget::Builtin(rua_core::BuiltinId::TypeString)) => {
                        Ty::Str
                    }
                    Some(crate::hir::TypeTarget::Builtin(rua_core::BuiltinId::TypeVec)) => {
                        Ty::Vec(Box::new(arg(0)))
                    }
                    Some(crate::hir::TypeTarget::Builtin(rua_core::BuiltinId::TypeOption)) => {
                        Ty::Option(Box::new(arg(0)))
                    }
                    Some(crate::hir::TypeTarget::Builtin(rua_core::BuiltinId::TypeResult)) => {
                        Ty::Result(Box::new(arg(0)), Box::new(arg(1)))
                    }
                    Some(crate::hir::TypeTarget::Builtin(rua_core::BuiltinId::TypeHashMap)) => {
                        Ty::Map(Box::new(arg(0)), Box::new(arg(1)))
                    }
                    Some(crate::hir::TypeTarget::Item(definition))
                        if matches!(
                            self.hir.definition(definition).kind,
                            crate::hir::DefKind::Struct | crate::hir::DefKind::Enum
                        ) =>
                    {
                        self.named_type(definition)
                    }
                    Some(crate::hir::TypeTarget::Item(definition))
                        if self.hir.definition(definition).kind == crate::hir::DefKind::Trait =>
                    {
                        Ty::Trait {
                            def: definition,
                            name: self.hir.definition(definition).name.clone(),
                        }
                    }
                    Some(crate::hir::TypeTarget::Generic(id)) => Ty::Generic {
                        id,
                        name: name.clone(),
                    },
                    _ => Ty::Unknown,
                }
            }
        }
    }

    // --- scope helpers -----------------------------------------------------

    fn push(&mut self) {
        self.scopes.push(HashMap::new());
        self.mutable_scopes.push(std::collections::HashSet::new());
    }
    fn pop(&mut self) {
        self.scopes.pop();
        self.mutable_scopes.pop();
    }
    fn bind(&mut self, name: &str, ty: Ty) {
        if let Some(s) = self.scopes.last_mut() {
            s.insert(name.to_string(), ty);
        }
    }
    fn bind_mutability(&mut self, name: &str, ty: Ty, mutable: bool) {
        self.bind(name, ty);
        if mutable && let Some(scope) = self.mutable_scopes.last_mut() {
            scope.insert(name.to_string());
        }
    }

    fn lookup(&self, name: &str) -> Option<Ty> {
        for s in self.scopes.iter().rev() {
            if let Some(t) = s.get(name) {
                return Some(t.clone());
            }
        }
        None
    }

    fn binding_scope(&self, name: &str) -> Option<(usize, bool)> {
        self.scopes
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, scope)| {
                scope.contains_key(name).then(|| {
                    let mutable = self.mutable_scopes[index].contains(name);
                    (index, mutable)
                })
            })
    }

    /// Best-effort, side-effect-free type of a simple argument expression:
    /// literals and already-bound locals. Returns `Ty::Unknown` for anything
    /// requiring full inference, so collection element inference degrades
    /// cleanly (never invents a wrong type, never double-reports diagnostics).
    fn quick_ty(&self, e: &Expr) -> Ty {
        match &e.kind {
            ExprKind::Int(_) => Ty::I64,
            ExprKind::Float(_) => Ty::F64,
            ExprKind::Bool(_) => Ty::Bool,
            ExprKind::Str(_) => Ty::Str,
            ExprKind::Path(segs) if segs.len() == 1 => self.lookup(&segs[0]).unwrap_or(Ty::Unknown),
            _ => Ty::Unknown,
        }
    }

    /// Refine a freshly-inferred empty collection type (`Vec::new()` starts as
    /// `Vec<?>`, `HashMap::new()` as `HashMap<?, ?>`) by scanning later
    /// statements in the same block for element-inserting method calls on
    /// `name` — `name.push(x)` fills the Vec element, `name.insert(k, v)` fills
    /// the map key/value. Only `Unknown` slots are filled; other types pass
    /// through unchanged.
    fn refine_collection_from_usage(&self, name: &str, ty: &Ty, rest: &[Stmt]) -> Ty {
        let needs = match ty {
            Ty::Vec(e) => matches!(**e, Ty::Unknown),
            Ty::Map(k, v) => matches!(**k, Ty::Unknown) || matches!(**v, Ty::Unknown),
            _ => false,
        };
        if !needs {
            return ty.clone();
        }

        let mut calls: Vec<(&str, &[Expr])> = Vec::new();
        for s in rest {
            collect_calls_on_stmt(name, s, &mut calls);
        }

        match ty {
            Ty::Vec(e) => {
                let mut elem = (**e).clone();
                if matches!(elem, Ty::Unknown) {
                    for (m, args) in &calls {
                        if *m == "push" && args.len() == 1 {
                            let t = self.quick_ty(&args[0]);
                            if t.is_concrete() {
                                elem = t;
                                break;
                            }
                        }
                    }
                }
                Ty::Vec(Box::new(elem))
            }
            Ty::Map(k, v) => {
                let mut kt = (**k).clone();
                let mut vt = (**v).clone();
                for (m, args) in &calls {
                    if *m == "insert" && args.len() == 2 {
                        if matches!(kt, Ty::Unknown) {
                            let t = self.quick_ty(&args[0]);
                            if t.is_concrete() {
                                kt = t;
                            }
                        }
                        if matches!(vt, Ty::Unknown) {
                            let t = self.quick_ty(&args[1]);
                            if t.is_concrete() {
                                vt = t;
                            }
                        }
                    }
                    if kt.is_concrete() && vt.is_concrete() {
                        break;
                    }
                }
                Ty::Map(Box::new(kt), Box::new(vt))
            }
            _ => ty.clone(),
        }
    }

    fn err(&mut self, sp: SourceRange, msg: String) {
        self.err_with_code(rua_core::DiagnosticCode::TypeMismatch, sp, msg);
    }

    fn err_with_code(&mut self, code: rua_core::DiagnosticCode, sp: SourceRange, msg: String) {
        self.errs
            .push(Diag::new(code, sp.file, sp.start, sp.len, sp.line, msg));
    }

    fn invalid_binary(&mut self, sp: SourceRange, msg: String) {
        self.err_with_code(rua_core::DiagnosticCode::TypeInvalidBinary, sp, msg);
    }

    // --- driver ------------------------------------------------------------

    fn run(&mut self, prog: &Program) {
        self.check_items(&prog.items, self.hir.root);
        self.block(&prog.chunk);
    }

    fn check_items(&mut self, items: &[Item], module: crate::hir::ModuleId) {
        for (item_index, item) in items.iter().enumerate() {
            match item {
                Item::Fn(f) => self.check_fn(&f.generics, &f.params, f.ret.as_ref(), &f.body, None),
                Item::Impl(im) => {
                    let self_ty = self
                        .hir
                        .impl_targets
                        .get(&(module, item_index))
                        .map(|target| self.named_type(target.owner))
                        .unwrap_or(Ty::Unknown);
                    for m in &im.methods {
                        let st = if m.has_self {
                            Some(self_ty.clone())
                        } else {
                            None
                        };
                        let gens = merge_generics(&im.generics, &m.generics);
                        self.check_fn(&gens, &m.params, m.ret.as_ref(), &m.body, st);
                    }
                }
                Item::Trait(t) => {
                    for tm in &t.methods {
                        if let Some(b) = &tm.default {
                            // `self` type is unknown for a default (any impl).
                            let st = if tm.has_self { Some(Ty::Unknown) } else { None };
                            self.check_fn(&t.generics, &tm.params, tm.ret.as_ref(), b, st);
                        }
                    }
                }
                Item::Mod(child) => {
                    if let Some(&child_module) =
                        self.hir.module(module).scope.modules.get(&child.name)
                    {
                        self.check_items(&child.items, child_module);
                        self.block(&child.chunk);
                    }
                }
                _ => {}
            }
        }
    }

    fn check_fn(
        &mut self,
        generics: &[GenericParam],
        params: &[Param],
        ret: Option<&Type>,
        body: &Block,
        self_ty: Option<Ty>,
    ) {
        // Install this function's generic parameters (name -> bounds) so `ty_of`
        // maps them to `Ty::Generic` and method calls resolve via their bounds.
        self.set_gen_bounds(generics);
        self.push();
        if let Some(st) = self_ty {
            self.bind("self", st);
        }
        for p in params {
            let ty = self.ty_of(&p.ty);
            self.bind(&p.name, ty);
        }
        let ret_ty = ret.map(|t| self.ty_of(t)).unwrap_or(Ty::Unit);
        let actual = self.block(body);
        // Only check a concrete, non-unit declared return against a concrete tail.
        if let Some(tail) = &body.tail
            && ret_ty.is_concrete()
            && ret_ty != Ty::Unit
            && actual.is_concrete()
            && !compatible(&ret_ty, &actual)
        {
            self.err(
                tail.span,
                format!(
                    "expected return type `{}`, found `{}`",
                    ret_ty.name(),
                    actual.name()
                ),
            );
        }
        self.pop();
        self.gen_bounds.clear();
    }

    /// Type-check a block; returns the type of its trailing expression (or Unit).
    fn block(&mut self, b: &Block) -> Ty {
        self.push();
        for (i, s) in b.stmts.iter().enumerate() {
            // Later statements in the same block are handed to `stmt` so a
            // `let`-bound empty collection can infer its element types from
            // subsequent `push`/`insert` calls (local flow inference).
            self.stmt(s, &b.stmts[i + 1..], b.tail.as_deref());
        }
        let t = match &b.tail {
            Some(e) => self.infer(e),
            None => Ty::Unit,
        };
        self.pop();
        t
    }

    fn stmt(&mut self, s: &Stmt, rest: &[Stmt], tail: Option<&Expr>) {
        match s {
            Stmt::Let {
                name,
                mutable,
                ty,
                init,
                ..
            } => {
                let init_ty = if let ExprKind::VecLit(elements) = &init.kind {
                    let expected = ty.as_ref().map(|ty| self.ty_of(ty));
                    self.infer_vec_literal(elements, expected.as_ref(), init.span)
                } else if let ExprKind::Closure { params, ret, body } = &init.kind {
                    let mut usage = ClosureUsage::default();
                    for statement in rest {
                        collect_closure_usage_stmt(name, statement, &mut usage);
                    }
                    if let Some(tail) = tail {
                        collect_closure_usage_expr(name, tail, &mut usage);
                    }
                    let expected = self.closure_params_from_calls(params.len(), &usage.calls);
                    let closure_ty = self.infer_closure(
                        init.span,
                        params,
                        ret.as_ref(),
                        body,
                        ClosureContext {
                            expected: &expected,
                            report_unknown_params: true,
                            allow_mutable_capture: false,
                        },
                    );
                    let has_unknown_param = matches!(
                        &closure_ty,
                        Ty::Closure(params, _) if params.iter().any(|ty| matches!(ty, Ty::Unknown))
                    );
                    if usage.escapes || (usage.calls.is_empty() && !has_unknown_param) {
                        self.err(init.span, "closure escape is not supported yet".to_string());
                    }
                    closure_ty
                } else {
                    self.infer(init)
                };
                let bind_ty = match ty {
                    Some(t) => {
                        let declared = self.ty_of(t);
                        if declared.is_concrete()
                            && init_ty.is_concrete()
                            && !compatible(&declared, &init_ty)
                        {
                            self.err(
                                init.span,
                                format!(
                                    "`{}` annotated as `{}` but initialized with `{}`",
                                    name,
                                    declared.name(),
                                    init_ty.name()
                                ),
                            );
                        }
                        declared
                    }
                    None => init_ty,
                };
                // For an inferred (un-annotated) empty collection, fill unknown
                // element/key/value slots from later `push`/`insert` calls in
                // this block (`let mut m = HashMap::new(); m.insert("a", 1)`).
                let bind_ty = if ty.is_none() {
                    self.refine_collection_from_usage(name, &bind_ty, rest)
                } else {
                    bind_ty
                };
                if ty.is_none()
                    && matches!(init.kind, ExprKind::VecLit(_))
                    && matches!(&bind_ty, Ty::Vec(element) if matches!(element.as_ref(), Ty::Unknown))
                {
                    self.err(
                        init.span,
                        "cannot infer the element type of an empty Vec literal".to_string(),
                    );
                }
                self.bind_mutability(name, bind_ty, *mutable);
            }
            Stmt::Expr(e) => {
                self.infer(e);
            }
            Stmt::Return(Some(e)) => {
                let ty = self.infer(e);
                if let Some(returns) = self.closure_returns.last_mut() {
                    returns.push(ty);
                }
            }
            Stmt::Return(None) => {
                if let Some(returns) = self.closure_returns.last_mut() {
                    returns.push(Ty::Unit);
                }
            }
            Stmt::While { cond, body } => {
                let c = self.infer(cond);
                self.expect_bool(&c, cond.span, "`while` condition");
                self.loop_breaks.push(None);
                self.block(body);
                self.loop_breaks.pop();
            }
            Stmt::Loop { body } => {
                self.loop_breaks.push(Some(Vec::new()));
                self.block(body);
                self.loop_breaks.pop();
            }
            Stmt::For {
                var, iter, body, ..
            } => {
                let iter_ty = self.infer(iter);
                let elem = match iter_ty {
                    Ty::Iter(item, draft) => {
                        let item = *item;
                        self.finish_iter_plan(
                            &draft,
                            IterConsumerKind::For,
                            iter.id,
                            iter.span,
                            &item,
                            &Ty::Unit,
                        );
                        item
                    }
                    Ty::Vec(item) => {
                        let item = *item;
                        let draft = self.iter_source(IterSourceKind::Vec, iter.span, &item);
                        self.finish_iter_plan(
                            &draft,
                            IterConsumerKind::For,
                            iter.id,
                            iter.span,
                            &item,
                            &Ty::Unit,
                        );
                        item
                    }
                    ty if ty.is_concrete() => {
                        self.err(iter.span, format!("type `{}` is not iterable", ty.name()));
                        Ty::Unknown
                    }
                    _ => Ty::Unknown,
                };
                self.push();
                self.bind(var, elem);
                self.loop_breaks.push(None);
                self.block(body);
                self.loop_breaks.pop();
                self.pop();
            }
            Stmt::WhileLet { pat, expr, body } => {
                let scrut = self.infer(expr);
                self.push();
                self.bind_pattern(pat, &scrut);
                self.loop_breaks.push(None);
                self.block(body);
                self.loop_breaks.pop();
                self.pop();
            }
            Stmt::Break(value) => {
                let inferred = value.as_ref().map(|value| (self.infer(value), value.span));
                match (self.loop_breaks.last_mut(), inferred) {
                    (Some(Some(values)), Some(value)) => values.push(value),
                    (Some(Some(values)), None) => values.push((Ty::Unit, SourceRange::EMPTY)),
                    (Some(None), Some((_, span))) => {
                        self.err(
                            span,
                            "`break` with a value is only allowed in `loop`".to_string(),
                        );
                    }
                    _ => {}
                }
            }
            Stmt::Continue => {}
        }
    }

    fn expect_bool(&mut self, ty: &Ty, sp: SourceRange, what: &str) {
        if ty.is_concrete() && *ty != Ty::Bool {
            self.err(
                sp,
                format!("{} must be `bool`, found `{}`", what, ty.name()),
            );
        }
    }

    // --- expression inference ---------------------------------------------

    fn closure_params_from_calls(&self, arity: usize, calls: &[&[Expr]]) -> Vec<Ty> {
        let mut expected = vec![Ty::Unknown; arity];
        let mut conflicted = vec![false; arity];
        for args in calls {
            if args.len() != arity {
                continue;
            }
            for (index, arg) in args.iter().enumerate() {
                let actual = self.quick_ty(arg);
                if !actual.is_concrete() || conflicted[index] {
                    continue;
                }
                if matches!(expected[index], Ty::Unknown) {
                    expected[index] = actual;
                } else if !compatible(&expected[index], &actual) {
                    expected[index] = Ty::Unknown;
                    conflicted[index] = true;
                } else {
                    expected[index] = join(&expected[index], &actual);
                }
            }
        }
        expected
    }

    fn infer_closure(
        &mut self,
        span: SourceRange,
        params: &[ClosureParam],
        ret: Option<&Type>,
        body: &ClosureBody,
        context: ClosureContext<'_>,
    ) -> Ty {
        self.first_closure.get_or_insert(span);
        let param_tys: Vec<Ty> = params
            .iter()
            .enumerate()
            .map(|(index, param)| {
                param
                    .ty
                    .as_ref()
                    .map(|ty| self.ty_of(ty))
                    .unwrap_or_else(|| context.expected.get(index).cloned().unwrap_or(Ty::Unknown))
            })
            .collect();

        if context.report_unknown_params {
            for (param, ty) in params.iter().zip(&param_tys) {
                if matches!(ty, Ty::Unknown) {
                    self.err(
                        param.name_span,
                        format!(
                            "cannot infer type of closure parameter `{}`; add a type annotation or use it in a supported call context",
                            param.name
                        ),
                    );
                }
            }
        }

        let boundary = self.scopes.len();
        self.closure_boundaries.push(boundary);
        self.closure_mutable_capture_allowed
            .push(context.allow_mutable_capture);
        self.closure_returns.push(Vec::new());
        self.push();
        for (param, ty) in params.iter().zip(&param_tys) {
            self.bind(&param.name, ty.clone());
        }
        let body_ty = match body {
            ClosureBody::Expr(expr) => self.infer(expr),
            ClosureBody::Block(block) => self.block(block),
        };
        self.pop();
        let returns = self.closure_returns.pop().unwrap_or_default();
        self.closure_boundaries.pop();
        self.closure_mutable_capture_allowed.pop();

        let mut inferred_ret = returns.first().cloned().unwrap_or_else(|| body_ty.clone());
        for return_ty in returns.iter().skip(1) {
            inferred_ret = join(&inferred_ret, return_ty);
        }
        let body_has_value = match body {
            ClosureBody::Expr(_) => true,
            ClosureBody::Block(block) => block.tail.is_some(),
        };
        if !returns.is_empty() && body_has_value {
            inferred_ret = join(&inferred_ret, &body_ty);
        }

        if let Some(ret) = ret {
            let declared = self.ty_of(ret);
            if declared.is_concrete()
                && inferred_ret.is_concrete()
                && !compatible(&declared, &inferred_ret)
            {
                self.err(
                    span,
                    format!(
                        "closure expects return type `{}`, found `{}`",
                        declared.name(),
                        inferred_ret.name()
                    ),
                );
            }
            inferred_ret = declared;
        }

        Ty::Closure(param_tys, Box::new(inferred_ret))
    }

    fn iter_source(&self, kind: IterSourceKind, range: SourceRange, item: &Ty) -> IterDraft {
        IterDraft {
            source: IterSource {
                kind,
                range,
                item_type: item.name(),
            },
            adapters: Vec::new(),
        }
    }

    fn finish_iter_plan(
        &mut self,
        draft: &IterDraft,
        consumer: IterConsumerKind,
        consumer_expression: ExprId,
        consumer_range: SourceRange,
        item: &Ty,
        output: &Ty,
    ) {
        self.iter_plans.insert(
            consumer_expression,
            IterPlan {
                source: draft.source.clone(),
                adapters: draft.adapters.clone(),
                consumer,
                consumer_range,
                item_type: item.name(),
                output_type: output.name(),
            },
        );
    }

    fn infer_method_args(&mut self, recv: &Ty, method: &str, args: &[Expr]) -> Vec<Ty> {
        // Option<T>::map(fn) and Result<T, E>::map(fn) propagate the payload
        // type to the closure parameter without treating the closure as escaped.
        if let Ty::Option(inner) | Ty::Result(inner, _) = recv {
            if method == "map" && args.len() == 1 {
                let expected = vec![(**inner).clone()];
                if let ExprKind::Closure { params, ret, body } = &args[0].kind {
                    return vec![self.infer_closure(
                        args[0].span,
                        params,
                        ret.as_ref(),
                        body,
                        ClosureContext {
                            expected: &expected,
                            report_unknown_params: false,
                            allow_mutable_capture: true,
                        },
                    )];
                }
            }
            return args.iter().map(|arg| self.infer(arg)).collect();
        }
        let Ty::Iter(item, _) = recv else {
            return args.iter().map(|arg| self.infer(arg)).collect();
        };
        let item = (**item).clone();
        let mut inferred = Vec::with_capacity(args.len());
        for (index, arg) in args.iter().enumerate() {
            let expected = match (method, index) {
                ("map" | "filter" | "filter_map" | "any" | "all" | "find", 0) => {
                    Some(vec![item.clone()])
                }
                ("fold", 1) => Some(vec![
                    inferred.first().cloned().unwrap_or(Ty::Unknown),
                    item.clone(),
                ]),
                _ => None,
            };
            if let Some(expected) = expected
                && let ExprKind::Closure { params, ret, body } = &arg.kind
            {
                inferred.push(self.infer_closure(
                    arg.span,
                    params,
                    ret.as_ref(),
                    body,
                    ClosureContext {
                        expected: &expected,
                        report_unknown_params: false,
                        allow_mutable_capture: true,
                    },
                ));
            } else {
                inferred.push(self.infer(arg));
            }
        }
        inferred
    }

    fn infer_iterator_method(
        &mut self,
        recv: &Ty,
        method: &str,
        type_args: &[Type],
        arg_tys: &[Ty],
        args: &[Expr],
        call: &Expr,
    ) -> Option<Ty> {
        let expression = call.id;
        let span = call.span;
        if let Ty::Vec(item) = recv
            && matches!(method, "iter" | "into_iter")
        {
            if !type_args.is_empty() {
                self.err(
                    span,
                    format!("iterator source `{method}` does not accept type arguments"),
                );
            }
            if !args.is_empty() {
                self.err(
                    span,
                    format!("iterator source `{method}` expects no arguments"),
                );
            }
            let kind = if method == "iter" {
                IterSourceKind::VecIter
            } else {
                IterSourceKind::VecIntoIter
            };
            return Some(Ty::Iter(
                item.clone(),
                Box::new(self.iter_source(kind, span, item)),
            ));
        }

        // Option<T>::map(fn) -> Option<U>
        if method == "map"
            && args.len() == 1
            && let Ty::Option(inner) = recv
        {
            let _param_ty = (**inner).clone();
            let ret_ty = match &arg_tys[0] {
                Ty::Closure(params, ret) if params.len() == 1 => (**ret).clone(),
                _ => Ty::Unknown,
            };
            return Some(Ty::Option(Box::new(ret_ty)));
        }

        // Result<T, E>::map(fn) -> Result<U, E>
        if method == "map"
            && args.len() == 1
            && let Ty::Result(_, error) = recv
        {
            let ret_ty = match &arg_tys[0] {
                Ty::Closure(params, ret) if params.len() == 1 => (**ret).clone(),
                _ => Ty::Unknown,
            };
            return Some(Ty::Result(Box::new(ret_ty), error.clone()));
        }

        let Ty::Iter(item, draft) = recv else {
            if matches!(
                method,
                "iter"
                    | "into_iter"
                    | "map"
                    | "filter"
                    | "filter_map"
                    | "enumerate"
                    | "take"
                    | "skip"
                    | "collect"
                    | "fold"
                    | "count"
                    | "any"
                    | "all"
                    | "find"
                    | "next"
            ) && recv.is_concrete()
                && !matches!(recv, Ty::Named { .. })
            {
                self.err(span, format!("type `{}` is not iterable", recv.name()));
                return Some(Ty::Unknown);
            }
            return None;
        };

        if method != "collect" && !type_args.is_empty() {
            self.err(
                span,
                format!("iterator method `{method}` does not accept explicit type arguments"),
            );
        }
        let closure_ret = |index: usize| match arg_tys.get(index) {
            Some(Ty::Closure(_, ret)) => Some((**ret).clone()),
            _ => None,
        };
        let closure_arity = |index: usize| match arg_tys.get(index) {
            Some(Ty::Closure(params, _)) => Some(params.len()),
            _ => None,
        };
        let append = |kind: IterAdapterKind, output: Ty| {
            let mut next = (**draft).clone();
            next.adapters.push(IterAdapter {
                kind,
                range: span,
                input_type: item.name(),
                output_type: output.name(),
            });
            Ty::Iter(Box::new(output), Box::new(next))
        };

        match method {
            "map" if args.len() == 1 => {
                let output = closure_ret(0).unwrap_or_else(|| {
                    self.err(
                        args[0].span,
                        "iterator map argument must be a closure".to_string(),
                    );
                    Ty::Unknown
                });
                if let Some(arity) = closure_arity(0)
                    && arity != 1
                {
                    self.err(
                        args[0].span,
                        format!("iterator map closure expects 1 parameter, found {arity}"),
                    );
                }
                Some(append(IterAdapterKind::Map, output))
            }
            "filter" if args.len() == 1 => {
                if let Some(ret) = closure_ret(0) {
                    self.expect_bool(&ret, args[0].span, "iterator filter predicate");
                } else {
                    self.err(
                        args[0].span,
                        "iterator filter argument must be a closure".to_string(),
                    );
                }
                if let Some(arity) = closure_arity(0)
                    && arity != 1
                {
                    self.err(
                        args[0].span,
                        format!("iterator filter closure expects 1 parameter, found {arity}"),
                    );
                }
                Some(append(IterAdapterKind::Filter, (**item).clone()))
            }
            "filter_map" if args.len() == 1 => {
                let mapped = match closure_ret(0) {
                    Some(Ty::Option(inner)) => *inner,
                    Some(ret) if ret.is_concrete() => {
                        self.err(
                            args[0].span,
                            format!(
                                "iterator filter_map closure must return `Option<_>`, found `{}`",
                                ret.name()
                            ),
                        );
                        Ty::Unknown
                    }
                    _ => {
                        self.err(
                            args[0].span,
                            "iterator filter_map argument must be a closure".to_string(),
                        );
                        Ty::Unknown
                    }
                };
                if let Some(arity) = closure_arity(0)
                    && arity != 1
                {
                    self.err(
                        args[0].span,
                        format!("iterator filter_map closure expects 1 parameter, found {arity}"),
                    );
                }
                Some(append(IterAdapterKind::FilterMap, mapped))
            }
            "enumerate" if args.is_empty() => Some(append(
                IterAdapterKind::Enumerate,
                Ty::Tuple(vec![Ty::I64, (**item).clone()]),
            )),
            "take" | "skip" if args.len() == 1 => {
                let count = &arg_tys[0];
                if count.is_concrete() && *count != Ty::I64 {
                    self.err(
                        args[0].span,
                        format!(
                            "iterator {method} count must be `i64`, found `{}`",
                            count.name()
                        ),
                    );
                }
                if matches!(args[0].kind, ExprKind::Unary { op: UnOp::Neg, .. }) {
                    self.err(
                        args[0].span,
                        format!("iterator {method} count must be non-negative"),
                    );
                }
                let kind = if method == "take" {
                    IterAdapterKind::Take
                } else {
                    IterAdapterKind::Skip
                };
                Some(append(kind, (**item).clone()))
            }
            "map" | "filter" | "filter_map" | "enumerate" | "take" | "skip" => {
                self.err(
                    span,
                    format!("iterator adapter `{method}` has invalid arguments"),
                );
                let kind = match method {
                    "filter" => IterAdapterKind::Filter,
                    "filter_map" => IterAdapterKind::FilterMap,
                    "enumerate" => IterAdapterKind::Enumerate,
                    "take" => IterAdapterKind::Take,
                    "skip" => IterAdapterKind::Skip,
                    _ => IterAdapterKind::Map,
                };
                Some(append(kind, Ty::Unknown))
            }
            "collect" if args.is_empty() => {
                let output = if type_args.len() == 1 {
                    match self.ty_of(&type_args[0]) {
                        Ty::Vec(target) => {
                            let target = if matches!(*target, Ty::Unknown) {
                                Box::new((**item).clone())
                            } else {
                                target
                            };
                            if !compatible(&target, item) {
                                self.err(
                                    span,
                                    format!(
                                        "collect target element type `{}` is incompatible with iterator item `{}`",
                                        target.name(),
                                        item.name()
                                    ),
                                );
                            }
                            Ty::Vec(target)
                        }
                        target => {
                            self.err(
                                span,
                                format!(
                                    "iterator collect target must be `Vec<_>`, found `{}`",
                                    target.name()
                                ),
                            );
                            Ty::Vec(Box::new(Ty::Unknown))
                        }
                    }
                } else {
                    // When collect() has no explicit type argument, infer
                    // `Vec<T>` from the iterator item type or from the
                    // surrounding let-binding type annotation.
                    Ty::Vec(item.clone())
                };
                self.finish_iter_plan(
                    draft,
                    IterConsumerKind::CollectVec,
                    expression,
                    span,
                    item,
                    &output,
                );
                Some(output)
            }
            "fold" if args.len() == 2 => {
                let accumulator = arg_tys[0].clone();
                match (closure_arity(1), closure_ret(1)) {
                    (Some(2), Some(ret)) => {
                        if !compatible(&accumulator, &ret) {
                            self.err(
                                args[1].span,
                                format!(
                                    "iterator fold closure must return accumulator type `{}`, found `{}`",
                                    accumulator.name(),
                                    ret.name()
                                ),
                            );
                        }
                    }
                    (Some(arity), _) => self.err(
                        args[1].span,
                        format!("iterator fold closure expects 2 parameters, found {arity}"),
                    ),
                    _ => self.err(
                        args[1].span,
                        "iterator fold second argument must be a closure".to_string(),
                    ),
                }
                self.finish_iter_plan(
                    draft,
                    IterConsumerKind::Fold,
                    expression,
                    span,
                    item,
                    &accumulator,
                );
                Some(accumulator)
            }
            "count" if args.is_empty() => {
                self.finish_iter_plan(
                    draft,
                    IterConsumerKind::Count,
                    expression,
                    span,
                    item,
                    &Ty::I64,
                );
                Some(Ty::I64)
            }
            "any" | "all" | "find" if args.len() == 1 => {
                if closure_arity(0) != Some(1) {
                    self.err(
                        args[0].span,
                        format!("iterator {method} argument must be a one-parameter closure"),
                    );
                }
                if let Some(ret) = closure_ret(0) {
                    self.expect_bool(&ret, args[0].span, &format!("iterator {method} predicate"));
                }
                let (consumer, output) = match method {
                    "any" => (IterConsumerKind::Any, Ty::Bool),
                    "all" => (IterConsumerKind::All, Ty::Bool),
                    _ => (IterConsumerKind::Find, Ty::Option(item.clone())),
                };
                self.finish_iter_plan(draft, consumer, expression, span, item, &output);
                Some(output)
            }
            "next" if args.is_empty() => {
                let output = Ty::Option(item.clone());
                self.finish_iter_plan(
                    draft,
                    IterConsumerKind::Next,
                    expression,
                    span,
                    item,
                    &output,
                );
                Some(output)
            }
            "collect" | "fold" | "count" | "any" | "all" | "find" | "next" => {
                self.err(
                    span,
                    format!("iterator consumer `{method}` has invalid arguments"),
                );
                Some(Ty::Unknown)
            }
            _ => None,
        }
    }

    fn infer(&mut self, e: &Expr) -> Ty {
        let sp = e.span;
        match &e.kind {
            ExprKind::Int(_) => Ty::I64,
            ExprKind::Float(_) => Ty::F64,
            ExprKind::Str(_) => Ty::Str,
            ExprKind::Bool(_) => Ty::Bool,
            ExprKind::VecLit(elements) => self.infer_vec_literal(elements, None, sp),
            ExprKind::Closure { params, ret, body } => {
                let ty = self.infer_closure(
                    sp,
                    params,
                    ret.as_ref(),
                    body,
                    ClosureContext {
                        expected: &[],
                        report_unknown_params: true,
                        allow_mutable_capture: false,
                    },
                );
                self.err(sp, "closure escape is not supported yet".to_string());
                ty
            }
            ExprKind::Path(segs) => {
                let target = self.resolved_target(e);
                if matches!(target, Some(crate::hir::ResolvedTarget::Local(_))) {
                    return self.lookup(&segs[0]).unwrap_or(Ty::Unknown);
                }
                if let Some(target) = target {
                    if let Some((owner, _)) = self.resolved_enum_variant_ids(target) {
                        return self.named_type(owner);
                    }
                    if let Some(definition) = self.definition_for_target(target)
                        && matches!(
                            definition.kind,
                            crate::hir::DefKind::Struct | crate::hir::DefKind::Enum
                        )
                    {
                        return self.named_type(definition.id);
                    }
                    match target {
                        crate::hir::ResolvedTarget::Builtin(
                            rua_core::BuiltinId::VariantOptionNone,
                        ) => return Ty::Option(Box::new(Ty::Unknown)),
                        crate::hir::ResolvedTarget::Builtin(
                            rua_core::BuiltinId::VariantResultOk
                            | rua_core::BuiltinId::VariantResultErr,
                        ) => {
                            return Ty::Result(Box::new(Ty::Unknown), Box::new(Ty::Unknown));
                        }
                        _ => {}
                    }
                }
                Ty::Unknown
            }
            ExprKind::Unary { op, expr } => {
                let t = self.infer(expr);
                if matches!(
                    (op, &t),
                    (UnOp::Neg, Ty::I64 | Ty::F64) | (UnOp::Not, Ty::Bool)
                ) {
                    self.pure_operators.insert(e.id);
                }
                match op {
                    UnOp::Neg => {
                        if t.is_concrete() && !t.is_numeric() && !matches!(t, Ty::Named { .. }) {
                            self.err(sp, format!("cannot negate `{}`", t.name()));
                        }
                        t
                    }
                    UnOp::Not => {
                        self.expect_bool(&t, sp, "operand of `!`");
                        Ty::Bool
                    }
                }
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let l = self.infer(lhs);
                let r = self.infer(rhs);
                if *op == BinOp::Contains {
                    let kind = match &r {
                        Ty::Vec(_) => Some(ContainsKind::Vec),
                        Ty::Map(_, _) => Some(ContainsKind::Map),
                        Ty::Str => Some(ContainsKind::String),
                        Ty::Iter(_, _) => Some(ContainsKind::Iter),
                        _ => None,
                    };
                    if let Some(kind) = kind {
                        self.contains.insert(e.id, kind);
                    }
                }
                if matches!(l, Ty::I64 | Ty::F64 | Ty::Bool | Ty::Str)
                    && matches!(r, Ty::I64 | Ty::F64 | Ty::Bool | Ty::Str)
                {
                    self.pure_operators.insert(e.id);
                }
                // Record `i64 / i64` and `i64 % i64` so codegen can emit the
                // truncating integer helpers from the `number` runtime export.
                if matches!(l, Ty::I64) && matches!(r, Ty::I64) {
                    if *op == BinOp::Div {
                        self.int_divs.insert(e.id);
                    } else if *op == BinOp::Rem {
                        self.int_rems.insert(e.id);
                    }
                }
                // Record `String + String` so codegen emits Lua concatenation.
                if *op == BinOp::Add && matches!(l, Ty::Str) && matches!(r, Ty::Str) {
                    self.str_concats.insert(e.id);
                }
                self.infer_binary(*op, &l, &r, sp)
            }
            ExprKind::Loop(body) => {
                self.loop_breaks.push(Some(Vec::new()));
                self.block(body);
                let values = self.loop_breaks.pop().flatten().unwrap_or_default();
                let mut result = Ty::Unknown;
                for (value, value_span) in values {
                    if matches!(result, Ty::Unknown) {
                        result = value;
                    } else if !compatible(&result, &value) {
                        self.err(
                            if value_span == SourceRange::EMPTY {
                                sp
                            } else {
                                value_span
                            },
                            format!(
                                "incompatible `break` value: expected `{}`, found `{}`",
                                result.name(),
                                value.name()
                            ),
                        );
                        result = Ty::Unknown;
                    } else {
                        result = join(&result, &value);
                    }
                }
                result
            }
            ExprKind::Call { callee, args } => self.infer_call(callee, args),
            ExprKind::MethodCall {
                recv,
                method,
                optional,
                type_args,
                args,
                ..
            } => {
                let receiver_ty = self.infer(recv);
                let (rt, optional_chain) = if *optional {
                    match receiver_ty {
                        Ty::Option(item) => (*item, true),
                        Ty::Unknown => (Ty::Unknown, true),
                        other => {
                            self.err(
                                recv.span,
                                format!(
                                    "optional chaining requires `Option`, found `{}`",
                                    other.name()
                                ),
                            );
                            (Ty::Unknown, true)
                        }
                    }
                } else {
                    (receiver_ty, false)
                };
                let result = (|| {
                    let standard_method = self.standard_method_definition(&rt, method);
                    if let Some(definition) = standard_method {
                        self.standard_methods.insert(e.id, definition);
                        if self.hir.language_item(definition)
                            == Some(crate::builtins::LanguageItem::OptionMap)
                        {
                            self.option_maps.insert(e.id);
                        }
                    }
                    let arg_tys = self.infer_method_args(&rt, method, args);
                    if let Ty::Named {
                        def: owner,
                        name: type_name,
                    } = &rt
                        && let Some(&definition) =
                            self.hir.associated_items.get(&(*owner, method.clone()))
                        && let Some(signature) = self.method_sigs.get(&definition)
                    {
                        self.user_methods
                            .insert(e.id, UserMethodDispatch::Static(definition));
                        let params = signature.params.clone();
                        let ret = signature.ret.clone();
                        let generics = signature.generics.clone();
                        let generic_bounds = signature.generic_bounds.clone();
                        self.check_method_call(type_name, method, &params, &arg_tys, args, sp);
                        if !generics.is_empty() {
                            let mut substitution = HashMap::new();
                            for (parameter, argument) in params.iter().zip(arg_tys.iter()) {
                                unify_generic(parameter, argument, &mut substitution);
                            }
                            let display = format!("{}::{}", type_name, method);
                            self.check_bound_satisfaction(
                                &display,
                                &generics,
                                &generic_bounds,
                                &substitution,
                                sp,
                            );
                            return subst_ty(&ret, &substitution);
                        }
                        return ret;
                    }
                    if let Ty::Trait {
                        def: trait_id,
                        name: trait_name,
                    } = &rt
                        && let Some(&definition) =
                            self.hir.trait_items.get(&(*trait_id, method.clone()))
                        && let Some(signature) = self.method_sigs.get(&definition)
                    {
                        self.user_methods.insert(e.id, UserMethodDispatch::Dynamic);
                        let params = signature.params.clone();
                        let ret = signature.ret.clone();
                        self.check_method_call(trait_name, method, &params, &arg_tys, args, sp);
                        return ret;
                    }
                    // Receiver typed as a generic parameter: resolve the method via
                    // its trait bounds. If some bound trait declares the method, use
                    // that signature; otherwise stay silent (Unknown).
                    if let Ty::Generic { id, .. } = &rt
                        && let Some((tname, signature)) = self.resolve_generic_method(*id, method)
                    {
                        self.user_methods.insert(e.id, UserMethodDispatch::Dynamic);
                        self.check_method_call(
                            &tname,
                            method,
                            &signature.params,
                            &arg_tys,
                            args,
                            sp,
                        );
                        if !signature.generics.is_empty() {
                            // Method-level generics: infer them from the call's
                            // arguments, verify their bounds, and substitute into
                            // the return type.
                            let mut subst: HashMap<GenericParamId, Ty> = HashMap::new();
                            for (p, a) in signature.params.iter().zip(arg_tys.iter()) {
                                unify_generic(p, a, &mut subst);
                            }
                            let owner = format!("{}::{}", tname, method);
                            self.check_bound_satisfaction(
                                &owner,
                                &signature.generics,
                                &signature.generic_bounds,
                                &subst,
                                sp,
                            );
                            return subst_ty(&signature.ret, &subst);
                        }
                        return signature.ret;
                    }
                    if standard_method.is_some()
                        && matches!(rt, Ty::Str)
                        && matches!(method.as_str(), "chars" | "split")
                    {
                        self.str_methods.insert(e.id);
                        let kind = if method == "chars" {
                            IterSourceKind::StringChars
                        } else {
                            IterSourceKind::StringSplit
                        };
                        return Ty::Iter(
                            Box::new(Ty::Str),
                            Box::new(self.iter_source(kind, e.span, &Ty::Str)),
                        );
                    }
                    if standard_method.is_some()
                        && let Some(ret) =
                            self.infer_iterator_method(&rt, method, type_args, &arg_tys, args, e)
                    {
                        return ret;
                    }
                    if let Some(definition) = standard_method {
                        if matches!(rt, Ty::Str) {
                            self.str_methods.insert(e.id);
                        }
                        return self.infer_standard_method_call(
                            definition, &rt, method, &arg_tys, args, sp,
                        );
                    }
                    Ty::Unknown
                })();
                if optional_chain {
                    if matches!(result, Ty::Option(_)) {
                        result
                    } else {
                        Ty::Option(Box::new(result))
                    }
                } else {
                    result
                }
            }
            ExprKind::Field {
                base,
                name,
                optional,
                ..
            } => {
                let base_ty = self.infer(base);
                let (bt, optional_chain) = if *optional {
                    match base_ty {
                        Ty::Option(item) => (*item, true),
                        Ty::Unknown => (Ty::Unknown, true),
                        other => {
                            self.err(
                                base.span,
                                format!(
                                    "optional chaining requires `Option`, found `{}`",
                                    other.name()
                                ),
                            );
                            (Ty::Unknown, true)
                        }
                    }
                } else {
                    (base_ty, false)
                };
                let result = if let Ty::Named {
                    def: definition,
                    name: sname,
                } = &bt
                {
                    // Pull the field's type + definition span out from under the
                    // immutable borrow so we can record a member hit afterwards.
                    let fields = self.struct_defs.get(definition);
                    let field = fields.map(|fields| {
                        fields
                            .iter()
                            .find(|(field, _, _)| field == name)
                            .map(|(_, ty, _)| ty.clone())
                    });
                    match field {
                        Some(Some(field_type)) => field_type,
                        // Struct known, field absent: report the error.
                        Some(None) => {
                            self.err(sp, format!("struct `{}` has no field `{}`", sname, name));
                            Ty::Unknown
                        }
                        // Named enum or not-yet-known: don't claim a field error.
                        None => Ty::Unknown,
                    }
                } else {
                    Ty::Unknown
                };
                if optional_chain {
                    if matches!(result, Ty::Option(_)) {
                        result
                    } else {
                        Ty::Option(Box::new(result))
                    }
                } else {
                    result
                }
            }
            ExprKind::Index { base, index } => {
                let bt = self.infer(base);
                self.infer(index);
                match bt {
                    Ty::Vec(t) => *t,
                    _ => Ty::Unknown,
                }
            }
            ExprKind::Range {
                start,
                end,
                inclusive,
            } => {
                let start_ty = self.infer(start);
                let end_ty = self.infer(end);
                for (bound, ty) in [(start.as_ref(), start_ty), (end.as_ref(), end_ty)] {
                    if ty.is_concrete() && ty != Ty::I64 {
                        self.err(
                            bound.span,
                            format!(
                                "range bound must be integer-compatible, found `{}`",
                                ty.name()
                            ),
                        );
                    }
                }
                let kind = if *inclusive {
                    IterSourceKind::InclusiveRange
                } else {
                    IterSourceKind::ExclusiveRange
                };
                Ty::Iter(
                    Box::new(Ty::I64),
                    Box::new(self.iter_source(kind, e.span, &Ty::I64)),
                )
            }
            ExprKind::StructLit { path, fields } => {
                for (_, field) in fields {
                    self.infer(field);
                }
                let target = self.resolved_target(e);
                if let Some(target) = target
                    && let Some((owner, _)) = self.resolved_enum_variant_ids(target)
                {
                    self.named_type(owner)
                } else if let Some(definition) =
                    target.and_then(|target| self.definition_for_target(target))
                    && definition.kind == crate::hir::DefKind::Struct
                {
                    self.named_type(definition.id)
                } else {
                    let _ = path;
                    Ty::Unknown
                }
            }
            ExprKind::MapLit(entries) => {
                let mut key_ty = Ty::Unknown;
                let mut value_ty = Ty::Unknown;
                for (key, value) in entries {
                    let current_key = self.infer(key);
                    let current_value = self.infer(value);
                    if key_ty.is_concrete()
                        && current_key.is_concrete()
                        && !compatible(&key_ty, &current_key)
                    {
                        self.err(
                            key.span,
                            format!(
                                "map key must be `{}`, found `{}`",
                                key_ty.name(),
                                current_key.name()
                            ),
                        );
                    } else {
                        key_ty = join(&key_ty, &current_key);
                    }
                    if value_ty.is_concrete()
                        && current_value.is_concrete()
                        && !compatible(&value_ty, &current_value)
                    {
                        self.err(
                            value.span,
                            format!(
                                "map value must be `{}`, found `{}`",
                                value_ty.name(),
                                current_value.name()
                            ),
                        );
                    } else {
                        value_ty = join(&value_ty, &current_value);
                    }
                }
                Ty::Map(Box::new(key_ty), Box::new(value_ty))
            }
            ExprKind::Try { expr } => {
                // `e?` unwraps a Result<T,_> or Option<T> to T.
                let inner = self.infer(expr);
                match &inner {
                    Ty::Result(t, _) => {
                        self.result_tries.insert(e.id);
                        t.as_ref().clone()
                    }
                    Ty::Option(t) => t.as_ref().clone(),
                    ty => {
                        self.err(
                            expr.span,
                            format!(
                                "`?` operator requires `Result` or `Option`, found `{}`",
                                ty.name()
                            ),
                        );
                        Ty::Unknown
                    }
                }
            }
            ExprKind::If {
                cond,
                then_block,
                else_block,
            } => {
                let c = self.infer(cond);
                self.expect_bool(&c, cond.span, "`if` condition");
                let t = self.block(then_block);
                let e_ty = match else_block.as_deref() {
                    Some(ElseBranch::Block(b)) => self.block(b),
                    Some(ElseBranch::If(inner)) => self.infer(inner),
                    None => Ty::Unit,
                };
                // Unify branches; if they disagree, fall back to Unknown.
                if compatible(&t, &e_ty) {
                    if t.is_concrete() { t } else { e_ty }
                } else {
                    Ty::Unknown
                }
            }
            ExprKind::IfLet {
                pat,
                expr,
                then_block,
                else_block,
            } => {
                let scrut = self.infer(expr);
                self.push();
                self.bind_pattern(pat, &scrut);
                let t = self.block(then_block);
                self.pop();
                let e_ty = match else_block.as_deref() {
                    Some(ElseBranch::Block(b)) => self.block(b),
                    Some(ElseBranch::If(inner)) => self.infer(inner),
                    None => Ty::Unit,
                };
                if compatible(&t, &e_ty) {
                    if t.is_concrete() { t } else { e_ty }
                } else {
                    Ty::Unknown
                }
            }
            ExprKind::Block(b) => self.block(b),
            ExprKind::Assign { op, target, value } => {
                let target_ty = self.infer(target);
                let value_ty = self.infer(value);
                if let Some(op) = op {
                    if matches!(target_ty, Ty::I64) && matches!(value_ty, Ty::I64) {
                        if *op == BinOp::Div {
                            self.int_divs.insert(e.id);
                        } else if *op == BinOp::Rem {
                            self.int_rems.insert(e.id);
                        }
                    }
                    if *op == BinOp::Add
                        && matches!(target_ty, Ty::Str)
                        && matches!(value_ty, Ty::Str)
                    {
                        self.str_concats.insert(e.id);
                    }
                    let assigned = self.infer_binary(*op, &target_ty, &value_ty, sp);
                    if assigned.is_concrete()
                        && target_ty.is_concrete()
                        && !compatible(&target_ty, &assigned)
                    {
                        self.err(
                            value.span,
                            format!(
                                "compound assignment produces `{}` for target `{}`",
                                assigned.name(),
                                target_ty.name()
                            ),
                        );
                    }
                } else if target_ty.is_concrete()
                    && value_ty.is_concrete()
                    && !compatible(&target_ty, &value_ty)
                {
                    self.err(
                        value.span,
                        format!(
                            "cannot assign `{}` to `{}`",
                            value_ty.name(),
                            target_ty.name()
                        ),
                    );
                }
                if let ExprKind::Path(segments) = &target.kind
                    && segments.len() == 1
                    && let Some(&boundary) = self.closure_boundaries.last()
                    && !self
                        .closure_mutable_capture_allowed
                        .last()
                        .copied()
                        .unwrap_or(false)
                    && let Some((scope, mutable)) = self.binding_scope(&segments[0])
                    && scope < boundary
                    && mutable
                {
                    self.err(
                        target.span,
                        format!(
                            "mutable capture of `{}` is only supported in a fused iterator consumer",
                            segments[0]
                        ),
                    );
                }
                Ty::Unit
            }
            ExprKind::Match { scrut, arms } => {
                let scrut_ty = self.infer(scrut);
                let mut result = Ty::Unknown;
                for arm in arms {
                    self.push();
                    for p in &arm.pats {
                        self.bind_pattern(p, &scrut_ty);
                    }
                    if let Some(g) = &arm.guard {
                        let gt = self.infer(g);
                        self.expect_bool(&gt, g.span, "match guard");
                    }
                    let bt = self.infer(&arm.body);
                    self.pop();
                    if result == Ty::Unknown {
                        result = bt;
                    } else if !compatible(&result, &bt) {
                        result = Ty::Unknown;
                    }
                }
                result
            }
        }
    }

    fn infer_vec_literal(
        &mut self,
        elements: &[Expr],
        expected: Option<&Ty>,
        _span: SourceRange,
    ) -> Ty {
        let expected_element = match expected {
            Some(Ty::Vec(element)) => Some(element.as_ref()),
            _ => None,
        };
        let mut element_ty = expected_element.cloned().unwrap_or(Ty::Unknown);
        for element in elements {
            let actual = self.infer(element);
            if let Some(expected) = expected_element {
                if actual.is_concrete() && !compatible(expected, &actual) {
                    self.err(
                        element.span,
                        format!(
                            "Vec element expects `{}`, found `{}`",
                            expected.name(),
                            actual.name()
                        ),
                    );
                }
                continue;
            }
            if element_ty.is_concrete() && actual.is_concrete() && !compatible(&element_ty, &actual)
            {
                self.err(
                    element.span,
                    format!(
                        "Vec literal mixes incompatible element types `{}` and `{}`",
                        element_ty.name(),
                        actual.name()
                    ),
                );
            } else {
                element_ty = join(&element_ty, &actual);
            }
        }
        Ty::Vec(Box::new(element_ty))
    }

    fn infer_binary(&mut self, op: BinOp, l: &Ty, r: &Ty, sp: SourceRange) -> Ty {
        use BinOp::*;
        match op {
            Add | Sub | Mul | Div | Rem => {
                // `String + String` is concatenation (codegen emits `..`).
                if op == Add && matches!(l, Ty::Str) && matches!(r, Ty::Str) {
                    return Ty::Str;
                }
                if let Ty::Generic { id, .. } = l {
                    let (trait_id, _) = arithmetic_trait(op);
                    let target = crate::hir::TraitTarget::Builtin(trait_id);
                    if self
                        .gen_bounds
                        .get(id)
                        .is_some_and(|bounds| bounds.contains(&target))
                    {
                        return Ty::Unknown;
                    }
                    self.invalid_binary(
                        sp,
                        format!(
                            "type `{}` does not implement operator trait `{}`",
                            l.name(),
                            trait_id.name()
                        ),
                    );
                    return Ty::Unknown;
                }
                if let Ty::Named { def: owner, .. } = l {
                    let (trait_id, method) = arithmetic_trait(op);
                    let target = crate::hir::TraitTarget::Builtin(trait_id);
                    if !self
                        .hir
                        .type_traits
                        .get(owner)
                        .is_some_and(|traits| traits.contains(&target))
                    {
                        self.invalid_binary(
                            sp,
                            format!(
                                "type `{}` does not implement operator trait `{}`",
                                l.name(),
                                trait_id.name()
                            ),
                        );
                        return Ty::Unknown;
                    }
                    let Some(signature) = self
                        .hir
                        .associated_items
                        .get(&(*owner, method.to_string()))
                        .and_then(|definition| self.method_sigs.get(definition))
                        .cloned()
                    else {
                        return Ty::Unknown;
                    };
                    if let Some(expected) = signature.params.first()
                        && expected.is_concrete()
                        && r.is_concrete()
                        && !compatible(expected, r)
                    {
                        self.invalid_binary(
                            sp,
                            format!(
                                "right operand of `{}` must be `{}`, found `{}`",
                                op.symbol(),
                                expected.name(),
                                r.name()
                            ),
                        );
                        return Ty::Unknown;
                    }
                    return signature.ret.clone();
                }
                if l.is_numeric() && r.is_numeric() {
                    if *l == Ty::F64 || *r == Ty::F64 {
                        Ty::F64
                    } else {
                        Ty::I64
                    }
                } else if !l.is_concrete() || !r.is_concrete() {
                    Ty::Unknown
                } else {
                    self.invalid_binary(
                        sp,
                        format!(
                            "cannot apply binary `{}` to `{}` and `{}`",
                            op.symbol(),
                            l.name(),
                            r.name()
                        ),
                    );
                    Ty::Unknown
                }
            }
            Coalesce => match l {
                Ty::Option(item) => {
                    if item.is_concrete() && r.is_concrete() && !compatible(item, r) {
                        self.err(
                            sp,
                            format!(
                                "right operand of `??` must be `{}`, found `{}`",
                                item.name(),
                                r.name()
                            ),
                        );
                        Ty::Unknown
                    } else {
                        join(item, r)
                    }
                }
                Ty::Unknown => r.clone(),
                other => {
                    self.invalid_binary(
                        sp,
                        format!(
                            "left operand of `??` must be `Option`, found `{}`",
                            other.name()
                        ),
                    );
                    Ty::Unknown
                }
            },
            Contains => {
                let element = match r {
                    Ty::Vec(item) | Ty::Iter(item, _) => Some(item.as_ref().clone()),
                    Ty::Map(key, _) => Some(key.as_ref().clone()),
                    Ty::Str => Some(Ty::Str),
                    Ty::Unknown => return Ty::Bool,
                    other => {
                        self.invalid_binary(
                            sp,
                            format!(
                                "right operand of `in` is not searchable: `{}`",
                                other.name()
                            ),
                        );
                        None
                    }
                };
                if let Some(element) = element
                    && element.is_concrete()
                    && l.is_concrete()
                    && !compatible(&element, l)
                {
                    self.err(
                        sp,
                        format!("`in` expects `{}`, found `{}`", element.name(), l.name()),
                    );
                }
                Ty::Bool
            }
            And | Or => {
                self.expect_bool(l, sp, "operand of `&&`/`||`");
                self.expect_bool(r, sp, "operand of `&&`/`||`");
                Ty::Bool
            }
            Eq | Ne => {
                if l.is_concrete() && r.is_concrete() && !compatible(l, r) {
                    self.invalid_binary(
                        sp,
                        format!(
                            "cannot apply binary `{}` to `{}` and `{}`",
                            op.symbol(),
                            l.name(),
                            r.name()
                        ),
                    );
                }
                Ty::Bool
            }
            Lt | Le | Gt | Ge => {
                let valid = (l.is_numeric() && r.is_numeric())
                    || (matches!(l, Ty::Str) && matches!(r, Ty::Str));
                if l.is_concrete() && r.is_concrete() && !valid {
                    self.invalid_binary(
                        sp,
                        format!(
                            "cannot apply binary `{}` to `{}` and `{}`",
                            op.symbol(),
                            l.name(),
                            r.name()
                        ),
                    );
                }
                Ty::Bool
            }
        }
    }

    fn infer_call(&mut self, callee: &Expr, args: &[Expr]) -> Ty {
        let arg_tys: Vec<Ty> = args.iter().map(|a| self.infer(a)).collect();
        if let ExprKind::Closure { params, ret, body } = &callee.kind {
            let closure = self.infer_closure(
                callee.span,
                params,
                ret.as_ref(),
                body,
                ClosureContext {
                    expected: &arg_tys,
                    report_unknown_params: true,
                    allow_mutable_capture: false,
                },
            );
            return self.check_closure_call("closure", &closure, &arg_tys, args, callee.span);
        }
        let ExprKind::Path(segs) = &callee.kind else {
            let callee_ty = self.infer(callee);
            return self.check_closure_call("closure", &callee_ty, &arg_tys, args, callee.span);
        };
        let target = self.resolved_target(callee);
        if let Some(crate::hir::ResolvedTarget::Builtin(builtin)) = target {
            let a0 = || arg_tys.first().cloned().unwrap_or(Ty::Unknown);
            match builtin {
                rua_core::BuiltinId::VariantOptionSome => {
                    return Ty::Option(Box::new(a0()));
                }
                rua_core::BuiltinId::VariantOptionNone => {
                    return Ty::Option(Box::new(Ty::Unknown));
                }
                rua_core::BuiltinId::VariantResultOk => {
                    return Ty::Result(Box::new(a0()), Box::new(Ty::Unknown));
                }
                rua_core::BuiltinId::VariantResultErr => {
                    return Ty::Result(Box::new(Ty::Unknown), Box::new(a0()));
                }
                _ => {}
            }
        }
        // A local closure shadows free functions and is callable through its
        // inferred signature.
        if matches!(target, Some(crate::hir::ResolvedTarget::Local(_)))
            && segs.len() == 1
            && let Some(closure @ Ty::Closure(_, _)) = self.lookup(&segs[0])
        {
            return self.check_closure_call(&segs[0], &closure, &arg_tys, args, callee.span);
        }
        if let Some(definition) = target.and_then(|target| self.definition_for_target(target))
            && matches!(
                definition.kind,
                crate::hir::DefKind::Function
                    | crate::hir::DefKind::Method { .. }
                    | crate::hir::DefKind::TraitMethod { .. }
                    | crate::hir::DefKind::ExternFunction { .. }
            )
            && let Some(sig) = self.fn_sigs.get(&definition.id)
        {
            let display = self.definition_key(definition);
            let signature = sig.clone();
            return self.check_free_call(
                &display,
                &signature.params,
                &signature.ret,
                &signature.generics,
                &signature.generic_bounds,
                signature.variadic,
                &arg_tys,
                args,
                callee.span,
            );
        }
        if let Some(target) = target
            && let Some((owner, _)) = self.resolved_enum_variant_ids(target)
        {
            return self.named_type(owner);
        }
        // Unresolved calls remain unknown; semantic checks never guess a target
        // from the source spelling.
        Ty::Unknown
    }

    fn standard_method_definition(&self, receiver: &Ty, method: &str) -> Option<crate::hir::DefId> {
        let owner_name = match receiver {
            Ty::Str => "String",
            Ty::Vec(_) => "Vec",
            Ty::Option(_) => "Option",
            Ty::Result(_, _) => "Result",
            Ty::Map(_, _) => "HashMap",
            Ty::Iter(_, _) => "Iter",
            _ => return None,
        };
        let prelude = self
            .hir
            .module(self.hir.root)
            .scope
            .modules
            .get("__rua_builtin")?;
        let owner = self.hir.module(*prelude).scope.types.get(owner_name)?;
        self.hir
            .associated_items
            .get(&(*owner, method.to_string()))
            .copied()
    }

    fn infer_standard_method_call(
        &mut self,
        definition: crate::hir::DefId,
        receiver: &Ty,
        method: &str,
        argument_types: &[Ty],
        arguments: &[Expr],
        span: SourceRange,
    ) -> Ty {
        let Some(signature) = self.method_sigs.get(&definition).cloned() else {
            return Ty::Unknown;
        };
        let receiver_arguments = match receiver {
            Ty::Vec(item) | Ty::Option(item) | Ty::Iter(item, _) => {
                vec![(**item).clone()]
            }
            Ty::Result(ok, error) | Ty::Map(ok, error) => {
                vec![(**ok).clone(), (**error).clone()]
            }
            Ty::Str => Vec::new(),
            _ => Vec::new(),
        };
        self.infer_standard_method_call_with_receiver_arguments(
            definition,
            method,
            &signature,
            &receiver_arguments,
            argument_types,
            arguments,
            span,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn infer_standard_method_call_with_receiver_arguments(
        &mut self,
        definition: crate::hir::DefId,
        method: &str,
        signature: &FnSig,
        receiver_arguments: &[Ty],
        argument_types: &[Ty],
        arguments: &[Expr],
        span: SourceRange,
    ) -> Ty {
        let mut substitution = HashMap::new();
        for (generic, argument) in signature.generics.iter().zip(receiver_arguments) {
            substitution.insert(generic.id, argument.clone());
        }
        let parameters = signature
            .params
            .iter()
            .map(|parameter| subst_ty(parameter, &substitution))
            .collect::<Vec<_>>();
        for (parameter, argument) in parameters.iter().zip(argument_types) {
            unify_generic(parameter, argument, &mut substitution);
        }
        let owner = match self.hir.definition(definition).kind {
            crate::hir::DefKind::Method { owner } | crate::hir::DefKind::TraitMethod { owner } => {
                self.hir.definition(owner).name.clone()
            }
            _ => "std".to_string(),
        };
        let parameters = signature
            .params
            .iter()
            .map(|parameter| subst_ty(parameter, &substitution))
            .collect::<Vec<_>>();
        self.check_method_call(&owner, method, &parameters, argument_types, arguments, span);
        self.check_bound_satisfaction(
            &format!("{owner}::{method}"),
            &signature.generics,
            &signature.generic_bounds,
            &substitution,
            span,
        );
        subst_ty(&signature.ret, &substitution)
    }

    fn check_closure_call(
        &mut self,
        name: &str,
        closure: &Ty,
        arg_tys: &[Ty],
        args: &[Expr],
        span: SourceRange,
    ) -> Ty {
        let Ty::Closure(params, ret) = closure else {
            return Ty::Unknown;
        };
        if params.len() != args.len() {
            self.err(
                span,
                format!(
                    "closure `{name}` expects {} argument(s), got {}",
                    params.len(),
                    args.len()
                ),
            );
            return (**ret).clone();
        }
        for (index, (expected, actual)) in params.iter().zip(arg_tys).enumerate() {
            if !compatible(expected, actual) {
                self.err(
                    args[index].span,
                    format!(
                        "argument {} of closure `{name}` expects `{}`, found `{}`",
                        index + 1,
                        expected.name(),
                        actual.name()
                    ),
                );
            }
        }
        (**ret).clone()
    }

    /// Resolve `method` on a value typed as the generic parameter `gname` by
    /// scanning its trait bounds. Returns the trait name and method signature
    /// for the first bound that declares the method (a real conflict would be a
    /// genuine ambiguity we simply don't diagnose here). `generics` are the
    /// method's own (method-level) generic parameters.
    fn resolve_generic_method(
        &self,
        generic: GenericParamId,
        method: &str,
    ) -> Option<(String, FnSig)> {
        let bounds = self.gen_bounds.get(&generic)?;
        for target in bounds {
            let crate::hir::TraitTarget::Item(trait_id) = target else {
                continue;
            };
            let Some(definition) = self.hir.trait_items.get(&(*trait_id, method.to_string()))
            else {
                continue;
            };
            let Some(sig) = self.method_sigs.get(definition) else {
                continue;
            };
            return Some((self.hir.definition(*trait_id).name.clone(), sig.clone()));
        }
        None
    }

    /// Shared arity/argument/generic checking for a resolved free (or
    /// module-qualified / extern) function call. `dispname` is used only in
    /// diagnostics. Returns the (possibly generic-substituted) return type.
    #[allow(clippy::too_many_arguments)]
    fn check_free_call(
        &mut self,
        dispname: &str,
        params: &[Ty],
        ret: &Ty,
        generics: &[GenericParam],
        generic_bounds: &HashMap<GenericParamId, Vec<crate::hir::TraitTarget>>,
        variadic: bool,
        arg_tys: &[Ty],
        args: &[Expr],
        callee_sp: SourceRange,
    ) -> Ty {
        if (!variadic && args.len() != params.len()) || (variadic && args.len() < params.len()) {
            self.err(
                callee_sp,
                format!(
                    "function `{}` expects {}{} argument(s), got {}",
                    dispname,
                    params.len(),
                    if variadic { " or more" } else { "" },
                    args.len()
                ),
            );
            return ret.clone();
        }
        for (i, at) in arg_tys.iter().take(params.len()).enumerate() {
            if !compatible(&params[i], at) {
                self.err(
                    args[i].span,
                    format!(
                        "argument {} of `{}` expects `{}`, found `{}`",
                        i + 1,
                        dispname,
                        params[i].name(),
                        at.name()
                    ),
                );
            }
        }
        // Generic instantiation: infer type arguments from the call's arguments,
        // verify each satisfies its trait bounds, substitute into the return type.
        if !generics.is_empty() {
            let mut subst: HashMap<GenericParamId, Ty> = HashMap::new();
            for (index, (parameter, argument)) in params.iter().zip(arg_tys.iter()).enumerate() {
                let expected = subst_ty(parameter, &subst);
                if !unify_generic(parameter, argument, &mut subst) {
                    self.err(
                        args[index].span,
                        format!(
                            "argument {} of `{}` expects `{}`, found `{}`",
                            index + 1,
                            dispname,
                            expected.name(),
                            argument.name()
                        ),
                    );
                }
            }
            self.check_bound_satisfaction(dispname, generics, generic_bounds, &subst, callee_sp);
            return subst_ty(ret, &subst);
        }
        ret.clone()
    }

    /// Verify that each inferred generic type argument satisfies its declared
    /// trait bounds. Conservative: only a concrete user type (struct/enum) bound
    /// by a user-declared trait is checked; builtin trait bounds (`Clone`, ...)
    /// and unknown/scalar/unbound type args are assumed to satisfy (no error).
    fn check_bound_satisfaction(
        &mut self,
        fname: &str,
        generics: &[GenericParam],
        generic_bounds: &HashMap<GenericParamId, Vec<crate::hir::TraitTarget>>,
        subst: &HashMap<GenericParamId, Ty>,
        sp: SourceRange,
    ) {
        for g in generics {
            let Some(Ty::Named {
                def: concrete,
                name: concrete_name,
            }) = subst.get(&g.id)
            else {
                continue;
            };
            for target in generic_bounds.get(&g.id).into_iter().flatten() {
                let crate::hir::TraitTarget::Item(trait_id) = target else {
                    continue;
                };
                let implemented = self
                    .hir
                    .type_traits
                    .get(concrete)
                    .is_some_and(|traits| traits.contains(target));
                if !implemented {
                    let trait_name = &self.hir.definition(*trait_id).name;
                    self.err(
                        sp,
                        format!(
                            "type `{}` does not implement trait `{}` (required by bound `{}: {}` of `{}`)",
                            concrete_name, trait_name, g.name, trait_name, fname
                        ),
                    );
                }
            }
        }
    }

    /// Shared arity/argument-type checking for a resolved method call.
    fn check_method_call(
        &mut self,
        owner: &str,
        method: &str,
        params: &[Ty],
        arg_tys: &[Ty],
        args: &[Expr],
        sp: SourceRange,
    ) {
        if arg_tys.len() != params.len() {
            self.err(
                sp,
                format!(
                    "method `{}::{}` expects {} argument(s), got {}",
                    owner,
                    method,
                    params.len(),
                    arg_tys.len()
                ),
            );
            return;
        }
        for (i, at) in arg_tys.iter().enumerate() {
            if !compatible(&params[i], at) {
                self.err(
                    args[i].span,
                    format!(
                        "argument {} of `{}::{}` expects `{}`, found `{}`",
                        i + 1,
                        owner,
                        method,
                        params[i].name(),
                        at.name()
                    ),
                );
            }
        }
    }

    /// Bind identifiers introduced by a pattern (all `Unknown` for now).
    /// Bind every name introduced by pattern `p`, propagating the scrutinee type
    /// `ty` so bindings get a concrete type where inferable. Only the built-in
    /// refutable payloads (`Some`/`Ok`/`Err`) are destructured; user enum
    /// variants and struct patterns bind their payloads as `Unknown` (their
    /// per-variant field types aren't tracked), which degrades hover cleanly.
    fn bind_pattern(&mut self, p: &Pattern, ty: &Ty) {
        match p {
            Pattern::Binding(name, _) => self.bind(name, ty.clone()),
            Pattern::TupleVariant {
                id, path, elems, ..
            } => {
                let payload = self.tuple_payload(*id, path, ty);
                for (i, e) in elems.iter().enumerate() {
                    let et = payload
                        .as_ref()
                        .and_then(|v| v.get(i).cloned())
                        .unwrap_or(Ty::Unknown);
                    self.bind_pattern(e, &et);
                }
            }
            Pattern::StructVariant {
                id, path, fields, ..
            } => {
                let field_tys = self.struct_payload(*id, path, ty);
                for (fname, fp) in fields {
                    let ft = field_tys
                        .as_ref()
                        .and_then(|m| m.iter().find(|(n, _)| n == fname).map(|(_, t)| t.clone()))
                        .unwrap_or(Ty::Unknown);
                    self.bind_pattern(fp, &ft);
                }
            }
            _ => {}
        }
    }

    /// Tuple-variant payload element types for pattern `path` against scrutinee
    /// `ty`: built-in `Some`/`Ok`/`Err` first, then user enum variants (when the
    /// scrutinee is typed `Ty::Named(enum)`). `None` → bind elements as Unknown.
    fn tuple_payload(&self, id: PatternId, _path: &[String], ty: &Ty) -> Option<Vec<Ty>> {
        let target = self.resolved_pattern_variant(id);
        if let Some(crate::hir::ResolvedTarget::Builtin(builtin)) = target {
            return builtin_payload(builtin, ty);
        }
        if let Some((owner, variant)) =
            target.and_then(|target| self.resolved_enum_variant_ids(target))
            && let Ty::Named { def: scrutinee, .. } = ty
            && *scrutinee == owner
            && let Some(VariantPayload::Tuple(types)) = self.variant_payloads.get(&variant)
        {
            return Some(types.clone());
        }
        None
    }

    /// Struct-variant field types (name → type) for pattern `path` against
    /// scrutinee `ty`, when it's a user enum variant. `None` → Unknown fields.
    fn struct_payload(
        &self,
        id: PatternId,
        _path: &[String],
        ty: &Ty,
    ) -> Option<Vec<(String, Ty)>> {
        let target = self.resolved_pattern_variant(id);
        if let Some((owner, variant)) =
            target.and_then(|target| self.resolved_enum_variant_ids(target))
            && let Ty::Named { def: scrutinee, .. } = ty
            && *scrutinee == owner
            && let Some(VariantPayload::Struct(fields)) = self.variant_payloads.get(&variant)
        {
            return Some(fields.clone());
        }
        None
    }
}

fn arithmetic_trait(op: BinOp) -> (rua_core::BuiltinTraitId, &'static str) {
    use rua_core::BuiltinTraitId;
    match op {
        BinOp::Add => (BuiltinTraitId::Add, "add"),
        BinOp::Sub => (BuiltinTraitId::Sub, "sub"),
        BinOp::Mul => (BuiltinTraitId::Mul, "mul"),
        BinOp::Div => (BuiltinTraitId::Div, "div"),
        BinOp::Rem => (BuiltinTraitId::Rem, "rem"),
        _ => unreachable!("only arithmetic operators have arithmetic traits"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser;

    /// Run type-checking and return the internal tables for inspection.
    fn run_tc(src: &str) -> Tc {
        let program = parser::parse(src).unwrap();
        let hir = crate::hir::resolve(&program);
        let mut tc = Tc::new(&program, &hir);
        tc.run(&program);
        tc
    }

    fn root_type(tc: &Tc, name: &str) -> crate::hir::DefId {
        tc.hir.module(tc.hir.root).scope.types[name]
    }

    #[test]
    fn struct_fields_are_keyed_by_type_identity() {
        let tc = run_tc("struct Point { x: f64, y: i64 }");
        let point = root_type(&tc, "Point");
        let fields = &tc.struct_defs[&point];
        assert_eq!(fields.len(), 2);
        assert_eq!((&fields[0].0, &fields[0].1), (&"x".to_string(), &Ty::F64));
        assert_eq!((&fields[1].0, &fields[1].1), (&"y".to_string(), &Ty::I64));
    }

    #[test]
    fn impl_methods_are_keyed_by_owner_identity() {
        let src = r#"
struct Point { x: f64, y: f64 }
impl Point {
    fn dist(&self) -> f64 { 0.0 }
    fn move_to(&mut self, nx: f64, ny: f64) {}
}
"#;
        let tc = run_tc(src);
        let point = root_type(&tc, "Point");
        let dist = tc.hir.associated_items[&(point, "dist".to_string())];
        let move_to = tc.hir.associated_items[&(point, "move_to".to_string())];
        assert_eq!(tc.method_sigs[&dist].ret, Ty::F64);
        assert_eq!(tc.method_sigs[&move_to].params, vec![Ty::F64, Ty::F64]);
    }

    #[test]
    fn trait_methods_are_keyed_by_trait_identity() {
        let src = r#"
trait Area {
    fn area(&self) -> f64;
    fn name(&self) -> String { "shape".to_string() }
}
"#;
        let tc = run_tc(src);
        let area_trait = root_type(&tc, "Area");
        let area = tc.hir.trait_items[&(area_trait, "area".to_string())];
        let name = tc.hir.trait_items[&(area_trait, "name".to_string())];
        assert_eq!(tc.method_sigs[&area].ret, Ty::F64);
        assert_eq!(tc.method_sigs[&name].ret, Ty::Str);
    }

    #[test]
    fn inherited_default_method_preserves_trait_origin() {
        let src = r#"
trait Shape {
    fn area(&self) -> f64;
    fn label(&self) -> String { "shape".to_string() }
}
struct Circle { r: f64 }
impl Shape for Circle {
    fn area(&self) -> f64 { 3.14 * self.r * self.r }
}
"#;
        let tc = run_tc(src);
        let shape = root_type(&tc, "Shape");
        let circle = root_type(&tc, "Circle");
        let trait_label = tc.hir.trait_items[&(shape, "label".to_string())];
        let inherited = tc.hir.associated_items[&(circle, "label".to_string())];
        assert_eq!(tc.hir.method_origins[&inherited], trait_label);
        assert_eq!(tc.method_sigs[&inherited].ret, Ty::Str);
    }
}
