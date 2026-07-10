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
use crate::diag::{Diag, render_all};
use crate::token::SourceRange;
use std::collections::HashMap;

#[derive(Clone)]
enum VKind {
    Unit,
    Tuple(usize),
    Struct(Vec<String>),
}

struct Info {
    /// struct name -> field names
    structs: HashMap<String, Vec<String>>,
    /// enum name -> (variant -> kind)
    enums: HashMap<String, HashMap<String, VKind>>,
    /// uniquely-named variant -> owning enum
    variant_owner: HashMap<String, String>,
    dup_errors: Vec<Diag>,
}

impl Info {
    fn collect(prog: &Program) -> Info {
        let mut structs: HashMap<String, Vec<String>> = HashMap::new();
        let mut enums: HashMap<String, HashMap<String, VKind>> = HashMap::new();
        let mut variant_owner: HashMap<String, String> = HashMap::new();
        let mut ambiguous: Vec<String> = Vec::new();
        let mut dup_errors = Vec::new();
        // How many scopes define each type name. A name defined in more than one
        // scope (e.g. `mod a { struct Point } mod b { struct Point }`) is
        // ambiguous by simple name, so it is dropped from the registries and all
        // checks on it degrade to `Unknown` — keeping zero false positives.
        let mut type_counts: HashMap<String, usize> = HashMap::new();

        register_scope(
            &prog.items,
            None,
            &mut structs,
            &mut enums,
            &mut variant_owner,
            &mut ambiguous,
            &mut type_counts,
            &mut dup_errors,
        );
        for a in ambiguous {
            variant_owner.remove(&a);
        }
        // Drop cross-scope-ambiguous type names from the registries.
        for (name, count) in &type_counts {
            if *count > 1 {
                structs.remove(name);
                enums.remove(name);
                variant_owner.retain(|_, owner| owner != name);
            }
        }
        Info {
            structs,
            enums,
            variant_owner,
            dup_errors,
        }
    }

    fn resolve_variant<'s>(&'s self, segs: &[String]) -> Option<(String, String, &'s VKind)> {
        if segs.len() >= 2 {
            let en = &segs[segs.len() - 2];
            let var = &segs[segs.len() - 1];
            let k = self.enums.get(en)?.get(var)?;
            Some((en.clone(), var.clone(), k))
        } else if segs.len() == 1 {
            let var = &segs[0];
            let en = self.variant_owner.get(var)?;
            let k = self.enums.get(en)?.get(var)?;
            Some((en.clone(), var.clone(), k))
        } else {
            None
        }
    }

    /// Is `en` a known enum name (for detecting `Enum::Unknown`)?
    fn is_enum(&self, name: &str) -> bool {
        self.enums.contains_key(name)
    }
}

