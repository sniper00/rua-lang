//! Scope-aware name resolution for go-to-definition and hover.
//!
//! # Architecture
//!
//! Two-phase resolution, tried in order:
//!
//! **S1 — Local scope resolution** (self-contained, no symbol table needed):
//! Walks the ancestor chain from the use-site Ident token outward, collecting
//! bindings from each scope-introducing node (`Block`, `FnDecl` params,
//! `ForStmt`, `IfExpr`/`WhileStmt` let-patterns, `MatchArm` patterns).
//! Innermost scope wins; within the same block, later `let` bindings shadow
//! earlier ones.
//!
//! **S2 — Path / item resolution** (uses the symbol table):
//! When no local binding matches, resolves the use-site as a reference to a
//! definition symbol (function, struct, enum, trait, module, use alias, etc.)
//! by walking the module context and matching against the collected symbols.
//!
//! # Scope (v2)
//!
//! - ✅ Local variables: fn params, `let`, `for` var, `if let`/`while let`/
//!   `match` pattern bindings, resolved by lexical scope + shadowing.
//! - ✅ Path/item resolution: `Type`, `Enum::Variant`, `mod::item`, free
//!   functions, `use` aliases resolved to a single `Symbol` via the module
//!   tree + current module context.
//! - ✅ Cursor on a path-segment: resolves to that segment's definition
//!   (e.g. cursor on `geo` in `geo::Point` → jumps to `mod geo`).
//! - ❌ Member access (`x.field` / `x.method()`): returns `None` (no type
//!   inference — v3).
//! - ❌ Cross-file: CST contains only the current file (file modules
//!   `mod x;` have their body elsewhere). v2 = same-file semantic resolution.
//! - ❌ Type inference: `x.field` where `x` is a value — needs type info to
//!   resolve the field, not done here.
//!
//! # Robustness
//!
//! - All tree accessors return `Option`; `resolve_at` never panics.
//! - Resolution failure returns `None` — the LSP layer shows nothing (no
//!   fallback to the old name-matching heuristic).
//! - `self`, built-in constructors (`Some`, `Ok`, `Err`, `None`), macro
//!   *names* (`vec!`, `println!`), and keywords are not resolved. Macro
//!   *arguments* are ordinary expressions and resolve normally.

use rowan::TextSize;

use crate::ast::{
    AstNode, Block, FnDecl, ForStmt, IfExpr, Item, LetStmt, MatchArm, ModDecl, Named, Pattern,
    PatternKind, SourceFile, Stmt, WhileStmt,
};
use crate::kind::SyntaxKind;
use crate::symbols::Symbol;
use crate::SyntaxNode;

// --- public types -----------------------------------------------------------

/// Classifies the kind of reference that was resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    /// A local variable binding (fn param, `let`, `for` var, pattern binding).
    Local,
    /// A top-level / module-level item (fn, struct, enum, trait, impl, module, etc.).
    Item,
}

/// The result of resolving an identifier use-site to its definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub kind: RefKind,
    /// Byte range of the definition name token (jump target / highlight).
    pub target_range: (usize, usize),
    /// Hover text: `"local <name>"` for locals; `Symbol::detail` for items.
    pub detail: String,
}

// --- public API -------------------------------------------------------------

/// Resolve the identifier at byte `offset` in `file` to its definition.
///
/// Returns `None` when:
/// - The offset doesn't land on an [`SyntaxKind::Ident`] token,
/// - The ident is a member access (`x.field` / `x.method()`),
/// - The ident is `self`, a built-in constructor, or a macro name,
/// - No binding or symbol matches in any scope.
pub fn resolve_at(file: &SourceFile, symbols: &[Symbol], offset: usize) -> Option<Resolution> {
    // 1. Find the ident token at the offset.
    let hit = crate::symbols::ident_at_offset(file, offset)?;
    let name = &hit.text;

    // 2. Bail out for member access, self, builtins, macros.
    if is_member_access(file, offset) {
        return None;
    }
    if is_self_or_builtin(name) {
        return None;
    }
    if is_macro_name(file, offset) {
        return None;
    }

    // 3. S1: try local scope resolution.
    if let Some(res) = resolve_local(file, offset, name) {
        return Some(res);
    }

    // 4. S2: try path / item resolution.
    if let Some(res) = resolve_item(file, symbols, offset, name) {
        return Some(res);
    }

    None
}

/// Resolve the canonical definition at byte `offset` — whether the cursor is
/// on a *use-site* (delegates to [`resolve_at`]) or on the *definition name*
/// itself.
///
/// This is the single entry-point for "what is being defined/referenced here?"
/// and is used by go-to-definition, find-references, and rename.
///
/// Returns `None` when the offset doesn't land on an ident, or the ident
/// cannot be resolved (keyword, builtin, macro, unknown name, etc.).
pub fn definition_at(
    file: &SourceFile,
    symbols: &[Symbol],
    offset: usize,
) -> Option<Resolution> {
    // 1. FIRST check whether the cursor is on a definition name itself.
    //    This must happen before resolve_at because resolve_at can
    //    accidentally resolve a definition name to an outer binding
    //    (e.g. the name token of `let x = 2` inside a block that also
    //    has an outer `let x = 1`).
    if let Some(hit) = crate::symbols::ident_at_offset(file, offset)
        && let Some(token) = token_at_offset(file.syntax(), offset)
        && let Some(parent) = token.parent()
    {
        let name = &hit.text;
        if let Some(def) = definition_name_at(&parent, offset, name, symbols) {
            return Some(def);
        }
    }

    // 2. Fall back to use-site resolution.
    resolve_at(file, symbols, offset)
}

