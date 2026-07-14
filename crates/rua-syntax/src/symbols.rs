//! CST-based symbol index — a lightweight pass over the rowan syntax tree that
//! collects every named definition with its kind, byte range, container path,
//! and a human-readable signature summary.
//!
//! All public types are rowan-free PODs; consumers (LSP) never touch
//! `SyntaxNode` / `SyntaxToken` / `TextRange`.
//!
//! # Architecture
//!
//! - [`collect_symbols`] walks [`SourceFile::items`](crate::ast::SourceFile)
//!   recursively, pushing/popping a container stack for nested definitions.
//! - [`ident_at_offset`] delegates to rowan's `token_at_offset` and returns
//!   `None` for non-Ident tokens / whitespace / out-of-bounds.

use rowan::TextSize;

use crate::ast::{
    AstNode, EnumDecl, ExternBlock, ExternFn, FieldDecl, FnDecl, ImplDecl, Item, ModDecl, Named,
    SourceFile, StructDecl, TraitDecl, TraitMethod,
};
use crate::kind::SyntaxKind;
use crate::{SyntaxElement, SyntaxNode};

// --- public types -----------------------------------------------------------

/// Grammar-level symbol kind. Maps to `lsp_types::SymbolKind` in the LSP crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Trait,
    Impl,
    Method,
    Field,
    Variant,
    Module,
    ExternFn,
}

/// A single definition site discovered in the CST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    /// Declaration name (the identifier text).
    pub name: String,
    pub kind: SymbolKind,
    /// Byte range of the name [`SyntaxKind::Ident`] token.
    pub name_range: (usize, usize),
    /// Byte range of the entire definition node (e.g. the full `FnDecl`).
    pub full_range: (usize, usize),
    /// Parent path from outermost to innermost (e.g. `["geo", "Point"]`).
    /// Empty for top-level definitions.
    pub container: Vec<String>,
    /// Human-readable signature summary (e.g. `"fn foo(x: i64) -> Bar"`).
    pub detail: String,
    /// Doc text: contiguous leading `//` / `/* */` comments immediately above
    /// the definition, with delimiters stripped. Empty when undocumented.
    pub doc: String,
}

/// Result of a cursor-hit lookup at a byte offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentHit {
    /// The text of the identifier under the cursor.
    pub text: String,
    /// Byte range of the identifier token.
    pub range: (usize, usize),
}

// --- public API -------------------------------------------------------------

/// Collect every named definition from a source file.
pub fn collect_symbols(file: &SourceFile) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    let mut container = Vec::new();
    collect_items(&mut file.items(), &mut container, &mut symbols);
    symbols
}