/// Register the types of one scope (root or a module) into the shared maps,
/// recursing into nested modules. Duplicate names are detected per scope, so the
/// same name may appear in sibling modules. `use` inside a module is rejected
/// (file-level only for now).
#[allow(clippy::too_many_arguments)]
fn register_scope(
    items: &[Item],
    module: Option<&str>,
    structs: &mut HashMap<String, Vec<String>>,
    enums: &mut HashMap<String, HashMap<String, VKind>>,
    variant_owner: &mut HashMap<String, String>,
    ambiguous: &mut Vec<String>,
    type_counts: &mut HashMap<String, usize>,
    errs: &mut Vec<Diag>,
) {
    let mut seen: HashMap<String, ()> = HashMap::new();
    let dup = |name: &str, seen: &mut HashMap<String, ()>, errs: &mut Vec<Diag>| {
        if seen.insert(name.to_string(), ()).is_some() {
            match module {
                None => errs.push(Diag::bare(format!(
                    "duplicate top-level definition `{}`",
                    name
                ))),
                Some(m) => errs.push(Diag::bare(format!(
                    "duplicate definition `{}` in module `{}`",
                    name, m
                ))),
            }
        }
    };

    for item in items {
        match item {
            Item::Fn(f) => dup(&f.name, &mut seen, errs),
            Item::Trait(t) => dup(&t.name, &mut seen, errs),
            Item::Struct(s) => {
                dup(&s.name, &mut seen, errs);
                *type_counts.entry(s.name.clone()).or_insert(0) += 1;
                let mut fields = Vec::new();
                for f in &s.fields {
                    if fields.contains(&f.name) {
                        errs.push(Diag::bare(format!(
                            "duplicate field `{}` in struct `{}`",
                            f.name, s.name
                        )));
                    } else {
                        fields.push(f.name.clone());
                    }
                }
                structs.insert(s.name.clone(), fields);
            }
            Item::Enum(e) => {
                dup(&e.name, &mut seen, errs);
                *type_counts.entry(e.name.clone()).or_insert(0) += 1;
                let mut vs = HashMap::new();
                for v in &e.variants {
                    if vs.contains_key(&v.name) {
                        errs.push(Diag::bare(format!(
                            "duplicate variant `{}` in enum `{}`",
                            v.name, e.name
                        )));
                    }
                    let kind = match &v.kind {
                        VariantKind::Unit => VKind::Unit,
                        VariantKind::Tuple(t) => VKind::Tuple(t.len()),
                        VariantKind::Struct(fs) => {
                            VKind::Struct(fs.iter().map(|f| f.name.clone()).collect())
                        }
                    };
                    vs.insert(v.name.clone(), kind);
                    if variant_owner.contains_key(&v.name) {
                        ambiguous.push(v.name.clone());
                    } else {
                        variant_owner.insert(v.name.clone(), e.name.clone());
                    }
                }
                enums.insert(e.name.clone(), vs);
            }
            Item::Impl(_) => {}
            Item::Extern(b) => {
                for ef in &b.fns {
                    dup(&ef.name, &mut seen, errs);
                }
            }
            Item::Mod(m) => {
                dup(&m.name, &mut seen, errs);
                register_scope(
                    &m.items,
                    Some(&m.name),
                    structs,
                    enums,
                    variant_owner,
                    ambiguous,
                    type_counts,
                    errs,
                );
            }
            Item::Use(_) => {}
        }
    }
}

fn walk_mod(info: &Info, m: &ModDecl, errs: &mut Vec<Diag>) {
    for it in &m.items {
        match it {
            Item::Fn(f) => walk_block(info, &f.body, errs),
            Item::Impl(im) => {
                for me in &im.methods {
                    walk_block(info, &me.body, errs);
                }
            }
            Item::Trait(t) => {
                for tm in &t.methods {
                    if let Some(b) = &tm.default {
                        walk_block(info, b, errs);
                    }
                }
            }
            Item::Mod(md) => walk_mod(info, md, errs),
            _ => {}
        }
    }
}

/// Run all structural checks and return every diagnostic. The returned vec is
/// suitable for LSP consumption (byte-offset spans are preserved from `Expr`).
pub fn collect_diags(prog: &Program) -> Vec<Diag> {
    let info = Info::collect(prog);
    let mut errs = info.dup_errors.clone();

    for item in &prog.items {
        match item {
            Item::Fn(f) => walk_block(&info, &f.body, &mut errs),
            Item::Impl(im) => {
                for m in &im.methods {
                    walk_block(&info, &m.body, &mut errs);
                }
            }
            Item::Trait(t) => {
                for tm in &t.methods {
                    if let Some(b) = &tm.default {
                        walk_block(&info, b, &mut errs);
                    }
                }
            }
            Item::Mod(m) => walk_mod(&info, m, &mut errs),
            _ => {}
        }
    }

    // Visibility (`pub`) enforcement: cross-module references to private items.
    let tree = build_tree(&prog.items);
    vis_items(&tree, &[], &prog.items, &mut errs);

    // Generic bound trait names must resolve to a declared or built-in trait.
    let mut trait_names = std::collections::HashSet::new();
    collect_trait_names(&prog.items, &mut trait_names);
    check_bounds(&prog.items, &trait_names, &mut errs);

    errs
}

pub fn check(prog: &Program, files: &[String]) -> Result<(), String> {
    let errs = collect_diags(prog);
    if errs.is_empty() {
        Ok(())
    } else {
        Err(render_all(&errs, files))
    }
}

// --- visibility (`pub`) enforcement -----------------------------------------

