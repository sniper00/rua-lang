//! Native completion: scope, member, and path completions using the
//! semantic HIR without any compiler bridge.

use std::{rc::Rc, sync::Arc};

use rua_syntax::{SyntaxKind, SyntaxToken};

use crate::{
    BaseDb,
    base::TextRange,
    hir::{
        Body, BodyScopes, BodySourceMap, DefKind, DefMap, Definition, Expr, ExprId,
        InferenceResult, ModuleId, ScopeKind, Ty,
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

    // Find the partial identifier prefix at the cursor (e.g. `val` before `ues`).
    let partial_range = partial_ident_range(text.as_ref(), offset);

    let token = token_at_offset(root, offset);

    // Member access: cursor right after `.` (token IS the dot) or after `.x`
    let after_dot = token.as_ref().is_some_and(|t| t.kind() == SyntaxKind::Dot)
        || token.as_ref().and_then(previous_significant).is_some_and(|t| t.kind() == SyntaxKind::Dot);
    let mut items = if after_dot {
        member_completions(db, position, token.as_ref(), offset)
    } else if let Some(ref tok) = token
        && previous_significant(tok).is_some_and(|t| t.kind() == SyntaxKind::ColonColon)
    {
        path_completions(db, position, tok, partial_range)
    } else {
        scope_completions(db, position, offset, partial_range, token.as_ref())
    };

    // Filter by the typed prefix so the client only sees relevant matches.
    // Only filter when the prefix contains at least one letter or underscore
    // (pure-digit prefixes like `42` shouldn't filter out everything).
    if !partial_range.is_empty() {
        let start = partial_range.start() as usize;
        let end = partial_range.end() as usize;
        if start <= end && end <= text.len() {
            let prefix = &text[start..end];
            // Skip filtering if prefix is purely numeric (e.g. cursor at `42`).
            let is_pure_numeric = !prefix.is_empty()
                && prefix.bytes().all(|b| b.is_ascii_digit());
            if !is_pure_numeric {
                let prefix_lower = prefix.to_lowercase();
                items.retain(|item| {
                    let label_lower = item.label().to_lowercase();
                    let matches = is_subsequence(&prefix_lower, &label_lower);
                    matches
                        || item.lookup().is_some_and(|l| {
                            is_subsequence(&prefix_lower, &l.to_lowercase())
                        })
                });
            }
        }
    }

    items
}

/// Check if `prefix` chars appear in order within `target` (case-insensitive
/// ASCII subsequence match). Used for fuzzy completion filtering.
fn is_subsequence(prefix: &str, target: &str) -> bool {
    let mut target_bytes = target.as_bytes().iter();
    for &pb in prefix.as_bytes() {
        let pb_lower = pb.to_ascii_lowercase();
        loop {
            match target_bytes.next() {
                Some(&tb) if tb.to_ascii_lowercase() == pb_lower => break,
                Some(_) => continue,
                None => return false,
            }
        }
    }
    true
}

/// Walk backwards from `offset` to find the start of a partial identifier.
/// Returns the byte range `[start, offset)` of the prefix, or a zero-width
/// range at `offset` if there is no identifier character before the cursor.
fn partial_ident_range(text: &str, offset: u32) -> TextRange {
    let bytes = text.as_bytes();
    let mut start = offset as usize;
    if start > bytes.len() {
        start = bytes.len();
    }
    while start > 0 {
        let byte = bytes[start - 1];
        if byte.is_ascii_alphanumeric() || byte == b'_' {
            start -= 1;
        } else {
            break;
        }
    }
    TextRange::new(start as u32, offset)
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

/// Keyword → snippet template for statement-level keywords that generate
/// common code patterns.
fn keyword_snippet(kw: &str) -> Option<&'static str> {
    match kw {
        "for" => Some("for ${1:item} in ${2:iter} {\n    $0\n}"),
        "match" => Some("match ${1:expr} {\n    $0\n}"),
        "if" => Some("if ${1:cond} {\n    $0\n}"),
        "while" => Some("while ${1:cond} {\n    $0\n}"),
        "loop" => Some("loop {\n    $0\n}"),
        "fn" => Some("fn ${1:name}(${2:params}) -> ${3:Ret} {\n    $0\n}"),
        "struct" => Some("struct ${1:Name} {\n    ${2:fields}\n}"),
        "enum" => Some("enum ${1:Name} {\n    ${2:variants}\n}"),
        "impl" => Some("impl ${1:Type} {\n    $0\n}"),
        "mod" => Some("mod ${1:name} {\n    $0\n}"),
        "trait" => Some("trait ${1:Name} {\n    $0\n}"),
        "let" => Some("let ${1:name} = ${2:expr};"),
        _ => None,
    }
}

/// Additional snippet-only completions for patterns not in RUA_KEYWORDS.
const SNIPPET_PATTERNS: &[(&str, &str)] = &[
    ("if let", "if let ${1:pattern} = ${2:expr} {\n    $0\n}"),
    ("while let", "while let ${1:pattern} = ${2:expr} {\n    $0\n}"),
];

/// Keywords that only make sense at the start of a statement/item, not in
/// expression position (e.g. after `=`, `return`, `(`).
const DECLARATION_KEYWORDS: &[&str] = &[
    "fn", "struct", "enum", "trait", "impl", "mod", "pub", "extern", "use", "type",
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
    partial_range: TextRange,
    token: Option<&SyntaxToken>,
) -> Vec<CompletionItem> {
    let mut seen = std::collections::HashSet::new();
    let mut items = Vec::new();

    let in_expr_context = token.is_some_and(is_expression_context);
    let in_type_pos = token.is_some_and(is_type_position);

    // 0. Match context — offer enum variants when inside a match expression.
    let def_map = db.def_map(position.file_id);
    let in_method = innermost_body_owner(&def_map, position, offset)
        .is_some_and(|d| d.kind() == DefKind::Method);
    if let Some((_enum_ty, enum_template)) =
        match_scrutinee_enum(db, &def_map, position, offset)
    {
        for candidate in db
            .member_index(position.file_id)
            .associated_candidates(&enum_template)
        {
            if seen.insert(candidate.name().to_string()) {
                let name = candidate.name().to_string();
                let detail = candidate.ty().to_string();
                // Build a snippet with placeholders for tuple variants.
                let insert = CompletionInsert::Call {
                    callee: name.clone(),
                    params: vec![], // variant constructors don't have named params
                };
                items.push(
                    CompletionItem::new(name, CompletionKind::Variant)
                        .with_detail(detail)
                        .with_insert(insert)
                        .with_relevance(93), // highest — above locals
                );
            }
        }
    }

    // 0b. Struct literal context — offer field names when inside `Type { | }`.
    if let Some(struct_ty) = struct_literal_type(db, &def_map, position, offset) {
        for candidate in db
            .member_index(position.file_id)
            .field_candidates(&struct_ty)
        {
            if seen.insert(candidate.name().to_string()) {
                let n = candidate.name().to_string();
                let ty = candidate.ty().to_string();
                items.push(
                    CompletionItem::new(n.clone(), CompletionKind::Field)
                        .with_detail(format!("{n}: {ty}"))
                        .with_relevance(93),
                );
            }
        }
    }

    // 0c. if-let / while-let pattern position — offer enum variants.
    if let Some((_ty, template)) = pattern_scrutinee_enum(db, &def_map, position, offset) {
        for candidate in db.member_index(position.file_id).associated_candidates(&template) {
            if seen.insert(candidate.name().to_string()) {
                let name = candidate.name().to_string();
                items.push(
                    CompletionItem::new(name, CompletionKind::Variant)
                        .with_detail(candidate.ty().to_string())
                        .with_relevance(94), // above match scrutinee variants
                );
            }
        }
    }

    // 1. Keywords — suppress declaration keywords in expression context;
    //    in type positions, suppress all keywords. Use snippet templates
    //    for statement-level keywords.
    for kw in RUA_KEYWORDS {
        if in_type_pos {
            continue;
        }
        if in_expr_context && DECLARATION_KEYWORDS.contains(kw) {
            continue;
        }
        if seen.insert(kw.to_string()) {
            let mut item =
                CompletionItem::new(*kw, CompletionKind::Keyword)
                    .with_relevance(50);
            if let Some(snippet) = keyword_snippet(kw) {
                item = item
                    .with_detail(format!("{kw} … (snippet)"))
                    .with_insert(CompletionInsert::Snippet(snippet.to_string()));
            } else {
                item = item.with_detail(format!("keyword {kw}"));
            }
            // Boost `self` inside method bodies.
            if *kw == "self" && in_method {
                item = item.with_relevance(96); // above locals
            }
            items.push(item);
        }
    }

    // 1b. Additional snippet patterns (if-let, while-let).
    for (label, snippet) in SNIPPET_PATTERNS {
        if seen.insert(label.to_string()) {
            items.push(
                CompletionItem::new(*label, CompletionKind::Keyword)
                    .with_detail(format!("{label} … (snippet)"))
                    .with_insert(CompletionInsert::Snippet(snippet.to_string()))
                    .with_relevance(51),
            );
        }
    }

    // 2. Locals in scope — skip in type position. Boost frequently-used locals.
    if !in_type_pos {
        let usage_counts = local_usage_counts(db, &def_map, position, offset);
        for local in visible_locals(db, &def_map, position, offset) {
            if seen.insert(local.name.clone()) {
                let extra = usage_counts
                    .get(&local.name)
                    .map(|c| (*c).min(5))
                    .unwrap_or(0);
                items.push(
                    CompletionItem::new(local.name.clone(), CompletionKind::Variable)
                        .with_detail(local.ty)
                        .with_relevance(95 + extra as u16),
                );
            }
        }
    }

    // 3. Module-level definitions — in type position, only include types.
    if let Some(module_id) = module_at_position(&def_map, position.file_id, offset) {
        for definition in def_map.definitions() {
            if definition.module_id() != module_id {
                continue;
            }
            // Fields and variants are not bare names in scope.
            if matches!(definition.kind(), DefKind::Field | DefKind::Variant) {
                continue;
            }
            // In type position, only struct/enum/trait/type alias.
            if in_type_pos
                && !matches!(
                    definition.kind(),
                    DefKind::Struct | DefKind::Enum | DefKind::Trait | DefKind::TypeAlias
                )
            {
                continue;
            }
            if seen.insert(definition.name().to_string()) {
                let kind = def_kind_to_completion_kind(definition.kind());
                let mut item =
                    CompletionItem::new(definition.name(), kind).with_relevance(85);
                if let Some(sig) = definition_signature(db, &def_map, definition) {
                    item = item.with_detail(sig);
                }
                if let Some(doc) = extract_doc_comment(db, position.file_id, definition) {
                    item = item.with_documentation(doc);
                }
                items.push(item);
            }
        }
    }

    // 3b. Cross-module pub symbols (auto-import candidates).
    if let Some(current_module) =
        module_at_position(&def_map, position.file_id, offset)
    {
        for definition in def_map.definitions() {
            if definition.module_id() == current_module {
                continue; // already shown
            }
            if !matches!(
                definition.visibility(),
                crate::hir::Visibility::Public
            ) {
                continue;
            }
            if !matches!(
                definition.kind(),
                DefKind::Function
                    | DefKind::Struct
                    | DefKind::Enum
                    | DefKind::Trait
                    | DefKind::TypeAlias
                    | DefKind::Module
            ) {
                continue;
            }
            if seen.insert(definition.name().to_string()) {
                let module = def_map.module(definition.module_id());
                let module_path = module
                    .and_then(|m| m.name())
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "?".to_string());
                let import_path =
                    format!("use {module_path}::{};", definition.name());
                let kind = def_kind_to_completion_kind(definition.kind());
                let mut item = CompletionItem::new(definition.name(), kind)
                    .with_detail(format!(
                        "{} (from {module_path})",
                        definition.name()
                    ))
                    .with_import_path(import_path)
                    .with_relevance(75);
                if let Some(sig) = definition_signature(db, &def_map, definition) {
                    item = item.with_detail(sig);
                }
                items.push(item);
            }
        }
    }

    // 4. Built-in types — boost numeric types in arithmetic context.
    let in_arithmetic = token.is_some_and(|t| {
        previous_significant(t)
            .is_some_and(|prev| {
                matches!(
                    prev.kind(),
                    SyntaxKind::Plus
                        | SyntaxKind::Minus
                        | SyntaxKind::Star
                        | SyntaxKind::Slash
                )
            })
    });
    let numeric_types: &[&str] = &["i64", "f64"];
    for ty in BUILTIN_TYPES {
        if seen.insert(ty.to_string()) {
            let relevance = if in_arithmetic && numeric_types.contains(ty) {
                88 // just below locals
            } else if in_type_pos {
                90
            } else {
                40
            };
            items.push(
                CompletionItem::new(*ty, CompletionKind::BuiltinType)
                    .with_detail(format!("{ty}  (built-in type)"))
                    .with_relevance(relevance),
            );
        }
    }

    // 5. Built-in constructors — skip in type position.
    if !in_type_pos {
        for (name, detail) in BUILTIN_VALUES {
            if seen.insert(name.to_string()) {
                items.push(
                    CompletionItem::new(*name, CompletionKind::Variant)
                        .with_detail(*detail)
                        .with_relevance(35),
                );
            }
        }
    }

    // 6. Built-in macros — skip in type position.
    if !in_type_pos {
        for (name, delimiter) in BUILTIN_MACROS {
            if seen.insert(name.to_string()) {
                let label = format!("{name}!");
                items.push(
                    CompletionItem::new(label, CompletionKind::Macro)
                        .with_detail(format!("{name}!(...)  (built-in macro)"))
                        .with_lookup(name.to_string())
                        .with_insert(CompletionInsert::MacroCall {
                            name: name.to_string(),
                            delimiter: *delimiter,
                        })
                        .with_relevance(20),
                );
            }
        }
    }

    // Boost items whose type matches the expected type at the cursor.
    if let Some(expected) = expected_type_at_cursor(db, &def_map, position, offset) {
        let expected_str = expected.to_string();
        for item in &mut items {
            if let Some(detail) = item.detail() {
                let boost = type_compatibility_score(
                    detail,
                    &expected_str,
                    &expected,
                );
                if boost > 0 {
                    *item = item
                        .clone()
                        .with_relevance(item.relevance() + boost);
                }
            }
        }
    }

    // Set replacement range: VS Code uses this to replace the typed prefix.
    if !partial_range.is_empty() {
        for item in &mut items {
            if item.replacement_range().is_none() {
                *item = item.clone().with_replacement_range(partial_range);
            }
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
    _token: Option<&SyntaxToken>,
    offset: u32,
) -> Vec<CompletionItem> {
    let def_map = db.def_map(position.file_id);
    let receiver_ty = infer_dot_receiver(db, &def_map, position, offset);

    let Some(receiver_ty) = receiver_ty else {
        return Vec::new();
    };

    // Get the receiver expression text for postfix template generation.
    let receiver_text = receiver_expr_text(db, &def_map, position, offset)
        .unwrap_or_else(|| "_".to_string());

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
            // Look up the method resolution; HIR params already exclude
            // `self` (it's stored as the receiver), so snippet params are
            // correct as-is. We prepend the receiver to the detail display.
            let method_res = member_index.resolve_method(&receiver_ty, &name);
            let callable = method_res.as_ref().and_then(|r| r.callable().cloned());

            let params: Vec<String> = {
                let types: Vec<String> = callable
                    .as_ref()
                    .map(|c| c.params().iter().map(|ty| ty.to_string()).collect())
                    .unwrap_or_default();
                // Try to get original parameter names from the definition's
                // CallableSignature so snippets show names, not just types.
                let names: Vec<Option<String>> = method_res
                    .as_ref()
                    .and_then(|res| match res.target() {
                        crate::hir::MemberTarget::Definition(def_id) => {
                            let def = def_map.definition(def_id)?;
                            if let crate::hir::ItemSignature::Callable(sig) = def.signature() {
                                // sig.params() already excludes self (it's in
                                // sig.receiver()), so params align 1:1 with
                                // callable.params().
                                Some(
                                    sig.params()
                                        .iter()
                                        .map(|p| p.name().map(|n| n.to_string()))
                                        .collect(),
                                )
                            } else {
                                None
                            }
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| vec![None; types.len()]);
                types
                    .iter()
                    .enumerate()
                    .map(|(i, ty)| match names.get(i).and_then(|n| n.clone()) {
                        Some(name) => format!("{name}: {ty}"),
                        None => ty.clone(),
                    })
                    .collect()
            };

            let sig_detail = match method_res.as_ref().and_then(|r| r.receiver()) {
                Some(receiver) => {
                    let self_str = match receiver {
                        crate::hir::ReceiverKind::Value => "self".to_string(),
                        crate::hir::ReceiverKind::SharedRef => "&self".to_string(),
                        crate::hir::ReceiverKind::MutRef => "&mut self".to_string(),
                    };
                    let mut pts = vec![self_str];
                    pts.extend(params.iter().cloned());
                    let ret = callable
                        .as_ref()
                        .map(|c| c.return_ty().to_string())
                        .unwrap_or_else(|| "?".to_string());
                    format!("fn {name}({}) -> {ret}", pts.join(", "))
                }
                None => {
                    // Associated function — just use the type string.
                    candidate.ty().to_string()
                }
            };

            item = item
                .with_detail(sig_detail)
                .with_insert(CompletionInsert::Call {
                    callee: name,
                    params,
                });
        }
        items.push(item);
    }

    // Postfix completions — template expansions like `.if`, `.match` that
    // wrap the receiver expression.
    for (suffix, label, insert_text) in postfix_templates(&receiver_text) {
        if seen.insert(suffix.to_string()) {
            items.push(
                CompletionItem::new(suffix, CompletionKind::Keyword)
                    .with_detail(label)
                    .with_insert(CompletionInsert::Snippet(insert_text))
                    .with_relevance(85), // below fields/methods, above keywords
            );
        }
    }

    CompletionItem::normalize(&mut items);
    items
}

/// Return postfix template completions for a receiver expression.
/// Each tuple: (completion_label, detail_text, snippet_insert_text).
fn postfix_templates(receiver: &str) -> Vec<(&'static str, &'static str, String)> {
    vec![
        (
            ".if",
            "if expr { … }",
            format!("if {receiver} {{ $0 }}"),
        ),
        (
            ".match",
            "match expr { … }",
            format!("match {receiver} {{ $0 }}"),
        ),
        (
            ".not",
            "!expr",
            format!("!{receiver}"),
        ),
        (
            ".ref",
            "&expr",
            format!("&{receiver}"),
        ),
        (
            ".while",
            "while expr { … }",
            format!("while {receiver} {{ $0 }}"),
        ),
    ]
}

/// Get the text of the expression immediately left of `.`.
fn receiver_expr_text(
    db: &BaseDb,
    def_map: &DefMap,
    position: FilePosition,
    offset: u32,
) -> Option<String> {
    let (body, source_map, _inference) =
        find_containing_body_data(db, def_map, position, offset)?;
    let dot_pos = offset.saturating_sub(1);

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
    candidates.sort_by_key(|(len, _)| *len);
    let expr_id = candidates.first().map(|(_, id)| *id)?;
    let range = source_map.expr_range(expr_id)?;
    let text = db.file_text(position.file_id)?;
    let start = range.range.start() as usize;
    let end = range.range.end() as usize;
    if start <= end && end <= text.len() {
        Some(text[start..end].to_string())
    } else {
        None
    }
}

/// Try to infer the type of the expression immediately left of `.`.
/// Walks UP the syntax tree from the cursor token to find a
/// MethodCallExpr or FieldExpr, then resolves the receiver via HIR.
pub(crate) fn infer_dot_receiver(
    db: &Rc<BaseDb>,
    def_map: &DefMap,
    position: FilePosition,
    offset: u32,
) -> Option<Ty> {
    let text = db.file_text(position.file_id)?;
    let offset = offset.min(text.len() as u32);
    let parse = db.parse(position.file_id);
    let root = parse.syntax_node();
    // Use raw rowan here (not the wrapper). The wrapper prefers `right`
    // when `left` is Dot, which is correct for hover/goto-def (cursor
    // on the member name). Here the cursor may be on the dot itself or
    // on trailing whitespace — preferring the left token keeps the dot
    // reachable for the parent-chain walk into FieldExpr/MethodCallExpr.
    let end: u32 = root.text_range().end().into();
    let token = match root.token_at_offset(offset.min(end).into()) {
        rowan::TokenAtOffset::Single(t) => Some(t),
        rowan::TokenAtOffset::Between(l, _) => Some(l),
        _ => None,
    }?;

    // Walk up the syntax tree to find an enclosing field access or
    // method call. This is how rust-analyzer does it.
    let mut node = token.parent()?;
    let receiver_range: TextRange;
    loop {
        let kind = node.kind();
            if kind == rua_syntax::SyntaxKind::FieldExpr
            || kind == rua_syntax::SyntaxKind::MethodCallExpr
        {
            // Found the member access node. Get the receiver expression.
            let receiver = node
                .children()
                .find(|c| {
                    matches!(
                        c.kind(),
                        rua_syntax::SyntaxKind::PathExpr
                            | rua_syntax::SyntaxKind::Ident
                            | rua_syntax::SyntaxKind::CallExpr
                            | rua_syntax::SyntaxKind::MethodCallExpr
                            | rua_syntax::SyntaxKind::FieldExpr
                            | rua_syntax::SyntaxKind::IndexExpr
                            | rua_syntax::SyntaxKind::ParenExpr
                            | rua_syntax::SyntaxKind::Block
                            | rua_syntax::SyntaxKind::ArrayExpr
                            | rua_syntax::SyntaxKind::UnaryExpr
                            | rua_syntax::SyntaxKind::BinExpr
                            | rua_syntax::SyntaxKind::StructLitExpr
                            | rua_syntax::SyntaxKind::ClosureExpr
                            | rua_syntax::SyntaxKind::LiteralExpr
                    )
                })?;
            receiver_range = {
                let start: u32 = receiver.text_range().start().into();
                let end: u32 = receiver.text_range().end().into();
                TextRange::new(start, end)
            };
            break;
        }
        node = node.parent()?;
    }

    // Find the HIR expression whose range matches the receiver's range.
    let (body, _source_map, inference) =
        find_containing_body_data(db, def_map, position, offset)?;
    for (expr_id, _expr) in body.exprs() {
        let fr = _source_map.expr_range(expr_id)?;
        if fr.range == receiver_range {
            return inference.type_of_expr(expr_id).cloned();
        }
    }
    // Fallback: try to find by containing the receiver range.
    for (expr_id, _expr) in body.exprs() {
        let fr = _source_map.expr_range(expr_id)?;
        if fr.range.contains(receiver_range.start())
            || fr.range.start() == receiver_range.start()
        {
            let ty = inference.type_of_expr(expr_id).cloned()?;
            if !matches!(&ty, Ty::Primitive(crate::hir::PrimitiveTy::Unit)) {
                return Some(ty);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Path completion (after `::`)
// ---------------------------------------------------------------------------

fn path_completions(
    db: &Rc<BaseDb>,
    position: FilePosition,
    token: &SyntaxToken,
    partial_range: TextRange,
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
                    if let Some(doc) = extract_doc_comment(db, position.file_id, definition) {
                        item = item.with_documentation(doc);
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

    if !partial_range.is_empty() {
        for item in &mut items {
            if item.replacement_range().is_none() {
                *item = item.clone().with_replacement_range(partial_range);
            }
        }
    }

    CompletionItem::normalize(&mut items);
    items
}

/// Collect path segments leading up to the `::` before `token`.
///
/// The caller has already verified that `previous_significant(token)` is `::`,
/// so we skip past it and walk left to collect the qualifying path segments
/// (e.g. for `std::collections::|` this returns `["std", "collections"]`).
fn path_segments_before(token: &SyntaxToken) -> Vec<String> {
    // Step past the `::` that the caller already found.
    let Some(colon) = previous_significant(token) else {
        return vec![];
    };
    debug_assert_eq!(colon.kind(), SyntaxKind::ColonColon);

    // The segment immediately left of `::` is the last path segment.
    let Some(mut current) = previous_significant(&colon) else {
        return vec![];
    };
    if !is_path_identifier(&current) {
        return vec![];
    }
    let mut segments = vec![current.text().to_string()];

    // Walk further left over `::segment` pairs.
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

/// Score how well a candidate type string matches an expected type.
/// Returns 0-10, where 10 = exact match, 5 = coercible, 0 = no match.
fn type_compatibility_score(detail: &str, expected_str: &str, _expected: &Ty) -> u16 {
    if detail.contains(expected_str) {
        return 10; // exact match
    }
    // Coercible numeric types
    if (expected_str == "i64" || expected_str == "f64")
        && (detail.contains("i64") || detail.contains("f64"))
    {
        return 5;
    }
    // Option/Result compatibility (simplified)
    if (expected_str.starts_with("Option") && detail.contains("Some"))
        || (expected_str.starts_with("Result") && (detail.contains("Ok") || detail.contains("Err")))
    {
        return 5;
    }
    0
}

/// Try to infer the expected type at the cursor position from surrounding
/// code patterns (call arguments, return position).
fn expected_type_at_cursor(
    db: &BaseDb,
    def_map: &DefMap,
    position: FilePosition,
    offset: u32,
) -> Option<Ty> {
    let (body, source_map, inference) =
        find_containing_body_data(db, def_map, position, offset)?;

    for (expr_id, expr) in body.exprs() {
        // Cursor inside a function call argument list.
        let args: &[ExprId] = match expr {
            Expr::Call { args, .. } => args.as_slice(),
            Expr::MethodCall { args, .. } => args.as_slice(),
            _ => continue,
        };
        let Some(expr_range) = source_map.expr_range(expr_id) else {
            continue;
        };
        if !expr_range.range.contains(offset) {
            continue;
        }
        // Find which argument the cursor is in.
        let callable_ty = inference.type_of_expr(expr_id)?.clone();
        let params = match &callable_ty {
            Ty::Function(c) | Ty::Closure(c) => c.params().to_vec(),
            _ => return None,
        };
        for (i, arg_id) in args.iter().enumerate() {
            let Some(arg_range) = source_map.expr_range(*arg_id) else {
                continue;
            };
            if arg_range.range.contains(offset) {
                return params.get(i).cloned();
            }
        }
        // Cursor between arguments or after `(` but before first arg.
        if params.len() == 1 {
            return params.first().cloned();
        }
    }
    None
}

/// Count how many times each local name is referenced in scope (for ranking).
fn local_usage_counts(
    db: &BaseDb,
    def_map: &DefMap,
    position: FilePosition,
    offset: u32,
) -> std::collections::HashMap<String, usize> {
    let mut counts = std::collections::HashMap::new();
    let Some((body, _source_map, _scopes, _inference)) =
        find_containing_body_full(db, def_map, position, offset)
    else {
        return counts;
    };
    let owner_id = match innermost_body_owner(def_map, position, offset) {
        Some(d) => d.id(),
        None => return counts,
    };
    let Some(resolution) = db.body_resolution(owner_id) else {
        return counts;
    };
    for (name_ref_id, nr) in body.name_refs() {
        if let Some(name) = nr.name()
            && let Some(crate::hir::LocalResolveResult::Resolved(_)) =
                resolution.resolve(name_ref_id)
        {
            *counts.entry(name.to_string()).or_insert(0) += 1;
        }
    }
    counts
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

pub(crate) type BodyData =
    (Arc<Body>, Arc<BodySourceMap>, Arc<InferenceResult>);

type BodyFullData =
    (Arc<Body>, Arc<BodySourceMap>, Arc<BodyScopes>, Option<Arc<InferenceResult>>);

/// Find the innermost function/method body containing `offset`.
pub(crate) fn find_containing_body_data(
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

pub(crate) fn token_at_offset(node: &rua_syntax::SyntaxNode, offset: u32) -> Option<SyntaxToken> {
    let end: u32 = node.text_range().end().into();
    match node.token_at_offset(offset.min(end).into()) {
        rowan::TokenAtOffset::Single(token) => Some(token),
        rowan::TokenAtOffset::Between(left, right) => {
            // If exactly at the boundary between `.` and the field/method name,
            // prefer the name token (right). This makes hover and goto-def
            // work when the cursor lands on the first character of the member.
            if left.kind() == SyntaxKind::Dot {
                Some(right)
            } else {
                Some(left)
            }
        }
        _ => None,
    }
}

fn is_path_identifier(token: &SyntaxToken) -> bool {
    matches!(token.kind(), SyntaxKind::Ident | SyntaxKind::KwSelf)
}

pub(crate) fn previous_significant(token: &SyntaxToken) -> Option<SyntaxToken> {
    let mut token = token.prev_token();
    while token.as_ref().is_some_and(|token| token.kind().is_trivia()) {
        token = token.and_then(|token| token.prev_token());
    }
    token
}

fn next_significant(token: &SyntaxToken) -> Option<SyntaxToken> {
    let mut token = token.next_token();
    while token.as_ref().is_some_and(|token| token.kind().is_trivia()) {
        token = token.and_then(|token| token.next_token());
    }
    token
}

/// If the cursor is inside a struct literal `Type { | }`, return the struct
/// type so we can enumerate fields.
fn struct_literal_type(
    db: &BaseDb,
    def_map: &DefMap,
    position: FilePosition,
    offset: u32,
) -> Option<Ty> {
    let (body, source_map, inference) =
        find_containing_body_data(db, def_map, position, offset)?;
    for (expr_id, expr) in body.exprs() {
        let Expr::StructLiteral { path: _, .. } = expr else { continue };
        let range = source_map.expr_range(expr_id)?;
        if !range.range.contains(offset) {
            continue;
        }
        // Get the struct type from inference.
        let ty = inference.type_of_expr(expr_id)?.clone();
        // Verify it's a struct (Named type with struct definition).
        if let Ty::Named(named) = &ty {
            let def = def_map.definition(named.definition())?;
            if def.kind() == DefKind::Struct {
                return Some(ty);
            }
        }
    }
    None
}

/// If the cursor is inside an if-let or while-let pattern with an enum
/// scrutinee, return the scrutinee type and its enum template.
fn pattern_scrutinee_enum(
    db: &BaseDb,
    def_map: &DefMap,
    position: FilePosition,
    offset: u32,
) -> Option<(Ty, Ty)> {
    let (body, source_map, inference) =
        find_containing_body_data(db, def_map, position, offset)?;

    // 1. if-let: the entire `if` is an expression; check whether the cursor
    //    falls inside one whose condition is `Condition::Let`.
    for (expr_id, expr) in body.exprs() {
        let scrutinee = match expr {
            crate::hir::Expr::If {
                condition: crate::hir::Condition::Let { scrutinee, .. },
                ..
            } => scrutinee,
            _ => continue,
        };
        let range = source_map.expr_range(expr_id)?;
        if !range.range.contains(offset) {
            continue;
        }
        let scrutinee_ty = inference.type_of_expr(*scrutinee)?.clone();
        if let Ty::Named(named) = &scrutinee_ty {
            let def = def_map.definition(named.definition())?;
            if def.kind() == DefKind::Enum {
                let member_index = db.member_index(position.file_id);
                let template = member_index.type_template(def.id())?.clone();
                return Some((scrutinee_ty, template));
            }
        }
    }

    // 2. while-let: `While` is a Statement, not an Expr, so it doesn't
    //    appear in body.exprs(). Walk the blocks to find one whose
    //    body expression range contains the cursor (the cursor is inside
    //    the while body), then check whether the enclosing statement is a
    //    `While` with `Condition::Let`.
    for (_expr_id, expr) in body.exprs() {
        let block = match expr {
            crate::hir::Expr::Block(b) => b,
            _ => continue,
        };
        for stmt in block.statements() {
            let (scrutinee, body_expr) = match stmt {
                crate::hir::Statement::While {
                    condition: crate::hir::Condition::Let { scrutinee, .. },
                    body,
                } => (scrutinee, body),
                _ => continue,
            };
            let Some(body_range) = source_map.expr_range(*body_expr) else {
                continue;
            };
            // The pattern sits to the left of the body's opening brace.
            // Accept any offset from a generous left margin up to the
            // body start so we cover `while let |` and `while let Some(|`.
            let left = body_range.range.start().saturating_sub(100);
            if offset >= left && offset <= body_range.range.start() {
                let scrutinee_ty = inference.type_of_expr(*scrutinee)?.clone();
                if let Ty::Named(named) = &scrutinee_ty {
                    let def = def_map.definition(named.definition())?;
                    if def.kind() == DefKind::Enum {
                        let member_index = db.member_index(position.file_id);
                        let template =
                            member_index.type_template(def.id())?.clone();
                        return Some((scrutinee_ty, template));
                    }
                }
            }
        }
    }

    None
}

/// If the cursor is inside a match expression body, return the scrutinee
/// enum type and its type template so we can enumerate variants.
fn match_scrutinee_enum(
    db: &BaseDb,
    def_map: &DefMap,
    position: FilePosition,
    offset: u32,
) -> Option<(Ty, Ty)> {
    let (body, source_map, inference) =
        find_containing_body_data(db, def_map, position, offset)?;
    for (expr_id, expr) in body.exprs() {
        let Expr::Match { scrutinee, .. } = expr else {
            continue;
        };
        let range = source_map.expr_range(expr_id)?;
        if !range.range.contains(offset) {
            continue;
        }
        let scrutinee_ty = inference.type_of_expr(*scrutinee)?.clone();
        // Check if the scrutinee is a named enum type.
        if let Ty::Named(named) = &scrutinee_ty {
            let enum_def = def_map.definition(named.definition())?;
            if enum_def.kind() == DefKind::Enum {
                let member_index = db.member_index(position.file_id);
                let template = member_index.type_template(enum_def.id())?.clone();
                return Some((scrutinee_ty, template));
            }
        }
    }
    None
}

/// Extract `///` doc comments immediately preceding a definition in the CST.
fn extract_doc_comment(
    db: &BaseDb,
    file_id: FileId,
    definition: &Definition,
) -> Option<String> {
    let parse = db.parse(file_id);
    let root = parse.syntax_node();
    // Start from the definition keyword (e.g. `fn`, `struct`).
    let range_start: u32 = definition.range().start();
    let token = token_at_offset(root, range_start)?;

    let mut doc_lines: Vec<String> = Vec::new();
    let mut current = Some(token);

    // Walk backwards through trivia to collect consecutive /// comments.
    loop {
        let prev = current.as_ref().and_then(|t| t.prev_token());
        match prev {
            None => break,
            Some(ref pt) if pt.kind() == SyntaxKind::LineComment && pt.text().starts_with("///") =>
            {
                let text = pt.text();
                let doc = text.strip_prefix("///").unwrap_or(text).trim();
                doc_lines.push(doc.to_string());
                current = prev;
            }
            Some(ref pt) if pt.kind().is_trivia() => {
                current = prev; // skip whitespace between doc comment and keyword
            }
            Some(_) => break, // hit a non-trivia token — stop
        }
    }

    if doc_lines.is_empty() {
        return None;
    }
    doc_lines.reverse();
    Some(doc_lines.join("\n"))
}

/// Token kinds that indicate the cursor is in expression context.
const EXPR_CONTEXT_TOKENS: &[SyntaxKind] = &[
    SyntaxKind::Eq,
    SyntaxKind::KwReturn,
    SyntaxKind::LParen,
    SyntaxKind::LBracket,
    SyntaxKind::Comma,
    SyntaxKind::Plus,
    SyntaxKind::Minus,
    SyntaxKind::Star,
    SyntaxKind::Slash,
    SyntaxKind::Amp,
    SyntaxKind::Pipe,
    SyntaxKind::Colon,
    SyntaxKind::FatArrow,
    SyntaxKind::KwIf,
    SyntaxKind::KwWhile,
    SyntaxKind::KwFor,
    SyntaxKind::KwMatch,
    SyntaxKind::KwIn,
    SyntaxKind::Dot,
];

/// Check whether the cursor is in a type position (after `:` in a type
/// annotation, not after `::`).
fn is_type_position(token: &SyntaxToken) -> bool {
    // Cursor on `:` itself (e.g. `let x:|`)
    if token.kind() == SyntaxKind::Colon {
        let before = previous_significant(token);
        return before.is_none_or(|t| t.kind() != SyntaxKind::Colon);
    }
    // Cursor on whitespace after `:` (e.g. `let x: |`)
    let Some(prev) = previous_significant(token) else {
        return false;
    };
    if prev.kind() != SyntaxKind::Colon {
        return false;
    }
    // Exclude `::` — the colon before another colon is a path separator.
    let before_colon = previous_significant(&prev);
    before_colon.is_none_or(|t| t.kind() != SyntaxKind::Colon)
}

/// Check whether the cursor is in an expression context (e.g. after `=`,
/// `return`, `(`, `[`, `,`, operators) where declaration keywords like `fn`,
/// `struct` shouldn't appear.
//
/// We check the token under the cursor, the preceding significant token,
/// and the next significant token (for boundary cases where the cursor
/// sits between whitespace and `=`).
fn is_expression_context(token: &SyntaxToken) -> bool {
    if EXPR_CONTEXT_TOKENS.contains(&token.kind()) {
        return true;
    }
    if let Some(prev) = previous_significant(token)
        && EXPR_CONTEXT_TOKENS.contains(&prev.kind())
    {
        return true;
    }
    // Boundary case: cursor is in trivia just before a context token
    // (e.g. the space between `x` and `=`).
    if token.kind().is_trivia()
        && let Some(next) = next_significant(token)
        && EXPR_CONTEXT_TOKENS.contains(&next.kind())
    {
        return true;
    }
    false
}
