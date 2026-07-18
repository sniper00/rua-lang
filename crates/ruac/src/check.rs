//! Conservative structural checker.
//!
//! This is intentionally *not* a full type checker (that arrives in a later P3
//! sub-pass once expressions carry source spans and `extern` exists). It only
//! reports errors it can be certain about, so it never rejects a valid program:
//!
//!   - duplicate top-level definitions (struct/enum/fn/trait) and duplicate
//!     struct fields / enum variants
//!   - struct / struct-variant literals with unknown or missing fields
//!   - enum tuple-variant construction with the wrong number of arguments
//!   - `Some`/`Ok`/`Err` used with the wrong arity
//!   - `match` patterns that misuse a *known* enum variant (unknown variant,
//!     wrong arity, wrong shape, unknown field)
//!
//! All checks fire only on names we know are user-defined types, so unknown
//! identifiers (e.g. Lua globals / future `extern` symbols) are left alone.

use crate::ast::*;
use crate::diag::Diag;
use crate::token::SourceRange;
use std::collections::HashMap;

#[derive(Clone)]
enum VKind {
    Unit,
    Tuple(usize),
    Struct(Vec<String>),
}

struct Info {
    structs: HashMap<crate::hir::DefId, Vec<String>>,
    variants: HashMap<crate::hir::DefId, VKind>,
    errors: Vec<Diag>,
}

impl Info {
    fn collect(program: &Program, hir: &crate::hir::ResolvedHir) -> Self {
        let mut info = Self {
            structs: HashMap::new(),
            variants: HashMap::new(),
            errors: Vec::new(),
        };
        collect_shapes(&program.items, hir.root, hir, &mut info);
        info
    }

    fn variant(&self, definition: crate::hir::DefId) -> Option<&VKind> {
        self.variants.get(&definition)
    }

    fn struct_fields(&self, definition: crate::hir::DefId) -> Option<&[String]> {
        self.structs.get(&definition).map(Vec::as_slice)
    }
}

fn collect_shapes(
    items: &[Item],
    module: crate::hir::ModuleId,
    hir: &crate::hir::ResolvedHir,
    info: &mut Info,
) {
    for item in items {
        match item {
            Item::Struct(structure) => {
                let Some(owner) = hir.module(module).scope.types.get(&structure.name).copied()
                else {
                    continue;
                };
                let mut fields = Vec::new();
                for field in &structure.fields {
                    if fields.contains(&field.name) {
                        info.errors.push(Diag::bare(
                            rua_core::DiagnosticCode::NameDuplicateDefinition,
                            format!(
                                "duplicate field `{}` in struct `{}`",
                                field.name, structure.name
                            ),
                        ));
                    } else {
                        fields.push(field.name.clone());
                    }
                }
                info.structs.insert(owner, fields);
            }
            Item::Enum(enumeration) => {
                let Some(owner) = hir
                    .module(module)
                    .scope
                    .types
                    .get(&enumeration.name)
                    .copied()
                else {
                    continue;
                };
                for variant in &enumeration.variants {
                    let Some(definition) = hir
                        .enum_variants
                        .get(&(owner, variant.name.clone()))
                        .copied()
                    else {
                        continue;
                    };
                    let kind = match &variant.kind {
                        VariantKind::Unit => VKind::Unit,
                        VariantKind::Tuple(types) => VKind::Tuple(types.len()),
                        VariantKind::Struct(fields) => {
                            VKind::Struct(fields.iter().map(|field| field.name.clone()).collect())
                        }
                    };
                    info.variants.insert(definition, kind);
                }
            }
            Item::Mod(child) => {
                if let Some(module) = hir.module(module).scope.modules.get(&child.name).copied() {
                    collect_shapes(&child.items, module, hir, info);
                }
            }
            Item::Annotation(_)
            | Item::Fn(_)
            | Item::Impl(_)
            | Item::Trait(_)
            | Item::Extern(_)
            | Item::Use(_) => {}
        }
    }
}

fn walk_mod(info: &Info, hir: &crate::hir::ResolvedHir, m: &ModDecl, errs: &mut Vec<Diag>) {
    for it in &m.items {
        match it {
            Item::Fn(f) => walk_block(info, hir, &f.body, errs),
            Item::Impl(im) => {
                for me in &im.methods {
                    walk_block(info, hir, &me.body, errs);
                }
            }
            Item::Trait(t) => {
                for tm in &t.methods {
                    if let Some(b) = &tm.default {
                        walk_block(info, hir, b, errs);
                    }
                }
            }
            Item::Mod(md) => walk_mod(info, hir, md, errs),
            _ => {}
        }
    }
    walk_block(info, hir, &m.chunk, errs);
}

