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
use crate::diag::{Diag, render_all};
use crate::token::SourceRange;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IterSourceKind {
    ExclusiveRange,
    InclusiveRange,
    Vec,
    VecIter,
    VecIntoIter,
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
    /// A user struct or enum, by name.
    Named(String),
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
    Generic(String),
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
        !matches!(self, Ty::Unknown | Ty::Generic(_))
    }
    fn name(&self) -> String {
        match self {
            Ty::I64 => "i64".into(),
            Ty::F64 => "f64".into(),
            Ty::Bool => "bool".into(),
            Ty::Str => "String".into(),
            Ty::Unit => "()".into(),
            Ty::Named(n) => n.clone(),
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
            Ty::Generic(n) => n.clone(),
            Ty::Unknown => "?".into(),
        }
    }
}

/// Two types are compatible unless both are concrete and genuinely different.
/// Numeric types are mutually compatible (Lua unifies numbers; we stay lenient).
/// Parameterized types recurse on their element types.
fn compatible(a: &Ty, b: &Ty) -> bool {
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
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
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
        ExprKind::Closure { body, .. } => match body {
            ClosureBody::Expr(expr) => collect_calls_on_expr(name, expr, out),
            ClosureBody::Block(block) => collect_calls_on_block(name, block, out),
        },
        ExprKind::MethodCall { recv, method, args, .. } => {
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
        ExprKind::MacroCall { args, .. } => {
            for a in args {
                collect_calls_on_expr(name, a, out);
            }
        }
        ExprKind::If { cond, then_block, else_block } => {
            collect_calls_on_expr(name, cond, out);
            collect_calls_on_block(name, then_block, out);
            if let Some(eb) = else_block {
                collect_calls_on_else(name, eb, out);
            }
        }
        ExprKind::IfLet { expr, then_block, else_block, .. } => {
            collect_calls_on_expr(name, expr, out);
            collect_calls_on_block(name, then_block, out);
            if let Some(eb) = else_block {
                collect_calls_on_else(name, eb, out);
            }
        }
        ExprKind::Block(b) => collect_calls_on_block(name, b, out),
        ExprKind::Assign { target, value } => {
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

fn collect_calls_on_else<'a>(
    name: &str,
    eb: &'a ElseBranch,
    out: &mut Vec<(&'a str, &'a [Expr])>,
) {
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
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
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
        ExprKind::Match { scrut, arms } => {
            collect_closure_usage_expr(name, scrut, usage);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_closure_usage_expr(name, guard, usage);
                }
                collect_closure_usage_expr(name, &arm.body, usage);
            }
        }
        ExprKind::MacroCall { args, .. } => {
            for arg in args {
                collect_closure_usage_expr(name, arg, usage);
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
        (Ty::Unknown, _) => b.clone(),
        (_, Ty::Unknown) => a.clone(),
        (Ty::F64, _) | (_, Ty::F64) if a.is_numeric() && b.is_numeric() => Ty::F64,
        _ => a.clone(),
    }
}

/// Infer bindings for generic parameters by structurally matching a declared
/// parameter type against a concrete argument type. Only concrete bindings are
/// recorded; conflicting bindings are joined (falling back to `Unknown`).
fn unify_generic(param: &Ty, arg: &Ty, subst: &mut HashMap<String, Ty>) {
    match (param, arg) {
        (Ty::Generic(g), a) if a.is_concrete() => {
            subst
                .entry(g.clone())
                .and_modify(|cur| *cur = join(cur, a))
                .or_insert_with(|| a.clone());
        }
        (Ty::Vec(p), Ty::Vec(a)) => unify_generic(p, a, subst),
        (Ty::Option(p), Ty::Option(a)) => unify_generic(p, a, subst),
        (Ty::Result(p1, e1), Ty::Result(p2, e2)) => {
            unify_generic(p1, p2, subst);
            unify_generic(e1, e2, subst);
        }
        (Ty::Map(k1, v1), Ty::Map(k2, v2)) => {
            unify_generic(k1, k2, subst);
            unify_generic(v1, v2, subst);
        }
        (Ty::Iter(p, _), Ty::Iter(a, _)) => unify_generic(p, a, subst),
        (Ty::Tuple(p), Ty::Tuple(a)) if p.len() == a.len() => {
            for (p, a) in p.iter().zip(a) {
                unify_generic(p, a, subst);
            }
        }
        (Ty::Closure(p1, r1), Ty::Closure(p2, r2)) if p1.len() == p2.len() => {
            for (p, a) in p1.iter().zip(p2) {
                unify_generic(p, a, subst);
            }
            unify_generic(r1, r2, subst);
        }
        _ => {}
    }
}

/// Replace generic parameters in `ty` with their inferred bindings; unbound
/// generics become `Unknown` (they carry no meaning outside the callee).
fn subst_ty(ty: &Ty, subst: &HashMap<String, Ty>) -> Ty {
    match ty {
        Ty::Generic(g) => subst.get(g).cloned().unwrap_or(Ty::Unknown),
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

struct FnSig {
    params: Vec<Ty>,
    ret: Ty,
    /// Generic parameters (with bounds) declared on this function, used to check
    /// bound satisfaction at call sites. Empty for non-generic fns and for
    /// methods/trait signatures (where call-site checking is not yet done).
    generics: Vec<GenericParam>,
}

/// Return type of a recognized builtin method on a parameterized collection
/// type. Returns `Unknown` for anything unrecognized (never an error), so
/// unmodeled methods on `Vec`/`HashMap` stay silently untyped.
fn builtin_method_ret(recv: &Ty, method: &str) -> Ty {
    match (recv, method) {
        (Ty::Vec(t), "get" | "pop") => Ty::Option(t.clone()),
        (Ty::Vec(_), "len") => Ty::I64,
        (Ty::Vec(_), "push" | "set") => Ty::Unit,
        (Ty::Map(_, v), "get" | "remove") => Ty::Option(v.clone()),
        (Ty::Map(_, _), "len") => Ty::I64,
        (Ty::Map(_, _), "contains_key") => Ty::Bool,
        (Ty::Map(_, _), "insert") => Ty::Unit,
        _ => Ty::Unknown,
    }
}

/// Human-readable signature detail for a recognized builtin method on a
/// parameterized collection type. Returns `None` when the method is not a
/// recognized builtin (the caller falls back to `Unknown` for the return type
/// without recording a member hit).
fn builtin_method_detail(recv: &Ty, method: &str) -> Option<String> {
    match recv {
        Ty::Vec(t) => {
            let elem = t.name();
            Some(match method {
                "len" => format!("fn len(&self) -> i64"),
                "get" => format!("fn get(&self, index: usize) -> Option<{}>", elem),
                "push" => format!("fn push(&mut self, value: {})", elem),
                "pop" => format!("fn pop(&mut self) -> Option<{}>", elem),
                "set" => format!("fn set(&mut self, index: usize, value: {})", elem),
                _ => return None,
            })
        }
        Ty::Map(k, v) => {
            let key = k.name();
            let val = v.name();
            Some(match method {
                "len" => "fn len(&self) -> i64".to_string(),
                "get" => format!("fn get(&self, key: &{}) -> Option<{}>", key, val),
                "insert" => format!("fn insert(&mut self, key: {}, value: {}) -> Option<{}>", key, val, val),
                "remove" => format!("fn remove(&mut self, key: &{}) -> Option<{}>", key, val),
                "contains_key" => format!("fn contains_key(&self, key: &{}) -> bool", key),
                _ => return None,
            })
        }
        _ => None,
    }
}

/// Human-readable signature detail for a recognized std `String` method.
/// Returns `None` when the method is not part of the shimmed surface.
fn str_method_detail(method: &str) -> Option<String> {
    Some(match method {
        "len" => "fn len(&self) -> i64".to_string(),
        "is_empty" => "fn is_empty(&self) -> bool".to_string(),
        "contains" => "fn contains(&self, pat: &str) -> bool".to_string(),
        "starts_with" => "fn starts_with(&self, pat: &str) -> bool".to_string(),
        "ends_with" => "fn ends_with(&self, pat: &str) -> bool".to_string(),
        "to_uppercase" => "fn to_uppercase(&self) -> String".to_string(),
        "to_lowercase" => "fn to_lowercase(&self) -> String".to_string(),
        "trim" => "fn trim(&self) -> String".to_string(),
        "trim_start" => "fn trim_start(&self) -> String".to_string(),
        "trim_end" => "fn trim_end(&self) -> String".to_string(),
        "replace" => "fn replace(&self, from: &str, to: &str) -> String".to_string(),
        "repeat" => "fn repeat(&self, n: usize) -> String".to_string(),
        "to_string" | "to_owned" | "clone" => "fn to_string(&self) -> String".to_string(),
        "chars" => "fn chars(&self) -> Vec<String>".to_string(),
        "split" => "fn split(&self, pat: &str) -> Vec<String>".to_string(),
        _ => return None,
    })
}

/// All completable members of a built-in collection / string type, for member
/// completion (`v.` / `s.` / `map.`). Built-ins expose no fields, so every entry
/// is a `Method`; detail text reuses the same signatures as hover. Names are
/// kept alphabetical so the emitted list is already ordered.
fn builtin_members(ty: &Ty) -> Vec<CompletionMember> {
    let names: &[&str] = match ty {
        Ty::Vec(_) => &["get", "len", "pop", "push", "set"],
        Ty::Map(_, _) => &["contains_key", "get", "insert", "len", "remove"],
        Ty::Str => &[
            "chars",
            "clone",
            "contains",
            "ends_with",
            "is_empty",
            "len",
            "repeat",
            "replace",
            "split",
            "starts_with",
            "to_lowercase",
            "to_owned",
            "to_string",
            "to_uppercase",
            "trim",
            "trim_end",
            "trim_start",
        ],
        _ => return Vec::new(),
    };
    names
        .iter()
        .filter_map(|&m| {
            let detail = if matches!(ty, Ty::Str) {
                str_method_detail(m)
            } else {
                builtin_method_detail(ty, m)
            }?;
            Some(CompletionMember {
                name: m.to_string(),
                kind: MemberKind::Method,
                detail,
            })
        })
        .collect()
}

/// Return type of a recognized std `String` method, or `None` if the method is
/// not part of the shimmed surface (so it stays `Unknown`, never an error).
/// Codegen routes recognized methods through the `rt.str` runtime helpers.
fn str_method_ret(method: &str) -> Option<Ty> {
    Some(match method {
        "len" => Ty::I64,
        "is_empty" | "contains" | "starts_with" | "ends_with" => Ty::Bool,
        "to_uppercase" | "to_lowercase" | "trim" | "trim_start" | "trim_end" | "replace"
        | "repeat" | "to_string" | "to_owned" | "clone" => Ty::Str,
        "chars" | "split" => Ty::Vec(Box::new(Ty::Str)),
        _ => return None,
    })
}

/// Type-derived facts the backend needs: the sets of `/` and `%` expressions
/// whose operands are both `i64`, so codegen can emit truncating integer helpers
/// (`rt.idiv`/`rt.irem`) that match Rust rather than Lua's floored `//`/`%`.
/// Keyed by `(span.start, span.len)`, which uniquely identifies each subexpr.
#[derive(Default)]
pub struct TypeInfo {
    int_divs: std::collections::HashSet<(usize, usize)>,
    int_rems: std::collections::HashSet<(usize, usize)>,
    /// Method-call expressions whose receiver is a `String` and whose method is
    /// a recognized std string method, so codegen routes them through `rt.str`.
    str_methods: std::collections::HashSet<(usize, usize)>,
    /// `+` expressions whose operands are both `String`, so codegen emits Lua
    /// string concatenation (`..`) instead of arithmetic.
    str_concats: std::collections::HashSet<(usize, usize)>,
    /// First closure encountered during type checking. The compiler entry point
    /// uses this as a temporary backend gate until fused closure codegen lands.
    first_closure: Option<SourceRange>,
    iter_plans: HashMap<(u32, usize, usize), IterPlan>,
}

impl TypeInfo {
    pub fn is_int_div(&self, start: usize, len: usize) -> bool {
        self.int_divs.contains(&(start, len))
    }

    pub fn is_int_rem(&self, start: usize, len: usize) -> bool {
        self.int_rems.contains(&(start, len))
    }

    pub fn is_str_method(&self, start: usize, len: usize) -> bool {
        self.str_methods.contains(&(start, len))
    }

    pub fn is_str_concat(&self, start: usize, len: usize) -> bool {
        self.str_concats.contains(&(start, len))
    }

    pub fn first_closure(&self) -> Option<SourceRange> {
        self.first_closure
    }

    pub fn iter_plan(&self, file: u32, start: usize, len: usize) -> Option<&IterPlan> {
        self.iter_plans.get(&(file, start, len))
    }

    pub fn iter_plans(&self) -> impl Iterator<Item = &IterPlan> {
        self.iter_plans.values()
    }

    pub fn pending_iter_codegen(&self) -> Option<SourceRange> {
        self.iter_plans
            .values()
            .filter(|plan| {
                !plan.adapters.is_empty()
                    || plan.consumer != IterConsumerKind::For
                    || !matches!(plan.source.kind, IterSourceKind::ExclusiveRange
                        | IterSourceKind::InclusiveRange
                        | IterSourceKind::Vec)
            })
            .map(|plan| plan.consumer_range)
            .min_by_key(|range| (range.file, range.start))
    }
}

/// Kind of a resolved member access, for the LSP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberKind {
    Field,
    Method,
}

/// One resolved member access (`x.field` or `x.method()`), keyed by the byte
/// span of the member identifier at the **use site**. Pure data (no AST / no
/// rowan) so the LSP crate can consume it directly for go-to-def / hover.
#[derive(Debug, Clone)]
pub struct MemberTarget {
    /// File id + byte span of the member identifier at the use site.
    pub member_file: u32,
    pub member_start: usize,
    pub member_len: usize,
    /// File id + byte span of the member's definition site.
    pub target_file: u32,
    pub target_start: usize,
    pub target_len: usize,
    /// Human-readable detail for hover (e.g. `x: f64`, `fn dist(&self) -> f64`).
    pub detail: String,
    pub kind: MemberKind,
}

/// Member-access resolutions produced by type-checking. Only accesses whose
/// receiver is a concrete user type (`struct`/`enum`) with the member actually
/// declared are recorded; `Vec`/`HashMap`/`String`/extern/generic/unknown
/// receivers are omitted (zero false positives, matching the checker's
/// conservative stance).
#[derive(Debug, Clone, Default)]
pub struct MemberIndex {
    hits: Vec<MemberTarget>,
}

impl MemberIndex {
    /// The member-access resolution in file `file` whose member-identifier span
    /// contains `offset`, if any.
    pub fn at(&self, file: u32, offset: usize) -> Option<&MemberTarget> {
        self.hits.iter().find(|h| {
            h.member_file == file
                && offset >= h.member_start
                && offset < h.member_start + h.member_len
        })
    }

    pub fn hits(&self) -> &[MemberTarget] {
        &self.hits
    }

    pub fn len(&self) -> usize {
        self.hits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hits.is_empty()
    }
}

/// Type-check `prog` and return the [`MemberIndex`] for LSP member resolution.
/// Diagnostics are discarded here (the LSP fetches those via `check_diags`).
pub fn member_index(prog: &Program) -> MemberIndex {
    let mut tc = Tc::new(prog);
    tc.run(prog);
    MemberIndex { hits: tc.members }
}

/// One completable member (field or method) of a user type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionMember {
    pub name: String,
    pub kind: MemberKind,
    /// Detail text: field `x: f64`, method `fn dist(&self) -> f64`.
    pub detail: String,
}

/// Member-completion catalog: type name → fields + methods.
/// Only user-defined types with at least one field or method are present; types
/// with the same simple name in multiple modules are already dropped by
/// `Tc::new`, matching the type checker's conservative stance.
#[derive(Debug, Clone, Default)]
pub struct TypeMembers {
    map: HashMap<String, Vec<CompletionMember>>,
}

impl TypeMembers {
    /// Members of `type_name` (fields then methods, each alphabetical). Empty
    /// slice when the type is unknown or has no members.
    pub fn get(&self, type_name: &str) -> &[CompletionMember] {
        self.map.get(type_name).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Assemble the [`TypeMembers`] catalog for `prog`. Only needs the tables built
/// by `Tc::new` — no `infer` pass, so it's cheap.
pub fn type_members(prog: &Program) -> TypeMembers {
    Tc::new(prog).build_type_members()
}

/// The receiver of a member access, typed to a concrete user type. Keyed by the
/// receiver expression's byte span; used by member completion to answer "what
/// are `recv.`'s fields/methods" when the member name isn't typed yet.
#[derive(Debug, Clone)]
pub struct ReceiverType {
    pub recv_file: u32,
    pub recv_start: usize,
    pub recv_len: usize,
    pub type_name: String,
}

/// Receiver-type index for member-completion lookups. Keyed by the receiver
/// expression's end offset (`recv_start + recv_len`), which is the stable anchor
/// for disambiguating nesting levels in chains like `a.b.c`.
#[derive(Debug, Clone, Default)]
pub struct ReceiverIndex {
    hits: Vec<ReceiverType>,
}

impl ReceiverIndex {
    /// The receiver in `file` whose span **ends** at `end` (i.e. the token just
    /// before the member `.`). End is the stable anchor: for `a.b.c`, `a` and
    /// `a.b` share a start but end differently, so `end` disambiguates the
    /// nesting level. Matches the CST receiver node's `text_range().end()`.
    pub fn at_end(&self, file: u32, end: usize) -> Option<&ReceiverType> {
        self.hits
            .iter()
            .find(|r| r.recv_file == file && r.recv_start + r.recv_len == end)
    }

    pub fn hits(&self) -> &[ReceiverType] {
        &self.hits
    }

    pub fn len(&self) -> usize {
        self.hits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hits.is_empty()
    }
}

/// Run type-checking on `prog` and return both the member-completion catalog
/// and the receiver-type index in one pass.
pub fn member_completion(prog: &Program) -> (TypeMembers, ReceiverIndex) {
    let mut tc = Tc::new(prog);
    tc.run(prog);
    let mut members = tc.build_type_members();
    // Merge built-in receiver catalogs (`Vec<..>` / `HashMap<..>` / `String`);
    // user types already own their key, so built-ins never overwrite them.
    for (key, list) in tc.builtin_type_members {
        members.map.entry(key).or_insert(list);
    }
    (members, ReceiverIndex { hits: tc.receivers })
}

/// The inferred type of a local binding (`let`, `for` variable, or function
/// parameter), keyed by the binding-name identifier's byte span. Powers richer
/// LSP hover for locals (e.g. `let mut i: i64` instead of a bare `local i`).
#[derive(Debug, Clone)]
pub struct BindingType {
    pub file: u32,
    pub name_start: usize,
    pub name_len: usize,
    /// Ready-to-display hover text, e.g. `let mut i: i64`, `n: i64`.
    pub display: String,
}

/// Binding-type index for LSP local-variable hover. Keyed by the binding-name
/// span; only bindings whose inferred type is *not* `Unknown` are recorded (so
/// an un-inferable local degrades to the plain `local <name>` hover).
#[derive(Debug, Clone, Default)]
pub struct BindingTypes {
    hits: Vec<BindingType>,
}

impl BindingTypes {
    /// The binding in `file` whose name span contains `offset`, if any.
    pub fn at(&self, file: u32, offset: usize) -> Option<&BindingType> {
        self.hits.iter().find(|b| {
            b.file == file && offset >= b.name_start && offset < b.name_start + b.name_len
        })
    }

    pub fn hits(&self) -> &[BindingType] {
        &self.hits
    }

    pub fn len(&self) -> usize {
        self.hits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hits.is_empty()
    }
}

/// Type-check `prog` and return the [`BindingTypes`] index for LSP local hover.
pub fn binding_types(prog: &Program) -> BindingTypes {
    let mut tc = Tc::new(prog);
    tc.run(prog);
    BindingTypes { hits: tc.bindings }
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
fn builtin_payload(path: &[String], ty: &Ty) -> Option<Vec<Ty>> {
    match (path.last()?.as_str(), ty) {
        ("Some", Ty::Option(inner)) => Some(vec![(**inner).clone()]),
        ("Ok", Ty::Result(ok, _)) => Some(vec![(**ok).clone()]),
        ("Err", Ty::Result(_, err)) => Some(vec![(**err).clone()]),
        _ => None,
    }
}

/// Depth-first list of every item including those nested in modules (the `Mod`
/// items themselves are included but ignored by the collection passes).
fn flatten_items<'p>(items: &'p [Item], out: &mut Vec<&'p Item>) {
    for it in items {
        out.push(it);
        if let Item::Mod(m) = it {
            flatten_items(&m.items, out);
        }
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
    let mut tc = Tc::new(prog);
    tc.run(prog);
    tc.errs
}

pub fn check(prog: &Program, files: &[String]) -> Result<TypeInfo, String> {
    let mut tc = Tc::new(prog);
    tc.run(prog);
    if tc.errs.is_empty() {
        Ok(TypeInfo {
            int_divs: tc.int_divs,
            int_rems: tc.int_rems,
            str_methods: tc.str_methods,
            str_concats: tc.str_concats,
            first_closure: tc.first_closure,
            iter_plans: tc.iter_plans,
        })
    } else {
        Err(render_all(&tc.errs, files))
    }
}

/// Format an AST `Type` node as a human-readable string for hover details.
fn type_display(t: &Type) -> String {
    match t {
        Type::Unit => "()".into(),
        Type::Ref { mutable, inner } => {
            if *mutable {
                format!("&mut {}", type_display(inner))
            } else {
                format!("&{}", type_display(inner))
            }
        }
        Type::Path { name, args } => {
            if args.is_empty() {
                name.clone()
            } else {
                let args: Vec<String> = args.iter().map(type_display).collect();
                format!("{}<{}>", name, args.join(", "))
            }
        }
    }
}

/// Build a signature display string from a method declaration.
fn method_detail(name: &str, has_self: bool, params: &[Param], ret: Option<&Type>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if has_self {
        parts.push("&self".to_string());
    }
    for p in params {
        parts.push(format!("{}: {}", p.name, type_display(&p.ty)));
    }
    let ret_str = match ret {
        Some(t) => format!(" -> {}", type_display(t)),
        None => String::new(),
    };
    format!("fn {}({}){}", name, parts.join(", "), ret_str)
}

struct Tc {
    fns: HashMap<String, FnSig>,
    /// Fully-qualified path (`a::b::f`) -> signature, for free/extern functions
    /// in nested modules and `.ruai` declaration files. Keys are unambiguous by
    /// construction, so qualified call sites are checked even when the same
    /// simple name is dropped from `fns`.
    qual_fns: HashMap<String, FnSig>,
    /// struct name -> [(field_name, field_type, name_definition_span)]
    structs: HashMap<String, Vec<(String, Ty, SourceRange)>>,
    enums: std::collections::HashSet<String>,
    /// type name -> (method name -> signature). Populated from `impl` blocks
    /// (inherent + trait impls) plus inherited trait default methods.
    methods: HashMap<String, HashMap<String, FnSig>>,
    /// type name -> (method name -> (definition span, signature detail))
    /// Populated from `impl` blocks (inherent + trait impls) plus inherited
    /// trait defaults. Detail is human-readable like `fn dist(&self) -> f64`.
    method_defs: HashMap<String, HashMap<String, (SourceRange, String)>>,
    /// trait name -> (method name -> signature). Used to resolve method calls on
    /// values typed as a generic parameter via its trait bounds.
    trait_methods: HashMap<String, HashMap<String, FnSig>>,
    /// trait name -> (method name -> (definition span, signature detail))
    trait_method_defs: HashMap<String, HashMap<String, (SourceRange, String)>>,
    /// type name -> set of trait names it implements (`impl Trait for Type`).
    /// Used at call sites to verify a concrete type argument satisfies the
    /// declared trait bounds of a generic function.
    impls: HashMap<String, std::collections::HashSet<String>>,
    /// Generic parameters in scope for the function being checked: name -> the
    /// trait names it is bounded by. Set on entry to each `check_fn`.
    gen_bounds: HashMap<String, Vec<String>>,
    scopes: Vec<HashMap<String, Ty>>,
    mutable_scopes: Vec<std::collections::HashSet<String>>,
    /// Scope-count boundaries for nested closures. Assignments resolving below
    /// the innermost boundary mutate an enclosing capture.
    closure_boundaries: Vec<usize>,
    closure_mutable_capture_allowed: Vec<bool>,
    /// Explicit return expression types for the currently inferred closure.
    closure_returns: Vec<Vec<Ty>>,
    errs: Vec<Diag>,
    /// `(span.start, span.len)` of every `i64 / i64` division expression.
    int_divs: std::collections::HashSet<(usize, usize)>,
    /// `(span.start, span.len)` of every `i64 % i64` remainder expression.
    int_rems: std::collections::HashSet<(usize, usize)>,
    /// `(span.start, span.len)` of recognized `String` method calls.
    str_methods: std::collections::HashSet<(usize, usize)>,
    /// `(span.start, span.len)` of `String + String` concatenations.
    str_concats: std::collections::HashSet<(usize, usize)>,
    /// Resolved member accesses (`x.field` / `x.method()`) for the LSP.
    members: Vec<MemberTarget>,
    /// Receiver types at member-access sites (for completion `x.`).
    receivers: Vec<ReceiverType>,
    /// Inferred types of local bindings (`let`/`for`/param) for LSP hover.
    bindings: Vec<BindingType>,
    /// User enum variant payloads: enum name → variant name → resolved payload.
    /// Types pattern bindings in `match`/`if let` arms over user enums.
    enum_variants: HashMap<String, HashMap<String, VariantPayload>>,
    /// Built-in receiver member catalogs (`Vec<i64>` / `HashMap<..>` / `String`
    /// → their methods), keyed by the receiver type's display name. Populated
    /// lazily as built-in receivers are encountered; merged into `TypeMembers`
    /// so `v.` / `s.` completion lists built-in methods.
    builtin_type_members: HashMap<String, Vec<CompletionMember>>,
    first_closure: Option<SourceRange>,
    iter_plans: HashMap<(u32, usize, usize), IterPlan>,
}

impl Tc {
    fn new(prog: &Program) -> Tc {
        // Types/fns/methods are collected by simple name across all scopes
        // (root + nested modules), so flatten the item tree up front.
        let mut flat: Vec<&Item> = Vec::new();
        flatten_items(&prog.items, &mut flat);

        // Count how many scopes define each top-level name. A name defined in
        // more than one scope is ambiguous by simple name and is dropped from the
        // registries below, so every check on it degrades to `Unknown` (zero
        // false positives when the same type/fn/trait name lives in two modules).
        let mut name_counts: HashMap<String, usize> = HashMap::new();
        for it in flat.iter().copied() {
            let n = match it {
                Item::Struct(s) => Some(&s.name),
                Item::Enum(e) => Some(&e.name),
                Item::Fn(f) => Some(&f.name),
                Item::Trait(t) => Some(&t.name),
                _ => None,
            };
            if let Some(n) = n {
                *name_counts.entry(n.clone()).or_insert(0) += 1;
            }
        }

        let mut structs: HashMap<String, Vec<(String, Ty, SourceRange)>> = HashMap::new();
        let mut enums = std::collections::HashSet::new();
        // First pass: collect type names so `ty_of` can resolve Named types.
        for item in flat.iter().copied() {
            match item {
                Item::Struct(s) => {
                    structs.insert(s.name.clone(), Vec::new());
                }
                Item::Enum(e) => {
                    enums.insert(e.name.clone());
                }
                _ => {}
            }
        }
        let mut tc = Tc {
            fns: HashMap::new(),
            qual_fns: HashMap::new(),
            structs: structs.clone(),
            enums,
            methods: HashMap::new(),
            method_defs: HashMap::new(),
            trait_methods: HashMap::new(),
            trait_method_defs: HashMap::new(),
            impls: HashMap::new(),
            gen_bounds: HashMap::new(),
            scopes: Vec::new(),
            mutable_scopes: Vec::new(),
            closure_boundaries: Vec::new(),
            closure_mutable_capture_allowed: Vec::new(),
            closure_returns: Vec::new(),
            errs: Vec::new(),
            int_divs: std::collections::HashSet::new(),
            int_rems: std::collections::HashSet::new(),
            str_methods: std::collections::HashSet::new(),
            str_concats: std::collections::HashSet::new(),
            members: Vec::new(),
            receivers: Vec::new(),
            bindings: Vec::new(),
            enum_variants: HashMap::new(),
            builtin_type_members: HashMap::new(),
            first_closure: None,
            iter_plans: HashMap::new(),
        };
        // Second pass: field types, free-fn signatures, and trait method sigs
        // (now that names resolve).
        let mut traits: HashMap<String, HashMap<String, (FnSig, bool)>> = HashMap::new();
        for item in flat.iter().copied() {
            match item {
                Item::Struct(s) => {
                    let fields = s
                        .fields
                        .iter()
                        .map(|f| (f.name.clone(), tc.ty_of(&f.ty), f.name_span))
                        .collect();
                    tc.structs.insert(s.name.clone(), fields);
                }
                Item::Enum(e) => {
                    // Resolve each variant's payload types. The enum's own
                    // generics map to `Ty::Generic` while resolving (so a
                    // `Circle(T)` payload stays abstract rather than Unknown).
                    tc.set_gen_bounds(&e.generics);
                    let mut variants = HashMap::new();
                    for v in &e.variants {
                        let payload = match &v.kind {
                            VariantKind::Unit => continue,
                            VariantKind::Tuple(tys) => {
                                VariantPayload::Tuple(tys.iter().map(|t| tc.ty_of(t)).collect())
                            }
                            VariantKind::Struct(fields) => VariantPayload::Struct(
                                fields
                                    .iter()
                                    .map(|f| (f.name.clone(), tc.ty_of(&f.ty)))
                                    .collect(),
                            ),
                        };
                        variants.insert(v.name.clone(), payload);
                    }
                    tc.gen_bounds.clear();
                    tc.enum_variants.insert(e.name.clone(), variants);
                }
                Item::Fn(f) => {
                    // Map the fn's own generic params to `Ty::Generic` while
                    // building its signature, so call sites can see which
                    // parameters are generic (and check their bounds).
                    tc.set_gen_bounds(&f.generics);
                    let mut sig = tc.sig_of(&f.params, f.ret.as_ref());
                    tc.gen_bounds.clear();
                    sig.generics = f.generics.clone();
                    tc.fns.insert(f.name.clone(), sig);
                }
                Item::Trait(t) => {
                    let mut ms = HashMap::new();
                    let mut sigs = HashMap::new();
                    for tm in &t.methods {
                        // Map the trait's and method's own generics to
                        // `Ty::Generic` while building the signature, so param /
                        // return types referencing them stay abstract.
                        let gens = merge_generics(&t.generics, &tm.generics);
                        tc.set_gen_bounds(&gens);
                        let sig = tc.sig_of(&tm.params, tm.ret.as_ref());
                        tc.gen_bounds.clear();
                        sigs.insert(
                            tm.name.clone(),
                            FnSig {
                                params: sig.params.clone(),
                                ret: sig.ret.clone(),
                                // Only the method-level generics are inferable
                                // from a call's arguments (trait-level generics
                                // are fixed by the impl), so store just those.
                                generics: tm.generics.clone(),
                            },
                        );
                        ms.insert(tm.name.clone(), (sig, tm.default.is_some()));
                        // Record definition span and signature detail.
                        let detail = method_detail(
                            &tm.name,
                            tm.has_self,
                            &tm.params,
                            tm.ret.as_ref(),
                        );
                        tc.trait_method_defs
                            .entry(t.name.clone())
                            .or_default()
                            .insert(tm.name.clone(), (tm.name_span, detail));
                    }
                    tc.trait_methods.insert(t.name.clone(), sigs);
                    traits.insert(t.name.clone(), ms);
                }
                _ => {}
            }
        }
        // Third pass: methods from impl blocks + inherited trait defaults.
        for item in flat.iter().copied() {
            if let Item::Impl(im) = item {
                // Record `impl Trait for Type` so call sites can check bounds.
                if let Some(tr) = &im.trait_name {
                    tc.impls
                        .entry(im.type_name.clone())
                        .or_default()
                        .insert(tr.clone());
                }
                // Build a local table first (calls `ty_of`), then merge, to avoid
                // holding a mutable borrow of `tc.methods` across `ty_of`.
                let mut local: HashMap<String, FnSig> = HashMap::new();
                let mut overridden = std::collections::HashSet::new();
                for m in &im.methods {
                    overridden.insert(m.name.clone());
                    // A method's effective generics are the impl's plus its own;
                    // map them to `Ty::Generic` while building the signature so
                    // call sites can infer type arguments and check their bounds.
                    let gens = merge_generics(&im.generics, &m.generics);
                    tc.set_gen_bounds(&gens);
                    let mut sig = tc.sig_of(&m.params, m.ret.as_ref());
                    tc.gen_bounds.clear();
                    sig.generics = gens;
                    local.insert(m.name.clone(), sig);
                }
                if let Some(tr) = &im.trait_name {
                    if let Some(tms) = traits.get(tr) {
                        for (mname, (sig, has_default)) in tms {
                            if *has_default && !overridden.contains(mname) {
                                local.entry(mname.clone()).or_insert(FnSig {
                                    params: sig.params.clone(),
                                    ret: sig.ret.clone(),
                                    generics: Vec::new(),
                                });
                            }
                        }
                    }
                }
                let table = tc.methods.entry(im.type_name.clone()).or_default();
                for (k, v) in local {
                    table.insert(k, v);
                }
                // Record definition spans and signature details for all methods
                // attached to this type (explicit + inherited defaults).
                let mut mdefs: HashMap<String, (SourceRange, String)> = HashMap::new();
                for m in &im.methods {
                    let detail = method_detail(
                        &m.name,
                        m.has_self,
                        &m.params,
                        m.ret.as_ref(),
                    );
                    mdefs.insert(m.name.clone(), (m.name_span, detail));
                }
                // Inherited trait default methods: look up their definition spans
                // from the trait's own table.
                if let Some(tr) = &im.trait_name {
                    if let Some(tms) = traits.get(tr) {
                        for (mname, (_, has_default)) in tms {
                            if *has_default && !mdefs.contains_key(mname) {
                                if let Some(tm_table) = tc.trait_method_defs.get(tr) {
                                    if let Some(&(sp, ref detail)) = tm_table.get(mname) {
                                        mdefs.insert(mname.clone(), (sp, detail.clone()));
                                    }
                                }
                            }
                        }
                    }
                }
                tc.method_defs.insert(im.type_name.clone(), mdefs);
            }
        }
        // Register every free/extern function under its fully-qualified path so
        // module-qualified call sites (`a::f(..)`, `moon::log(..)`) are checked.
        tc.collect_qual_fns(&prog.items, &[]);
        // Drop cross-scope-ambiguous names so their checks degrade to `Unknown`.
        for (name, count) in &name_counts {
            if *count > 1 {
                tc.structs.remove(name);
                tc.enums.remove(name);
                tc.enum_variants.remove(name);
                tc.fns.remove(name);
                tc.trait_methods.remove(name);
                tc.trait_method_defs.remove(name);
                tc.methods.remove(name);
                tc.method_defs.remove(name);
            }
        }
        tc
    }

    /// Build the member-completion catalog from the collected struct-field and
    /// method tables. Fields come from `structs`; methods from `method_defs`
    /// (inherent + trait-impl + inherited-default methods).
    fn build_type_members(&self) -> TypeMembers {
        let mut map: HashMap<String, Vec<CompletionMember>> = HashMap::new();
        for (ty, fields) in &self.structs {
            let entry = map.entry(ty.clone()).or_default();
            for (fname, fty, _span) in fields {
                entry.push(CompletionMember {
                    name: fname.clone(),
                    kind: MemberKind::Field,
                    detail: format!("{}: {}", fname, fty.name()),
                });
            }
        }
        for (ty, methods) in &self.method_defs {
            let entry = map.entry(ty.clone()).or_default();
            for (mname, (_span, detail)) in methods {
                entry.push(CompletionMember {
                    name: mname.clone(),
                    kind: MemberKind::Method,
                    detail: detail.clone(),
                });
            }
        }
        // Deterministic: fields before methods, each alphabetical (HashMap order
        // is not stable, so tests need this).
        for members in map.values_mut() {
            let rank = |k: MemberKind| match k {
                MemberKind::Field => 0u8,
                MemberKind::Method => 1u8,
            };
            members.sort_by(|a, b| {
                rank(a.kind)
                    .cmp(&rank(b.kind))
                    .then_with(|| a.name.cmp(&b.name))
            });
        }
        // Drop fieldless + methodless structs (nothing to complete).
        map.retain(|_, v| !v.is_empty());
        TypeMembers { map }
    }

    /// Record `span`'s receiver type for member completion. Concrete user types
    /// (`Ty::Named`) key into `TypeMembers`; built-in `Vec`/`HashMap`/`String`
    /// receivers key into a lazily-built built-in catalog (so `v.` / `s.` list
    /// their methods). Extern/generic/unknown receivers are skipped (zero false
    /// positives).
    fn record_receiver(&mut self, span: SourceRange, ty: &Ty) {
        let type_name = match ty {
            Ty::Named(name) => name.clone(),
            Ty::Vec(_) | Ty::Map(_, _) | Ty::Str => {
                let key = ty.name();
                self.builtin_type_members
                    .entry(key.clone())
                    .or_insert_with(|| builtin_members(ty));
                key
            }
            _ => return,
        };
        self.receivers.push(ReceiverType {
            recv_file: span.file,
            recv_start: span.start,
            recv_len: span.len,
            type_name,
        });
    }

    fn sig_of(&self, params: &[Param], ret: Option<&Type>) -> FnSig {
        FnSig {
            params: params.iter().map(|p| self.ty_of(&p.ty)).collect(),
            ret: ret.map(|t| self.ty_of(t)).unwrap_or(Ty::Unit),
            generics: Vec::new(),
        }
    }

    /// Recursively register free `fn`s and `extern` functions under their
    /// fully-qualified path (`prefix::name`). Variadic extern fns are skipped so
    /// their arity is never (wrongly) enforced.
    fn collect_qual_fns(&mut self, items: &[Item], prefix: &[String]) {
        let qual = |name: &str| {
            let mut p = prefix.to_vec();
            p.push(name.to_string());
            p.join("::")
        };
        for it in items {
            match it {
                Item::Fn(f) => {
                    self.set_gen_bounds(&f.generics);
                    let mut sig = self.sig_of(&f.params, f.ret.as_ref());
                    self.gen_bounds.clear();
                    sig.generics = f.generics.clone();
                    self.qual_fns.insert(qual(&f.name), sig);
                }
                Item::Extern(b) => {
                    for ef in &b.fns {
                        if ef.variadic {
                            continue;
                        }
                        let sig = self.sig_of(&ef.params, ef.ret.as_ref());
                        self.qual_fns.insert(qual(&ef.name), sig);
                    }
                }
                Item::Mod(m) => {
                    let mut p = prefix.to_vec();
                    p.push(m.name.clone());
                    self.collect_qual_fns(&m.items, &p);
                }
                _ => {}
            }
        }
    }

    /// Install `name -> bounds` for a set of generic parameters as the current
    /// generic scope (used by `ty_of` and bound-based method resolution).
    fn set_gen_bounds(&mut self, generics: &[GenericParam]) {
        self.gen_bounds = generics
            .iter()
            .map(|g| (g.name.clone(), g.bounds.clone()))
            .collect();
    }

    fn ty_of(&self, t: &Type) -> Ty {
        match t {
            Type::Unit => Ty::Unit,
            Type::Ref { inner, .. } => self.ty_of(inner),
            Type::Path { name, args } => {
                let arg = |i: usize| args.get(i).map(|t| self.ty_of(t)).unwrap_or(Ty::Unknown);
                match name.as_str() {
                    "i8" | "i16" | "i32" | "i64" | "isize" | "u8" | "u16" | "u32" | "u64"
                    | "usize" => Ty::I64,
                    "f32" | "f64" => Ty::F64,
                    "bool" => Ty::Bool,
                    "String" | "str" => Ty::Str,
                    "Vec" => Ty::Vec(Box::new(arg(0))),
                    "Option" => Ty::Option(Box::new(arg(0))),
                    "Result" => Ty::Result(Box::new(arg(0)), Box::new(arg(1))),
                    "HashMap" => Ty::Map(Box::new(arg(0)), Box::new(arg(1))),
                    "Box" => arg(0), // transparent
                    _ if self.gen_bounds.contains_key(name) => Ty::Generic(name.clone()),
                    _ if self.structs.contains_key(name) => Ty::Named(name.clone()),
                    _ if self.enums.contains(name) => Ty::Named(name.clone()),
                    // Unknown external types.
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

    /// Record a local binding's inferred type for LSP hover, keyed by its
    /// name span. `prefix` is the leading text (e.g. `let mut i`, `n`); the
    /// type name is appended as `: <ty>`. Bindings with an unknown type or an
    /// empty span (parser could not locate the name) are skipped so hover
    /// degrades cleanly to the plain `local <name>`.
    fn record_binding(&mut self, span: &SourceRange, ty: &Ty, prefix: &str) {
        if matches!(ty, Ty::Unknown) || span.len == 0 {
            return;
        }
        self.bindings.push(BindingType {
            file: span.file,
            name_start: span.start,
            name_len: span.len,
            display: format!("{prefix}: {}", ty.name()),
        });
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
        self.scopes.iter().enumerate().rev().find_map(|(index, scope)| {
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
            ExprKind::Path(segs) if segs.len() == 1 => {
                self.lookup(&segs[0]).unwrap_or(Ty::Unknown)
            }
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
        self.errs
            .push(Diag::new(sp.file, sp.start, sp.len, sp.line, msg));
    }

    // --- driver ------------------------------------------------------------

    fn run(&mut self, prog: &Program) {
        for item in &prog.items {
            match item {
                Item::Fn(f) => self.check_fn(&f.generics, &f.params, f.ret.as_ref(), &f.body, None),
                Item::Impl(im) => {
                    let self_ty = Ty::Named(im.type_name.clone());
                    for m in &im.methods {
                        let st = if m.has_self { Some(self_ty.clone()) } else { None };
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
                Item::Mod(m) => self.check_mod(m),
                _ => {}
            }
        }
    }

    /// Check the bodies of a module's `fn`/`impl`/`trait` items (recursing into
    /// nested modules). Free-fn signatures inside modules aren't registered
    /// globally, so cross-module `::` calls resolve to `Unknown` (conservatively
    /// unchecked, never a false positive); methods are keyed by simple type name.
    fn check_mod(&mut self, m: &ModDecl) {
        for it in &m.items {
            match it {
                Item::Fn(f) => self.check_fn(&f.generics, &f.params, f.ret.as_ref(), &f.body, None),
                Item::Impl(im) => {
                    let self_ty = Ty::Named(im.type_name.clone());
                    for me in &im.methods {
                        let st = if me.has_self { Some(self_ty.clone()) } else { None };
                        let gens = merge_generics(&im.generics, &me.generics);
                        self.check_fn(&gens, &me.params, me.ret.as_ref(), &me.body, st);
                    }
                }
                Item::Trait(t) => {
                    for tm in &t.methods {
                        if let Some(b) = &tm.default {
                            let st = if tm.has_self { Some(Ty::Unknown) } else { None };
                            self.check_fn(&t.generics, &tm.params, tm.ret.as_ref(), b, st);
                        }
                    }
                }
                Item::Mod(md) => self.check_mod(md),
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
            self.record_binding(&p.name_span, &ty, &p.name);
            self.bind(&p.name, ty);
        }
        let ret_ty = ret.map(|t| self.ty_of(t)).unwrap_or(Ty::Unit);
        let actual = self.block(body);
        if let Some(tail) = &body.tail {
            self.reject_iter_escape(&actual, tail.span);
        }
        // Only check a concrete, non-unit declared return against a concrete tail.
        if let Some(tail) = &body.tail {
            if ret_ty.is_concrete()
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
            Stmt::Let { name, name_span, mutable, ty, init } => {
                let init_ty = if let ExprKind::Closure {
                    params,
                    ret,
                    body,
                } = &init.kind
                {
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
                self.reject_iter_escape(&bind_ty, init.span);
                // For an inferred (un-annotated) empty collection, fill unknown
                // element/key/value slots from later `push`/`insert` calls in
                // this block (`let mut m = HashMap::new(); m.insert("a", 1)`).
                let bind_ty = if ty.is_none() {
                    self.refine_collection_from_usage(name, &bind_ty, rest)
                } else {
                    bind_ty
                };
                let prefix = if *mutable {
                    format!("let mut {name}")
                } else {
                    format!("let {name}")
                };
                self.record_binding(name_span, &bind_ty, &prefix);
                self.bind_mutability(name, bind_ty, *mutable);
            }
            Stmt::Expr(e) => {
                let ty = self.infer(e);
                self.reject_iter_escape(&ty, e.span);
            }
            Stmt::Return(Some(e)) => {
                let ty = self.infer(e);
                self.reject_iter_escape(&ty, e.span);
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
                self.block(body);
            }
            Stmt::Loop { body } => {
                self.block(body);
            }
            Stmt::For { var, var_span, iter, body } => {
                let iter_ty = self.infer(iter);
                let elem = match iter_ty {
                    Ty::Iter(item, draft) => {
                        let item = *item;
                        self.finish_iter_plan(
                            &draft,
                            IterConsumerKind::For,
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
                            iter.span,
                            &item,
                            &Ty::Unit,
                        );
                        item
                    }
                    ty if ty.is_concrete() => {
                        self.err(
                            iter.span,
                            format!("type `{}` is not iterable", ty.name()),
                        );
                        Ty::Unknown
                    }
                    _ => Ty::Unknown,
                };
                self.push();
                self.record_binding(var_span, &elem, &format!("for {var}"));
                self.bind(var, elem);
                self.block(body);
                self.pop();
            }
            Stmt::WhileLet { pat, expr, body } => {
                let scrut = self.infer(expr);
                self.push();
                self.bind_pattern(pat, &scrut);
                self.block(body);
                self.pop();
            }
            Stmt::Break | Stmt::Continue => {}
        }
    }

    fn expect_bool(&mut self, ty: &Ty, sp: SourceRange, what: &str) {
        if ty.is_concrete() && *ty != Ty::Bool {
            self.err(sp, format!("{} must be `bool`, found `{}`", what, ty.name()));
        }
    }

    fn reject_iter_escape(&mut self, ty: &Ty, span: SourceRange) {
        if matches!(ty, Ty::Iter(_, _)) {
            self.err(span, "iterator escape is not supported yet".to_string());
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
                    .unwrap_or_else(|| {
                        context
                            .expected
                            .get(index)
                            .cloned()
                            .unwrap_or(Ty::Unknown)
                    })
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
            self.record_binding(
                &param.name_span,
                ty,
                &format!("closure parameter {}", param.name),
            );
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

        self.reject_iter_escape(&inferred_ret, span);

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
        consumer_range: SourceRange,
        item: &Ty,
        output: &Ty,
    ) {
        self.iter_plans.insert(
            (
                consumer_range.file,
                consumer_range.start,
                consumer_range.len,
            ),
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
        // Option<T>::map(fn) — propagate the inner type to the closure param.
        if let Ty::Option(inner) = recv {
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
        span: SourceRange,
    ) -> Option<Ty> {
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
                self.err(span, format!("iterator source `{method}` expects no arguments"));
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
        if method == "map" && args.len() == 1 {
            if let Ty::Option(inner) = recv {
                let param_ty = (**inner).clone();
                let ret_ty = match &arg_tys[0] {
                    Ty::Closure(params, ret) if params.len() == 1 => (**ret).clone(),
                    _ => Ty::Unknown,
                };
                return Some(Ty::Option(Box::new(ret_ty)));
            }
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
            ) && recv.is_concrete()
                && !matches!(recv, Ty::Named(_))
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
                    self.err(args[0].span, "iterator map argument must be a closure".to_string());
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
                        format!("iterator {method} count must be `i64`, found `{}`", count.name()),
                    );
                }
                if matches!(
                    args[0].kind,
                    ExprKind::Unary {
                        op: UnOp::Neg,
                        ..
                    }
                ) {
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
                    let target = if matches!(**item, Ty::Unknown) {
                        item
                    } else {
                        item
                    };
                    Ty::Vec(target.clone())
                };
                self.finish_iter_plan(
                    draft,
                    IterConsumerKind::CollectVec,
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
                self.finish_iter_plan(draft, consumer, span, item, &output);
                Some(output)
            }
            "collect" | "fold" | "count" | "any" | "all" | "find" => {
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
                if segs.len() == 1 {
                    self.lookup(&segs[0]).unwrap_or(Ty::Unknown)
                } else {
                    // `Enum::Variant` value. Return the parameterized enum
                    // type so that generic inference works.
                    let en = &segs[segs.len() - 2];
                    if self.enums.contains(en) {
                        match en.as_str() {
                            "Option" => Ty::Option(Box::new(Ty::Unknown)),
                            "Result" => Ty::Result(
                                Box::new(Ty::Unknown),
                                Box::new(Ty::Unknown),
                            ),
                            _ => Ty::Named(en.clone()),
                        }
                    } else {
                        Ty::Unknown
                    }
                }
            }
            ExprKind::Unary { op, expr } => {
                let t = self.infer(expr);
                match op {
                    UnOp::Neg => {
                        if t.is_concrete() && !t.is_numeric() && !matches!(t, Ty::Named(_)) {
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
                // Record `i64 / i64` and `i64 % i64` so codegen can emit the
                // truncating integer helpers (`rt.idiv`/`rt.irem`).
                if matches!(l, Ty::I64) && matches!(r, Ty::I64) {
                    if *op == BinOp::Div {
                        self.int_divs.insert((e.span.start, e.span.len));
                    } else if *op == BinOp::Rem {
                        self.int_rems.insert((e.span.start, e.span.len));
                    }
                }
                // Record `String + String` so codegen emits Lua concatenation.
                if *op == BinOp::Add && matches!(l, Ty::Str) && matches!(r, Ty::Str) {
                    self.str_concats.insert((e.span.start, e.span.len));
                }
                self.infer_binary(*op, &l, &r, sp)
            }
            ExprKind::Call { callee, args } => self.infer_call(callee, args),
            ExprKind::MethodCall {
                recv,
                method,
                type_args,
                args,
                method_span,
            } => {
                let rt = self.infer(recv);
                self.record_receiver(recv.span, &rt); // C1
                let arg_tys = self.infer_method_args(&rt, method, args);
                for (arg, ty) in args.iter().zip(&arg_tys) {
                    self.reject_iter_escape(ty, arg.span);
                }
                // Only check calls whose receiver is a known user type that
                // actually declares the method; everything else is Unknown so
                // Vec/HashMap/String/extern method calls are never flagged.
                if let Ty::Named(tname) = &rt {
                    if let Some(sig) = self.methods.get(tname).and_then(|m| m.get(method)) {
                        let params = sig.params.clone();
                        let ret = sig.ret.clone();
                        let generics = sig.generics.clone();
                        // Record the member hit (definition span + detail) for the LSP.
                        let mdef = self
                            .method_defs
                            .get(tname)
                            .and_then(|m| m.get(method))
                            .cloned();
                        if let Some((dsp, detail)) = mdef {
                            self.members.push(MemberTarget {
                                member_file: method_span.file,
                                member_start: method_span.start,
                                member_len: method_span.len,
                                target_file: dsp.file,
                                target_start: dsp.start,
                                target_len: dsp.len,
                                detail,
                                kind: MemberKind::Method,
                            });
                        }
                        self.check_method_call(tname, method, &params, &arg_tys, args, sp);
                        if !generics.is_empty() {
                            // Infer the method's type arguments from the call's
                            // arguments, verify their bounds, and substitute them
                            // into the return type.
                            let mut subst: HashMap<String, Ty> = HashMap::new();
                            for (p, a) in params.iter().zip(arg_tys.iter()) {
                                unify_generic(p, a, &mut subst);
                            }
                            let owner = format!("{}::{}", tname, method);
                            self.check_bound_satisfaction(&owner, &generics, &subst, sp);
                            return subst_ty(&ret, &subst);
                        }
                        return ret;
                    }
                }
                // Receiver typed as a generic parameter: resolve the method via
                // its trait bounds. If some bound trait declares the method, use
                // that signature; otherwise stay silent (Unknown).
                if let Ty::Generic(gname) = &rt {
                    if let Some((tname, params, ret, generics)) =
                        self.resolve_generic_method(gname, method)
                    {
                        self.check_method_call(&tname, method, &params, &arg_tys, args, sp);
                        if !generics.is_empty() {
                            // Method-level generics: infer them from the call's
                            // arguments, verify their bounds, and substitute into
                            // the return type.
                            let mut subst: HashMap<String, Ty> = HashMap::new();
                            for (p, a) in params.iter().zip(arg_tys.iter()) {
                                unify_generic(p, a, &mut subst);
                            }
                            let owner = format!("{}::{}", tname, method);
                            self.check_bound_satisfaction(&owner, &generics, &subst, sp);
                            return subst_ty(&ret, &subst);
                        }
                        return ret;
                    }
                }
                // Std `String` methods: record the call so codegen routes it
                // through `rt.str`, and yield the method's return type.
                if matches!(rt, Ty::Str) {
                    if let Some(ret) = str_method_ret(method) {
                        self.str_methods.insert((sp.start, sp.len));
                        // Record a member hit for LSP hover (no real def site).
                        if let Some(detail) = str_method_detail(method) {
                            self.members.push(MemberTarget {
                                member_file: method_span.file,
                                member_start: method_span.start,
                                member_len: method_span.len,
                                target_file: 0,
                                target_start: 0,
                                target_len: 0, // sentinel: no jump target
                                detail,
                                kind: MemberKind::Method,
                            });
                        }
                        return ret;
                    }
                }
                if let Some(ret) =
                    self.infer_iterator_method(&rt, method, type_args, &arg_tys, args, sp)
                {
                    return ret;
                }
                // Builtin methods on parameterized collection types: check the
                // element/key/value argument types against the (known) receiver
                // parameters, then infer the return type. Unmodeled methods stay
                // silently `Unknown` and are never flagged.
                self.check_builtin_method_call(&rt, method, &arg_tys, args);
                let ret = builtin_method_ret(&rt, method);
                // Record a member hit for LSP hover when this is a recognized
                // builtin (Vec/HashMap method with a real return type). Sentinel
                // target spans (0, 0) signal "no jump target" to the LSP layer.
                if ret != Ty::Unknown {
                    if let Some(detail) = builtin_method_detail(&rt, method) {
                        self.members.push(MemberTarget {
                            member_file: method_span.file,
                            member_start: method_span.start,
                            member_len: method_span.len,
                            target_file: 0,
                            target_start: 0,
                            target_len: 0, // sentinel: no jump target
                            detail,
                            kind: MemberKind::Method,
                        });
                    }
                }
                ret
            }
            ExprKind::Field {
                base,
                name,
                name_span,
            } => {
                let bt = self.infer(base);
                self.record_receiver(base.span, &bt); // C1
                if let Ty::Named(sname) = &bt {
                    // Pull the field's type + definition span out from under the
                    // immutable borrow so we can record a member hit afterwards.
                    let field = self.structs.get(sname).map(|fields| {
                        fields
                            .iter()
                            .find(|(f, _, _)| f == name)
                            .map(|(_, ft, fsp)| (ft.clone(), *fsp))
                    });
                    match field {
                        // Struct known, field found: record the member and yield its type.
                        Some(Some((ft, fsp))) => {
                            self.members.push(MemberTarget {
                                member_file: name_span.file,
                                member_start: name_span.start,
                                member_len: name_span.len,
                                target_file: fsp.file,
                                target_start: fsp.start,
                                target_len: fsp.len,
                                detail: format!("{}: {}", name, ft.name()),
                                kind: MemberKind::Field,
                            });
                            ft
                        }
                        // Struct known, field absent: report the error.
                        Some(None) => {
                            self.err(
                                sp,
                                format!("struct `{}` has no field `{}`", sname, name),
                            );
                            Ty::Unknown
                        }
                        // Named enum or not-yet-known: don't claim a field error.
                        None => Ty::Unknown,
                    }
                } else {
                    Ty::Unknown
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
                            format!("range bound must be integer-compatible, found `{}`", ty.name()),
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
            ExprKind::MacroCall { name, args } => {
                let arg_tys: Vec<Ty> = args.iter().map(|a| self.infer(a)).collect();
                for (arg, ty) in args.iter().zip(&arg_tys) {
                    self.reject_iter_escape(ty, arg.span);
                }
                match name.as_str() {
                    "format" => Ty::Str,
                    "println" | "print" | "panic" => Ty::Unit,
                    "vec" => {
                        // Element type is the join of the literal's elements.
                        let elem = arg_tys
                            .iter()
                            .fold(Ty::Unknown, |acc, t| join(&acc, t));
                        Ty::Vec(Box::new(elem))
                    }
                    _ => Ty::Unknown,
                }
            }
            ExprKind::StructLit { path, fields } => {
                for (_, field) in fields {
                    let ty = self.infer(field);
                    self.reject_iter_escape(&ty, field.span);
                }
                let name = path.last().cloned().unwrap_or_default();
                if self.structs.contains_key(&name) {
                    Ty::Named(name)
                } else if path.len() >= 2 && self.enums.contains(&path[path.len() - 2]) {
                    Ty::Named(path[path.len() - 2].clone())
                } else {
                    Ty::Unknown
                }
            }
            ExprKind::Try { expr } => {
                // `e?` unwraps a Result<T,_> or Option<T> to T.
                match self.infer(expr) {
                    Ty::Result(t, _) => *t,
                    Ty::Option(t) => *t,
                    _ => Ty::Unknown,
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
            ExprKind::Assign { target, value } => {
                self.infer(target);
                let value_ty = self.infer(value);
                self.reject_iter_escape(&value_ty, value.span);
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

    fn infer_binary(&mut self, op: BinOp, l: &Ty, r: &Ty, sp: SourceRange) -> Ty {
        use BinOp::*;
        match op {
            Add | Sub | Mul | Div | Rem => {
                // `String + String` is concatenation (codegen emits `..`).
                if op == Add && matches!(l, Ty::Str) && matches!(r, Ty::Str) {
                    return Ty::Str;
                }
                // Named types may implement operator traits (Add/Mul/...), so we
                // only flag operands that can never be arithmetic: bool / unit.
                for t in [l, r] {
                    if matches!(t, Ty::Bool | Ty::Unit) {
                        self.err(
                            sp,
                            format!("arithmetic operator applied to `{}`", t.name()),
                        );
                    }
                }
                if matches!(l, Ty::Named(_)) || matches!(r, Ty::Named(_)) {
                    Ty::Unknown // result of an overloaded operator: unknown
                } else if *l == Ty::F64 || *r == Ty::F64 {
                    Ty::F64
                } else if l.is_numeric() && r.is_numeric() {
                    Ty::I64
                } else {
                    Ty::Unknown
                }
            }
            And | Or => {
                self.expect_bool(l, sp, "operand of `&&`/`||`");
                self.expect_bool(r, sp, "operand of `&&`/`||`");
                Ty::Bool
            }
            // Comparisons/equality may be overloaded (PartialOrd/PartialEq); we
            // conservatively accept any operands and yield `bool`.
            Eq | Ne | Lt | Le | Gt | Ge => Ty::Bool,
        }
    }

    fn infer_call(&mut self, callee: &Expr, args: &[Expr]) -> Ty {
        let arg_tys: Vec<Ty> = args.iter().map(|a| self.infer(a)).collect();
        for (arg, ty) in args.iter().zip(&arg_tys) {
            self.reject_iter_escape(ty, arg.span);
        }
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
        // Option/Result constructors carry element types.
        if segs.len() == 1 {
            let a0 = || arg_tys.first().cloned().unwrap_or(Ty::Unknown);
            match segs[0].as_str() {
                "Some" => return Ty::Option(Box::new(a0())),
                "None" => return Ty::Option(Box::new(Ty::Unknown)),
                "Ok" => return Ty::Result(Box::new(a0()), Box::new(Ty::Unknown)),
                "Err" => return Ty::Result(Box::new(Ty::Unknown), Box::new(a0())),
                _ => {}
            }
        }
        // Qualified builtin constructors: Option::Some/None, Result::Ok/Err.
        if segs.len() == 2 {
            let a0 = || arg_tys.first().cloned().unwrap_or(Ty::Unknown);
            match (segs[0].as_str(), segs[1].as_str()) {
                ("Option", "Some") => return Ty::Option(Box::new(a0())),
                ("Option", "None") => return Ty::Option(Box::new(Ty::Unknown)),
                ("Result", "Ok") => return Ty::Result(Box::new(a0()), Box::new(Ty::Unknown)),
                ("Result", "Err") => return Ty::Result(Box::new(Ty::Unknown), Box::new(a0())),
                ("Vec", "new") => return Ty::Vec(Box::new(Ty::Unknown)),
                ("HashMap", "new") => {
                    return Ty::Map(Box::new(Ty::Unknown), Box::new(Ty::Unknown));
                }
                _ => {}
            }
        }
        // A local closure shadows free functions and is callable through its
        // inferred signature.
        if segs.len() == 1
            && let Some(closure @ Ty::Closure(_, _)) = self.lookup(&segs[0])
        {
            return self.check_closure_call(
                &segs[0],
                &closure,
                &arg_tys,
                args,
                callee.span,
            );
        }
        // User free function (unqualified): check arity + argument types.
        if segs.len() == 1
            && let Some(sig) = self.fns.get(&segs[0])
        {
            let (params, ret, generics) =
                (sig.params.clone(), sig.ret.clone(), sig.generics.clone());
            return self.check_free_call(&segs[0], &params, &ret, &generics, &arg_tys, args, callee.span);
        }
        // Module-qualified free/extern function (incl. `.ruai` declarations):
        // check against its fully-qualified declared signature.
        if segs.len() >= 2 {
            let key = segs.join("::");
            if let Some(sig) = self.qual_fns.get(&key) {
                let (params, ret, generics) =
                    (sig.params.clone(), sig.ret.clone(), sig.generics.clone());
                return self
                    .check_free_call(&key, &params, &ret, &generics, &arg_tys, args, callee.span);
            }
        }
        // Enum tuple-variant constructor -> the enum type.
        if segs.len() >= 2 && self.enums.contains(&segs[segs.len() - 2]) {
            return Ty::Named(segs[segs.len() - 2].clone());
        }
        // Some/Ok/Err, collection constructors, extern fns: unknown.
        Ty::Unknown
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
    /// scanning its trait bounds. Returns `(trait_name, params, ret, generics)`
    /// for the first bound that declares the method (a real conflict would be a
    /// genuine ambiguity we simply don't diagnose here). `generics` are the
    /// method's own (method-level) generic parameters.
    fn resolve_generic_method(
        &self,
        gname: &str,
        method: &str,
    ) -> Option<(String, Vec<Ty>, Ty, Vec<GenericParam>)> {
        let bounds = self.gen_bounds.get(gname)?;
        for tr in bounds {
            if let Some(sig) = self.trait_methods.get(tr).and_then(|m| m.get(method)) {
                return Some((
                    tr.clone(),
                    sig.params.clone(),
                    sig.ret.clone(),
                    sig.generics.clone(),
                ));
            }
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
        arg_tys: &[Ty],
        args: &[Expr],
        callee_sp: SourceRange,
    ) -> Ty {
        if args.len() != params.len() {
            self.err(
                callee_sp,
                format!(
                    "function `{}` expects {} argument(s), got {}",
                    dispname,
                    params.len(),
                    args.len()
                ),
            );
            return ret.clone();
        }
        for (i, at) in arg_tys.iter().enumerate() {
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
            let mut subst: HashMap<String, Ty> = HashMap::new();
            for (p, a) in params.iter().zip(arg_tys.iter()) {
                unify_generic(p, a, &mut subst);
            }
            self.check_bound_satisfaction(dispname, generics, &subst, callee_sp);
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
        subst: &HashMap<String, Ty>,
        sp: SourceRange,
    ) {
        for g in generics {
            let Some(Ty::Named(c)) = subst.get(&g.name) else {
                continue;
            };
            if !self.structs.contains_key(c) && !self.enums.contains(c) {
                continue;
            }
            for b in &g.bounds {
                if !self.trait_methods.contains_key(b) {
                    continue; // builtin / unknown trait: not verifiable
                }
                let implemented = self.impls.get(c).is_some_and(|s| s.contains(b));
                if !implemented {
                    self.err(
                        sp,
                        format!(
                            "type `{}` does not implement trait `{}` (required by bound `{}: {}` of `{}`)",
                            c, b, g.name, b, fname
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

    /// Type-check the element/key/value arguments of a built-in collection
    /// method (`Vec::push`/`set`, `HashMap::insert`/`get`/`remove`/
    /// `contains_key`) against the receiver's known type parameters. Only fires
    /// when both the receiver parameter and the argument are concrete, so
    /// unmodeled methods and un-inferred (`?`) collections stay silent.
    fn check_builtin_method_call(
        &mut self,
        recv: &Ty,
        method: &str,
        arg_tys: &[Ty],
        args: &[Expr],
    ) {
        match recv {
            Ty::Vec(elem) => match method {
                "push" if arg_tys.len() == 1 => {
                    self.check_elem_compat(elem, &arg_tys[0], args[0].span, "Vec element");
                }
                "set" if arg_tys.len() == 2 => {
                    self.check_elem_compat(elem, &arg_tys[1], args[1].span, "Vec element");
                }
                _ => {}
            },
            Ty::Map(k, v) => match method {
                "insert" if arg_tys.len() == 2 => {
                    self.check_elem_compat(k, &arg_tys[0], args[0].span, "HashMap key");
                    self.check_elem_compat(v, &arg_tys[1], args[1].span, "HashMap value");
                }
                "get" | "remove" | "contains_key" if arg_tys.len() == 1 => {
                    self.check_elem_compat(k, &arg_tys[0], args[0].span, "HashMap key");
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// Report a type mismatch between a collection's declared element/key/value
    /// type and a supplied argument. No-op unless both are concrete and
    /// genuinely incompatible (numeric types stay mutually compatible).
    fn check_elem_compat(&mut self, expected: &Ty, actual: &Ty, sp: SourceRange, what: &str) {
        if expected.is_concrete() && actual.is_concrete() && !compatible(expected, actual) {
            self.err(
                sp,
                format!(
                    "{} type mismatch: expected `{}`, found `{}`",
                    what,
                    expected.name(),
                    actual.name()
                ),
            );
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
            Pattern::Binding(name, span) => {
                self.record_binding(span, ty, name);
                self.bind(name, ty.clone());
            }
            Pattern::TupleVariant { path, elems } => {
                let payload = self.tuple_payload(path, ty);
                for (i, e) in elems.iter().enumerate() {
                    let et = payload
                        .as_ref()
                        .and_then(|v| v.get(i).cloned())
                        .unwrap_or(Ty::Unknown);
                    self.bind_pattern(e, &et);
                }
            }
            Pattern::StructVariant { path, fields, .. } => {
                let field_tys = self.struct_payload(path, ty);
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
    fn tuple_payload(&self, path: &[String], ty: &Ty) -> Option<Vec<Ty>> {
        if let Some(v) = builtin_payload(path, ty) {
            return Some(v);
        }
        let variant = path.last()?;
        if let Ty::Named(enum_name) = ty {
            if let Some(VariantPayload::Tuple(tys)) =
                self.enum_variants.get(enum_name).and_then(|m| m.get(variant))
            {
                return Some(tys.clone());
            }
        }
        None
    }

    /// Struct-variant field types (name → type) for pattern `path` against
    /// scrutinee `ty`, when it's a user enum variant. `None` → Unknown fields.
    fn struct_payload(&self, path: &[String], ty: &Ty) -> Option<Vec<(String, Ty)>> {
        let variant = path.last()?;
        if let Ty::Named(enum_name) = ty {
            if let Some(VariantPayload::Struct(fields)) =
                self.enum_variants.get(enum_name).and_then(|m| m.get(variant))
            {
                return Some(fields.clone());
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser;

    /// Run type-checking and return the internal tables for inspection.
    fn run_tc(src: &str) -> Tc {
        let program = parser::parse(src).unwrap();
        let mut tc = Tc::new(&program);
        tc.run(&program);
        tc
    }

    #[test]
    fn b0_struct_field_spans() {
        let src = "struct Point { x: f64, y: f64 }";
        let tc = run_tc(src);
        let fields = tc.structs.get("Point").expect("Point should be registered");
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].0, "x");
        assert!(fields[0].2.start > 0, "field 'x' should have a non-zero span start");
        let x_src = &src[fields[0].2.start..fields[0].2.end()];
        assert_eq!(x_src, "x");
        assert_eq!(fields[1].0, "y");
        let y_src = &src[fields[1].2.start..fields[1].2.end()];
        assert_eq!(y_src, "y");
    }

    #[test]
    fn b0_struct_field_span_positions() {
        let src = "struct Data { name: String, age: i64 }";
        let tc = run_tc(src);
        let fields = tc.structs.get("Data").unwrap();
        assert_eq!(&src[fields[0].2.start..fields[0].2.end()], "name");
        assert_eq!(&src[fields[1].2.start..fields[1].2.end()], "age");
    }

    #[test]
    fn b0_impl_method_defs() {
        let src = r#"
struct Point { x: f64, y: f64 }
impl Point {
    fn dist(&self) -> f64 { 0.0 }
    fn move_to(&mut self, nx: f64, ny: f64) {}
}
"#;
        let tc = run_tc(src);
        let mdefs = tc.method_defs
            .get("Point")
            .expect("Point should have method defs");

        // Verify method "dist" has a definition span and detail.
        let (sp, detail) = mdefs.get("dist").expect("dist should be registered");
        assert_eq!(&src[sp.start..sp.end()], "dist");
        assert_eq!(detail, "fn dist(&self) -> f64");

        // Verify method "move_to" has correct detail.
        let (sp2, detail2) = mdefs.get("move_to").expect("move_to should be registered");
        assert_eq!(&src[sp2.start..sp2.end()], "move_to");
        assert_eq!(detail2, "fn move_to(&self, nx: f64, ny: f64)");
    }

    #[test]
    fn b0_trait_method_defs() {
        let src = r#"
trait Area {
    fn area(&self) -> f64;
    fn name(&self) -> String { "shape".to_string() }
}
"#;
        let tc = run_tc(src);
        let tdefs = tc.trait_method_defs
            .get("Area")
            .expect("Area should have trait method defs");

        let (sp, detail) = tdefs.get("area").expect("area should be registered");
        assert_eq!(&src[sp.start..sp.end()], "area");
        assert_eq!(detail, "fn area(&self) -> f64");

        let (sp2, detail2) = tdefs.get("name").expect("name should be registered");
        assert_eq!(&src[sp2.start..sp2.end()], "name");
        assert_eq!(detail2, "fn name(&self) -> String");
    }

    #[test]
    fn b0_trait_impl_inherits_default_method_defs() {
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
        let mdefs = tc.method_defs
            .get("Circle")
            .expect("Circle should have method defs");

        // Explicit method: area
        let (sp, detail) = mdefs.get("area").expect("area should be registered");
        assert_eq!(&src[sp.start..sp.end()], "area");
        assert_eq!(detail, "fn area(&self) -> f64");

        // Inherited default: label — span should come from trait definition
        let (sp2, detail2) = mdefs.get("label").expect("label should be inherited");
        assert_eq!(&src[sp2.start..sp2.end()], "label");
        assert_eq!(detail2, "fn label(&self) -> String");
    }

    #[test]
    fn b0_method_detail_without_self() {
        let src = r#"
struct Factory {}
impl Factory {
    fn new(x: i64) -> Factory { Factory {} }
}
"#;
        let tc = run_tc(src);
        let mdefs = tc.method_defs.get("Factory").unwrap();
        let (_, detail) = mdefs.get("new").unwrap();
        assert_eq!(detail, "fn new(x: i64) -> Factory");
    }
}
