//! Native completion: scope, member, and path completions using the
//! semantic HIR without any compiler bridge.

use std::{rc::Rc, sync::Arc};

use rua_syntax::{SyntaxKind, SyntaxToken};

use crate::{
    BaseDb,
    base::TextRange,
    hir::{
        Body, BodyScopes, BodySourceMap, DefKind, DefMap, Definition, ExprId, InferenceResult,
        ModuleId, ScopeKind, Ty,
    },
    vfs::FileId,
};

use super::{
    CompletionInsert, CompletionItem, CompletionKind, FilePosition, MacroDelimiter,
};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub(crate) fn completions(
    db: &Rc<BaseDb>,
    position: FilePosition,
) -> Vec<CompletionItem> {
    let Some(text) = db.file_text(position.file_id) else {
        return Vec::new();
    };
    let parse = db.parse(position.file_id);
    let root = parse.syntax_node();
    let offset = position.offset.min(text.len() as u32);
    let token = token_at_offset(root, offset);

    // Member access: cursor is after `.`
    if let Some(ref tok) = token
        && previous_significant(tok).is_some_and(|t| t.kind() == SyntaxKind::Dot) {
            return member_completions(db, position, tok, offset);
        }

    // Path context: cursor is after `::`
    if let Some(ref tok) = token
        && previous_significant(tok).is_some_and(|t| t.kind() == SyntaxKind::ColonColon) {
            return path_completions(db, position, tok);
        }

    // Default: scope completion
    scope_completions(db, position, offset)
}

// ---------------------------------------------------------------------------
// Scope completion: keywords + locals + module items + builtins
// ---------------------------------------------------------------------------

const RUA_KEYWORDS: &[&str] = &[
    "fn", "let", "mut", "if", "else", "while", "loop", "for", "in",
    "return", "break", "continue", "match", "struct", "enum", "trait",
    "impl", "mod", "pub", "use", "as", "extern", "where", "type",
    "move", "self", "true", "false",
];

const BUILTIN_TYPES: &[&str] = &[
    "i64", "f64", "bool", "String", "str", "Vec", "HashMap", "Option", "Result", "Box",
];

const BUILTIN_VALUES: &[(&str, &str)] = &[
    ("Some", "Some(value) -> Option<T>"),
    ("None", "None: Option<T>"),
    ("Ok", "Ok(value) -> Result<T, E>"),
    ("Err", "Err(error) -> Result<T, E>"),
];

const BUILTIN_MACROS: &[(&str, MacroDelimiter)] = &[
    ("println", MacroDelimiter::Parentheses),
    ("print", MacroDelimiter::Parentheses),
    ("format", MacroDelimiter::Parentheses),
    ("vec", MacroDelimiter::Brackets),
    ("panic", MacroDelimiter::Parentheses),
    ("assert", MacroDelimiter::Parentheses),
    ("assert_eq", MacroDelimiter::Parentheses),
    ("assert_ne", MacroDelimiter::Parentheses),
    ("unreachable", MacroDelimiter::Parentheses),
    ("unimplemented", MacroDelimiter::Parentheses),
    ("todo", MacroDelimiter::Parentheses),
    ("dbg", MacroDelimiter::Parentheses),
    ("include_str", MacroDelimiter::Parentheses),
    ("include_bytes", MacroDelimiter::Parentheses),
];