/// Run all structural checks and return every diagnostic. The returned vec is
/// suitable for LSP consumption (byte-offset spans are preserved from `Expr`).
pub fn collect_diags(prog: &Program) -> Vec<Diag> {
    let hir = crate::hir::resolve(prog);
    collect_diags_resolved(prog, &hir)
}

pub fn collect_diags_resolved(prog: &Program, hir: &crate::hir::ResolvedHir) -> Vec<Diag> {
    let info = Info::collect(prog, hir);
    let mut errs = info.errors.clone();
    errs.extend(hir.diagnostics.iter().filter_map(|diagnostic| {
        let file = diagnostic.file.map(rua_core::FileId::index).unwrap_or(0);
        let (start, len) = diagnostic
            .range
            .map(|range| (range.start() as usize, range.len() as usize))
            .unwrap_or((0, 0));
        let argument = |name| {
            diagnostic
                .arguments
                .iter()
                .find(|argument| argument.name == name)
                .map(|argument| argument.value.as_str())
                .unwrap_or("<unknown>")
        };
        let line = argument("line").parse().unwrap_or(0);
        let message = match diagnostic.code {
            rua_core::DiagnosticCode::NameUnresolved => {
                format!("cannot resolve name `{}`", argument("name"))
            }
            rua_core::DiagnosticCode::NameUnknownMember => {
                let owner = argument("owner");
                let member = argument("member");
                if argument("kind") == "enum" {
                    format!("enum `{owner}` has no variant `{member}`")
                } else {
                    format!("`{owner}` has no member `{member}`")
                }
            }
            rua_core::DiagnosticCode::NamePrivateAccess => {
                format!("`{}` is private to {}", argument("name"), argument("owner"))
            }
            rua_core::DiagnosticCode::NameDuplicateDefinition => {
                let name = argument("name");
                let owner = argument("owner");
                if argument("kind") == "variant" {
                    format!("duplicate variant `{name}` in enum `{owner}`")
                } else if owner.is_empty() {
                    format!("duplicate top-level definition `{name}`")
                } else {
                    format!("duplicate definition `{name}` in module `{owner}`")
                }
            }
            rua_core::DiagnosticCode::TypeImmutableAssignment => {
                format!("cannot assign to immutable binding `{}`", argument("name"))
            }
            _ => return None,
        };
        Some(Diag::new(diagnostic.code, file, start, len, line, message))
    }));

    for item in &prog.items {
        match item {
            Item::Fn(f) => walk_block(&info, hir, &f.body, &mut errs),
            Item::Impl(im) => {
                for m in &im.methods {
                    walk_block(&info, hir, &m.body, &mut errs);
                }
            }
            Item::Trait(t) => {
                for tm in &t.methods {
                    if let Some(b) = &tm.default {
                        walk_block(&info, hir, b, &mut errs);
                    }
                }
            }
            Item::Mod(m) => walk_mod(&info, hir, m, &mut errs),
            _ => {}
        }
    }
    walk_block(&info, hir, &prog.chunk, &mut errs);

    // Generic bound trait names must resolve to a declared or built-in trait.
    check_bounds(&prog.items, hir.root, hir, &mut errs);
    check_extern_adapters(&prog.items, hir.root, hir, &mut errs);

    errs
}

fn check_extern_adapters(
    items: &[Item],
    module: crate::hir::ModuleId,
    hir: &crate::hir::ResolvedHir,
    errors: &mut Vec<Diag>,
) {
    for (item_index, item) in items.iter().enumerate() {
        match item {
            Item::Extern(block) if block.abi == "lua-result" => {
                for function in &block.fns {
                    if function.variadic {
                        errors.push(at_code(
                            rua_core::DiagnosticCode::TypeInvalidFfiAdapter,
                            function.name_span,
                            "`lua-result` adapters cannot be variadic".to_string(),
                        ));
                    }
                    if function
                        .ret
                        .as_ref()
                        .is_none_or(|ty| !hir.type_is_builtin(ty, rua_core::BuiltinId::TypeResult))
                    {
                        errors.push(at_code(
                            rua_core::DiagnosticCode::TypeInvalidFfiAdapter,
                            function.name_span,
                            "`lua-result` adapter must return builtin `Result<T, E>`".to_string(),
                        ));
                    }
                }
            }
            Item::Mod(child) => {
                if let Some(crate::hir::ResolvedTarget::Module(child_module)) =
                    hir.item_targets.get(&(module, item_index)).copied()
                {
                    check_extern_adapters(&child.items, child_module, hir, errors);
                }
            }
            _ => {}
        }
    }
}