/// Find the [`SyntaxKind::Ident`] token at a byte offset within the file.
///
/// Returns `None` when `offset` falls on whitespace, a comment, a keyword, or
/// any non-Ident token. For offsets exactly *between* two tokens, prefers the
/// right-hand Ident if one exists, otherwise the left.
pub fn ident_at_offset(file: &SourceFile, offset: usize) -> Option<IdentHit> {
    // Clamp to the tree's text range to avoid rowan panicking on out-of-bounds.
    let max_offset = usize::from(file.syntax().text_range().end());
    let offset = offset.min(max_offset);
    let pos = TextSize::from(offset as u32);
    let token = match file.syntax().token_at_offset(pos) {
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

    let r = token.text_range();
    Some(IdentHit {
        text: token.text().to_string(),
        range: (usize::from(r.start()), usize::from(r.end())),
    })
}

// --- internal walkers -------------------------------------------------------

fn collect_items(
    items: &mut dyn Iterator<Item = Item>,
    container: &mut Vec<String>,
    symbols: &mut Vec<Symbol>,
) {
    for item in items {
        match item {
            Item::Fn(f) => collect_fn(&f, container, symbols),
            Item::Struct(s) => collect_struct(&s, container, symbols),
            Item::Enum(e) => collect_enum(&e, container, symbols),
            Item::Trait(t) => collect_trait(&t, container, symbols),
            Item::Impl(i) => collect_impl(&i, container, symbols),
            Item::Extern(eb) => collect_extern(&eb, container, symbols),
            Item::Mod(m) => collect_mod(&m, container, symbols),
            Item::Use(_) => { /* use declarations are not definition symbols */ }
        }
    }
}

fn collect_fn(f: &FnDecl, container: &[String], symbols: &mut Vec<Symbol>) {
    collect_fn_with_kind(f, container, symbols, SymbolKind::Function);
}

/// Collect a function definition, using `kind` for the symbol kind
/// (e.g. `Method` for impl/trait methods).
fn collect_fn_with_kind(
    f: &FnDecl,
    container: &[String],
    symbols: &mut Vec<Symbol>,
    kind: SymbolKind,
) {
    let name = f.name_text().unwrap_or_default();
    if name.is_empty() {
        return;
    }
    let detail = fn_detail(f);
    symbols.push(Symbol {
        name,
        kind,
        name_range: token_byte_range(f.name()),
        full_range: node_byte_range(f.syntax()),
        container: container.to_vec(),
        detail,
        doc: documentation(f.syntax()).unwrap_or_default(),
    });
}

fn collect_struct(s: &StructDecl, container: &mut Vec<String>, symbols: &mut Vec<Symbol>) {
    let name = s.name_text().unwrap_or_default();
    if name.is_empty() {
        return;
    }
    symbols.push(Symbol {
        name: name.clone(),
        kind: SymbolKind::Struct,
        name_range: token_byte_range(s.name()),
        full_range: node_byte_range(s.syntax()),
        container: container.clone(),
        detail: format!("struct {}", name),
        doc: documentation(s.syntax()).unwrap_or_default(),
    });

    container.push(name);
    if let Some(fl) = s.field_list() {
        for field in fl.fields() {
            collect_field(&field, container, symbols);
        }
    }
    container.pop();
}

fn collect_enum(e: &EnumDecl, container: &mut Vec<String>, symbols: &mut Vec<Symbol>) {
    let name = e.name_text().unwrap_or_default();
    if name.is_empty() {
        return;
    }
    symbols.push(Symbol {
        name: name.clone(),
        kind: SymbolKind::Enum,
        name_range: token_byte_range(e.name()),
        full_range: node_byte_range(e.syntax()),
        container: container.clone(),
        detail: format!("enum {}", name),
        doc: documentation(e.syntax()).unwrap_or_default(),
    });

    container.push(name);
    if let Some(vl) = e.variant_list() {
        for variant in vl.variants() {
            let vname = variant.name_text().unwrap_or_default();
            if vname.is_empty() {
                continue;
            }
            symbols.push(Symbol {
                name: vname,
                kind: SymbolKind::Variant,
                name_range: token_byte_range(variant.name()),
                full_range: node_byte_range(variant.syntax()),
                container: container.clone(),
                detail: String::new(),
                doc: documentation(variant.syntax()).unwrap_or_default(),
            });
        }
    }
    container.pop();
}

fn collect_trait(t: &TraitDecl, container: &mut Vec<String>, symbols: &mut Vec<Symbol>) {
    let name = t.name_text().unwrap_or_default();
    if name.is_empty() {
        return;
    }
    symbols.push(Symbol {
        name: name.clone(),
        kind: SymbolKind::Trait,
        name_range: token_byte_range(t.name()),
        full_range: node_byte_range(t.syntax()),
        container: container.clone(),
        detail: format!("trait {}", name),
        doc: documentation(t.syntax()).unwrap_or_default(),
    });

    container.push(name);
    for method in t.methods() {
        collect_trait_method(&method, container, symbols);
    }
    container.pop();
}

fn collect_impl(i: &ImplDecl, container: &mut Vec<String>, symbols: &mut Vec<Symbol>) {
    let type_name = i
        .type_name()
        .map(|t| t.text().to_string())
        .unwrap_or_default();
    if type_name.is_empty() {
        return;
    }
    let trait_name = i.trait_name().map(|t| t.text().to_string());
    let detail = match trait_name {
        Some(ref tn) => format!("impl {} for {}", tn, type_name),
        None => format!("impl {}", type_name),
    };
    let name_for_impl = format!("impl {}", type_name);

    let name_range = i
        .type_name()
        .map(|t| {
            let r = t.text_range();
            (usize::from(r.start()), usize::from(r.end()))
        })
        .unwrap_or_default();

    symbols.push(Symbol {
        name: name_for_impl.clone(),
        kind: SymbolKind::Impl,
        name_range,
        full_range: node_byte_range(i.syntax()),
        container: container.clone(),
        detail,
        doc: documentation(i.syntax()).unwrap_or_default(),
    });

    // Nest methods under the impl node itself (its symbol name, e.g. "impl
    // Point"), not the bare type name — otherwise they would attach to a
    // same-named `struct`/`enum` node in the outline and leave the impl empty.
    container.push(name_for_impl);
    for method in i.methods() {
        collect_fn_with_kind(&method, container, symbols, SymbolKind::Method);
    }
    container.pop();
}

fn collect_extern(eb: &ExternBlock, container: &[String], symbols: &mut Vec<Symbol>) {
    for ef in eb.fns() {
        let name = ef.name_text().unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let detail = extern_fn_detail(&ef);
        symbols.push(Symbol {
            name,
            kind: SymbolKind::ExternFn,
            name_range: token_byte_range(ef.name()),
            full_range: node_byte_range(ef.syntax()),
            container: container.to_vec(),
            detail,
            doc: documentation(ef.syntax()).unwrap_or_default(),
        });
    }
}

fn collect_mod(m: &ModDecl, container: &mut Vec<String>, symbols: &mut Vec<Symbol>) {
    let name = m.name_text().unwrap_or_default();
    if name.is_empty() {
        return;
    }
    symbols.push(Symbol {
        name: name.clone(),
        kind: SymbolKind::Module,
        name_range: token_byte_range(m.name()),
        full_range: node_byte_range(m.syntax()),
        container: container.clone(),
        detail: format!("mod {}", name),
        doc: module_documentation(m.syntax())
            .or_else(|| documentation(m.syntax()))
            .unwrap_or_default(),
    });

    // Only recurse into inline modules; file modules have their body
    // in a different file and won't be in this tree.
    if !m.is_file() {
        container.push(name);
        collect_items(&mut m.items(), container, symbols);
        container.pop();
    }
}

fn collect_field(f: &FieldDecl, container: &[String], symbols: &mut Vec<Symbol>) {
    let name = f.name_text().unwrap_or_default();
    if name.is_empty() {
        return;
    }
    symbols.push(Symbol {
        name,
        kind: SymbolKind::Field,
        name_range: token_byte_range(f.name()),
        full_range: node_byte_range(f.syntax()),
        container: container.to_vec(),
        detail: String::new(),
        doc: documentation(f.syntax()).unwrap_or_default(),
    });
}

fn collect_trait_method(m: &TraitMethod, container: &[String], symbols: &mut Vec<Symbol>) {
    let name = m.name_text().unwrap_or_default();
    if name.is_empty() {
        return;
    }
    let detail = trait_method_detail(m);
    symbols.push(Symbol {
        name,
        kind: SymbolKind::Method,
        name_range: token_byte_range(m.name()),
        full_range: node_byte_range(m.syntax()),
        container: container.to_vec(),
        detail,
        doc: documentation(m.syntax()).unwrap_or_default(),
    });
}

// --- detail synthesis -------------------------------------------------------

/// Signature summary for a function/method: the declaration's own source text
/// up to its body, so parameter types, the return type, generics, `self`
/// receiver, and `where` clause are all shown faithfully (e.g.
/// `fn find(v: Vec<i64>, target: i64) -> Option<i64>`).
fn fn_detail(f: &FnDecl) -> String {
    let cut = f
        .body()
        .map(|b| usize::from(b.syntax().text_range().start()));
    node_signature(f.syntax(), cut)
}

/// Signature for a trait method — like [`fn_detail`], cutting at the default
/// body when one is present (declaration-only methods have none).
fn trait_method_detail(m: &TraitMethod) -> String {
    let cut = m
        .default_body()
        .map(|b| usize::from(b.syntax().text_range().start()));
    node_signature(m.syntax(), cut)
}

/// Signature for an `extern` function declaration (never has a body). The
/// `extern` keyword lives on the enclosing block, not the fn node, so re-add it.
fn extern_fn_detail(ef: &ExternFn) -> String {
    format!("extern {}", node_signature(ef.syntax(), None))
}

/// Render a declaration's signature: the node's source text up to `cut_abs`
/// (an absolute byte offset, typically the body block's start; `None` uses the
/// whole node), with all whitespace runs collapsed to single spaces and any
/// trailing `{` / `;` / space removed.
fn node_signature(node: &SyntaxNode, cut_abs: Option<usize>) -> String {
    let node_start = usize::from(node.text_range().start());
    let full = node.text().to_string();
    let end = cut_abs
        .map(|c| c.saturating_sub(node_start).min(full.len()))
        .unwrap_or(full.len());
    let collapsed = full[..end].split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
        .trim_end_matches([' ', '{', ';'])
        .trim()
        .to_string()
}

// --- doc-comment extraction -------------------------------------------------

/// Extract the doc text for a definition `node`: the contiguous run of leading
/// `//` / `/* */` comments that sit immediately above it, with delimiters
/// stripped. Returns an empty string when the node is undocumented.
///
/// Handles both CST placements: comments the parser kept as preceding siblings
/// (top-level items, block/mod children, impl & trait methods) and comments it
/// absorbed as the node's own leading trivia (struct fields, enum variants,
/// extern fns). A blank line between the comment block and the node detaches it
/// (matching the convention that a doc comment hugs its item).
pub fn documentation(node: &SyntaxNode) -> Option<String> {
    let mut texts = absorbed_leading_comments(node);
    if texts.is_empty() {
        texts = preceding_sibling_comments(node);
    }
    let rendered = render_doc(&texts);
    (!rendered.is_empty()).then_some(rendered)
}

/// Extract inner module documentation (`//!` / `/*! ... */`) immediately after
/// an inline module's opening brace.
pub fn module_documentation(node: &SyntaxNode) -> Option<String> {
    let mut saw_open_brace = false;
    let mut texts = Vec::new();
    for element in node.children_with_tokens() {
        match element {
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::LBrace => {
                saw_open_brace = true;
            }
            _ if !saw_open_brace => {}
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::Whitespace => {
                if token.text().matches('\n').count() >= 2 && !texts.is_empty() {
                    break;
                }
            }
            SyntaxElement::Token(token) if token.kind().is_comment() => {
                let text = token.text().trim();
                if text.starts_with("//!") || text.starts_with("/*!") {
                    texts.push(token.text().to_string());
                } else {
                    break;
                }
            }
            _ => break,
        }
    }
    let rendered = render_doc(&texts);
    (!rendered.is_empty()).then_some(rendered)
}

/// Comments the parser absorbed as `node`'s own leading trivia (before its first
/// real token). Only comments that follow a newline count as leading — a
/// same-line comment belongs to the previous sibling, so it is ignored here.
fn absorbed_leading_comments(node: &SyntaxNode) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen_nl = false;
    for elem in node.children_with_tokens() {
        match elem {
            SyntaxElement::Token(t) if t.kind().is_comment() => {
                if seen_nl {
                    out.push(t.text().to_string());
                }
            }
            SyntaxElement::Token(t) if t.kind() == SyntaxKind::Whitespace => {
                if t.text().contains('\n') {
                    seen_nl = true;
                }
                // A blank line detaches any comments gathered so far.
                if t.text().matches('\n').count() >= 2 {
                    out.clear();
                }
            }
            // First real token or child node ends the leading-trivia region.
            _ => break,
        }
    }
    out
}