fn scope_completions(
    db: &Rc<BaseDb>,
    position: FilePosition,
    offset: u32,
) -> Vec<CompletionItem> {
    let mut seen = std::collections::HashSet::new();
    let mut items = Vec::new();

    // 1. Keywords
    for kw in RUA_KEYWORDS {
        if seen.insert(kw.to_string()) {
            items.push(
                CompletionItem::new(*kw, CompletionKind::Keyword)
                    .with_detail(format!("keyword {kw}"))
                    .with_relevance(100),
            );
        }
    }

    // 2. Locals in scope
    let def_map = db.def_map(position.file_id);
    for local in visible_locals(db, &def_map, position, offset) {
        if seen.insert(local.name.clone()) {
            items.push(
                CompletionItem::new(local.name.clone(), CompletionKind::Variable)
                    .with_detail(local.ty)
                    .with_relevance(90),
            );
        }
    }

    // 3. Module-level definitions
    if let Some(module_id) = module_at_position(&def_map, position.file_id, offset) {
        for definition in def_map.definitions() {
            if definition.module_id() != module_id {
                continue;
            }
            if seen.insert(definition.name().to_string()) {
                let kind = def_kind_to_completion_kind(definition.kind());
                let mut item =
                    CompletionItem::new(definition.name(), kind).with_relevance(80);
                if let Some(sig) = definition_signature(db, &def_map, definition) {
                    item = item.with_detail(sig);
                }
                items.push(item);
            }
        }
    }

    // 4. Built-in types
    for ty in BUILTIN_TYPES {
        if seen.insert(ty.to_string()) {
            items.push(
                CompletionItem::new(*ty, CompletionKind::BuiltinType)
                    .with_detail(format!("{ty}  (built-in type)"))
                    .with_relevance(70),
            );
        }
    }

    // 5. Built-in constructors
    for (name, detail) in BUILTIN_VALUES {
        if seen.insert(name.to_string()) {
            items.push(
                CompletionItem::new(*name, CompletionKind::Variant)
                    .with_detail(*detail)
                    .with_relevance(70),
            );
        }
    }

    // 6. Built-in macros
    for (name, delimiter) in BUILTIN_MACROS {
        if seen.insert(name.to_string()) {
            let label = format!("{name}!");
            items.push(
                CompletionItem::new(label, CompletionKind::Macro)
                    .with_detail(format!("{name}!(...)  (built-in macro)"))
                    .with_insert(CompletionInsert::MacroCall {
                        name: name.to_string(),
                        delimiter: *delimiter,
                    })
                    .with_relevance(60),
            );
        }
    }

    CompletionItem::normalize(&mut items);
    items
}

// ---------------------------------------------------------------------------
// Member completion (after `.`)
// ---------------------------------------------------------------------------

fn member_completions(
    db: &Rc<BaseDb>,
    position: FilePosition,
    _token: &SyntaxToken,
    offset: u32,
) -> Vec<CompletionItem> {
    let def_map = db.def_map(position.file_id);
    let receiver_ty = infer_dot_receiver(db, &def_map, position, offset);

    let Some(receiver_ty) = receiver_ty else {
        return Vec::new();
    };

    let member_index = db.member_index(position.file_id);
    let candidates = member_index.instance_candidates(&receiver_ty);

    let mut seen = std::collections::HashSet::new();
    let mut items = Vec::new();

    for candidate in candidates {
        if !seen.insert(candidate.name().to_string()) {
            continue;
        }
        let kind = match candidate.kind() {
            crate::hir::MemberKind::Field => CompletionKind::Field,
            crate::hir::MemberKind::Method => CompletionKind::Method,
            crate::hir::MemberKind::AssociatedFunction => CompletionKind::Function,
            crate::hir::MemberKind::Variant => CompletionKind::Variant,
        };
        let detail = candidate.ty().to_string();
        let name = candidate.name().to_string();
        let mut item = CompletionItem::new(name.clone(), kind)
            .with_detail(detail)
            .with_relevance(90);

        if candidate.kind() == crate::hir::MemberKind::Method {
            item = item.with_insert(CompletionInsert::Call {
                callee: name,
                has_arguments: false,
            });
        }
        items.push(item);
    }

    CompletionItem::normalize(&mut items);
    items
}

/// Try to infer the type of the expression immediately left of `.`.
fn infer_dot_receiver(
    db: &Rc<BaseDb>,
    def_map: &DefMap,
    position: FilePosition,
    offset: u32,
) -> Option<Ty> {
    let (body, source_map, inference) =
        find_containing_body_data(db, def_map, position, offset)?;
    let dot_pos = offset.saturating_sub(1);

    // Find the expression whose range contains the dot position.
    // Look for expressions that end at or contain the position before dot.
    let mut candidates: Vec<(u32, ExprId)> = body
        .exprs()
        .filter_map(|(expr_id, _expr)| {
            let range = source_map.expr_range(expr_id)?;
            let text_range = range.range;
            if text_range.contains(dot_pos) || text_range.end() == dot_pos {
                Some((text_range.len(), expr_id))
            } else {
                None
            }
        })
        .collect();

    // Prefer the innermost expression (smallest range)
    candidates.sort_by_key(|(len, _)| *len);
    let expr_id = candidates.first().map(|(_, id)| *id)?;

    inference.type_of_expr(expr_id).cloned()
}