pub fn check(prog: &Program) -> Result<(), Vec<Diag>> {
    let hir = crate::hir::resolve(prog);
    check_resolved(prog, &hir)
}

pub fn check_resolved(prog: &Program, hir: &crate::hir::ResolvedHir) -> Result<(), Vec<Diag>> {
    let errs = collect_diags_resolved(prog, hir);
    if errs.is_empty() { Ok(()) } else { Err(errs) }
}

fn walk_block(info: &Info, hir: &crate::hir::ResolvedHir, b: &Block, errs: &mut Vec<Diag>) {
    for s in &b.stmts {
        walk_stmt(info, hir, s, errs);
    }
    if let Some(e) = &b.tail {
        walk_expr(info, hir, e, errs);
    }
}

fn walk_stmt(info: &Info, hir: &crate::hir::ResolvedHir, s: &Stmt, errs: &mut Vec<Diag>) {
    match s {
        Stmt::Lua { .. } => {}
        Stmt::Let { init, .. } => walk_expr(info, hir, init, errs),
        Stmt::Expr(e) => walk_expr(info, hir, e, errs),
        Stmt::Return(Some(e)) => walk_expr(info, hir, e, errs),
        Stmt::Return(None) => {}
        Stmt::While { cond, body } => {
            walk_expr(info, hir, cond, errs);
            walk_block(info, hir, body, errs);
        }
        Stmt::Loop { body } => walk_block(info, hir, body, errs),
        Stmt::For { iter, body, .. } => {
            walk_expr(info, hir, iter, errs);
            walk_block(info, hir, body, errs);
        }
        Stmt::WhileLet { pat, expr, body } => {
            check_pattern(info, hir, pat, expr.span, errs);
            walk_expr(info, hir, expr, errs);
            walk_block(info, hir, body, errs);
        }
        Stmt::Break(value) => {
            if let Some(value) = value {
                walk_expr(info, hir, value, errs);
            }
        }
        Stmt::Continue => {}
    }
}