/// When `offset` is on a definition name (the first Ident child of a
/// definition-introducing node), return a self-referential [`Resolution`].
fn definition_name_at(
    parent: &SyntaxNode,
    offset: usize,
    name: &str,
    symbols: &[Symbol],
) -> Option<Resolution> {

    match parent.kind() {
        // --- local definitions ---

        SyntaxKind::LetStmt => {
            // The first direct Ident child is the binding name.
            let name_tok = first_ident_child(parent)?;
            if token_range_contains(&name_tok, offset) {
                let r = name_tok.text_range();
                return Some(Resolution {
                    kind: RefKind::Local,
                    target_range: (usize::from(r.start()), usize::from(r.end())),
                    detail: format!("local {}", name),
                });
            }
        }
        SyntaxKind::Param => {
            let name_tok = first_ident_child(parent)?;
            if token_range_contains(&name_tok, offset) {
                let r = name_tok.text_range();
                return Some(Resolution {
                    kind: RefKind::Local,
                    target_range: (usize::from(r.start()), usize::from(r.end())),
                    detail: format!("local {}", name),
                });
            }
        }
        SyntaxKind::ForExpr => {
            // Only the loop variable (`for VAR in ...`) is a definition.
            if let Some(fs) = ForStmt::cast(parent.clone())
                && let Some(var) = fs.var()
                && token_range_contains(&var, offset)
            {
                let r = var.text_range();
                return Some(Resolution {
                    kind: RefKind::Local,
                    target_range: (usize::from(r.start()), usize::from(r.end())),
                    detail: format!("local {}", name),
                });
            }
        }
        SyntaxKind::Pattern => {
            // Pattern bindings inside `let`, `if let`, `while let`, `match` arms, `for`.
            if let Some(pat) = Pattern::cast(parent.clone()) {
                match pat.kind() {
                    PatternKind::Binding => {
                        if let Some(bind_tok) = pat.binding_name()
                            && token_range_contains(&bind_tok, offset)
                        {
                            let r = bind_tok.text_range();
                            return Some(Resolution {
                                kind: RefKind::Local,
                                target_range: (usize::from(r.start()), usize::from(r.end())),
                                detail: format!("local {}", name),
                            });
                        }
                    }
                    PatternKind::StructVariant => {
                        // Struct-pattern shorthand `P { x, y }`: the field name
                        // token (no sub-pattern) is itself the binding.
                        for (field_name, sub) in pat.struct_fields() {
                            if sub.is_none() && token_range_contains(&field_name, offset) {
                                let r = field_name.text_range();
                                return Some(Resolution {
                                    kind: RefKind::Local,
                                    target_range: (usize::from(r.start()), usize::from(r.end())),
                                    detail: format!("local {}", name),
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // --- item definitions ---

        SyntaxKind::FnDecl
        | SyntaxKind::StructDecl
        | SyntaxKind::EnumDecl
        | SyntaxKind::TraitDecl
        | SyntaxKind::ModDecl
        | SyntaxKind::ExternFn
        | SyntaxKind::FieldDecl
        | SyntaxKind::EnumVariant
        | SyntaxKind::TraitMethod => {
            let name_tok = first_ident_child(parent)?;
            if token_range_contains(&name_tok, offset) {
                let r = name_tok.text_range();
                let rng = (usize::from(r.start()), usize::from(r.end()));
                // Prefer the symbol's own detail if available.
                let detail = symbols
                    .iter()
                    .find(|s| s.name == *name && s.name_range == rng)
                    .map(|s| s.detail.clone())
                    .unwrap_or_else(|| format!("item {}", name));
                return Some(Resolution {
                    kind: RefKind::Item,
                    target_range: rng,
                    detail,
                });
            }
        }

        _ => {}
    }

    None
}

/// Find all references (including the definition itself) within `file` that
/// resolve to the same canonical definition as the identifier at `offset`.
///
/// Uses a name pre-filter for performance: only idents whose text matches the
/// definition name are checked via [`resolve_at`].  Returns byte ranges in
/// ascending position order with no duplicates.
///
/// Returns an empty vec when the offset is not on a resolvable ident, or when
/// the ident is on a member-access / keyword / builtin / macro.
pub fn references_at(
    file: &SourceFile,
    symbols: &[Symbol],
    offset: usize,
) -> Vec<(usize, usize)> {
    // 1. Get the canonical definition (kind + target range).
    let def = match definition_at(file, symbols, offset) {
        Some(d) => d,
        None => return Vec::new(),
    };

    let def_name = {
        let src_text = file.syntax().text().to_string();
        let end = def.target_range.1.min(src_text.len());
        if def.target_range.0 >= end {
            return Vec::new();
        }
        src_text[def.target_range.0..end].to_string()
    };

    // 2. Walk every Ident token in the file; name pre-filter first.
    let mut refs: Vec<(usize, usize)> = Vec::new();
    let root = file.syntax();

    for element in root.descendants_with_tokens() {
        let tok = match element.into_token() {
            Some(t) => t,
            None => continue,
        };
        if tok.kind() != SyntaxKind::Ident {
            continue;
        }
        if tok.text() != def_name.as_str() {
            continue; // name pre-filter
        }

        let r = tok.text_range();
        let range = (usize::from(r.start()), usize::from(r.end()));

        // Definition site itself — always include.
        if range == def.target_range {
            refs.push(range);
            continue;
        }

        // Use-site or inner definition — resolve and check target.
        // We use definition_at (not resolve_at) so that tokens on inner
        // definition names are correctly identified (resolve_at would
        // resolve an inner `let x = 2` name token to an outer `let x = 1`).
        let tok_offset = usize::from(r.start());
        if let Some(res) = definition_at(file, symbols, tok_offset)
            && res.kind == def.kind && res.target_range == def.target_range
        {
            refs.push(range);
        }
    }

    // 3. Deduplicate and sort by start position.
    refs.sort_by_key(|r| r.0);
    refs.dedup();
    refs
}

// --- rename helpers ----------------------------------------------------------

/// Error returned by [`rename_edits`] when renaming is not possible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameError {
    /// No references found at this position (not a definition or use-site).
    NoReferences,
    /// The new name is not a valid Rua identifier.
    InvalidName,
}

/// Produce a list of `(start, end, replacement_text)` edits that rename the
/// identifier at `offset` to `new_name`.  Returns an error when the position
/// is not resolvable or the new name is invalid.
///
/// The returned ranges correspond exactly to the Ident token boundaries of
/// every reference (definition included).  Callers should replace each range
/// with `new_name` as a whole-word substitution — no string search needed.
pub fn rename_edits(
    file: &SourceFile,
    symbols: &[Symbol],
    offset: usize,
    new_name: &str,
) -> Result<Vec<(usize, usize, String)>, RenameError> {
    if !is_valid_ident(new_name) {
        return Err(RenameError::InvalidName);
    }

    let refs = references_at(file, symbols, offset);
    if refs.is_empty() {
        return Err(RenameError::NoReferences);
    }

    Ok(refs
        .into_iter()
        .map(|(start, end)| (start, end, new_name.to_string()))
        .collect())
}

/// Rua language keywords. Used by [`is_valid_ident`] to reject illegal rename
/// targets and re-used by the LSP completion provider (plus the literals
/// `self`/`true`/`false`, which are handled separately here).
pub const RUA_KEYWORDS: &[&str] = &[
    "fn", "let", "mut", "struct", "enum", "trait", "impl", "match", "if", "else", "for",
    "while", "loop", "return", "pub", "use", "mod", "as", "extern", "break", "continue", "in",
    "where",
];

/// Return `true` when `name` is a valid Rua identifier that can be used as a
/// new name in a rename operation.
///
/// Rules:
/// - Non-empty, ASCII-only.
/// - First character: `[A-Za-z_]`.
/// - Remaining characters: `[A-Za-z0-9_]`.
/// - Not a Rua keyword, `self`, `true`, or `false`.
pub fn is_valid_ident(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let bytes = name.as_bytes();
    // First char: alphabetic or underscore.
    if !bytes[0].is_ascii_alphabetic() && bytes[0] != b'_' {
        return false;
    }
    // Remaining chars: alphanumeric or underscore.
    if !bytes.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_') {
        return false;
    }
    // Not a keyword or reserved word.
    if RUA_KEYWORDS.contains(&name) || matches!(name, "self" | "true" | "false") {
        return false;
    }
    true
}

// --- scope enumeration (completion) ----------------------------------------

/// Collect every local binding visible at `offset`, walking the ancestor scope
/// chain outward (same visibility rules as [`resolve_local`], but gathering all
/// names instead of matching one). Inner bindings shadow outer ones with the
/// same name; the returned list is deduplicated keeping the innermost.
///
/// Used by completion to offer in-scope variables. Returns `(name, def_range)`
/// pairs; the caller enriches the detail (e.g. inferred type) from the range.
pub fn locals_in_scope(file: &SourceFile, offset: usize) -> Vec<(String, (usize, usize))> {
    let mut out: Vec<(String, (usize, usize))> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let token = match token_at_offset_any(file.syntax(), offset) {
        Some(t) => t,
        None => return out,
    };

    let mut push = |name: String, range: (usize, usize), out: &mut Vec<_>| {
        if seen.insert(name.clone()) {
            out.push((name, range));
        }
    };

    let mut ancestor = token.parent();
    while let Some(node) = ancestor {
        match node.kind() {
            SyntaxKind::Block => {
                if let Some(block) = Block::cast(node.clone()) {
                    for stmt in block.stmts() {
                        if let Stmt::Let(lt) = stmt {
                            // A `let` binding is visible only after its whole
                            // statement (not in its own initializer).
                            if usize::from(lt.syntax().text_range().end()) > offset {
                                continue;
                            }
                            for (n, r) in let_bindings(&lt) {
                                push(n, r, &mut out);
                            }
                        }
                    }
                }
            }
            SyntaxKind::FnDecl => {
                if let Some(f) = FnDecl::cast(node.clone()) {
                    for param in f.params() {
                        if let Some(n) = param.name()
                            && n.kind() == SyntaxKind::Ident
                        {
                            let r = n.text_range();
                            push(
                                n.text().to_string(),
                                (usize::from(r.start()), usize::from(r.end())),
                                &mut out,
                            );
                        }
                    }
                }
            }
            SyntaxKind::ForExpr => {
                if let Some(f) = ForStmt::cast(node.clone())
                    && let Some(body) = f.body()
                    && offset_in_range(offset, body.syntax())
                    && let Some(v) = f.var()
                {
                    let r = v.text_range();
                    push(
                        v.text().to_string(),
                        (usize::from(r.start()), usize::from(r.end())),
                        &mut out,
                    );
                }
            }
            SyntaxKind::IfExpr => {
                if let Some(ife) = IfExpr::cast(node.clone())
                    && ife.is_if_let()
                    && let Some(then_block) = ife.then_block()
                    && offset_in_range(offset, then_block.syntax())
                    && let Some(pat) = ife.let_pattern()
                {
                    let mut binds = Vec::new();
                    collect_pattern_bindings(&pat, &mut binds);
                    for (n, r) in binds {
                        push(n, r, &mut out);
                    }
                }
            }
            SyntaxKind::WhileExpr => {
                if let Some(w) = WhileStmt::cast(node.clone())
                    && w.is_while_let()
                    && let Some(body) = w.body()
                    && offset_in_range(offset, body.syntax())
                    && let Some(pat) = w.let_pattern()
                {
                    let mut binds = Vec::new();
                    collect_pattern_bindings(&pat, &mut binds);
                    for (n, r) in binds {
                        push(n, r, &mut out);
                    }
                }
            }
            SyntaxKind::MatchArm => {
                if let Some(arm) = MatchArm::cast(node.clone())
                    && let Some(body) = arm.body()
                    && offset_in_range(offset, body.syntax())
                {
                    for pat in arm.patterns() {
                        let mut binds = Vec::new();
                        collect_pattern_bindings(&pat, &mut binds);
                        for (n, r) in binds {
                            push(n, r, &mut out);
                        }
                    }
                }
            }
            _ => {}
        }
        ancestor = node.parent();
    }

    out
}

/// Whether `offset` falls within `node`'s text range (inclusive of both ends,
/// so a cursor at the closing brace still counts as inside the scope).
fn offset_in_range(offset: usize, node: &SyntaxNode) -> bool {
    let r = node.text_range();
    offset >= usize::from(r.start()) && offset <= usize::from(r.end())
}

// --- S1: local scope resolution --------------------------------------------

/// Walk the ancestor chain from the ident at `offset`, collecting bindings
/// from each scope-introducing node.  Innermost scope wins.
fn resolve_local(file: &SourceFile, offset: usize, name: &str) -> Option<Resolution> {
    // Locate the SyntaxToken at the offset (via the rowan tree directly).
    let token = token_at_offset(file.syntax(), offset)?;

    // Walk ancestors outward; test each scope node.
    let mut ancestor = token.parent();
    while let Some(node) = ancestor {
        match node.kind() {
            SyntaxKind::Block => {
                if let Some(res) = find_in_block(&node, offset, name) {
                    return Some(res);
                }
            }
            SyntaxKind::FnDecl => {
                if let Some(res) = find_in_fn_params(&node, name) {
                    return Some(res);
                }
            }
            SyntaxKind::ForExpr => {
                if let Some(res) = find_in_for(&node, offset, name) {
                    return Some(res);
                }
            }
            SyntaxKind::IfExpr => {
                if let Some(res) = find_in_if_let(&node, offset, name) {
                    return Some(res);
                }
            }
            SyntaxKind::WhileExpr => {
                if let Some(res) = find_in_while_let(&node, offset, name) {
                    return Some(res);
                }
            }
            SyntaxKind::MatchArm => {
                if let Some(res) = find_in_match_arm(&node, offset, name) {
                    return Some(res);
                }
            }
            _ => {}
        }
        ancestor = node.parent();
    }

    None
}

// --- S1 helpers: binding collectors per scope node --------------------------

/// Collect `let` bindings from a `Block`. Only bindings whose *entire*
/// `LetStmt` ends before `use_offset` are visible (a `let` binding is not
/// visible in its own initializer, and `let x = x` resolves the RHS `x` to
/// the outer scope).
fn find_in_block(block_node: &SyntaxNode, use_offset: usize, name: &str) -> Option<Resolution> {
    let block = Block::cast(block_node.clone())?;
    // Iterate statements; collect let bindings.
    let mut candidate: Option<((usize, usize), String)> = None;
    for stmt in block.stmts() {
        if let Stmt::Let(lt) = stmt {
            let stmt_end = usize::from(lt.syntax().text_range().end());
            // Binding is only visible AFTER the entire let statement.
            if stmt_end > use_offset {
                // use_offset is inside or before this let — stop looking
                // (subsequent bindings are even further away).
                break;
            }
            // Collect all binding names from this let (simple name or destructure).
            let bindings = let_bindings(&lt);
            for (bind_name, bind_range) in bindings {
                if bind_name == name {
                    candidate = Some((bind_range, format!("local {}", bind_name)));
                }
            }
        }
    }
    candidate.map(|(range, detail)| Resolution {
        kind: RefKind::Local,
        target_range: range,
        detail,
    })
}

/// Extract all binding names from a `LetStmt`, handling both simple
/// (`let x = ...`) and destructured (`let (x, y) = ...`) patterns.
/// For now the parser uses `name_token()` for simple lets; destructured
/// patterns may have a `Pattern` child.
fn let_bindings(lt: &LetStmt) -> Vec<(String, (usize, usize))> {
    let mut bindings = Vec::new();

    // Simple case: the let's name token is the direct Ident child.
    // This covers `let x = ...`, `let mut x = ...`.
    if let Some(tok) = lt.name()
        && tok.kind() == SyntaxKind::Ident
    {
        let r = tok.text_range();
        bindings.push((
            tok.text().to_string(),
            (usize::from(r.start()), usize::from(r.end())),
        ));
    }

    // Also check for Pattern children (destructured let: `let (x, y) = ...`).
    for child in lt.syntax().children() {
        if let Some(pat) = Pattern::cast(child) {
            collect_pattern_bindings(&pat, &mut bindings);
        }
    }

    bindings
}

/// Recursively collect all `Binding` names from a [`Pattern`] tree.
fn collect_pattern_bindings(pat: &Pattern, out: &mut Vec<(String, (usize, usize))>) {
    match pat.kind() {
        PatternKind::Binding => {
            if let Some(tok) = pat.binding_name() {
                let r = tok.text_range();
                out.push((
                    tok.text().to_string(),
                    (usize::from(r.start()), usize::from(r.end())),
                ));
            }
        }
        PatternKind::TupleVariant => {
            for sub in pat.sub_patterns() {
                collect_pattern_bindings(&sub, out);
            }
        }
        PatternKind::StructVariant => {
            for (field_name, sub_pat) in pat.struct_fields() {
                let r = field_name.text_range();
                match sub_pat {
                    Some(sp) => collect_pattern_bindings(&sp, out),
                    None => {
                        // Shorthand: `Point { x, y }` — the field name IS the binding.
                        out.push((
                            field_name.text().to_string(),
                            (usize::from(r.start()), usize::from(r.end())),
                        ));
                    }
                }
            }
        }
        // Wildcard, Literal, Range, Path — no bindings.
        _ => {}
    }
}

/// Check fn params. All params are visible in the entire body (the ancestor
/// walk ensures we're inside the body).
fn find_in_fn_params(fn_node: &SyntaxNode, name: &str) -> Option<Resolution> {
    let f = FnDecl::cast(fn_node.clone())?;
    for param in f.params() {
        if let Some(n) = param.name()
            && n.text() == name
            && n.kind() == SyntaxKind::Ident
        {
            let r = n.text_range();
            return Some(Resolution {
                kind: RefKind::Local,
                target_range: (usize::from(r.start()), usize::from(r.end())),
                detail: format!("local {}", name),
            });
        }
    }
    None
}

/// Check the `for` loop variable. Visible only inside the body.
fn find_in_for(for_node: &SyntaxNode, use_offset: usize, name: &str) -> Option<Resolution> {
    let f = ForStmt::cast(for_node.clone())?;
    // The loop variable is only visible inside the body.
    let body = f.body()?;
    let body_range = body.syntax().text_range();
    if use_offset < usize::from(body_range.start()) || use_offset > usize::from(body_range.end()) {
        return None;
    }
    if let Some(v) = f.var()
        && v.text() == name
    {
        let r = v.text_range();
        return Some(Resolution {
            kind: RefKind::Local,
            target_range: (usize::from(r.start()), usize::from(r.end())),
            detail: format!("local {}", name),
        });
    }
    None
}

/// Check `if let` pattern bindings. Visible only in the `then` block.
fn find_in_if_let(if_node: &SyntaxNode, use_offset: usize, name: &str) -> Option<Resolution> {
    let ife = IfExpr::cast(if_node.clone())?;
    if !ife.is_if_let() {
        return None;
    }
    let then_block = ife.then_block()?;
    let then_range = then_block.syntax().text_range();
    if use_offset < usize::from(then_range.start()) || use_offset > usize::from(then_range.end()) {
        return None;
    }
    let pat = ife.let_pattern()?;
    let mut bindings = Vec::new();
    collect_pattern_bindings(&pat, &mut bindings);
    bindings
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, range)| Resolution {
            kind: RefKind::Local,
            target_range: range,
            detail: format!("local {}", name),
        })
}

/// Check `while let` pattern bindings. Visible only in the body.
fn find_in_while_let(while_node: &SyntaxNode, use_offset: usize, name: &str) -> Option<Resolution> {
    let w = WhileStmt::cast(while_node.clone())?;
    if !w.is_while_let() {
        return None;
    }
    let body = w.body()?;
    let body_range = body.syntax().text_range();
    if use_offset < usize::from(body_range.start()) || use_offset > usize::from(body_range.end()) {
        return None;
    }
    let pat = w.let_pattern()?;
    let mut bindings = Vec::new();
    collect_pattern_bindings(&pat, &mut bindings);
    bindings
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, range)| Resolution {
            kind: RefKind::Local,
            target_range: range,
            detail: format!("local {}", name),
        })
}

/// Check match arm pattern bindings. Visible only in that arm's body.
fn find_in_match_arm(arm_node: &SyntaxNode, use_offset: usize, name: &str) -> Option<Resolution> {
    let arm = MatchArm::cast(arm_node.clone())?;
    // Check whether use_offset is in the arm's body (after `=>`).
    let body = arm.body()?;
    let body_range = body.syntax().text_range();
    if use_offset < usize::from(body_range.start()) || use_offset > usize::from(body_range.end()) {
        return None;
    }
    for pat in arm.patterns() {
        let mut bindings = Vec::new();
        collect_pattern_bindings(&pat, &mut bindings);
        if let Some((_, range)) = bindings.into_iter().find(|(n, _)| n == name) {
            return Some(Resolution {
                kind: RefKind::Local,
                target_range: range,
                detail: format!("local {}", name),
            });
        }
    }
    None
}

// --- S2: path / item resolution --------------------------------------------

/// Resolve a use-site ident as a reference to a definition symbol.
///
/// Strategy:
/// 1. Walk ancestors to determine the current module context.
/// 2. Parse the full path at the cursor position.
/// 3. Resolve each segment: check `use` aliases first, then match against
///    symbols in the appropriate container.
fn resolve_item(
    file: &SourceFile,
    symbols: &[Symbol],
    offset: usize,
    name: &str,
) -> Option<Resolution> {
    // Determine the module context from ancestor ModDecl nodes.
    let mod_ctx = module_context(file, offset);

    // Collect use aliases visible from this position.
    let aliases = collect_use_aliases(file, offset, &mod_ctx);

    // Try to parse the ident as part of a path expression or type path.
    // If the ident is inside a PathExpr/PathType, we can resolve multi-segment
    // paths. Otherwise, fall back to single-segment item resolution.
    match path_context(file, offset) {
        Some(path_ctx) => {
            let segment_index = path_ctx
                .segments
                .iter()
                .position(|(seg_name, _)| seg_name == name)?;

            if segment_index == 0 && path_ctx.segments.len() == 1 {
                // Single-segment path: try use alias first, then symbol match.
                if let Some(res) = resolve_via_alias(name, &aliases, symbols) {
                    return Some(res);
                }
                return resolve_single_segment(name, &mod_ctx, symbols);
            }

            if segment_index == 0 {
                // Cursor is on the first segment of a multi-segment path.
                if let Some(res) = resolve_via_alias(name, &aliases, symbols) {
                    return Some(res);
                }
                return resolve_single_segment(name, &mod_ctx, symbols);
            }

            // Cursor is on a later segment — resolve the prefix chain.
            resolve_path_segment(
                &path_ctx.segments, segment_index, name, &mod_ctx, &aliases, symbols,
            )
        }
        None => {
            // Not in a PathExpr/PathType: try single-segment item resolution
            // (e.g. ident in StructLitExpr path, or direct child of CallExpr).
            if let Some(res) = resolve_via_alias(name, &aliases, symbols) {
                return Some(res);
            }
            resolve_single_segment(name, &mod_ctx, symbols)
        }
    }
}

/// Context for a path at the cursor position.
pub(crate) struct PathContext {
    /// Each segment's (name, byte_range).
    pub(crate) segments: Vec<(String, (usize, usize))>,
    /// Whether this is a type path (`PathType`) or expression path (`PathExpr`).
    /// Reserved for future use (e.g. filtering type-only vs value-only symbols).
    #[allow(dead_code)]
    pub(crate) is_type_path: bool,
}

/// Extract path context from the tree at `offset`.
pub(crate) fn path_context(file: &SourceFile, offset: usize) -> Option<PathContext> {
    let token = token_at_offset(file.syntax(), offset)?;
    let parent = token.parent()?;

    match parent.kind() {
        // Expression path: `foo::bar::baz`
        SyntaxKind::PathExpr => {
            let segments: Vec<_> = parent
                .children_with_tokens()
                .filter_map(|e| e.into_token())
                .filter(|t| t.kind() == SyntaxKind::Ident || t.kind() == SyntaxKind::KwSelf)
                .map(|t| {
                    let r = t.text_range();
                    (t.text().to_string(), (usize::from(r.start()), usize::from(r.end())))
                })
                .collect();
            if segments.is_empty() {
                return None;
            }
            Some(PathContext {
                segments,
                is_type_path: false,
            })
        }
        // Type path: `Vec<T>`, `std::collections::HashMap<K,V>`
        SyntaxKind::PathType => {
            let segments: Vec<_> = parent
                .children_with_tokens()
                .filter_map(|e| e.into_token())
                .filter(|t| t.kind() == SyntaxKind::Ident)
                .map(|t| {
                    let r = t.text_range();
                    (t.text().to_string(), (usize::from(r.start()), usize::from(r.end())))
                })
                .collect();
            if segments.is_empty() {
                return None;
            }
            Some(PathContext {
                segments,
                is_type_path: true,
            })
        }
        _ => None,
    }
}

/// A resolved `use` alias: `use foo::bar as baz` → alias `baz` → path `["foo","bar"]`.
struct UseAlias {
    /// The alias name (e.g. `baz` in `use foo::bar as baz`).
    name: String,
    /// The full path the alias points to.
    path: Vec<String>,
}

/// Collect visible `use` aliases by walking ancestor blocks/modules.
fn collect_use_aliases(
    _file: &SourceFile,
    offset: usize,
    mod_ctx: &[String],
) -> Vec<UseAlias> {
    // Walk ancestors looking for Item::Use declarations.
    // For now, walk the items in the current module context level.
    // Full implementation would also handle wildcard imports and nested use groups.
    let mut aliases = Vec::new();

    // Walk from the use-site outward through the item tree.
    // We collect uses that are visible: in the same module scope and textually
    // before the use-site.
    let file_root = _file;

    // Find the right module body to search.
    let items_iter = items_in_module(file_root, mod_ctx);

    for item in items_iter {
        if let Item::Use(use_decl) = item {
            // Check if this use is textually before the use-site.
            let use_end = usize::from(use_decl.syntax().text_range().end());
            if use_end > offset {
                continue; // use appears after the cursor — not yet visible.
            }
            for imp in use_decl.imports() {
                let path: Vec<String> = imp
                    .path
                    .iter()
                    .map(|t| t.text().to_string())
                    .collect();
                let alias_name = imp
                    .alias
                    .as_ref()
                    .map(|t| t.text().to_string())
                    .unwrap_or_else(|| path.last().cloned().unwrap_or_default());
                if !alias_name.is_empty() {
                    aliases.push(UseAlias {
                        name: alias_name,
                        path,
                    });
                }
            }
        }
    }

    aliases
}

/// Get items visible in a specific module context.
fn items_in_module(file: &SourceFile, mod_ctx: &[String]) -> Vec<Item> {
    if mod_ctx.is_empty() {
        return file.items().collect();
    }

    // Descend into the module path.
    fn descend(items: Vec<Item>, path: &[String]) -> Vec<Item> {
        if path.is_empty() {
            return items;
        }
        let target = &path[0];
        for item in &items {
            if let Item::Mod(m) = item
                && m.name_text().as_deref() == Some(target.as_str())
                && !m.is_file()
            {
                let child_items: Vec<Item> = m.items().collect();
                return descend(child_items, &path[1..]);
            }
        }
        Vec::new()
    }

    let top: Vec<Item> = file.items().collect();
    descend(top, mod_ctx)
}

/// Try to resolve a name via a `use` alias.
fn resolve_via_alias(
    name: &str,
    aliases: &[UseAlias],
    symbols: &[Symbol],
) -> Option<Resolution> {
    for alias in aliases {
        if alias.name == name {
            // The alias path gives us the full path to resolve.
            let target = alias.path.last()?;
            // The definition module is the alias path minus the last segment.
            let def_mod: Vec<String> = alias.path[..alias.path.len() - 1].to_vec();
            if let Some(sym) = find_symbol(target, &def_mod, symbols) {
                return Some(Resolution {
                    kind: RefKind::Item,
                    target_range: sym.name_range,
                    detail: sym.detail.clone(),
                });
            }
        }
    }
    None
}

/// Resolve a single-segment name against symbols in the module context.
fn resolve_single_segment(
    name: &str,
    mod_ctx: &[String],
    symbols: &[Symbol],
) -> Option<Resolution> {
    // Try the current module first, then walk up through parent modules.
    let mut ctx = mod_ctx.to_vec();
    loop {
        if let Some(sym) = find_symbol(name, &ctx, symbols) {
            return Some(Resolution {
                kind: RefKind::Item,
                target_range: sym.name_range,
                detail: sym.detail.clone(),
            });
        }
        if ctx.is_empty() {
            break;
        }
        ctx.pop();
    }
    None
}

/// Find a symbol by name and container path.
fn find_symbol(name: &str, container: &[String], symbols: &[Symbol]) -> Option<Symbol> {
    symbols
        .iter()
        .find(|s| s.name == name && s.container == container)
        .cloned()
}

/// Resolve a specific segment of a multi-segment path.
fn resolve_path_segment(
    segments: &[(String, (usize, usize))],
    segment_index: usize,
    _name: &str,
    mod_ctx: &[String],
    aliases: &[UseAlias],
    symbols: &[Symbol],
) -> Option<Resolution> {
    let current_mod = mod_ctx.to_vec();
    let first = &segments[0].0;

    // Try alias first, then direct symbol match for segment 0.
    let sym0 = resolve_symbol(first, &current_mod, aliases, symbols)?;

    if segment_index == 0 {
        return Some(symbol_to_resolution(&sym0));
    }

    // Walk the container tree for subsequent segments.
    let mut current_container = sym0.container.clone();
    current_container.push(sym0.name.clone());

    for (i, (seg_name, _)) in segments.iter().enumerate().skip(1) {
        if i == segment_index {
            // This is the target segment — resolve to a symbol.
            let sym = find_symbol(seg_name, &current_container, symbols)?;
            return Some(symbol_to_resolution(&sym));
        }
        // Intermediate segment: look for ANY symbol (typically Module).
        let _sym = find_symbol(seg_name, &current_container, symbols)?;
        current_container.push(seg_name.clone());
    }

    None
}

/// Resolve a name as a Symbol (not Resolution), trying alias first.
fn resolve_symbol(
    name: &str,
    mod_ctx: &[String],
    aliases: &[UseAlias],
    symbols: &[Symbol],
) -> Option<Symbol> {
    // Try use alias first.
    for alias in aliases {
        if alias.name == name {
            let target = alias.path.last()?;
            let def_mod: Vec<String> = alias.path[..alias.path.len() - 1].to_vec();
            if let Some(sym) = find_symbol(target, &def_mod, symbols) {
                return Some(sym);
            }
        }
    }
    // Try direct module-context match.
    let mut ctx = mod_ctx.to_vec();
    loop {
        if let Some(sym) = find_symbol(name, &ctx, symbols) {
            return Some(sym);
        }
        if ctx.is_empty() {
            break;
        }
        ctx.pop();
    }
    None
}

/// Convert a Symbol into a Resolution.
fn symbol_to_resolution(sym: &Symbol) -> Resolution {
    Resolution {
        kind: RefKind::Item,
        target_range: sym.name_range,
        detail: sym.detail.clone(),
    }
}

// --- module context ---------------------------------------------------------

/// Collect the module path by walking ancestors for `ModDecl` nodes.
/// Returns the path from outermost to innermost (e.g. `["geo", "shapes"]`).
pub(crate) fn module_context(file: &SourceFile, offset: usize) -> Vec<String> {
    let token = match token_at_offset(file.syntax(), offset) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut path = Vec::new();
    let mut ancestor = token.parent();
    while let Some(node) = ancestor {
        if node.kind() == SyntaxKind::ModDecl
            && let Some(m) = ModDecl::cast(node.clone())
            && let Some(name) = m.name_text()
        {
            path.push(name);
        }
        ancestor = node.parent();
    }
    path.reverse();
    path
}

// --- guard predicates --------------------------------------------------------

/// Check if the offset is inside a member-access expression
/// (`x.field` or `x.method()`). These cannot be resolved without type
/// information (v3).
pub fn is_member_access(file: &SourceFile, offset: usize) -> bool {
    let token = match token_at_offset(file.syntax(), offset) {
        Some(t) => t,
        None => return false,
    };
    let parent = match token.parent() {
        Some(p) => p,
        None => return false,
    };
    // The ident is at offset; check if parent is FieldExpr or MethodCallExpr
    // AND we are the field/method name (not the receiver).
    match parent.kind() {
        SyntaxKind::FieldExpr => {
            // The field_name() is the ident after the dot. If our token is that
            // field name, it's a member access.
            if let Some(fe) = crate::ast::FieldExpr::cast(parent.clone())
                && let Some(fn_tok) = fe.field_name()
            {
                let r = fn_tok.text_range();
                return offset >= usize::from(r.start())
                    && offset <= usize::from(r.end());
            }
            false
        }
        SyntaxKind::MethodCallExpr => {
            if let Some(mc) = crate::ast::MethodCallExpr::cast(parent.clone())
                && let Some(mn) = mc.method_name()
            {
                let r = mn.text_range();
                return offset >= usize::from(r.start())
                    && offset <= usize::from(r.end());
            }
            false
        }
        _ => false,
    }
}

/// Check whether the identifier is `self`, a built-in constructor, or a
/// keyword that should not be resolved.
fn is_self_or_builtin(name: &str) -> bool {
    matches!(
        name,
        "self"
            | "Some"
            | "None"
            | "Ok"
            | "Err"
            | "true"
            | "false"
            // Standard library macros — not resolvable.
            | "vec"
            | "println"
            | "print"
            | "format"
            | "assert"
            | "assert_eq"
            | "assert_ne"
            | "panic"
            | "unreachable"
            | "unimplemented"
            | "todo"
            | "dbg"
            | "include_str"
            | "include_bytes"
    )
}

/// Check whether the ident at `offset` is a macro *name* (`println` in
/// `println!(...)`). The macro name is unresolvable, but the macro's
/// *arguments* are ordinary expressions and must resolve normally (e.g. the
/// `x` in `println!("{}", x)` or the `stack` in `stack.len()`). The name token
/// is the only [`SyntaxKind::Ident`] that is a direct child of the
/// `MacroCallExpr` node; arguments live in nested expression nodes.
fn is_macro_name(file: &SourceFile, offset: usize) -> bool {
    let token = match token_at_offset(file.syntax(), offset) {
        Some(t) => t,
        None => return false,
    };
    match token.parent() {
        Some(parent) => parent.kind() == SyntaxKind::MacroCallExpr,
        None => false,
    }
}

// --- low-level helpers ------------------------------------------------------

/// The first direct [`SyntaxKind::Ident`] child token of `node`.
fn first_ident_child(node: &SyntaxNode) -> Option<crate::SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == SyntaxKind::Ident)
}