/// A module in the visibility tree: named children with their `pub` flag and,
/// for submodules, their own subtree.
struct ModNode {
    children: HashMap<String, Child>,
}

struct Child {
    is_pub: bool,
    sub: Option<ModNode>,
}

fn build_tree(items: &[Item]) -> ModNode {
    let mut children = HashMap::new();
    for it in items {
        let (name, is_pub, sub) = match it {
            Item::Fn(f) => (f.name.clone(), f.is_pub, None),
            Item::Struct(s) => (s.name.clone(), s.is_pub, None),
            Item::Enum(e) => (e.name.clone(), e.is_pub, None),
            Item::Trait(t) => (t.name.clone(), t.is_pub, None),
            Item::Mod(m) => (m.name.clone(), m.is_pub, Some(build_tree(&m.items))),
            _ => continue,
        };
        children.insert(name, Child { is_pub, sub });
    }
    ModNode { children }
}

fn descend<'a>(root: &'a ModNode, path: &[String]) -> Option<&'a ModNode> {
    let mut node = root;
    for seg in path {
        node = node.children.get(seg)?.sub.as_ref()?;
    }
    Some(node)
}

fn is_prefix(pre: &[String], of: &[String]) -> bool {
    pre.len() <= of.len() && pre.iter().zip(of).all(|(a, b)| a == b)
}

/// Check a `::` path reference for privacy violations. Only paths that resolve
/// through the module tree are checked; anything else (locals, root types,
/// methods, enum variants, built-ins) is left alone — so no false positives.
fn check_vis(root: &ModNode, cur: &[String], segs: &[String], sp: SourceRange, errs: &mut Vec<Diag>) {
    if segs.is_empty() {
        return;
    }
    // Anchor on the current module (relative) if it defines seg0, else root.
    let cur_node = descend(root, cur);
    let (mut node, mut dpath): (&ModNode, Vec<String>) =
        if cur_node.is_some_and(|n| n.children.contains_key(&segs[0])) {
            (cur_node.unwrap(), cur.to_vec())
        } else if root.children.contains_key(&segs[0]) {
            (root, Vec::new())
        } else {
            return;
        };

    for seg in segs {
        let Some(child) = node.children.get(seg) else {
            return;
        };
        // Accessible if public, or the defining module is an ancestor-or-equal
        // of the access site (which can see its own/ancestors' private items).
        if !child.is_pub && !is_prefix(&dpath, cur) {
            let where_ = if dpath.is_empty() {
                "crate root".to_string()
            } else {
                format!("module `{}`", dpath.join("::"))
            };
            errs.push(at(sp, format!("`{}` is private to {}", seg, where_)));
            return;
        }
        match &child.sub {
            Some(sub) => {
                dpath.push(seg.clone());
                node = sub;
            }
            None => return, // reached a non-module item; stop.
        }
    }
}

fn vis_items(root: &ModNode, cur: &[String], items: &[Item], errs: &mut Vec<Diag>) {
    for it in items {
        match it {
            Item::Fn(f) => vis_block(root, cur, &f.body, errs),
            Item::Impl(im) => {
                for m in &im.methods {
                    vis_block(root, cur, &m.body, errs);
                }
            }
            Item::Trait(t) => {
                for tm in &t.methods {
                    if let Some(b) = &tm.default {
                        vis_block(root, cur, b, errs);
                    }
                }
            }
            Item::Use(u) => {
                for imp in &u.imports {
                    check_vis(root, cur, &imp.path, SourceRange::EMPTY, errs);
                }
            }
            Item::Mod(m) => {
                let mut sub = cur.to_vec();
                sub.push(m.name.clone());
                vis_items(root, &sub, &m.items, errs);
            }
            _ => {}
        }
    }
}

fn vis_block(root: &ModNode, cur: &[String], b: &Block, errs: &mut Vec<Diag>) {
    for s in &b.stmts {
        vis_stmt(root, cur, s, errs);
    }
    if let Some(e) = &b.tail {
        vis_expr(root, cur, e, errs);
    }
}

