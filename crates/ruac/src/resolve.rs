//! Module resolution: splice file modules (`mod name;`) into the AST.
//!
//! For a file module `mod foo;` declared in a source file living in directory
//! `dir`, the module body is loaded from `dir/foo.rua` (or `dir/foo/mod.rua`).
//! A module's own file-based children are then resolved relative to `dir/foo/`,
//! mirroring Rust's `foo.rs` + `foo/` layout. Inline modules extend the search
//! directory the same way (`mod bar { mod baz; }` looks for `dir/bar/baz.rua`).

use crate::ast::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Recursively load file modules under `items`. `dir` is the directory used to
/// resolve this scope's file modules; `None` disables file modules (e.g. when
/// compiling from an in-memory string). `files` is the compile-time file registry
/// (index = file id); each newly loaded file is appended and its AST spans are
/// stamped with the resulting id so diagnostics can attribute `path:line`.
pub fn resolve_modules(
    items: &mut [Item],
    dir: Option<&Path>,
    files: &mut Vec<String>,
) -> Result<(), String> {
    for item in items.iter_mut() {
        let Item::Mod(m) = item else { continue };
        if m.is_file {
            let dir = dir.ok_or_else(|| {
                format!(
                    "`mod {};` (file module) requires compiling from a file, not a string",
                    m.name
                )
            })?;
            let (file, child_dir, is_decl) = resolve_mod_file(dir, &m.name)?;
            let src = std::fs::read_to_string(&file)
                .map_err(|e| format!("reading {}: {}", file.display(), e))?;
            let mut sub = crate::parser::parse(&src)
                .map_err(|e| format!("{}: {}", file.display(), e))?;
            let id = files.len() as u32;
            files.push(file.display().to_string());
            set_file_items(&mut sub.items, id);
            m.items = sub.items;
            // A `.ruai` module (and everything under it) is declaration-only.
            m.is_decl = is_decl;
            resolve_modules(&mut m.items, Some(&child_dir), files)?;
            if is_decl {
                mark_decl(&mut m.items);
            }
        } else {
            // Inline module: its file-children live under `dir/<name>/`.
            let child_dir = dir.map(|d| d.join(&m.name));
            resolve_modules(&mut m.items, child_dir.as_deref(), files)?;
        }
    }
    Ok(())
}

// --- file-id stamping -------------------------------------------------------
//
// Freshly parsed file modules carry `file = 0` in every span (the parser is
// file-agnostic). After loading a file we walk its AST and stamp the correct
// file id onto each expression span, so a later diagnostic knows which file it
// came from even though all files are merged into one program.

fn set_file_items(items: &mut [Item], id: u32) {
    for it in items.iter_mut() {
        match it {
            Item::Fn(f) => {
                f.name_span.file = id;
                set_file_block(&mut f.body, id);
            }
            Item::Struct(s) => {
                // Field definition spans (used by LSP cross-file member go-to-def).
                for field in &mut s.fields {
                    field.name_span.file = id;
                }
            }
            Item::Impl(im) => {
                for m in &mut im.methods {
                    m.name_span.file = id;
                    set_file_block(&mut m.body, id);
                }
            }
            Item::Trait(t) => {
                for tm in &mut t.methods {
                    tm.name_span.file = id;
                    if let Some(b) = &mut tm.default {
                        set_file_block(b, id);
                    }
                }
            }
            // A nested inline module shares this file's id.
            Item::Mod(m) => set_file_items(&mut m.items, id),
            _ => {}
        }
    }
}

fn set_file_block(b: &mut Block, id: u32) {
    for s in &mut b.stmts {
        set_file_stmt(s, id);
    }
    if let Some(t) = &mut b.tail {
        set_file_expr(t, id);
    }
}

fn set_file_stmt(s: &mut Stmt, id: u32) {
    match s {
        Stmt::Let { init, .. } => set_file_expr(init, id),
        Stmt::Expr(e) => set_file_expr(e, id),
        Stmt::Return(Some(e)) => set_file_expr(e, id),
        Stmt::Return(None) => {}
        Stmt::While { cond, body } => {
            set_file_expr(cond, id);
            set_file_block(body, id);
        }
        Stmt::Loop { body } => set_file_block(body, id),
        Stmt::For { iter, body, .. } => {
            set_file_expr(iter, id);
            set_file_block(body, id);
        }
        Stmt::WhileLet { expr, body, .. } => {
            set_file_expr(expr, id);
            set_file_block(body, id);
        }
        Stmt::Break | Stmt::Continue => {}
    }
}