/// Whether `offset` falls within the text range of `token`.
fn token_range_contains(token: &crate::SyntaxToken, offset: usize) -> bool {
    let r = token.text_range();
    let start = usize::from(r.start());
    let end = usize::from(r.end());
    offset >= start && offset < end
}

/// Find *any* [`SyntaxToken`] at a byte offset (whitespace included), used for
/// scope-walking in completion where the cursor rarely sits on an Ident.
/// Prefers the left token in boundary cases so a cursor at `foo.|` or `x |`
/// stays anchored to the enclosing scope rather than the following token.
fn token_at_offset_any(root: &SyntaxNode, offset: usize) -> Option<crate::SyntaxToken> {
    let max_offset = usize::from(root.text_range().end());
    let offset = offset.min(max_offset);
    let pos = TextSize::from(offset as u32);
    match root.token_at_offset(pos) {
        rowan::TokenAtOffset::Single(t) => Some(t),
        rowan::TokenAtOffset::Between(left, _right) => Some(left),
        rowan::TokenAtOffset::None => None,
    }
}

/// Find the [`SyntaxToken`] at a byte offset, returning the token handle.
/// Returns `None` on whitespace, out-of-bounds, or non-Ident tokens.
/// Prefers the right-hand Ident in boundary cases.
fn token_at_offset(root: &SyntaxNode, offset: usize) -> Option<crate::SyntaxToken> {
    let max_offset = usize::from(root.text_range().end());
    let offset = offset.min(max_offset);
    let pos = TextSize::from(offset as u32);
    let token = match root.token_at_offset(pos) {
        rowan::TokenAtOffset::Single(t) => t,
        rowan::TokenAtOffset::Between(left, right) => {
            if right.kind() == SyntaxKind::Ident {
                right
            } else if left.kind() == SyntaxKind::Ident {
                left
            } else {
                return None;
            }
        }
        rowan::TokenAtOffset::None => return None,
    };
    if token.kind() != SyntaxKind::Ident {
        return None;
    }
    Some(token)
}