/// Comments that appear as preceding siblings of `node` (in source order),
/// walking backwards until a real token/node or a blank line is reached.
fn preceding_sibling_comments(node: &SyntaxNode) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = node.prev_sibling_or_token();
    while let Some(SyntaxElement::Token(t)) = cur {
        if t.kind().is_comment() {
            out.push(t.text().to_string());
        } else if t.kind() == SyntaxKind::Whitespace {
            // A blank line (≥2 newlines) detaches the comment from the node.
            if t.text().matches('\n').count() >= 2 {
                break;
            }
        } else {
            break;
        }
        cur = t.prev_sibling_or_token();
    }
    out.reverse();
    out
}

/// Strip comment delimiters from raw comment texts and join into doc lines.
fn render_doc(texts: &[String]) -> String {
    let mut lines: Vec<String> = Vec::new();
    let docs = texts
        .iter()
        .rev()
        .take_while(|raw| is_doc_comment(raw))
        .collect::<Vec<_>>();
    for raw in docs.into_iter().rev() {
        let s = raw.trim();
        if let Some(rest) = s.strip_prefix("///").or_else(|| s.strip_prefix("//!")) {
            lines.push(rest.trim().to_string());
        } else if let Some(inner) = s
            .strip_prefix("/**")
            .or_else(|| s.strip_prefix("/*!"))
            .and_then(|body| body.strip_suffix("*/"))
        {
            for l in inner.lines() {
                lines.push(l.trim().trim_start_matches('*').trim().to_string());
            }
        }
    }
    lines.join("\n").trim().to_string()
}