fn set_file_expr(e: &mut Expr, id: u32) {
    e.span.file = id;
    match &mut e.kind {
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Bool(_)
        | ExprKind::Path(_) => {}
        ExprKind::Unary { expr, .. } => set_file_expr(expr, id),
        ExprKind::Binary { lhs, rhs, .. } => {
            set_file_expr(lhs, id);
            set_file_expr(rhs, id);
        }
        ExprKind::Call { callee, args } => {
            set_file_expr(callee, id);
            for a in args {
                set_file_expr(a, id);
            }
        }
        ExprKind::MethodCall {
            recv,
            args,
            method_span,
            ..
        } => {
            method_span.file = id;
            set_file_expr(recv, id);
            for a in args {
                set_file_expr(a, id);
            }
        }
        ExprKind::Field {
            base, name_span, ..
        } => {
            name_span.file = id;
            set_file_expr(base, id);
        }
        ExprKind::StructLit { fields, .. } => {
            for (_, v) in fields {
                set_file_expr(v, id);
            }
        }
        ExprKind::Try { expr } => set_file_expr(expr, id),
        ExprKind::Range { start, end, .. } => {
            set_file_expr(start, id);
            set_file_expr(end, id);
        }
        ExprKind::Index { base, index } => {
            set_file_expr(base, id);
            set_file_expr(index, id);
        }
        ExprKind::MacroCall { args, .. } => {
            for a in args {
                set_file_expr(a, id);
            }
        }
        ExprKind::If {
            cond,
            then_block,
            else_block,
        } => {
            set_file_expr(cond, id);
            set_file_block(then_block, id);
            set_file_else(else_block, id);
        }
        ExprKind::IfLet {
            expr,
            then_block,
            else_block,
            ..
        } => {
            set_file_expr(expr, id);
            set_file_block(then_block, id);
            set_file_else(else_block, id);
        }
        ExprKind::Block(b) => set_file_block(b, id),
        ExprKind::Assign { target, value } => {
            set_file_expr(target, id);
            set_file_expr(value, id);
        }
        ExprKind::Match { scrut, arms } => {
            set_file_expr(scrut, id);
            for arm in arms {
                if let Some(g) = &mut arm.guard {
                    set_file_expr(g, id);
                }
                set_file_expr(&mut arm.body, id);
            }
        }
    }
}

fn set_file_else(else_block: &mut Option<Box<ElseBranch>>, id: u32) {
    match else_block.as_deref_mut() {
        Some(ElseBranch::Block(b)) => set_file_block(b, id),
        Some(ElseBranch::If(inner)) => set_file_expr(inner, id),
        None => {}
    }
}

// --- `use` desugaring -------------------------------------------------------
//
// Each `use a::b as c;` introduces, in its module scope, an alias `c -> [a, b]`.
// Rather than emit runtime alias locals (which would have module-ordering
// hazards and complicate codegen), we rewrite every bare reference whose head is
// an in-scope alias to the fully-qualified path (`c::x` -> `a::b::x`). Local
// bindings that shadow an alias are respected via a lexical scope stack.

/// Rewrite all `use`-aliased paths in the program to fully-qualified paths.
pub fn resolve_uses(program: &mut Program) {
    resolve_uses_in_scope(&mut program.items);
}

fn resolve_uses_in_scope(items: &mut [Item]) {
    let mut aliases: HashMap<String, Vec<String>> = HashMap::new();
    for it in items.iter() {
        if let Item::Use(u) = it {
            for imp in &u.imports {
                let name = imp
                    .alias
                    .clone()
                    .unwrap_or_else(|| imp.path.last().cloned().unwrap_or_default());
                aliases.insert(name, imp.path.clone());
            }
        }
    }
    for it in items.iter_mut() {
        match it {
            Item::Fn(f) => rewrite_fn(&f.params, f.has_self, &mut f.body, &aliases),
            Item::Impl(im) => {
                for m in &mut im.methods {
                    rewrite_fn(&m.params, m.has_self, &mut m.body, &aliases);
                }
            }
            Item::Trait(t) => {
                for tm in &mut t.methods {
                    if let Some(b) = &mut tm.default {
                        rewrite_fn(&tm.params, tm.has_self, b, &aliases);
                    }
                }
            }
            // Nested modules carry their own `use` scope.
            Item::Mod(md) => resolve_uses_in_scope(&mut md.items),
            _ => {}
        }
    }
}

type Aliases = HashMap<String, Vec<String>>;
type Scopes = Vec<HashSet<String>>;