// --- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::Analysis;

    fn resolve(src: &str, offset: usize) -> Option<Resolution> {
        let analysis = Analysis::new(src);
        analysis.resolve_at(offset)
    }

    /// Return true if `c` can be part of an identifier (alphanumeric or underscore).
    fn is_ident_char(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    /// Find byte offset of the n-th occurrence of `name` as a standalone
    /// identifier in `src` (not part of a larger word like "i" in "in").
    fn nth_ident(src: &str, name: &str, n: usize) -> usize {
        let bytes = src.as_bytes();
        let mut remaining = n;
        let mut pos = 0;
        loop {
            let found = src[pos..].find(name).expect("identifier not found");
            let abs = pos + found;
            let after = abs + name.len();
            // Check boundaries: preceded and followed by non-ident chars.
            let left_ok = abs == 0 || !is_ident_char(bytes[abs - 1] as char);
            let right_ok = after >= bytes.len() || !is_ident_char(bytes[after] as char);
            if left_ok && right_ok {
                if remaining == 0 {
                    return abs;
                }
                remaining -= 1;
            }
            pos = abs + 1; // advance at least one byte
        }
    }

    /// Offset of the first standalone identifier occurrence.
    fn ident_at(src: &str, name: &str) -> usize {
        nth_ident(src, name, 0)
    }

    /// Offset of the second standalone identifier occurrence.
    fn ident_at2(src: &str, name: &str) -> usize {
        nth_ident(src, name, 1)
    }

    /// Offset of the third standalone identifier occurrence.
    fn ident_at3(src: &str, name: &str) -> usize {
        nth_ident(src, name, 2)
    }

    /// Deprecated: use ident_at / ident_at2 / ident_at3 instead.
    /// These were broken because they matched substrings (e.g. "i" in "in").
    fn offset_of(src: &str, substr: &str) -> usize {
        ident_at(src, substr)
    }
    fn offset_of2(src: &str, substr: &str) -> usize {
        ident_at2(src, substr)
    }
    fn offset_of3(src: &str, substr: &str) -> usize {
        ident_at3(src, substr)
    }

    // --- S1: local scope resolution tests -----------------------------------

    #[test]
    fn resolve_fn_param() {
        // fn f(x: i64) { x }
        //          ^ def   ^ use
        let src = "fn f(x: i64) -> i64 { x }";
        // "x" use is the second "x" in the source.
        let use_off = offset_of2(src, "x");
        let res = resolve(src, use_off).expect("should resolve fn param");
        assert_eq!(res.kind, RefKind::Local);
        // Detail is enriched with the declared type via the compiler bridge.
        assert_eq!(res.detail, "x: i64");
        // Target range should point to the param definition (first "x").
        let def_range = res.target_range;
        assert_eq!(&src[def_range.0..def_range.1], "x");
        // The definition "x" starts at the first occurrence.
        let def_off = offset_of(src, "x");
        assert_eq!(def_range.0, def_off);
    }

    #[test]
    fn resolve_let_binding() {
        let src = "fn f() { let x = 1; x }";
        // "x" use is the third "x" (the one in `x }`). Wait — actually:
        // fn f() { let x = 1; x }
        //                  ^1   ^2
        // let name = "x" at first occurrence, use at second.
        let use_off = offset_of2(src, "x");
        let res = resolve(src, use_off).expect("should resolve let binding");
        assert_eq!(res.kind, RefKind::Local);
        // Detail is enriched with the inferred type via the compiler bridge.
        assert_eq!(res.detail, "let x: i64");
        let def_range = res.target_range;
        assert_eq!(&src[def_range.0..def_range.1], "x");
    }

    #[test]
    fn let_binding_not_visible_in_own_init() {
        // `let x = x` — the RHS `x` should NOT resolve to the let binding.
        // It should look outward. If there's no outer binding, return None.
        let src = "fn f() { let x = x; }";
        // There are two "x": the binding name and the init use.
        let use_off = offset_of2(src, "x"); // the RHS x
        let res = resolve(src, use_off);
        assert!(res.is_none(), "let x = x: RHS should not resolve to the binding itself");
    }

    #[test]
    fn shadowing_inner_block() {
        let src = "fn f() { let x = 1; { let x = 2; x } }";
        // The inner "x" use (last one) should resolve to inner let.
        let use_off = offset_of3(src, "x"); // third x - the use inside inner block
        let res = resolve(src, use_off).expect("should resolve to inner let");
        let def_range = res.target_range;
        // The inner binding "x" value is "2", so the definition `let x = 2` is second x.
        let inner_def = offset_of2(src, "x");
        assert_eq!(def_range.0, inner_def, "should resolve to inner let (let x = 2)");
    }

    #[test]
    fn shadowing_same_block_later_let() {
        let src = "fn f() { let x = 1; let y = x; let x = 2; }";
        // The `x` in `let y = x` is the second "x" overall, should resolve to first let.
        let use_off = offset_of2(src, "x"); // x in `y = x`
        let res = resolve(src, use_off).expect("should resolve to first let");
        let def_range = res.target_range;
        let first_def = offset_of(src, "x");
        assert_eq!(def_range.0, first_def, "should resolve to first let (let x = 1)");
    }

    #[test]
    fn resolve_for_var() {
        let src = "fn f() { for i in 0..10 { i } }";
        // "i" at use (second occurrence, in body) should resolve to loop var.
        let use_off = offset_of2(src, "i");
        let res = resolve(src, use_off).expect("should resolve for var");
        assert_eq!(res.kind, RefKind::Local);
        let def_range = res.target_range;
        let def_off = offset_of(src, "i");
        assert_eq!(def_range.0, def_off);
    }

    #[test]
    fn for_var_not_visible_outside_body() {
        let src = "fn f() { for i in 0..10 { i } i }";
        // The second "i" (outside the body) should NOT resolve to the for var.
        let use_off = offset_of3(src, "i"); // third i - after the body
        let res = resolve(src, use_off);
        assert!(res.is_none(), "for var should not be visible outside body");
    }

    #[test]
    fn resolve_if_let() {
        let src = "fn f() { let opt = 1; if let Some(x) = opt { x } else { 0 } }";
        // "x" in then block.
        let use_off = offset_of2(src, "x"); // second x - the use
        let res = resolve(src, use_off).expect("should resolve if let binding");
        assert_eq!(res.kind, RefKind::Local);
        let def_range = res.target_range;
        let def_off = offset_of(src, "x");
        assert_eq!(def_range.0, def_off);
    }

    #[test]
    fn if_let_not_visible_in_else() {
        let src = "fn f() { if let Some(x) = opt { 1 } else { x } }";
        // "x" in else block should NOT resolve (not visible there).
        let use_off = offset_of2(src, "x"); // second x - in else block
        let res = resolve(src, use_off);
        assert!(res.is_none(), "if let binding should not be visible in else branch");
    }

    #[test]
    fn resolve_while_let() {
        let src = "fn f() { while let Some(v) = pop() { v } }";
        let use_off = offset_of2(src, "v"); // second v - the use
        let res = resolve(src, use_off).expect("should resolve while let binding");
        assert_eq!(res.kind, RefKind::Local);
        let def_range = res.target_range;
        let def_off = offset_of(src, "v");
        assert_eq!(def_range.0, def_off);
    }

    #[test]
    fn resolve_match_arm_binding() {
        let src = "fn f(x: i64) -> i64 { match x { n => n, _ => 0 } }";
        // "n" use (second n) in arm body should resolve to arm pattern.
        let use_off = offset_of2(src, "n"); // second n - the use after =>
        let res = resolve(src, use_off).expect("should resolve match arm binding");
        assert_eq!(res.kind, RefKind::Local);
        let def_range = res.target_range;
        let def_off = offset_of(src, "n");
        assert_eq!(def_range.0, def_off);
    }

    #[test]
    fn match_tuple_variant_binding() {
        let src = "fn f() { match Some(1) { Some(x) => x, None => 0 } }";
        // The use of "x" after => should resolve to the pattern binding.
        let use_off = offset_of2(src, "x"); // second x
        let res = resolve(src, use_off).expect("should resolve tuple variant binding");
        assert_eq!(res.kind, RefKind::Local);
    }

    #[test]
    fn match_struct_variant_binding() {
        let src =
            "struct P { x: i64, y: i64 }\nfn f(p: P) { match p { P { x, y } => x + y } }";
        // x appears: (1) struct field decl, (2) pattern shorthand, (3) body use.
        // The use in `x + y` (3rd x) should resolve to pattern field binding (2nd x).
        let use_off = ident_at3(src, "x"); // third x = use-site in arm body
        let res = resolve(src, use_off);
        assert!(res.is_some(), "should resolve struct pattern binding: {res:?}");
        if let Some(r) = res {
            assert_eq!(r.kind, RefKind::Local);
        }
    }

    // --- S2: item / path resolution tests -----------------------------------

    #[test]
    fn resolve_fn_item() {
        let src = "fn hello() {}\nfn main() { hello() }";
        // "hello" use (second occurrence) should resolve to the fn decl.
        let use_off = offset_of2(src, "hello");
        let res = resolve(src, use_off).expect("should resolve to fn hello");
        assert_eq!(res.kind, RefKind::Item);
        let def_range = res.target_range;
        let def_off = offset_of(src, "hello");
        assert_eq!(def_range.0, def_off);
        assert!(res.detail.contains("hello"));
    }

    #[test]
    fn resolve_struct_item() {
        let src = "struct Point { x: f64 }\nfn f() { let p = Point { x: 0.0 }; p }";
        // "Point" use (second occurrence) should resolve to the struct decl.
        let use_off = offset_of2(src, "Point");
        let res = resolve(src, use_off).expect("should resolve to struct Point");
        assert_eq!(res.kind, RefKind::Item);
        assert!(res.detail.contains("Point"));
    }

    #[test]
    fn resolve_enum_item() {
        let src = "enum Color { Red, Green }\nfn f() -> Color { Color::Red }";
        let use_off = offset_of2(src, "Color"); // second Color - use
        let res = resolve(src, use_off).expect("should resolve to enum Color");
        assert_eq!(res.kind, RefKind::Item);
    }

    #[test]
    fn resolve_module_item() {
        let src = "mod geo { fn area() -> f64 { 0.0 } }\nfn main() { geo::area() }";
        // "geo" use (second occurrence) should resolve to mod decl.
        let use_off = offset_of2(src, "geo");
        let res = resolve(src, use_off).expect("should resolve to mod geo");
        assert_eq!(res.kind, RefKind::Item);
    }

    #[test]
    fn resolve_nested_module_item() {
        let src = "mod geo {\n  pub fn area() -> f64 { 0.0 }\n}\nfn main() { geo::area() }";
        // "area" use — second occurrence.
        let use_off = offset_of2(src, "area");
        let res = resolve(src, use_off).expect("should resolve to geo::area");
        assert_eq!(res.kind, RefKind::Item);
        assert!(res.detail.contains("area"));
    }

    #[test]
    fn resolve_cursor_on_first_path_segment() {
        let src = "mod geo { fn area() -> f64 { 0.0 } }\nfn main() { geo::area() }";
        // Cursor on "geo" (the second occurrence, in `geo::area()`).
        let use_off = offset_of2(src, "geo");
        let res = resolve(src, use_off).expect("cursor on first segment should resolve to mod");
        assert_eq!(res.kind, RefKind::Item);
        assert!(res.detail.contains("mod geo"));
    }

    #[test]
    fn resolve_cursor_on_second_path_segment() {
        let src = "mod geo { fn area() -> f64 { 0.0 } }\nfn main() { geo::area() }";
        // Cursor on "area" (the second occurrence) in path `geo::area()`.
        let use_off = offset_of2(src, "area");
        let res = resolve(src, use_off).expect("cursor on second segment should resolve to fn");
        assert_eq!(res.kind, RefKind::Item);
        assert!(res.detail.contains("area"));
    }

    // --- negative tests: things that should NOT resolve ---------------------

    #[test]
    fn member_access_field_returns_none() {
        // x.field: hovering "field" returns None (v3 — needs type info).
        let src = "fn f() { let p = 1; p.x }";
        // Only one "x" in source. The field access "p.x" parses with "x" inside FieldExpr.
        let use_off = ident_at(src, "x");
        let res = resolve(src, use_off);
        assert!(res.is_none(), "field access should not resolve (no type info), got {res:?}");
    }

    #[test]
    fn method_call_returns_none() {
        let src = "fn f() { let v = 1; v.len() }";
        // Only one "len" in source. The method call "v.len()" parses with "len" inside MethodCallExpr.
        let use_off = ident_at(src, "len");
        let res = resolve(src, use_off);
        assert!(res.is_none(), "method call should not resolve (no type info), got {res:?}");
    }

    #[test]
    fn self_returns_none() {
        let src = "impl S { fn m(&self) -> i64 { self.x } }";
        // "self" in body
        let use_off = offset_of2(src, "self");
        let res = resolve(src, use_off);
        assert!(res.is_none(), "self should not resolve");
    }

    #[test]
    fn builtin_some_returns_none() {
        let src = "fn f() { Some(1) }";
        let use_off = offset_of(src, "Some");
        let res = resolve(src, use_off);
        assert!(res.is_none(), "Some should not resolve");
    }

    #[test]
    fn macro_call_returns_none() {
        let src = "fn f() { println!(\"hi\") }";
        let use_off = offset_of(src, "println");
        let res = resolve(src, use_off);
        assert!(res.is_none(), "println! macro name should not resolve");
    }

    #[test]
    fn macro_argument_local_resolves() {
        // The `x` argument of println! is an ordinary expression and must
        // resolve to its `while let` binding (previously blocked by the
        // blanket macro-call guard).
        let src = "fn f() { while let Some(x) = pop() { println!(\"{}\", x); } }";
        let use_off = offset_of2(src, "x"); // the x inside println!
        let res = resolve(src, use_off).expect("macro arg local should resolve");
        assert_eq!(res.kind, RefKind::Local);
        let def_off = offset_of(src, "x");
        assert_eq!(res.target_range.0, def_off);
    }

    #[test]
    fn macro_argument_receiver_resolves() {
        // The `stack` receiver in `stack.len()` inside println! must resolve to
        // the `let mut stack` binding.
        let src = "fn f() { let mut stack = 1; println!(\"{}\", stack.len()); }";
        let use_off = offset_of2(src, "stack"); // the stack inside println!
        let res = resolve(src, use_off).expect("macro arg receiver should resolve");
        assert_eq!(res.kind, RefKind::Local);
        let def_off = offset_of(src, "stack");
        assert_eq!(res.target_range.0, def_off);
    }

    #[test]
    fn macro_argument_item_resolves() {
        // A free function used as a macro argument resolves to its item.
        let src = "fn helper() -> i64 { 1 }\nfn f() { println!(\"{}\", helper()); }";
        let use_off = offset_of2(src, "helper");
        let res = resolve(src, use_off).expect("macro arg item should resolve");
        assert_eq!(res.kind, RefKind::Item);
    }

    #[test]
    fn whitespace_returns_none() {
        let src = "fn f() {}";
        let res = resolve(src, 2); // space between fn and f
        assert!(res.is_none());
    }

    #[test]
    fn keyword_returns_none() {
        let src = "fn f() { let x = 1; }";
        let res = resolve(src, 0); // "fn" keyword
        assert!(res.is_none());
    }

    #[test]
    fn unknown_name_returns_none() {
        let src = "fn f() { foobar }";
        let use_off = offset_of(src, "foobar");
        let res = resolve(src, use_off);
        assert!(res.is_none(), "unknown name should return None");
    }

    // --- resolution never panics on malformed input -------------------------

    #[test]
    fn empty_source_does_not_panic() {
        assert!(resolve("", 0).is_none());
    }

    #[test]
    fn offset_past_end_does_not_panic() {
        assert!(resolve("fn f() {}", 999).is_none());
    }

    #[test]
    fn malformed_block_does_not_panic() {
        // Missing semicolons / braces — resilient parse tree.
        let src = "fn f() { let x = ";
        // Any offset — must not panic.
        let _ = resolve(src, 10);
    }

    // --- use alias tests ----------------------------------------------------

    #[test]
    fn resolve_use_alias() {
        let src = "mod foo { pub fn bar() {} }\nuse foo::bar as baz;\nfn main() { baz() }";
        // "baz" use (second occurrence, the call) should resolve via use alias.
        let use_off = offset_of2(src, "baz");
        let res = resolve(src, use_off);
        // We expect resolution to the underlying fn `bar`.
        assert!(res.is_some(), "use alias should resolve to the aliased fn");
        if let Some(r) = res {
            assert_eq!(r.kind, RefKind::Item);
            // Should point to "bar" definition.
            let bar_def = offset_of(src, "bar");
            assert_eq!(r.target_range.0, bar_def);
        }
    }

    // --- local takes priority over item -------------------------------------

    #[test]
    fn local_shadows_item() {
        let src = "fn foo() {}\nfn main() { let foo = 1; foo }";
        // "foo" use in body should resolve to local let, NOT fn foo.
        let use_off = offset_of3(src, "foo"); // third foo - the use after let
        let res = resolve(src, use_off).expect("should resolve to local");
        assert_eq!(res.kind, RefKind::Local, "local should shadow item");
    }

    // --- definition_at tests --------------------------------------------------

    fn defn_at(src: &str, offset: usize) -> Option<Resolution> {
        let analysis = Analysis::new(src);
        analysis.definition_at(offset)
    }

    #[test]
    fn definition_at_on_use_site() {
        // cursor on use-site: definition_at should work like resolve_at.
        let src = "fn hello() {}\nfn main() { hello() }";
        let use_off = offset_of2(src, "hello");
        let def = defn_at(src, use_off).expect("use-site should have definition");
        assert_eq!(def.kind, RefKind::Item);
        let def_off = offset_of(src, "hello");
        assert_eq!(def.target_range.0, def_off);
    }

    #[test]
    fn definition_at_on_fn_name() {
        let src = "fn hello() { hello() }";
        let def_off = offset_of(src, "hello"); // cursor on fn name
        let def = defn_at(src, def_off).expect("cursor on fn name should give definition");
        assert_eq!(def.kind, RefKind::Item);
        assert_eq!(def.target_range, (3, 8)); // "hello" at bytes 3..8
        assert!(def.detail.contains("hello"));
    }

    #[test]
    fn definition_at_on_struct_name() {
        let src = "struct Point { x: f64 }";
        let def_off = offset_of(src, "Point");
        let def = defn_at(src, def_off).expect("cursor on struct name should give definition");
        assert_eq!(def.kind, RefKind::Item);
        assert!(def.detail.contains("struct Point"));
    }

    #[test]
    fn definition_at_on_let_binding() {
        let src = "fn f() { let x = 1; x }";
        // Cursor on the "x" in `let x = 1`.
        let def_off = offset_of(src, "x");
        let def = defn_at(src, def_off).expect("cursor on let binding should give definition");
        assert_eq!(def.kind, RefKind::Local);
        assert_eq!(def.detail, "let x: i64");
    }

    #[test]
    fn definition_at_on_param() {
        let src = "fn f(x: i64) -> i64 { x }";
        let def_off = offset_of(src, "x"); // param name
        let def = defn_at(src, def_off).expect("cursor on param should give definition");
        assert_eq!(def.kind, RefKind::Local);
        assert_eq!(def.detail, "x: i64");
    }

    #[test]
    fn definition_at_on_for_var() {
        let src = "fn f() { for i in 0..10 { i } }";
        let def_off = offset_of(src, "i"); // loop var
        let def = defn_at(src, def_off).expect("cursor on for var should give definition");
        assert_eq!(def.kind, RefKind::Local);
    }

    #[test]
    fn definition_at_on_pattern_binding() {
        let src = "fn f() { if let Some(x) = opt { x } else { 0 } }";
        let def_off = offset_of(src, "x"); // pattern binding name
        let def = defn_at(src, def_off).expect("cursor on pattern binding should give definition");
        assert_eq!(def.kind, RefKind::Local);
        assert!(def.detail.contains("local x"));
    }

    #[test]
    fn definition_at_on_struct_shorthand_binding() {
        // Cursor on the shorthand field binding `x` in `P { x, y }` (which is
        // both the field name and the binding) must resolve to itself.
        let src =
            "struct P { x: i64, y: i64 }\nfn f(p: P) { match p { P { x, y } => x + y } }";
        let def_off = ident_at2(src, "x"); // 2nd x = shorthand binding in pattern
        let def = defn_at(src, def_off).expect("cursor on shorthand binding should resolve");
        assert_eq!(def.kind, RefKind::Local);
        assert_eq!(def.target_range.0, def_off);
        // References from the binding site include the body use.
        let r = refs(src, def_off);
        assert!(r.len() >= 2, "shorthand binding + body use, got {r:?}");
    }

    #[test]
    fn definition_at_none_for_keyword() {
        let src = "fn f() { let x = 1; }";
        assert!(defn_at(src, 0).is_none()); // "fn" keyword
    }

    #[test]
    fn definition_at_none_for_self() {
        let src = "impl S { fn m(&self) -> i64 { self.x } }";
        let self_off = offset_of2(src, "self"); // use of self in body
        assert!(defn_at(src, self_off).is_none());
    }

    // --- references_at tests --------------------------------------------------

    fn refs(src: &str, offset: usize) -> Vec<(usize, usize)> {
        let analysis = Analysis::new(src);
        analysis.references_at(offset)
    }

    #[test]
    fn references_includes_definition_and_uses() {
        let src = "fn f() { let x = 1; x + x }";
        // All three x's: definition (let x), use1 (x +), use2 (x })
        // let x at offset_of1, x + at offset_of2, x }) at offset_of3
        let cursor = offset_of2(src, "x"); // first use
        let r = refs(src, cursor);
        assert_eq!(r.len(), 3, "should find 3 references: 1 def + 2 uses");
        // All should be "x"
        for (start, end) in &r {
            assert_eq!(&src[*start..*end], "x");
        }
    }

    #[test]
    fn references_from_definition_site() {
        let src = "fn f() { let x = 1; x + x }";
        let cursor = offset_of(src, "x"); // definition site
        let r = refs(src, cursor);
        assert_eq!(r.len(), 3, "from definition site should also find all 3 refs");
    }

    #[test]
    fn references_scope_isolation() {
        // Two functions both use "x" — only refs in the right scope.
        let src = "fn a() { let x = 1; x }\nfn b() { let x = 2; x }";
        // Cursor on "x" use in fn a (second x overall).
        let use_in_a = offset_of2(src, "x");
        let r = refs(src, use_in_a);
        assert_eq!(r.len(), 2, "only 2 refs in fn a (def + use), not fn b's x");
        // Cursor on "x" use in fn b (fourth x overall).
        let use_in_b = offset_of4(src, "x");
        let r2 = refs(src, use_in_b);
        assert_eq!(r2.len(), 2, "only 2 refs in fn b");
        // The two sets should be disjoint.
        let set_a: std::collections::HashSet<_> = r.into_iter().collect();
        let set_b: std::collections::HashSet<_> = r2.into_iter().collect();
        assert!(set_a.is_disjoint(&set_b), "different scopes should not overlap");
    }

    #[test]
    fn references_item_scope() {
        let src = "fn foo() {}\nfn main() { foo(); foo() }";
        // Cursor on first use of "foo" (second overall).
        let cursor = offset_of2(src, "foo");
        let r = refs(src, cursor);
        assert_eq!(r.len(), 3, "3 refs: def + 2 uses of foo");
    }

    #[test]
    fn references_none_for_member_access() {
        let src = "fn f() { let p = 1; p.x }";
        let cursor = ident_at(src, "x");
        let r = refs(src, cursor);
        assert!(r.is_empty(), "member access should have no references (v3)");
    }

    #[test]
    fn references_empty_for_unknown() {
        let src = "fn f() { foobar }";
        let cursor = offset_of(src, "foobar");
        let r = refs(src, cursor);
        assert!(r.is_empty());
    }

    #[test]
    fn references_nested_scopes() {
        // Inner shadow: outer x and inner x are different bindings.
        let src = "fn f() { let x = 1; { let x = 2; x } x }";
        // outer scope use (last x — 4th overall)
        // occurrences: let x(=1), let x(=2), x(inner use), x(outer use)
        let outer_use = ident_at4(src, "x");
        let r = refs(src, outer_use);
        assert_eq!(r.len(), 2, "outer x: def (let x = 1) + use (last x)");
    }

    #[test]
    fn references_name_prefilter() {
        // Different names: only tokens matching the def name are checked.
        let src = "fn f() { let x = 1; let y = x; y }";
        let cursor = ident_at2(src, "x"); // first use (in y = x)
        let r = refs(src, cursor);
        assert_eq!(r.len(), 2, "2 refs for x: def + use in y = x");
        for (start, end) in &r {
            assert_eq!(&src[*start..*end], "x");
        }
    }

    // --- rename tests ---------------------------------------------------------

    fn renames(src: &str, offset: usize, new_name: &str) -> Result<Vec<(usize, usize, String)>, RenameError> {
        let analysis = Analysis::new(src);
        analysis.rename_edits(offset, new_name)
    }

    #[test]
    fn rename_local_var() {
        let src = "fn f() { let x = 1; x + x }";
        let cursor = ident_at2(src, "x"); // first use
        let edits = renames(src, cursor, "y").expect("rename should succeed");
        assert_eq!(edits.len(), 3); // def + 2 uses
        for (_start, _end, text) in &edits {
            assert_eq!(text, "y");
        }
    }

    #[test]
    fn rename_from_definition_site() {
        let src = "fn f() { let count = 1; count + count }";
        let cursor = ident_at(src, "count"); // definition
        let edits = renames(src, cursor, "n").expect("rename from def should succeed");
        assert_eq!(edits.len(), 3);
        for (_start, _end, text) in &edits {
            assert_eq!(text, "n");
        }
    }

    #[test]
    fn rename_invalid_name_keyword() {
        let src = "fn f() { let x = 1; x }";
        let cursor = ident_at2(src, "x");
        let err = renames(src, cursor, "fn").unwrap_err();
        assert_eq!(err, RenameError::InvalidName);
    }

    #[test]
    fn rename_invalid_name_starts_with_number() {
        let src = "fn f() { let x = 1; x }";
        let cursor = ident_at2(src, "x");
        let err = renames(src, cursor, "1x").unwrap_err();
        assert_eq!(err, RenameError::InvalidName);
    }

    #[test]
    fn rename_invalid_name_with_spaces() {
        let src = "fn f() { let x = 1; x }";
        let cursor = ident_at2(src, "x");
        let err = renames(src, cursor, "foo bar").unwrap_err();
        assert_eq!(err, RenameError::InvalidName);
    }

    #[test]
    fn rename_no_references() {
        let src = "fn f() { let x = 1; }";
        // cursor on whitespace
        let err = renames(src, 2, "y").unwrap_err();
        assert_eq!(err, RenameError::NoReferences);
    }

    #[test]
    fn rename_item() {
        let src = "fn greet() {}\nfn main() { greet() }";
        let cursor = ident_at2(src, "greet"); // use
        let edits = renames(src, cursor, "hello").expect("rename item should succeed");
        assert_eq!(edits.len(), 2); // def + use
        for (_start, _end, text) in &edits {
            assert_eq!(text, "hello");
        }
    }

    // --- is_valid_ident tests -------------------------------------------------

    #[test]
    fn valid_ident_simple() {
        assert!(is_valid_ident("x"));
        assert!(is_valid_ident("foo_bar"));
        assert!(is_valid_ident("_private"));
        assert!(is_valid_ident("CamelCase"));
        assert!(is_valid_ident("ABC123"));
    }

    #[test]
    fn invalid_ident_empty() {
        assert!(!is_valid_ident(""));
    }

    #[test]
    fn invalid_ident_leading_digit() {
        assert!(!is_valid_ident("1foo"));
    }

    #[test]
    fn invalid_ident_keyword() {
        assert!(!is_valid_ident("fn"));
        assert!(!is_valid_ident("let"));
        assert!(!is_valid_ident("struct"));
        assert!(!is_valid_ident("self"));
        assert!(!is_valid_ident("true"));
        assert!(!is_valid_ident("false"));
    }

    // --- helper: offset_of for 4th+ occurrence ---------------------------------

    fn offset_of4(src: &str, name: &str) -> usize {
        nth_ident(src, name, 3)
    }

    fn ident_at4(src: &str, name: &str) -> usize {
        nth_ident(src, name, 3)
    }

}