fn vis_stmt(root: &ModNode, cur: &[String], s: &Stmt, errs: &mut Vec<Diag>) {
    match s {
        Stmt::Let { init, .. } => vis_expr(root, cur, init, errs),
        Stmt::Expr(e) => vis_expr(root, cur, e, errs),
        Stmt::Return(Some(e)) => vis_expr(root, cur, e, errs),
        Stmt::Return(None) => {}
        Stmt::While { cond, body } => {
            vis_expr(root, cur, cond, errs);
            vis_block(root, cur, body, errs);
        }
        Stmt::Loop { body } => vis_block(root, cur, body, errs),
        Stmt::For { iter, body, .. } => {
            vis_expr(root, cur, iter, errs);
            vis_block(root, cur, body, errs);
        }
        Stmt::WhileLet { pat, expr, body } => {
            vis_pattern(root, cur, pat, errs);
            vis_expr(root, cur, expr, errs);
            vis_block(root, cur, body, errs);
        }
        Stmt::Break | Stmt::Continue => {}
    }
}

fn vis_expr(root: &ModNode, cur: &[String], e: &Expr, errs: &mut Vec<Diag>) {
    let sp = e.span;
    match &e.kind {
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Bool(_) => {}
        ExprKind::Path(segs) => check_vis(root, cur, segs, sp, errs),
        ExprKind::StructLit { path, fields } => {
            check_vis(root, cur, path, sp, errs);
            for (_, v) in fields {
                vis_expr(root, cur, v, errs);
            }
        }
        ExprKind::Unary { expr, .. } => vis_expr(root, cur, expr, errs),
        ExprKind::Binary { lhs, rhs, .. } => {
            vis_expr(root, cur, lhs, errs);
            vis_expr(root, cur, rhs, errs);
        }
        ExprKind::Call { callee, args } => {
            vis_expr(root, cur, callee, errs);
            for a in args {
                vis_expr(root, cur, a, errs);
            }
        }
        ExprKind::MethodCall { recv, args, .. } => {
            vis_expr(root, cur, recv, errs);
            for a in args {
                vis_expr(root, cur, a, errs);
            }
        }
        ExprKind::Field { base, .. } => vis_expr(root, cur, base, errs),
        ExprKind::Index { base, index } => {
            vis_expr(root, cur, base, errs);
            vis_expr(root, cur, index, errs);
        }
        ExprKind::Range { start, end, .. } => {
            vis_expr(root, cur, start, errs);
            vis_expr(root, cur, end, errs);
        }
        ExprKind::MacroCall { args, .. } => {
            for a in args {
                vis_expr(root, cur, a, errs);
            }
        }
        ExprKind::Try { expr } => vis_expr(root, cur, expr, errs),
        ExprKind::If {
            cond,
            then_block,
            else_block,
        } => {
            vis_expr(root, cur, cond, errs);
            vis_block(root, cur, then_block, errs);
            match else_block.as_deref() {
                Some(ElseBranch::Block(b)) => vis_block(root, cur, b, errs),
                Some(ElseBranch::If(inner)) => vis_expr(root, cur, inner, errs),
                None => {}
            }
        }
        ExprKind::IfLet {
            pat,
            expr,
            then_block,
            else_block,
        } => {
            vis_pattern(root, cur, pat, errs);
            vis_expr(root, cur, expr, errs);
            vis_block(root, cur, then_block, errs);
            match else_block.as_deref() {
                Some(ElseBranch::Block(b)) => vis_block(root, cur, b, errs),
                Some(ElseBranch::If(inner)) => vis_expr(root, cur, inner, errs),
                None => {}
            }
        }
        ExprKind::Block(b) => vis_block(root, cur, b, errs),
        ExprKind::Assign { target, value } => {
            vis_expr(root, cur, target, errs);
            vis_expr(root, cur, value, errs);
        }
        ExprKind::Match { scrut, arms } => {
            vis_expr(root, cur, scrut, errs);
            for arm in arms {
                for p in &arm.pats {
                    vis_pattern(root, cur, p, errs);
                }
                if let Some(g) = &arm.guard {
                    vis_expr(root, cur, g, errs);
                }
                vis_expr(root, cur, &arm.body, errs);
            }
        }
    }
}