fn walk_expr(info: &Info, hir: &crate::hir::ResolvedHir, e: &Expr, errs: &mut Vec<Diag>) {
    let sp = e.span;
    match &e.kind {
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Bool(_) => {}
        ExprKind::VecLit(elements) => {
            for element in elements {
                walk_expr(info, hir, element, errs);
            }
        }
        ExprKind::Closure { body, .. } => match body {
            ClosureBody::Expr(expr) => walk_expr(info, hir, expr, errs),
            ClosureBody::Block(block) => walk_block(info, hir, block, errs),
        },
        ExprKind::Path(_) => {}
        ExprKind::Unary { expr, .. } => walk_expr(info, hir, expr, errs),
        ExprKind::Binary { lhs, rhs, .. } => {
            walk_expr(info, hir, lhs, errs);
            walk_expr(info, hir, rhs, errs);
        }
        ExprKind::Loop(body) => walk_block(info, hir, body, errs),
        ExprKind::Call { callee, args } => {
            check_call(info, hir, callee, args, errs);
            for a in args {
                walk_expr(info, hir, a, errs);
            }
            // callee itself (unless a path, already handled by check_call)
            if !matches!(callee.kind, ExprKind::Path(_)) {
                walk_expr(info, hir, callee, errs);
            }
        }
        ExprKind::MethodCall { recv, args, .. } => {
            walk_expr(info, hir, recv, errs);
            for a in args {
                walk_expr(info, hir, a, errs);
            }
        }
        ExprKind::Field { base, .. } => walk_expr(info, hir, base, errs),
        ExprKind::Index { base, index } => {
            walk_expr(info, hir, base, errs);
            walk_expr(info, hir, index, errs);
        }
        ExprKind::Range { start, end, .. } => {
            walk_expr(info, hir, start, errs);
            walk_expr(info, hir, end, errs);
        }
        ExprKind::StructLit { fields, .. } => {
            check_struct_lit(info, hir, e, fields, sp, errs);
            for (_, v) in fields {
                walk_expr(info, hir, v, errs);
            }
        }
        ExprKind::MapLit(entries) => {
            for (key, value) in entries {
                walk_expr(info, hir, key, errs);
                walk_expr(info, hir, value, errs);
            }
        }
        ExprKind::Try { expr } => walk_expr(info, hir, expr, errs),
        ExprKind::If {
            cond,
            then_block,
            else_block,
        } => {
            walk_expr(info, hir, cond, errs);
            walk_block(info, hir, then_block, errs);
            match else_block.as_deref() {
                Some(ElseBranch::Block(b)) => walk_block(info, hir, b, errs),
                Some(ElseBranch::If(inner)) => walk_expr(info, hir, inner, errs),
                None => {}
            }
        }
        ExprKind::IfLet {
            pat,
            expr,
            then_block,
            else_block,
        } => {
            check_pattern(info, hir, pat, expr.span, errs);
            walk_expr(info, hir, expr, errs);
            walk_block(info, hir, then_block, errs);
            match else_block.as_deref() {
                Some(ElseBranch::Block(b)) => walk_block(info, hir, b, errs),
                Some(ElseBranch::If(inner)) => walk_expr(info, hir, inner, errs),
                None => {}
            }
        }
        ExprKind::Block(b) => walk_block(info, hir, b, errs),
        ExprKind::Assign { target, value, .. } => {
            walk_expr(info, hir, target, errs);
            walk_expr(info, hir, value, errs);
        }
        ExprKind::Match { scrut, arms } => {
            walk_expr(info, hir, scrut, errs);
            for arm in arms {
                for p in &arm.pats {
                    check_pattern(info, hir, p, arm.body.span, errs);
                }
                if let Some(g) = &arm.guard {
                    walk_expr(info, hir, g, errs);
                }
                walk_expr(info, hir, &arm.body, errs);
            }
        }
    }
}

// --- generic bound validation -----------------------------------------------

/// Well-known standard traits accepted as bounds without a local declaration.
fn check_bounds(
    items: &[Item],
    module: crate::hir::ModuleId,
    hir: &crate::hir::ResolvedHir,
    errs: &mut Vec<Diag>,
) {
    let check = |gens: &[GenericParam], errs: &mut Vec<Diag>| {
        for g in gens {
            for b in &g.bounds {
                if !hir.trait_ref_targets.contains_key(&b.id) {
                    errs.push(Diag::bare(
                        rua_core::DiagnosticCode::TypeUnsatisfiedTraitBound,
                        format!(
                            "unknown trait `{}` in bound `{}: {}`",
                            b.path, g.name, b.path
                        ),
                    ));
                }
            }
        }
    };
    for it in items {
        match it {
            Item::Fn(f) => check(&f.generics, errs),
            Item::Struct(s) => check(&s.generics, errs),
            Item::Enum(e) => check(&e.generics, errs),
            Item::Trait(t) => {
                check(&t.generics, errs);
                for m in &t.methods {
                    check(&m.generics, errs);
                }
            }
            Item::Impl(im) => {
                check(&im.generics, errs);
                for m in &im.methods {
                    check(&m.generics, errs);
                }
            }
            Item::Extern(block) => {
                for function in &block.fns {
                    check(&function.generics, errs);
                }
            }
            Item::Mod(m) => {
                if let Some(&child) = hir.module(module).scope.modules.get(&m.name) {
                    check_bounds(&m.items, child, hir, errs);
                }
            }
            _ => {}
        }
    }
}

/// Build a located diagnostic from a span (carrying file id + byte range + line).
fn at(sp: SourceRange, msg: String) -> Diag {
    at_code(rua_core::DiagnosticCode::TypeMismatch, sp, msg)
}

fn at_code(code: rua_core::DiagnosticCode, sp: SourceRange, msg: String) -> Diag {
    Diag::new(code, sp.file, sp.start, sp.len, sp.line, msg)
}

fn variant_names(
    hir: &crate::hir::ResolvedHir,
    definition: crate::hir::DefId,
) -> Option<(&str, &str)> {
    let variant = hir.definition(definition);
    let crate::hir::DefKind::EnumVariant { owner, .. } = variant.kind else {
        return None;
    };
    Some((&hir.definition(owner).name, &variant.name))
}