// ---------------------------------------------------------------------------
// Path completion (after `::`)
// ---------------------------------------------------------------------------

fn path_completions(
    db: &Rc<BaseDb>,
    position: FilePosition,
    token: &SyntaxToken,
) -> Vec<CompletionItem> {
    let def_map = db.def_map(position.file_id);
    let segments = path_segments_before(token);
    if segments.is_empty() {
        return Vec::new();
    }

    let mut seen = std::collections::HashSet::new();
    let mut items = Vec::new();

    // Resolve path prefix to a module and list its members
    if let Some(module_id) =
        resolve_path_prefix_module(&def_map, position.file_id, position.offset, &segments)
    {
        let member_index = db.member_index(position.file_id);
        for definition in def_map.definitions() {
            if definition.module_id() == module_id
                && seen.insert(definition.name().to_string()) {
                    let kind = def_kind_to_completion_kind(definition.kind());
                    let mut item =
                        CompletionItem::new(definition.name(), kind).with_relevance(80);
                    if let Some(sig) = definition_signature(db, &def_map, definition) {
                        item = item.with_detail(sig);
                    }
                    items.push(item);
                }
        }

        // Also try enum variants if the resolved module contains enums
        for definition in def_map.definitions() {
            if definition.module_id() == module_id
                && definition.kind() == DefKind::Enum
                && let Some(template_ty) = member_index.type_template(definition.id()) {
                    for candidate in member_index.associated_candidates(template_ty) {
                        if seen.insert(candidate.name().to_string()) {
                            items.push(
                                CompletionItem::new(
                                    candidate.name(),
                                    CompletionKind::Variant,
                                )
                                .with_detail(candidate.ty().to_string())
                                .with_relevance(85),
                            );
                        }
                    }
                }
        }
    }

    CompletionItem::normalize(&mut items);
    items
}

/// Collect path segments leading up to the `::` before `token`.
fn path_segments_before(token: &SyntaxToken) -> Vec<String> {
    let mut segments = Vec::new();

    let before_colons = previous_significant(token);
    let Some(ref prev) = before_colons else {
        return segments;
    };
    if !is_path_identifier(prev) {
        return segments;
    }
    segments.push(prev.text().to_string());
    let mut current = prev.clone();

    loop {
        let sep = previous_significant(&current);
        let Some(sep) = sep else { break };
        if sep.kind() != SyntaxKind::ColonColon {
            break;
        }
        let segment = previous_significant(&sep);
        let Some(segment) = segment else { break };
        if !is_path_identifier(&segment) {
            break;
        }
        segments.push(segment.text().to_string());
        current = segment;
    }

    segments.reverse();
    segments
}