fn rewrite_fn(params: &[Param], has_self: bool, body: &mut Block, aliases: &Aliases) {
    let mut scopes: Scopes = vec![HashSet::new()];
    if has_self {
        scopes[0].insert("self".to_string());
    }
    for p in params {
        scopes[0].insert(p.name.clone());
    }
    rewrite_block(body, aliases, &mut scopes);
}

fn rewrite_block(b: &mut Block, aliases: &Aliases, scopes: &mut Scopes) {
    scopes.push(HashSet::new());
    for s in &mut b.stmts {
        rewrite_stmt(s, aliases, scopes);
    }
    if let Some(t) = &mut b.tail {
        rewrite_expr(t, aliases, scopes);
    }
    scopes.pop();
}

fn rewrite_stmt(s: &mut Stmt, aliases: &Aliases, scopes: &mut Scopes) {
    match s {
        Stmt::Let { name, init, .. } => {
            rewrite_expr(init, aliases, scopes);
            bind(scopes, name.clone());
        }
        Stmt::Expr(e) => rewrite_expr(e, aliases, scopes),
        Stmt::Return(Some(e)) => rewrite_expr(e, aliases, scopes),
        Stmt::Return(None) => {}
        Stmt::While { cond, body } => {
            rewrite_expr(cond, aliases, scopes);
            rewrite_block(body, aliases, scopes);
        }
        Stmt::Loop { body } => rewrite_block(body, aliases, scopes),
        Stmt::For { var, iter, body, .. } => {
            rewrite_expr(iter, aliases, scopes);
            scopes.push(HashSet::new());
            bind(scopes, var.clone());
            rewrite_block(body, aliases, scopes);
            scopes.pop();
        }
        Stmt::WhileLet { pat, expr, body } => {
            rewrite_expr(expr, aliases, scopes);
            scopes.push(HashSet::new());
            bind_pattern(scopes, pat);
            rewrite_block(body, aliases, scopes);
            scopes.pop();
        }
        Stmt::Break | Stmt::Continue => {}
    }
}

fn rewrite_expr(e: &mut Expr, aliases: &Aliases, scopes: &mut Scopes) {
    match &mut e.kind {
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Bool(_) => {}
        ExprKind::Path(segs) => rewrite_path(segs, aliases, scopes),
        ExprKind::Unary { expr, .. } => rewrite_expr(expr, aliases, scopes),
        ExprKind::Binary { lhs, rhs, .. } => {
            rewrite_expr(lhs, aliases, scopes);
            rewrite_expr(rhs, aliases, scopes);
        }
        ExprKind::Call { callee, args } => {
            rewrite_expr(callee, aliases, scopes);
            for a in args {
                rewrite_expr(a, aliases, scopes);
            }
        }
        ExprKind::MethodCall { recv, args, .. } => {
            rewrite_expr(recv, aliases, scopes);
            for a in args {
                rewrite_expr(a, aliases, scopes);
            }
        }
        ExprKind::Field { base, .. } => rewrite_expr(base, aliases, scopes),
        ExprKind::StructLit { path, fields } => {
            rewrite_path(path, aliases, scopes);
            for (_, v) in fields {
                rewrite_expr(v, aliases, scopes);
            }
        }
        ExprKind::Try { expr } => rewrite_expr(expr, aliases, scopes),
        ExprKind::Match { scrut, arms } => {
            rewrite_expr(scrut, aliases, scopes);
            for arm in arms {
                for p in &mut arm.pats {
                    rewrite_pattern_paths(p, aliases, scopes);
                }
                scopes.push(HashSet::new());
                for p in &arm.pats {
                    bind_pattern(scopes, p);
                }
                if let Some(g) = &mut arm.guard {
                    rewrite_expr(g, aliases, scopes);
                }
                rewrite_expr(&mut arm.body, aliases, scopes);
                scopes.pop();
            }
        }
        ExprKind::Range { start, end, .. } => {
            rewrite_expr(start, aliases, scopes);
            rewrite_expr(end, aliases, scopes);
        }
        ExprKind::Index { base, index } => {
            rewrite_expr(base, aliases, scopes);
            rewrite_expr(index, aliases, scopes);
        }
        ExprKind::MacroCall { args, .. } => {
            for a in args {
                rewrite_expr(a, aliases, scopes);
            }
        }
        ExprKind::If {
            cond,
            then_block,
            else_block,
        } => {
            rewrite_expr(cond, aliases, scopes);
            rewrite_block(then_block, aliases, scopes);
            rewrite_else(else_block, aliases, scopes);
        }
        ExprKind::IfLet {
            pat,
            expr,
            then_block,
            else_block,
        } => {
            rewrite_expr(expr, aliases, scopes);
            rewrite_pattern_paths(pat, aliases, scopes);
            scopes.push(HashSet::new());
            bind_pattern(scopes, pat);
            rewrite_block(then_block, aliases, scopes);
            scopes.pop();
            rewrite_else(else_block, aliases, scopes);
        }
        ExprKind::Block(b) => rewrite_block(b, aliases, scopes),
        ExprKind::Assign { target, value } => {
            rewrite_expr(target, aliases, scopes);
            rewrite_expr(value, aliases, scopes);
        }
    }
}