fn check_call(
    info: &Info,
    hir: &crate::hir::ResolvedHir,
    callee: &Expr,
    args: &[Expr],
    errs: &mut Vec<Diag>,
) {
    let Some(target) = hir.expression_targets.get(&callee.id).copied() else {
        return;
    };
    let sp = callee.span;
    match target {
        crate::hir::ResolvedTarget::Builtin(
            builtin @ (rua_core::BuiltinId::VariantOptionSome
            | rua_core::BuiltinId::VariantResultOk
            | rua_core::BuiltinId::VariantResultErr),
        ) => {
            if args.len() != 1 {
                let name = match builtin {
                    rua_core::BuiltinId::VariantOptionSome => "Some",
                    rua_core::BuiltinId::VariantResultOk => "Ok",
                    rua_core::BuiltinId::VariantResultErr => "Err",
                    _ => unreachable!(),
                };
                errs.push(at_code(
                    rua_core::DiagnosticCode::TypeArgumentCount,
                    sp,
                    format!("`{name}` takes exactly 1 argument"),
                ));
            }
        }
        crate::hir::ResolvedTarget::Builtin(rua_core::BuiltinId::VariantOptionNone) => {
            errs.push(at(
                sp,
                "unit variant `Option::None` is not called with `()`".to_string(),
            ));
        }
        crate::hir::ResolvedTarget::Item(definition) => {
            let Some(kind) = info.variant(definition) else {
                return;
            };
            let Some((owner, variant)) = variant_names(hir, definition) else {
                return;
            };
            match kind {
                VKind::Tuple(expected) if args.len() != *expected => errs.push(at_code(
                    rua_core::DiagnosticCode::TypeArgumentCount,
                    sp,
                    format!(
                        "variant `{}::{}` expects {} argument(s), got {}",
                        owner,
                        variant,
                        expected,
                        args.len()
                    ),
                )),
                VKind::Tuple(_) => {}
                VKind::Unit => errs.push(at(
                    sp,
                    format!(
                        "unit variant `{}::{}` is not called with `()`",
                        owner, variant
                    ),
                )),
                VKind::Struct(_) => errs.push(at(
                    sp,
                    format!(
                        "struct variant `{}::{}` must be built with `{{ .. }}`",
                        owner, variant
                    ),
                )),
            }
        }
        _ => {}
    }
}

fn check_struct_lit(
    info: &Info,
    hir: &crate::hir::ResolvedHir,
    expression: &Expr,
    fields: &[(String, Expr)],
    sp: SourceRange,
    errs: &mut Vec<Diag>,
) {
    let Some(crate::hir::ResolvedTarget::Item(definition)) =
        hir.expression_targets.get(&expression.id).copied()
    else {
        return;
    };
    if let Some(kind) = info.variant(definition) {
        let Some((owner, variant)) = variant_names(hir, definition) else {
            return;
        };
        match kind {
            VKind::Struct(declared) => validate_fields(
                &format!("variant `{}::{}`", owner, variant),
                declared,
                fields,
                sp,
                errs,
            ),
            _ => errs.push(at(
                sp,
                format!("variant `{}::{}` is not a struct variant", owner, variant),
            )),
        }
    } else if let Some(declared) = info.struct_fields(definition) {
        let name = &hir.definition(definition).name;
        validate_fields(&format!("struct `{name}`"), declared, fields, sp, errs);
    }
}

fn validate_fields(
    what: &str,
    decl: &[String],
    provided: &[(String, Expr)],
    sp: SourceRange,
    errs: &mut Vec<Diag>,
) {
    for (fname, _) in provided {
        if !decl.contains(fname) {
            errs.push(at_code(
                rua_core::DiagnosticCode::TypeUnknownField,
                sp,
                format!("{} has no field `{}`", what, fname),
            ));
        }
    }
    for want in decl {
        if !provided.iter().any(|(n, _)| n == want) {
            errs.push(at_code(
                rua_core::DiagnosticCode::TypeUnknownField,
                sp,
                format!("{} is missing field `{}`", what, want),
            ));
        }
    }
}