/// Resolve a path prefix to its target module.
fn resolve_path_prefix_module(
    map: &DefMap,
    file_id: FileId,
    offset: u32,
    segments: &[String],
) -> Option<ModuleId> {
    let current = module_at_position(map, file_id, offset)?;
    let (last, parents) = segments.split_last()?;

    let mut module_id = current;
    for segment in parents {
        let def = resolve_lexical_name(map, module_id, segment)?;
        module_id = def.target_module()?;
    }
    // Resolve the last segment
    if let Some(def) = resolve_lexical_name(map, module_id, last) {
        def.target_module()
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Local variable enumeration
// ---------------------------------------------------------------------------

struct LocalInfo {
    name: String,
    ty: String,
}

/// Collect all local bindings visible at `offset` by walking up the scope chain.
fn visible_locals(
    db: &BaseDb,
    def_map: &DefMap,
    position: FilePosition,
    offset: u32,
) -> Vec<LocalInfo> {
    let Some((body, source_map, scopes, inference)) =
        find_containing_body_full(db, def_map, position, offset)
    else {
        return Vec::new();
    };

    let Some(scope) = find_innermost_scope(&body, &source_map, &scopes, offset) else {
        return Vec::new();
    };

    let mut result = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut current = Some(scope);

    while let Some(scope_id) = current {
        let Some(scope_data) = scopes.scope(scope_id) else {
            break;
        };
        for binding_id in scope_data.bindings() {
            let Some(binding) = body.binding(*binding_id) else {
                continue;
            };
            let Some(name) = binding.name() else {
                continue;
            };
            if seen.insert(name.to_string()) {
                let ty_str = inference
                    .as_ref()
                    .and_then(|inf| inf.type_of_binding(*binding_id))
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "?".to_string());
                result.push(LocalInfo {
                    name: name.to_string(),
                    ty: format!("{name}: {ty_str}"),
                });
            }
        }
        current = scope_data.parent();
    }

    result
}

/// Find the innermost scope containing `offset`.
///
/// For "forward" scopes (AfterLet, ForBody), the scope extends from after the
/// element forward to the end of the parent scope, so we check whether the
/// offset is *after* the element range rather than *inside* it.
fn find_innermost_scope(
    _body: &Body,
    source_map: &BodySourceMap,
    scopes: &BodyScopes,
    offset: u32,
) -> Option<crate::hir::ScopeId> {
    #[derive(Clone, Copy)]
    enum ScopeRange {
        /// offset must be inside the element range.
        Within(TextRange),
        /// offset must be at or after the element's end (forward-extending scope).
        After(u32),
    }

    // Collect candidate scopes that contain the offset.
    let mut best: Option<(u32, crate::hir::ScopeId)> = None;

    for (scope_id, scope_data) in scopes.scopes() {
        let candidate: Option<ScopeRange> = match scope_data.kind() {
            ScopeKind::Root => continue,
            ScopeKind::Block { expr } => {
                source_map.expr_range(expr).map(|fr| ScopeRange::Within(fr.range))
            }
            // AfterLet: scope covers code after the semicolon, i.e. after the
            // binding identifier (which sits inside the `let` statement).
            ScopeKind::AfterLet { binding } => source_map
                .binding_range(binding)
                .map(|fr| ScopeRange::After(fr.range.end())),
            ScopeKind::Closure { expr } => {
                source_map.expr_range(expr).map(|fr| ScopeRange::Within(fr.range))
            }
            // ForBody: scope covers the loop body, which starts after the
            // binding identifier in the `for` header.
            ScopeKind::ForBody { binding } => source_map
                .binding_range(binding)
                .map(|fr| ScopeRange::After(fr.range.end())),
            ScopeKind::IfLetBody { pattern } => {
                source_map.pat_range(pattern).map(|fr| ScopeRange::Within(fr.range))
            }
            ScopeKind::WhileLetBody { pattern } => {
                source_map.pat_range(pattern).map(|fr| ScopeRange::Within(fr.range))
            }
            ScopeKind::MatchArm => continue,
        };

        match candidate {
            Some(ScopeRange::Within(range)) if range.contains(offset) => {
                let len = range.len();
                if best.is_none_or(|(best_len, _)| len < best_len) {
                    best = Some((len, scope_id));
                }
            }
            Some(ScopeRange::After(end)) if offset >= end => {
                let len = offset - end; // prefer latest (smallest distance).
                if best.is_none_or(|(best_len, _)| len < best_len) {
                    best = Some((len, scope_id));
                }
            }
            _ => {}
        }
    }

    best.map(|(_, id)| id)
}

// ---------------------------------------------------------------------------
// Common helpers
// ---------------------------------------------------------------------------

type BodyData =
    (Arc<Body>, Arc<BodySourceMap>, Arc<InferenceResult>);

type BodyFullData =
    (Arc<Body>, Arc<BodySourceMap>, Arc<BodyScopes>, Option<Arc<InferenceResult>>);

/// Find the innermost function/method body containing `offset`.
fn find_containing_body_data(
    db: &BaseDb,
    def_map: &DefMap,
    position: FilePosition,
    offset: u32,
) -> Option<BodyData> {
    let owner = innermost_body_owner(def_map, position, offset)?;
    Some((
        db.body(owner.id())?,
        db.body_source_map(owner.id())?,
        db.infer(owner.id())?,
    ))
}

fn find_containing_body_full(
    db: &BaseDb,
    def_map: &DefMap,
    position: FilePosition,
    offset: u32,
) -> Option<BodyFullData> {
    let owner = innermost_body_owner(def_map, position, offset)?;
    Some((
        db.body(owner.id())?,
        db.body_source_map(owner.id())?,
        db.body_scopes(owner.id())?,
        db.infer(owner.id()),
    ))
}

fn innermost_body_owner(
    def_map: &DefMap,
    position: FilePosition,
    offset: u32,
) -> Option<Definition> {
    def_map
        .definitions()
        .filter(|definition| {
            definition.file_id() == position.file_id
                && definition.range().contains(offset)
                && matches!(definition.kind(), DefKind::Function | DefKind::Method)
        })
        .min_by_key(|definition| definition.range().len())
        .cloned()
}

fn module_at_position(map: &DefMap, file_id: FileId, offset: u32) -> Option<ModuleId> {
    let mut module_id = map.module_for_file(file_id)?;
    loop {
        let nested = map
            .definitions()
            .filter(|definition| definition.module_id() == module_id)
            .filter_map(|definition| {
                let target = definition.target_module()?;
                let target_data = map.module(target)?;
                (target_data.file_id() == Some(file_id)
                    && definition.range().contains(offset))
                .then_some((definition.range().len(), target))
            })
            .min_by_key(|(length, _)| *length);
        let Some((_, nested)) = nested else {
            return Some(module_id);
        };
        module_id = nested;
    }
}

fn resolve_lexical_name<'map>(
    map: &'map DefMap,
    mut module_id: ModuleId,
    name: &str,
) -> Option<&'map Definition> {
    loop {
        if let Some(definition) = map.resolve_name(module_id, name) {
            return Some(definition);
        }
        module_id = map.module(module_id)?.parent()?;
    }
}