fn vis_pattern(root: &ModNode, cur: &[String], p: &Pattern, errs: &mut Vec<Diag>) {
    match p {
        Pattern::Path(segs) => check_vis(root, cur, segs, SourceRange::EMPTY, errs),
        Pattern::TupleVariant { path, elems } => {
            check_vis(root, cur, path, SourceRange::EMPTY, errs);
            for e in elems {
                vis_pattern(root, cur, e, errs);
            }
        }
        Pattern::StructVariant { path, fields, .. } => {
            check_vis(root, cur, path, SourceRange::EMPTY, errs);
            for (_, fp) in fields {
                vis_pattern(root, cur, fp, errs);
            }
        }
        _ => {}
    }
}

fn walk_block(info: &Info, b: &Block, errs: &mut Vec<Diag>) {
    for s in &b.stmts {
        walk_stmt(info, s, errs);
    }
    if let Some(e) = &b.tail {
        walk_expr(info, e, errs);
    }
}

fn walk_stmt(info: &Info, s: &Stmt, errs: &mut Vec<Diag>) {
    match s {
        Stmt::Let { init, .. } => walk_expr(info, init, errs),
        Stmt::Expr(e) => walk_expr(info, e, errs),
        Stmt::Return(Some(e)) => walk_expr(info, e, errs),
        Stmt::Return(None) => {}
        Stmt::While { cond, body } => {
            walk_expr(info, cond, errs);
            walk_block(info, body, errs);
        }
        Stmt::Loop { body } => walk_block(info, body, errs),
        Stmt::For { iter, body, .. } => {
            walk_expr(info, iter, errs);
            walk_block(info, body, errs);
        }
        Stmt::WhileLet { pat, expr, body } => {
            check_pattern(info, pat, expr.span, errs);
            walk_expr(info, expr, errs);
            walk_block(info, body, errs);
        }
        Stmt::Break | Stmt::Continue => {}
    }
}

fn walk_expr(info: &Info, e: &Expr, errs: &mut Vec<Diag>) {
    let sp = e.span;
    match &e.kind {
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Bool(_) => {}
        ExprKind::Path(segs) => check_path(info, segs, sp, errs),
        ExprKind::Unary { expr, .. } => walk_expr(info, expr, errs),
        ExprKind::Binary { lhs, rhs, .. } => {
            walk_expr(info, lhs, errs);
            walk_expr(info, rhs, errs);
        }
        ExprKind::Call { callee, args } => {
            check_call(info, callee, args, errs);
            for a in args {
                walk_expr(info, a, errs);
            }
            // callee itself (unless a path, already handled by check_call)
            if !matches!(callee.kind, ExprKind::Path(_)) {
                walk_expr(info, callee, errs);
            }
        }
        ExprKind::MethodCall { recv, args, .. } => {
            walk_expr(info, recv, errs);
            for a in args {
                walk_expr(info, a, errs);
            }
        }
        ExprKind::Field { base, .. } => walk_expr(info, base, errs),
        ExprKind::Index { base, index } => {
            walk_expr(info, base, errs);
            walk_expr(info, index, errs);
        }
        ExprKind::Range { start, end, .. } => {
            walk_expr(info, start, errs);
            walk_expr(info, end, errs);
        }
        ExprKind::MacroCall { args, .. } => {
            for a in args {
                walk_expr(info, a, errs);
            }
        }
        ExprKind::StructLit { path, fields } => {
            check_struct_lit(info, path, fields, sp, errs);
            for (_, v) in fields {
                walk_expr(info, v, errs);
            }
        }
        ExprKind::Try { expr } => walk_expr(info, expr, errs),
        ExprKind::If {
            cond,
            then_block,
            else_block,
        } => {
            walk_expr(info, cond, errs);
            walk_block(info, then_block, errs);
            match else_block.as_deref() {
                Some(ElseBranch::Block(b)) => walk_block(info, b, errs),
                Some(ElseBranch::If(inner)) => walk_expr(info, inner, errs),
                None => {}
            }
        }
        ExprKind::IfLet {
            pat,
            expr,
            then_block,
            else_block,
        } => {
            check_pattern(info, pat, expr.span, errs);
            walk_expr(info, expr, errs);
            walk_block(info, then_block, errs);
            match else_block.as_deref() {
                Some(ElseBranch::Block(b)) => walk_block(info, b, errs),
                Some(ElseBranch::If(inner)) => walk_expr(info, inner, errs),
                None => {}
            }
        }
        ExprKind::Block(b) => walk_block(info, b, errs),
        ExprKind::Assign { target, value } => {
            walk_expr(info, target, errs);
            walk_expr(info, value, errs);
        }
        ExprKind::Match { scrut, arms } => {
            walk_expr(info, scrut, errs);
            for arm in arms {
                for p in &arm.pats {
                    check_pattern(info, p, arm.body.span, errs);
                }
                if let Some(g) = &arm.guard {
                    walk_expr(info, g, errs);
                }
                walk_expr(info, &arm.body, errs);
            }
        }
    }
}