/// `sp` is a fallback location (the enclosing `match`/`if let`/`while let`
/// expression's span) used for pattern diagnostics, since `Pattern` nodes do not
/// carry their own spans. It gets a diagnostic near the offending construct
/// instead of degrading to the top of the file.
fn check_pattern(
    info: &Info,
    hir: &crate::hir::ResolvedHir,
    p: &Pattern,
    sp: SourceRange,
    errs: &mut Vec<Diag>,
) {
    match p {
        Pattern::Wildcard | Pattern::Binding(..) | Pattern::Path { .. } => {}
        Pattern::Literal(expression) => walk_expr(info, hir, expression, errs),
        Pattern::Range { lo, hi, .. } => {
            walk_expr(info, hir, lo, errs);
            walk_expr(info, hir, hi, errs);
        }
        Pattern::TupleVariant { id, elems, .. } => {
            match hir.pattern_targets.get(id).copied() {
                Some(crate::hir::ResolvedTarget::Builtin(
                    builtin @ (rua_core::BuiltinId::VariantOptionSome
                    | rua_core::BuiltinId::VariantResultOk
                    | rua_core::BuiltinId::VariantResultErr),
                )) => {
                    if elems.len() != 1 {
                        let name = match builtin {
                            rua_core::BuiltinId::VariantOptionSome => "Some",
                            rua_core::BuiltinId::VariantResultOk => "Ok",
                            rua_core::BuiltinId::VariantResultErr => "Err",
                            _ => unreachable!(),
                        };
                        errs.push(at_code(
                            rua_core::DiagnosticCode::TypeArgumentCount,
                            sp,
                            format!("`{name}` pattern takes exactly 1 element"),
                        ));
                    }
                }
                Some(crate::hir::ResolvedTarget::Builtin(
                    rua_core::BuiltinId::VariantOptionNone,
                )) => errs.push(at(
                    sp,
                    "unit variant `Option::None` has no tuple payload".to_string(),
                )),
                Some(crate::hir::ResolvedTarget::Item(definition)) => {
                    if let (Some(kind), Some((owner, variant))) =
                        (info.variant(definition), variant_names(hir, definition))
                    {
                        match kind {
                            VKind::Tuple(expected) if elems.len() != *expected => {
                                errs.push(at_code(
                                    rua_core::DiagnosticCode::TypeArgumentCount,
                                    sp,
                                    format!(
                                        "variant `{}::{}` expects {} element(s) in pattern, got {}",
                                        owner,
                                        variant,
                                        expected,
                                        elems.len()
                                    ),
                                ));
                            }
                            VKind::Unit => errs.push(at(
                                sp,
                                format!(
                                    "unit variant `{}::{}` has no tuple payload",
                                    owner, variant
                                ),
                            )),
                            VKind::Struct(_) => errs.push(at(
                                sp,
                                format!(
                                    "struct variant `{}::{}` must be matched with `{{ .. }}`",
                                    owner, variant
                                ),
                            )),
                            VKind::Tuple(_) => {}
                        }
                    }
                }
                _ => {}
            }
            for element in elems {
                check_pattern(info, hir, element, sp, errs);
            }
        }
        Pattern::StructVariant {
            id, fields, rest, ..
        } => {
            if let Some(crate::hir::ResolvedTarget::Item(definition)) =
                hir.pattern_targets.get(id).copied()
            {
                if let Some(kind) = info.variant(definition) {
                    if let Some((owner, variant)) = variant_names(hir, definition) {
                        if let VKind::Struct(declared) = kind {
                            for (name, _) in fields {
                                if !declared.contains(name) {
                                    errs.push(at_code(
                                        rua_core::DiagnosticCode::TypeUnknownField,
                                        sp,
                                        format!(
                                            "variant `{}::{}` has no field `{}`",
                                            owner, variant, name
                                        ),
                                    ));
                                }
                            }
                        } else {
                            errs.push(at(
                                sp,
                                format!("variant `{}::{}` is not a struct variant", owner, variant),
                            ));
                        }
                    }
                } else if let Some(declared) = info.struct_fields(definition) {
                    let name = &hir.definition(definition).name;
                    for (field, _) in fields {
                        if !declared.contains(field) {
                            errs.push(at_code(
                                rua_core::DiagnosticCode::TypeUnknownField,
                                sp,
                                format!("struct `{name}` has no field `{field}`"),
                            ));
                        }
                    }
                }
            }
            let _ = rest;
            for (_, pattern) in fields {
                check_pattern(info, hir, pattern, sp, errs);
            }
        }
    }
}