fn def_kind_to_completion_kind(kind: DefKind) -> CompletionKind {
    match kind {
        DefKind::Function | DefKind::ExternFunction | DefKind::Method => CompletionKind::Function,
        DefKind::Struct => CompletionKind::Struct,
        DefKind::Enum => CompletionKind::Enum,
        DefKind::Trait => CompletionKind::Trait,
        DefKind::Impl => CompletionKind::Impl,
        DefKind::Module => CompletionKind::Module,
        DefKind::Field => CompletionKind::Field,
        DefKind::Variant => CompletionKind::Variant,
        DefKind::TypeAlias => CompletionKind::TypeAlias,
    }
}

fn definition_signature(
    db: &BaseDb,
    def_map: &DefMap,
    definition: &Definition,
) -> Option<String> {
    match definition.kind() {
        DefKind::Function | DefKind::ExternFunction | DefKind::Method => {
            let member_index = db.member_index(def_map.root_file());
            member_index.callable(definition.id()).map(|callable| {
                let params: Vec<String> =
                    callable.params().iter().map(|ty| ty.to_string()).collect();
                format!(
                    "fn {}({}) -> {}",
                    definition.name(),
                    params.join(", "),
                    callable.return_ty()
                )
            })
        }
        DefKind::Struct => Some(format!("struct {}", definition.name())),
        DefKind::Enum => Some(format!("enum {}", definition.name())),
        DefKind::Trait => Some(format!("trait {}", definition.name())),
        DefKind::Module => Some(format!("mod {}", definition.name())),
        DefKind::Field | DefKind::Variant | DefKind::TypeAlias => {
            Some(definition.name().to_string())
        }
        DefKind::Impl => None,
    }
}

// ---------------------------------------------------------------------------
// Syntax navigation
// ---------------------------------------------------------------------------

fn token_at_offset(node: &rua_syntax::SyntaxNode, offset: u32) -> Option<SyntaxToken> {
    let end: u32 = node.text_range().end().into();
    match node.token_at_offset(offset.min(end).into()) {
        rowan::TokenAtOffset::Single(token) => Some(token),
        rowan::TokenAtOffset::Between(left, _right) => Some(left),
        _ => None,
    }
}

fn is_path_identifier(token: &SyntaxToken) -> bool {
    matches!(token.kind(), SyntaxKind::Ident | SyntaxKind::KwSelf)
}

fn previous_significant(token: &SyntaxToken) -> Option<SyntaxToken> {
    let mut token = token.prev_token();
    while token.as_ref().is_some_and(|token| token.kind().is_trivia()) {
        token = token.and_then(|token| token.prev_token());
    }
    token
}