// --- generic bound validation -----------------------------------------------

/// Well-known standard traits accepted as bounds without a local declaration.
const BUILTIN_TRAITS: &[&str] = &[
    "Copy", "Clone", "Debug", "Display", "Default", "PartialEq", "Eq", "PartialOrd", "Ord", "Hash",
    "Sized", "Send", "Sync", "Add", "Sub", "Mul", "Div", "Rem", "Neg", "Not", "Iterator",
    "IntoIterator", "ToString", "From", "Into",
];

fn collect_trait_names(items: &[Item], out: &mut std::collections::HashSet<String>) {
    for it in items {
        match it {
            Item::Trait(t) => {
                out.insert(t.name.clone());
            }
            Item::Mod(m) => collect_trait_names(&m.items, out),
            _ => {}
        }
    }
}

fn check_bounds(
    items: &[Item],
    traits: &std::collections::HashSet<String>,
    errs: &mut Vec<Diag>,
) {
    let check = |gens: &[GenericParam], errs: &mut Vec<Diag>| {
        for g in gens {
            for b in &g.bounds {
                if !traits.contains(b) && !BUILTIN_TRAITS.contains(&b.as_str()) {
                    errs.push(Diag::bare(format!(
                        "unknown trait `{}` in bound `{}: {}`",
                        b, g.name, b
                    )));
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
            Item::Mod(m) => check_bounds(&m.items, traits, errs),
            _ => {}
        }
    }
}

/// Build a located diagnostic from a span (carrying file id + byte range + line).
fn at(sp: SourceRange, msg: String) -> Diag {
    Diag::new(sp.file, sp.start, sp.len, sp.line, msg)
}

fn check_path(info: &Info, segs: &[String], sp: SourceRange, errs: &mut Vec<Diag>) {
    // `Enum::Unknown` where Enum is a known enum but the variant is not.
    if segs.len() >= 2 {
        let en = &segs[segs.len() - 2];
        let var = &segs[segs.len() - 1];
        if info.is_enum(en) {
            if let Some(vs) = info.enums.get(en) {
                if !vs.contains_key(var) {
                    errs.push(at(sp, format!("enum `{}` has no variant `{}`", en, var)));
                }
            }
        }
    }
}

fn check_call(info: &Info, callee: &Expr, args: &[Expr], errs: &mut Vec<Diag>) {
    let ExprKind::Path(segs) = &callee.kind else {
        return;
    };
    let sp = callee.span;
    // Option/Result built-ins expect exactly one argument.
    if segs.len() == 1 {
        match segs[0].as_str() {
            "Some" | "Ok" | "Err" => {
                if args.len() != 1 {
                    errs.push(at(sp, format!("`{}` takes exactly 1 argument", segs[0])));
                }
                return;
            }
            _ => {}
        }
    }
    check_path(info, segs, sp, errs);
    if let Some((en, var, kind)) = info.resolve_variant(segs) {
        match kind {
            VKind::Tuple(n) => {
                if args.len() != *n {
                    errs.push(at(
                        sp,
                        format!(
                            "variant `{}::{}` expects {} argument(s), got {}",
                            en,
                            var,
                            n,
                            args.len()
                        ),
                    ));
                }
            }
            VKind::Unit => errs.push(at(
                sp,
                format!("unit variant `{}::{}` is not called with `()`", en, var),
            )),
            VKind::Struct(_) => errs.push(at(
                sp,
                format!("struct variant `{}::{}` must be built with `{{ .. }}`", en, var),
            )),
        }
    }
}

fn check_struct_lit(
    info: &Info,
    path: &[String],
    fields: &[(String, Expr)],
    sp: SourceRange,
    errs: &mut Vec<Diag>,
) {
    // enum struct-variant literal, e.g. `Shape::Rect { w, h }`
    if let Some((en, var, kind)) = info.resolve_variant(path) {
        match kind {
            VKind::Struct(decl) => {
                validate_fields(&format!("variant `{}::{}`", en, var), decl, fields, sp, errs)
            }
            _ => errs.push(at(
                sp,
                format!("variant `{}::{}` is not a struct variant", en, var),
            )),
        }
        return;
    }
    // plain struct literal
    let name = match path.last() {
        Some(n) => n,
        None => return,
    };
    if let Some(decl) = info.structs.get(name) {
        validate_fields(&format!("struct `{}`", name), decl, fields, sp, errs);
    }
    // Unknown name: assume external; do not flag.
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
            errs.push(at(sp, format!("{} has no field `{}`", what, fname)));
        }
    }
    for want in decl {
        if !provided.iter().any(|(n, _)| n == want) {
            errs.push(at(sp, format!("{} is missing field `{}`", what, want)));
        }
    }
}