fn rewrite_else(else_block: &mut Option<Box<ElseBranch>>, aliases: &Aliases, scopes: &mut Scopes) {
    match else_block.as_deref_mut() {
        Some(ElseBranch::Block(b)) => rewrite_block(b, aliases, scopes),
        Some(ElseBranch::If(inner)) => rewrite_expr(inner, aliases, scopes),
        None => {}
    }
}

/// Replace `alias::rest` / bare `alias` with the alias target, unless shadowed.
fn rewrite_path(segs: &mut Vec<String>, aliases: &Aliases, scopes: &Scopes) {
    if segs.is_empty() {
        return;
    }
    let head = &segs[0];
    if is_local(scopes, head) {
        return;
    }
    if let Some(target) = aliases.get(head) {
        let mut new = target.clone();
        new.extend(segs.iter().skip(1).cloned());
        *segs = new;
    }
}

/// Rewrite the leading path of variant patterns (bindings never alias).
fn rewrite_pattern_paths(p: &mut Pattern, aliases: &Aliases, scopes: &Scopes) {
    match p {
        Pattern::Path(segs) => rewrite_path(segs, aliases, scopes),
        Pattern::TupleVariant { path, elems } => {
            rewrite_path(path, aliases, scopes);
            for e in elems {
                rewrite_pattern_paths(e, aliases, scopes);
            }
        }
        Pattern::StructVariant { path, fields, .. } => {
            rewrite_path(path, aliases, scopes);
            for (_, fp) in fields {
                rewrite_pattern_paths(fp, aliases, scopes);
            }
        }
        _ => {}
    }
}

fn bind(scopes: &mut Scopes, name: String) {
    if let Some(top) = scopes.last_mut() {
        top.insert(name);
    }
}

fn bind_pattern(scopes: &mut Scopes, p: &Pattern) {
    match p {
        Pattern::Binding(n, _) => bind(scopes, n.clone()),
        Pattern::TupleVariant { elems, .. } => {
            for e in elems {
                bind_pattern(scopes, e);
            }
        }
        Pattern::StructVariant { fields, .. } => {
            for (_, fp) in fields {
                bind_pattern(scopes, fp);
            }
        }
        _ => {}
    }
}

fn is_local(scopes: &Scopes, name: &str) -> bool {
    scopes.iter().any(|s| s.contains(name))
}

/// Locate the file backing `mod <name>;` and the directory for its children,
/// plus whether it is a declaration-only `.ruai` file. Search order:
/// `dir/name.rua`, `dir/name/mod.rua`, then the `.ruai` equivalents.
fn resolve_mod_file(dir: &Path, name: &str) -> Result<(PathBuf, PathBuf, bool), String> {
    let child_dir = dir.join(name);
    let flat = dir.join(format!("{name}.rua"));
    if flat.is_file() {
        return Ok((flat, child_dir, false));
    }
    let nested = child_dir.join("mod.rua");
    if nested.is_file() {
        return Ok((nested, child_dir, false));
    }
    let flat_decl = dir.join(format!("{name}.ruai"));
    if flat_decl.is_file() {
        return Ok((flat_decl, child_dir, true));
    }
    let nested_decl = child_dir.join("mod.ruai");
    if nested_decl.is_file() {
        return Ok((nested_decl, child_dir, true));
    }
    Err(format!(
        "cannot find file for module `{}` (looked for {}, {} and their `.ruai` forms)",
        name,
        flat.display(),
        nested.display()
    ))
}

/// Recursively mark nested inline modules of a `.ruai` module as declaration-only
/// (file sub-modules are marked at load time in `resolve_modules`).
fn mark_decl(items: &mut [Item]) {
    for it in items.iter_mut() {
        if let Item::Mod(m) = it {
            m.is_decl = true;
            mark_decl(&mut m.items);
        }
    }
}