fn is_doc_comment(raw: &str) -> bool {
    let text = raw.trim();
    text.starts_with("///")
        || text.starts_with("//!")
        || (text.starts_with("/**") && text.ends_with("*/"))
        || (text.starts_with("/*!") && text.ends_with("*/"))
}

// --- byte-range helpers -----------------------------------------------------

fn token_byte_range(tok: Option<crate::SyntaxToken>) -> (usize, usize) {
    tok.map(|t| {
        let r = t.text_range();
        (usize::from(r.start()), usize::from(r.end()))
    })
    .unwrap_or_default()
}

fn node_byte_range(node: &SyntaxNode) -> (usize, usize) {
    let r = node.text_range();
    (usize::from(r.start()), usize::from(r.end()))
}

// --- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_source_file;

    fn file(src: &str) -> SourceFile {
        parse_source_file(src).tree
    }

    fn syms(src: &str) -> Vec<Symbol> {
        collect_symbols(&file(src))
    }

    fn find<'a>(syms: &'a [Symbol], name: &str) -> &'a Symbol {
        syms.iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("symbol `{name}` not found in {syms:#?}"))
    }

    // --- top-level items ----------------------------------------------------

    #[test]
    fn top_level_fn() {
        let s = syms("fn add(x: i64, y: i64) -> i64 { x + y }");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "add");
        assert_eq!(s[0].kind, SymbolKind::Function);
        assert!(s[0].container.is_empty());
        assert_eq!(s[0].detail, "fn add(x: i64, y: i64) -> i64");
    }

    #[test]
    fn fn_detail_shows_param_and_return_types() {
        let s = syms("fn find(v: Vec<i64>, target: i64) -> Option<i64> { None }");
        assert_eq!(
            find(&s, "find").detail,
            "fn find(v: Vec<i64>, target: i64) -> Option<i64>"
        );
    }

    #[test]
    fn method_detail_shows_self_receiver() {
        let s = syms("impl P { fn get(&self) -> i64 { 0 } }");
        assert_eq!(find(&s, "get").detail, "fn get(&self) -> i64");
    }

    #[test]
    fn fn_detail_collapses_multiline_signature() {
        let s = syms("fn wide(\n    a: i64,\n    b: i64,\n) -> i64 { 0 }");
        let d = &find(&s, "wide").detail;
        assert!(!d.contains('\n'), "collapsed to one line: {d}");
        assert!(d.contains("a: i64") && d.contains("b: i64") && d.contains("-> i64"));
    }

    #[test]
    fn top_level_fn_no_params() {
        let s = syms("fn main() {}");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "main");
        assert_eq!(s[0].kind, SymbolKind::Function);
        assert!(s[0].detail.contains("fn main()"));
        assert!(!s[0].detail.contains("->"));
    }

    #[test]
    fn top_level_struct_with_fields() {
        let s = syms("struct P { x: i64, y: f64 }");
        assert_eq!(s.len(), 3);
        let st = find(&s, "P");
        assert_eq!(st.kind, SymbolKind::Struct);
        let fx = find(&s, "x");
        assert_eq!(fx.kind, SymbolKind::Field);
        assert_eq!(fx.container, vec!["P"]);
        let fy = find(&s, "y");
        assert_eq!(fy.kind, SymbolKind::Field);
        assert_eq!(fy.container, vec!["P"]);
    }

    #[test]
    fn unit_struct() {
        let s = syms("struct Marker;");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].kind, SymbolKind::Struct);
        assert_eq!(s[0].name, "Marker");
    }

    #[test]
    fn enum_with_variants() {
        let s = syms("enum Shape { Circle(f64), Rect { w: f64, h: f64 }, Dot }");
        assert_eq!(s.len(), 4); // 1 enum + 3 variants
        let en = find(&s, "Shape");
        assert_eq!(en.kind, SymbolKind::Enum);
        let v = find(&s, "Circle");
        assert_eq!(v.kind, SymbolKind::Variant);
        assert_eq!(v.container, vec!["Shape"]);
        assert!(
            s.iter()
                .any(|x| x.name == "Rect" && x.kind == SymbolKind::Variant)
        );
        assert!(
            s.iter()
                .any(|x| x.name == "Dot" && x.kind == SymbolKind::Variant)
        );
    }

    #[test]
    fn trait_with_methods() {
        let s = syms("trait Draw { fn draw(&self); fn size() -> i64 { 0 } }");
        let trait_count = s.iter().filter(|x| x.kind == SymbolKind::Trait).count();
        assert_eq!(trait_count, 1);
        let t = find(&s, "Draw");
        assert_eq!(t.kind, SymbolKind::Trait);
        let method_count = s
            .iter()
            .filter(|x| x.kind == SymbolKind::Method && x.container == vec!["Draw"])
            .count();
        assert_eq!(method_count, 2);
        assert!(s.iter().any(|x| x.name == "draw"));
        assert!(s.iter().any(|x| x.name == "size"));
    }

    #[test]
    fn impl_inherent() {
        let s = syms("impl Point { fn new() -> Point { Point{} } fn bump(&mut self) {} }");
        let imp = s.iter().find(|x| x.kind == SymbolKind::Impl).unwrap();
        assert!(imp.detail.contains("impl Point"));
        assert!(imp.detail.contains("Point"));
        let new_fn = find(&s, "new");
        assert_eq!(new_fn.kind, SymbolKind::Method);
        assert_eq!(new_fn.container, vec!["impl Point"]);
        let bump = find(&s, "bump");
        assert_eq!(bump.container, vec!["impl Point"]);
    }

    #[test]
    fn impl_trait() {
        let s = syms("impl Draw for Circle { fn draw(&self) {} }");
        let imp = s.iter().find(|x| x.kind == SymbolKind::Impl).unwrap();
        assert!(imp.detail.contains("Draw"));
        assert!(imp.detail.contains("Circle"));
        let draw = find(&s, "draw");
        assert_eq!(draw.container, vec!["impl Circle"]);
    }

    #[test]
    fn nested_modules() {
        let s = syms("mod a { mod b { fn f(){} } }");
        let ma = find(&s, "a");
        assert_eq!(ma.kind, SymbolKind::Module);
        assert!(ma.container.is_empty());
        let mb = find(&s, "b");
        assert_eq!(mb.kind, SymbolKind::Module);
        assert_eq!(mb.container, vec!["a"]);
        let f = find(&s, "f");
        assert_eq!(f.kind, SymbolKind::Function);
        assert_eq!(f.container, vec!["a", "b"]);
    }

    #[test]
    fn file_module_is_not_recursed() {
        // `mod name;` — file module: is_file() == true, no body in tree.
        let s = syms("mod other;\nfn main() {}");
        let mo = find(&s, "other");
        assert_eq!(mo.kind, SymbolKind::Module);
        // No items from `other` should appear.
        assert_eq!(s.len(), 2); // mod + main
    }

    #[test]
    fn extern_block_fns() {
        let s = syms("extern \"lua\" { fn printf(fmt: &str, ...); }");
        let ef = find(&s, "printf");
        assert_eq!(ef.kind, SymbolKind::ExternFn);
        assert!(ef.detail.contains("extern fn printf"));
        assert!(ef.detail.contains("fmt"));
    }

    // --- doc-comment extraction ---------------------------------------------

    #[test]
    fn doc_comment_on_top_level_fn() {
        let s = syms("// ordinary comment\nfn add_one(x: i64) {}");
        assert_eq!(find(&s, "add_one").doc, "");
    }

    #[test]
    fn doc_comment_multiline_joined() {
        let s = syms("/// first line\n/// second line\nfn f() {}");
        assert_eq!(find(&s, "f").doc, "first line\nsecond line");
    }

    #[test]
    fn doc_comment_blank_line_detaches() {
        // A blank line between the comment and the fn means it is not a doc.
        let s = syms("// header\n\nfn f() {}");
        assert_eq!(find(&s, "f").doc, "");
    }

    #[test]
    fn doc_comment_on_struct_field_peeled() {
        // Field comments are absorbed as the field node's own leading trivia.
        let s = syms("struct S {\n    /// the answer\n    a: i64,\n}");
        assert_eq!(find(&s, "a").doc, "the answer");
    }

    #[test]
    fn doc_comment_on_enum_variant() {
        let s = syms("enum E {\n    /// red one\n    Red,\n    Green,\n}");
        assert_eq!(find(&s, "Red").doc, "red one");
        assert_eq!(find(&s, "Green").doc, "");
    }

    #[test]
    fn doc_comment_block_style() {
        let s = syms("/** a block doc */\nstruct S;");
        assert_eq!(find(&s, "S").doc, "a block doc");
    }

    #[test]
    fn trailing_comment_is_not_doc_of_next_field() {
        // `// t` sits on field a's line, so it must not become b's doc.
        let s = syms("struct S {\n    a: i64, // t\n    b: i64,\n}");
        assert_eq!(find(&s, "b").doc, "");
    }

    #[test]
    fn use_declarations_are_skipped() {
        let s = syms("use std::print;\nfn f() {}");
        // Only `f` should appear; no symbols for `use`.
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "f");
    }

    // --- name_range correctness ---------------------------------------------

    #[test]
    fn name_range_slices_to_name_in_source() {
        let src = "fn my_func() {}";
        let s = syms(src);
        let r = s[0].name_range;
        assert_eq!(&src[r.0..r.1], "my_func");
    }

    #[test]
    fn struct_field_name_range() {
        let src = "struct S { field_name: i64 }";
        let s = syms(src);
        let f = find(&s, "field_name");
        let r = f.name_range;
        assert_eq!(&src[r.0..r.1], "field_name");
    }

    // --- ident_at_offset ----------------------------------------------------

    #[test]
    fn ident_at_offset_hits_middle_of_name() {
        let f = file("fn add_one(x: i64) -> i64 { x + 1 }");
        // "add_one" starts at byte 3; offset 5 is middle of name.
        let hit = ident_at_offset(&f, 5).unwrap();
        assert_eq!(hit.text, "add_one");
        // Verify range matches the actual token text.
        let ft = f.syntax().text().to_string();
        assert_eq!(&ft[hit.range.0..hit.range.1], "add_one");
    }

    #[test]
    fn ident_at_offset_on_whitespace_returns_none() {
        let f = file("fn add_one() {}");
        // Byte 2 is the space between "fn" and "add_one".
        assert!(ident_at_offset(&f, 2).is_none());
    }

    #[test]
    fn ident_at_offset_on_keyword_returns_none() {
        let f = file("fn add() {}");
        // Byte 0 is the start of "fn".
        assert!(ident_at_offset(&f, 0).is_none());
    }

    #[test]
    fn ident_at_offset_at_ident_boundary() {
        // "fn a() {}": byte 3 is boundary between Whitespace(2..3) and Ident(3..4).
        // rowan's token_at_offset returns Between(ws, ident) at exact boundaries.
        let f = file("fn a() {}");
        let hit = ident_at_offset(&f, 3);
        // Between grants us the Ident on the right.
        assert!(hit.is_some(), "boundary at ident start should yield a hit");
        assert_eq!(hit.unwrap().text, "a");
    }

    #[test]
    fn ident_at_offset_empty_source() {
        let f = file("");
        assert!(ident_at_offset(&f, 0).is_none());
    }

    #[test]
    fn ident_at_offset_past_end() {
        let f = file("fn a() {}");
        assert!(ident_at_offset(&f, 999).is_none());
    }

    // --- integration: example file ------------------------------------------

    #[test]
    fn example_p4c_types_symbols() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/examples/example_rua_p4c_types.rua"
        );
        let src = std::fs::read_to_string(path).expect("read example file");
        let s = syms(&src);
        assert!(s.len() > 5, "should have multiple symbols");
        assert!(
            s.iter()
                .any(|x| x.name == "main" && x.kind == SymbolKind::Function)
        );
        assert!(
            s.iter()
                .any(|x| x.name == "Point" && x.kind == SymbolKind::Struct)
        );
        assert!(
            s.iter()
                .any(|x| x.name == "geo" && x.kind == SymbolKind::Module)
        );
        assert!(
            s.iter()
                .any(|x| x.name == "norm_sq" && x.kind == SymbolKind::Method)
        );
        assert!(s.iter().any(|x| x.name == "area"
            && x.kind == SymbolKind::Function
            && x.container == vec!["geo"]));
    }
}