/// `sp` is a fallback location (the enclosing `match`/`if let`/`while let`
/// expression's span) used for pattern diagnostics, since `Pattern` nodes do not
/// carry their own spans. It gets a diagnostic near the offending construct
/// instead of degrading to the top of the file.
fn check_pattern(info: &Info, p: &Pattern, sp: SourceRange, errs: &mut Vec<Diag>) {
    match p {
        Pattern::Wildcard | Pattern::Binding(..) | Pattern::Literal(_) | Pattern::Range { .. } => {}
        Pattern::Path(segs) => check_path(info, segs, sp, errs),
        Pattern::TupleVariant { path, elems } => {
            let head = path.last().map(String::as_str).unwrap_or("");
            if matches!(head, "Some" | "Ok" | "Err") {
                if elems.len() != 1 {
                    errs.push(at(sp, format!("`{}` pattern takes exactly 1 element", head)));
                }
            } else {
                check_path(info, path, sp, errs);
                if let Some((en, var, kind)) = info.resolve_variant(path) {
                    match kind {
                        VKind::Tuple(n) if elems.len() != *n => errs.push(at(
                            sp,
                            format!(
                                "variant `{}::{}` expects {} element(s) in pattern, got {}",
                                en,
                                var,
                                n,
                                elems.len()
                            ),
                        )),
                        VKind::Unit => errs.push(at(
                            sp,
                            format!("unit variant `{}::{}` has no tuple payload", en, var),
                        )),
                        VKind::Struct(_) => errs.push(at(
                            sp,
                            format!(
                                "struct variant `{}::{}` must be matched with `{{ .. }}`",
                                en, var
                            ),
                        )),
                        _ => {}
                    }
                }
            }
            for e in elems {
                check_pattern(info, e, sp, errs);
            }
        }
        Pattern::StructVariant { path, fields, rest } => {
            if let Some((en, var, kind)) = info.resolve_variant(path) {
                if let VKind::Struct(decl) = kind {
                    for (fname, _) in fields {
                        if !decl.contains(fname) {
                            errs.push(at(
                                sp,
                                format!("variant `{}::{}` has no field `{}`", en, var, fname),
                            ));
                        }
                    }
                } else {
                    errs.push(at(
                        sp,
                        format!("variant `{}::{}` is not a struct variant", en, var),
                    ));
                }
            } else if let Some(decl) = path.last().and_then(|n| info.structs.get(n)) {
                for (fname, _) in fields {
                    if !decl.contains(fname) {
                        errs.push(at(
                            sp,
                            format!(
                                "struct `{}` has no field `{}`",
                                path.last().unwrap(),
                                fname
                            ),
                        ));
                    }
                }
            }
            let _ = rest;
            for (_, fp) in fields {
                check_pattern(info, fp, sp, errs);
            }
        }
    }
}
