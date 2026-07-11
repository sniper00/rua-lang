//! Analysis cache — one parse + derived indices per document version.
//!
//! Owns the rowan tree so callers (LSP) never touch rowan directly.
//! All queries return rowan-free PODs.

use crate::ast::{AstNode, SourceFile};
use crate::symbols::{self, IdentHit, Symbol, SymbolKind};
use crate::LineIndex;
use crate::transition::BindingTypes;

pub use crate::transition::{CompletionMember, MemberIndex, MemberKind, MemberTarget};

/// One parse + derived indices for a single document version.
///
/// Owns the rowan tree (via [`SourceFile`]) so callers never touch
/// `SyntaxNode` / `SyntaxToken` / `TextRange` directly.  All queries
/// return [`Symbol`], [`IdentHit`], or byte-offset pairs — plain data
/// that the LSP crate can consume without a `rowan` dependency.
pub struct Analysis {
    text: String,
    file: SourceFile,
    line_index: LineIndex,
    symbols: Vec<Symbol>,
    /// Member-access resolution table (`x.field` / `x.method()`), produced by
    /// `ruac`'s type checker (single-file view). Keyed by the byte span of
    /// the member identifier at the use site.
    members: MemberIndex,
    /// Inferred types of local bindings (`let` / `for` / parameter), produced by
    /// the type checker (single-file view). Keyed by the binding-name byte span;
    /// enriches local hover from `local x` to e.g. `let mut i: i64`.
    bindings: BindingTypes,
}

/// An in-scope local variable offered as a completion. Rowan-free POD so the
/// LSP crate can consume it directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalCompletion {
    /// The variable name (the completion label / insert text).
    pub name: String,
    /// Hover-style detail: inferred type (`let mut i: i64`) or `local <name>`.
    pub detail: String,
}

impl Analysis {
    /// Parse `src`, build the [`LineIndex`], and collect all definition
    /// symbols.  This is the single point where a document version enters
    /// the analysis world; every read request hits the cached result.
    ///
    /// The source text is retained so this `Analysis` is fully self-contained:
    /// [`line_index`](Self::line_index) queries and byte-range slicing need the
    /// original text, and owning it lets the [`Workspace`](crate::workspace)
    /// be the single owner of per-file state (no parallel text cache).
    pub fn new(src: &str) -> Analysis {
        let file = crate::parse_source_file(src).tree;
        let symbols = symbols::collect_symbols(&file);
        // Semantic member resolution comes from the compiler's type checker
        // (byte-span parity: both trees derive from the same lexer offsets).
        let members = crate::transition::member_index(src);
        let bindings = crate::transition::binding_types(src);
        Analysis {
            text: src.to_string(),
            file,
            line_index: LineIndex::new(src),
            symbols,
            members,
            bindings,
        }
    }

    /// The source text this analysis was built from.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Byte-offset ↔ (line, UTF-16 column) index for this document.
    pub fn line_index(&self) -> &LineIndex {
        &self.line_index
    }

    /// The parsed source file (rowan-free CST view).
    /// Used by [`crate::workspace::Workspace`] for cross-file resolution.
    pub fn source_file(&self) -> &SourceFile {
        &self.file
    }

    /// Byte offsets of every [`SyntaxKind::Ident`](crate::SyntaxKind::Ident)
    /// token whose text matches `name`. Used by workspace-level reference
    /// search for name pre-filtering across files.
    pub fn ident_offsets_by_name(&self, name: &str) -> Vec<usize> {
        let mut offsets = Vec::new();
        for element in self.file.syntax().descendants_with_tokens() {
            let tok: crate::SyntaxToken = match element.into_token() {
                Some(t) => t,
                None => continue,
            };
            if tok.kind() != crate::SyntaxKind::Ident {
                continue;
            }
            if tok.text() != name {
                continue;
            }
            let r = tok.text_range();
            offsets.push(usize::from(r.start()));
        }
        offsets
    }

    /// All definition symbols collected from this document.
    pub fn symbols(&self) -> &[Symbol] {
        &self.symbols
    }

    /// The full member-access resolution table for this document.
    pub fn members(&self) -> &MemberIndex {
        &self.members
    }

    /// The member access (`x.field` / `x.method()`) whose member-identifier span
    /// contains `offset`, if any. Used by name resolution to jump/hover on a
    /// member without type inference in the CST layer.
    pub fn member_at(&self, offset: usize) -> Option<&MemberTarget> {
        // Single-file analysis: member use-sites are always in file 0.
        self.members.at(0, offset)
    }

    /// Whether the identifier at `offset` is the member part of a field access
    /// (`x.field`) or method call (`x.method()`). This is a purely structural
    /// CST check (no type inference), so it reports `true` even when the
    /// receiver's type is unknown or defined in another file — used to suppress
    /// rename on members, which is not yet supported.
    pub fn is_member_access(&self, offset: usize) -> bool {
        crate::nameres::is_member_access(&self.file, offset)
    }

    /// Field/method completions for a member access (`x.` / `x.par`) at `offset`.
    ///
    /// Returns `None` when the cursor is **not** on a member slot (caller should
    /// fall back to global completion). Returns `Some(list)` when it is a member
    /// slot — the list is empty if the receiver type is unknown / non-concrete,
    /// so the caller can suppress keyword/global noise after the `.`.
    pub fn member_completions(&self, offset: usize) -> Option<Vec<CompletionMember>> {
        let ctx = crate::completion::completion_context(&self.file, offset)?;
        let repaired = crate::completion::repair(&self.text, &ctx);
        Some(crate::transition::member_completions(
            &repaired,
            ctx.receiver_end,
        ))
    }

    /// Path-context completions for `Type::` / `mod::` (`Enum::Variant`,
    /// associated methods, module items). Purely lexical: scans back from
    /// `offset` over an optional partial segment, requires a `::`, then reads
    /// the left segment and returns the members of the type/module it names.
    ///
    /// Returns `None` when the cursor is **not** in a `::` path slot whose left
    /// segment is a known enum/struct/trait/module (caller falls back to global
    /// completion). Returns `Some(list)` in a path slot — the list is empty when
    /// the container has no members, so the caller can suppress global noise
    /// after the `::`.
    pub fn path_completions(&self, offset: usize) -> Option<Vec<Symbol>> {
        let seg = path_receiver_segment(&self.text, offset)?;
        // The left segment must name a container-like symbol; otherwise this is
        // an unrelated `::` (or a typo) and globals are the better fallback.
        let is_container = self.symbols.iter().any(|s| {
            s.name == seg
                && matches!(
                    s.kind,
                    SymbolKind::Enum | SymbolKind::Struct | SymbolKind::Trait | SymbolKind::Module
                )
        });
        if !is_container {
            return None;
        }
        let impl_name = format!("impl {seg}");
        let items: Vec<Symbol> = self
            .symbols
            .iter()
            .filter(|s| {
                s.container
                    .last()
                    .is_some_and(|c| c == &seg || c == &impl_name)
            })
            .cloned()
            .collect();
        Some(items)
    }

    /// Local variables visible at `offset` (fn params, `let`, `for` var,
    /// `if let`/`while let`/`match` pattern bindings), for global completion.
    ///
    /// Each entry's `detail` is the compiler's inferred-type text when known
    /// (e.g. `let mut i: i64`), falling back to `local <name>`. Inner bindings
    /// shadow outer ones of the same name.
    pub fn scope_locals(&self, offset: usize) -> Vec<LocalCompletion> {
        crate::nameres::locals_in_scope(&self.file, offset)
            .into_iter()
            .map(|(name, range)| {
                let detail = self
                    .bindings
                    .display_at(0, range.0)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("local {name}"));
                LocalCompletion { name, detail }
            })
            .collect()
    }

    /// Find the [`SyntaxKind::Ident`](crate::SyntaxKind::Ident) token at
    /// a byte offset.  Returns `None` when the offset falls on
    /// whitespace, a comment, a keyword, or any non-Ident token.
    pub fn ident_at_offset(&self, offset: usize) -> Option<IdentHit> {
        symbols::ident_at_offset(&self.file, offset)
    }

    /// Scope-aware name resolution at a byte offset.
    ///
    /// Returns a [`nameres::Resolution`] when the identifier at `offset` can
    /// be resolved to its definition (local binding or item symbol). Returns
    /// `None` when resolution is not possible — the LSP layer shows nothing
    /// rather than falling back to heuristic name matching.
    ///
    /// See [`crate::nameres::resolve_at`] for the full resolution strategy
    /// and scope boundaries.
    pub fn resolve_at(&self, offset: usize) -> Option<crate::nameres::Resolution> {
        // Member access (`x.field` / `x.method()`) resolves via the compiler's
        // type-checked member table; the CST-only resolver can't infer receiver
        // types, so this takes priority when the cursor is on a member ident.
        if let Some(m) = self.members.at(0, offset) {
            return Some(member_resolution(m));
        }
        let res = crate::nameres::resolve_at(&self.file, &self.symbols, offset)?;
        Some(self.enrich_local(res))
    }

    /// Canonical definition at `offset` — works on both use-sites and
    /// definition names. Use this instead of [`resolve_at`](Self::resolve_at)
    /// when the cursor may be on a definition (go-to-def, prepare-rename,
    /// find-references).
    pub fn definition_at(&self, offset: usize) -> Option<crate::nameres::Resolution> {
        if let Some(m) = self.members.at(0, offset) {
            return Some(member_resolution(m));
        }
        let res = crate::nameres::definition_at(&self.file, &self.symbols, offset)?;
        Some(self.enrich_local(res))
    }

    /// Replace a local binding's plain `local <name>` hover with the compiler's
    /// inferred-type text (e.g. `let mut i: i64`) when available. Non-local
    /// resolutions and bindings whose type could not be inferred are returned
    /// unchanged.
    fn enrich_local(&self, mut res: crate::nameres::Resolution) -> crate::nameres::Resolution {
        if res.kind == crate::nameres::RefKind::Local
            && let Some(display) = self.bindings.display_at(0, res.target_range.0)
        {
            res.detail = display.to_owned();
        }
        res
    }

    /// All references (including the definition) to the same binding as the
    /// identifier at `offset`. Returns byte ranges in ascending position
    /// order with no duplicates. Returns an empty vec when not resolvable.
    pub fn references_at(&self, offset: usize) -> Vec<(usize, usize)> {
        crate::nameres::references_at(&self.file, &self.symbols, offset)
    }

    /// Produce rename edits for the identifier at `offset`, replacing every
    /// reference (definition included) with `new_name`.
    pub fn rename_edits(
        &self,
        offset: usize,
        new_name: &str,
    ) -> Result<Vec<(usize, usize, String)>, crate::nameres::RenameError> {
        crate::nameres::rename_edits(&self.file, &self.symbols, offset, new_name)
    }
}

/// Build a [`Resolution`](crate::nameres::Resolution) from a type-checked member
/// access. The target is a same-file definition span (single-file view), marked
/// [`RefKind::Item`](crate::nameres::RefKind::Item) since it points at a field
/// or method declaration.
/// Byte is part of an identifier (`[A-Za-z0-9_]`).
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Given a cursor `offset`, return the left path segment of a `Segment::partial`
/// context (e.g. cursor after `Color::` or `Color::Re` → `"Color"`). Returns
/// `None` when the cursor is not immediately preceded by `<ident>::<partial>`.
fn path_receiver_segment(text: &str, offset: usize) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = offset.min(text.len());
    // Skip the partial member segment currently being typed.
    while i > 0 && is_ident_byte(bytes[i - 1]) {
        i -= 1;
    }
    // Require an immediately-preceding `::` (allow surrounding whitespace).
    while i > 0 && bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    if i < 2 || &text[i - 2..i] != "::" {
        return None;
    }
    i -= 2;
    while i > 0 && bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    // Read the left segment identifier.
    let end = i;
    while i > 0 && is_ident_byte(bytes[i - 1]) {
        i -= 1;
    }
    if i == end {
        return None;
    }
    Some(text[i..end].to_string())
}

fn member_resolution(m: &MemberTarget) -> crate::nameres::Resolution {
    crate::nameres::Resolution {
        kind: crate::nameres::RefKind::Item,
        target_range: (m.target_start, m.target_start + m.target_len),
        detail: m.detail.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Byte offset of the `n`-th (0-based) whole-word occurrence of `needle`.
    fn nth_word(src: &str, needle: &str, n: usize) -> usize {
        let b = src.as_bytes();
        let is_id = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
        let mut rem = n;
        let mut pos = 0;
        loop {
            let off = pos + src[pos..].find(needle).expect("occurrence not found");
            let after = off + needle.len();
            let left = off == 0 || !is_id(b[off - 1]);
            let right = after >= b.len() || !is_id(b[after]);
            if left && right {
                if rem == 0 {
                    return off;
                }
                rem -= 1;
            }
            pos = off + 1;
        }
    }

    #[test]
    fn member_at_resolves_field() {
        let src = "struct Point { x: f64, y: f64 }\nfn main() { let p = Point { x: 1.0, y: 2.0 }; let _ = p.x; }";
        let a = Analysis::new(src);
        let use_off = nth_word(src, "x", 2); // the `x` in `p.x`
        let hit = a.member_at(use_off).expect("p.x should resolve");
        assert_eq!(hit.detail, "x: f64");
        // Target points at the field definition.
        assert_eq!(&src[hit.target_start..hit.target_start + hit.target_len], "x");
    }

    #[test]
    fn member_at_resolves_method() {
        let src = "struct P { v: i64 }\nimpl P {\n    fn get(&self) -> i64 { self.v }\n}\nfn main() { let p = P { v: 1 }; let _ = p.get(); }";
        let a = Analysis::new(src);
        let use_off = nth_word(src, "get", 1); // `get` in `p.get()`
        let hit = a.member_at(use_off).expect("p.get() should resolve");
        assert_eq!(hit.detail, "fn get(&self) -> i64");
    }

    #[test]
    fn member_span_has_cst_byte_parity() {
        // The member-identifier span recorded by ruac must line up byte-for
        // -byte with the CST's Ident token at that offset (both derive from the
        // same lexer offsets). This is the invariant B3 relies on.
        let src = "struct Point { x: f64, y: f64 }\nfn main() { let p = Point { x: 1.0, y: 2.0 }; let _ = p.x; }";
        let a = Analysis::new(src);
        let use_off = nth_word(src, "x", 2);
        let hit = a.member_at(use_off).expect("p.x should resolve");
        // The CST token at member_start is an Ident whose text is the member name.
        let cst_hit = a
            .ident_at_offset(hit.member_start)
            .expect("CST should have an Ident at the member span start");
        assert_eq!(cst_hit.text, "x");
        assert_eq!(cst_hit.range.0, hit.member_start);
        assert_eq!(cst_hit.range.1, hit.member_start + hit.member_len);
    }

    #[test]
    fn member_at_builtin_is_hover_only_sentinel() {
        let src = "fn main() { let v = vec![1, 2]; v.push(3); }";
        let a = Analysis::new(src);
        // Cursor on `push`: a member hit exists for hover (detail), but Vec has
        // no Rua definition, so its target span is a zero-length sentinel (the
        // LSP suppresses go-to-definition on it).
        let off = nth_word(src, "push", 0);
        let hit = a.member_at(off).expect("builtin method has a hover hit");
        assert!(hit.detail.contains("push"), "detail: {}", hit.detail);
        assert_eq!(hit.target_len, 0, "builtin target is a sentinel");
    }

    #[test]
    fn member_at_none_for_unknown_receiver() {
        let src = "fn main() { let _ = foo().bar; }";
        let a = Analysis::new(src);
        let off = nth_word(src, "bar", 0);
        assert!(a.member_at(off).is_none());
    }

    #[test]
    fn is_member_access_structural_check() {
        // Structural: true on member idents even when the type is unknown, false
        // on the receiver and on plain identifiers.
        let src = "fn main() { let v = foo(); v.push(3); let w = v; }";
        let a = Analysis::new(src);
        // `push` is a member ident (unknown receiver type) → true.
        assert!(a.is_member_access(nth_word(src, "push", 0)));
        // The receiver `v` in `v.push` is not a member ident → false.
        assert!(!a.is_member_access(nth_word(src, "v", 1)));
        // A plain local `w` → false.
        assert!(!a.is_member_access(nth_word(src, "w", 0)));
    }

    // --- B3: nameres consults the member table --------------------------------

    #[test]
    fn definition_at_resolves_member_field() {
        let src = "struct Point { x: f64, y: f64 }\nfn main() { let p = Point { x: 1.0, y: 2.0 }; let _ = p.x; }";
        let a = Analysis::new(src);
        let use_off = nth_word(src, "x", 2);
        let res = a.definition_at(use_off).expect("p.x should resolve via members");
        assert_eq!(res.kind, crate::nameres::RefKind::Item);
        assert_eq!(res.detail, "x: f64");
        assert_eq!(&src[res.target_range.0..res.target_range.1], "x");
    }

    #[test]
    fn resolve_at_resolves_member_method() {
        let src = "struct P { v: i64 }\nimpl P {\n    fn get(&self) -> i64 { self.v }\n}\nfn main() { let p = P { v: 1 }; let _ = p.get(); }";
        let a = Analysis::new(src);
        let use_off = nth_word(src, "get", 1);
        let res = a.resolve_at(use_off).expect("p.get() should resolve via members");
        assert_eq!(res.detail, "fn get(&self) -> i64");
        assert_eq!(&src[res.target_range.0..res.target_range.1], "get");
    }

    #[test]
    fn definition_at_receiver_still_resolves_to_local() {
        // Cursor on the receiver `p` (not the member) must still resolve to the
        // local binding via nameres, unaffected by the member table.
        let src = "struct Point { x: f64 }\nfn main() { let p = Point { x: 1.0 }; let _ = p.x; }";
        let a = Analysis::new(src);
        let recv_off = nth_word(src, "p", 1); // `p` in `p.x` (def is occurrence 0)
        let res = a.definition_at(recv_off).expect("receiver should resolve to its let");
        assert_eq!(res.kind, crate::nameres::RefKind::Local);
    }

    #[test]
    fn definition_at_member_on_unknown_receiver_is_none() {
        let src = "fn main() { let _ = foo().bar; }";
        let a = Analysis::new(src);
        let off = nth_word(src, "bar", 0);
        assert!(a.definition_at(off).is_none());
    }

    // --- local hover enrichment (binding types) -------------------------------

    #[test]
    fn local_let_hover_shows_inferred_type() {
        let src = "fn main() { let mut i = 0; let _ = i; }";
        let a = Analysis::new(src);
        // Hover on the def site of `i`.
        let def_off = nth_word(src, "i", 0);
        let res = a.definition_at(def_off).expect("let binding should resolve");
        assert_eq!(res.kind, crate::nameres::RefKind::Local);
        assert_eq!(res.detail, "let mut i: i64");
    }

    #[test]
    fn local_use_site_hover_shows_inferred_type() {
        let src = "fn main() { let mut i = 0; let _ = i; }";
        let a = Analysis::new(src);
        // Hover on the use site `i` (second occurrence).
        let use_off = nth_word(src, "i", 1);
        let res = a.resolve_at(use_off).expect("use of local should resolve");
        assert_eq!(res.detail, "let mut i: i64");
    }

    #[test]
    fn fn_param_hover_shows_declared_type() {
        let src = "fn add(x: i64, y: i64) -> i64 { x + y }";
        let a = Analysis::new(src);
        let use_off = nth_word(src, "x", 1); // `x` in `x + y`
        let res = a.resolve_at(use_off).expect("param use should resolve");
        assert_eq!(res.detail, "x: i64");
    }

    #[test]
    fn closure_iterator_ide_resolves_types_completion_references_and_rename() {
        let src = concat!(
            "fn main() {\n",
            "  let values = vec![1, 2, 3];\n",
            "  let count = values.iter().map(|item| item + 1).count();\n",
            "}\n",
        );
        let analysis = Analysis::new(src);
        let definition = nth_word(src, "item", 0);
        let use_site = nth_word(src, "item", 1);

        let resolved = analysis.resolve_at(use_site).expect("closure parameter use");
        assert_eq!(resolved.target_range.0, definition);
        assert_eq!(resolved.detail, "closure parameter item: i64");

        let locals = analysis.scope_locals(use_site);
        assert!(locals.iter().any(|local| {
            local.name == "item" && local.detail == "closure parameter item: i64"
        }));
        assert_eq!(analysis.references_at(use_site).len(), 2);
        let edits = analysis.rename_edits(use_site, "element").expect("rename closure param");
        assert_eq!(edits.len(), 2);
        assert!(edits.iter().all(|edit| edit.2 == "element"));
    }

    #[test]
    fn local_without_inferable_type_stays_plain() {
        // `foo()` is unresolved, so the binding type is Unknown → plain hover.
        let src = "fn main() { let z = foo(); let _ = z; }";
        let a = Analysis::new(src);
        let use_off = nth_word(src, "z", 1);
        let res = a.resolve_at(use_off).expect("local should resolve");
        assert_eq!(res.detail, "local z");
    }

    #[test]
    fn if_let_some_binding_shows_element_type() {
        let src = "fn f(opt: Option<i64>) { if let Some(x) = opt { let _ = x; } }";
        let a = Analysis::new(src);
        let use_off = nth_word(src, "x", 1); // `x` inside the block
        let res = a.resolve_at(use_off).expect("pattern binding should resolve");
        assert_eq!(res.detail, "x: i64");
    }

    #[test]
    fn match_ok_err_bindings_show_payload_types() {
        let src = "fn f(r: Result<i64, String>) -> i64 { match r { Ok(v) => v, Err(e) => 0 } }";
        let a = Analysis::new(src);
        let v_off = nth_word(src, "v", 1); // `v` in the arm body
        let v_res = a.resolve_at(v_off).expect("Ok(v) binding should resolve");
        assert_eq!(v_res.detail, "v: i64");
        let e_off = nth_word(src, "e", 0); // `e` binding in `Err(e)`
        let e_res = a.definition_at(e_off).expect("Err(e) binding should resolve");
        assert_eq!(e_res.detail, "e: String");
    }

    #[test]
    fn user_enum_tuple_variant_binding_shows_payload_type() {
        let src = "enum Shape { Circle(f64), Rect(i64, i64) }\n\
                   fn area(s: Shape) -> f64 { match s { Shape::Circle(r) => r, Shape::Rect(w, h) => 0.0 } }";
        let a = Analysis::new(src);
        let r_off = nth_word(src, "r", 1); // `r` in the arm body
        let r_res = a.resolve_at(r_off).expect("Circle(r) binding should resolve");
        assert_eq!(r_res.detail, "r: f64");
        let w_off = nth_word(src, "w", 0); // `w` binding in `Rect(w, h)`
        let w_res = a.definition_at(w_off).expect("Rect(w, ..) binding should resolve");
        assert_eq!(w_res.detail, "w: i64");
    }

    #[test]
    fn user_enum_struct_variant_binding_shows_field_type() {
        let src = "enum Msg { Move { x: i64, y: i64 }, Quit }\n\
                   fn f(m: Msg) { match m { Msg::Move { x, y } => { let _ = x; }, Msg::Quit => {} } }";
        let a = Analysis::new(src);
        let x_off = nth_word(src, "x", 1); // `x` binding in the struct pattern
        let x_res = a.definition_at(x_off).expect("Move { x } binding should resolve");
        assert_eq!(x_res.detail, "x: i64");
    }

    #[test]
    fn pattern_binding_with_unknown_scrutinee_stays_plain() {
        // `opt` is undefined → Unknown → payload not destructured → plain hover.
        let src = "fn f() { if let Some(x) = opt { let _ = x; } }";
        let a = Analysis::new(src);
        let use_off = nth_word(src, "x", 1);
        let res = a.resolve_at(use_off).expect("pattern binding should resolve");
        assert_eq!(res.detail, "local x");
    }

    // --- C2: member completions -----------------------------------------------

    #[test]
    fn member_completions_after_bare_dot() {
        let src = "struct P { x: i64 }\nimpl P { fn go(&self) -> i64 { 0 } }\nfn main() { let p = P { x: 1 }; p. }";
        let a = Analysis::new(src);
        let items = a.member_completions(src.rfind('.').unwrap() + 1).expect("member slot");
        let names: Vec<&str> = items.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"x"));
        assert!(names.contains(&"go"));
    }

    #[test]
    fn member_completions_partial_prefix_returns_all() {
        let src = "struct P { alpha: i64, beta: i64 }\nfn main() { let p = P { alpha: 1, beta: 2 }; let _ = p.al; }";
        let a = Analysis::new(src);
        let items = a.member_completions(src.rfind("al").unwrap() + 2).expect("member slot");
        let names: Vec<&str> = items.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"alpha") && names.contains(&"beta")); // client filters
    }

    #[test]
    fn member_completions_self_in_impl() {
        let src = "struct P { v: i64 }\nimpl P { fn get(&self) -> i64 { self. } }";
        let a = Analysis::new(src);
        let items = a.member_completions(src.rfind('.').unwrap() + 1).expect("member slot");
        assert!(items.iter().any(|m| m.name == "v"));
        assert!(items.iter().any(|m| m.name == "get"));
    }

    #[test]
    fn member_completions_vec_receiver_lists_builtin_methods() {
        // `v.` lists Vec's built-in methods (len/get/push/pop/set).
        let src = "fn main() { let v = vec![1]; v. }";
        let a = Analysis::new(src);
        let items = a
            .member_completions(src.rfind('.').unwrap() + 1)
            .expect("member slot");
        let names: Vec<&str> = items.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"push"), "Vec methods: {names:?}");
        assert!(names.contains(&"len"));
        assert!(names.contains(&"get"));
    }

    #[test]
    fn member_completions_vec_receiver_followed_by_statement() {
        // Regression: `stack.` on its own line, followed by more statements. The
        // repair must terminate the statement (`;`) so type-check doesn't bail.
        let src = "fn main() {\n    let mut stack = vec![1, 2, 3];\n    stack.\n    while let Some(x) = stack.pop() {\n        let _ = x;\n    }\n}\n";
        let a = Analysis::new(src);
        let off = src.find("stack.\n").unwrap() + "stack.".len();
        let items = a.member_completions(off).expect("member slot");
        let names: Vec<&str> = items.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"push"), "Vec methods: {names:?}");
        assert!(names.contains(&"pop"));
    }

    #[test]
    fn member_completions_partial_prefix_followed_by_statement() {
        // Partial member ident before another statement: `;` inserted past it.
        let src = "fn main() {\n    let mut stack = vec![1];\n    stack.pu\n    let _ = stack;\n}\n";
        let a = Analysis::new(src);
        let off = src.find("stack.pu").unwrap() + "stack.pu".len();
        let items = a.member_completions(off).expect("member slot");
        let names: Vec<&str> = items.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"push"), "Vec methods: {names:?}");
    }

    #[test]
    fn member_completions_string_receiver_lists_builtin_methods() {
        let src = "fn f(s: String) { s. }";
        let a = Analysis::new(src);
        let items = a
            .member_completions(src.rfind('.').unwrap() + 1)
            .expect("member slot");
        let names: Vec<&str> = items.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"trim"), "String methods: {names:?}");
        assert!(names.contains(&"to_uppercase"));
    }

    #[test]
    fn member_completions_non_member_position_is_none() {
        let src = "fn main() { let p = 1; }";
        let a = Analysis::new(src);
        assert!(a.member_completions(src.find('p').unwrap() + 1).is_none());
    }

    // --- path completions (`Type::` / `mod::`) --------------------------------

    #[test]
    fn path_completions_enum_variants() {
        let src = "enum Color { Red, Green, Blue }\nfn main() { let c = Color:: }";
        let a = Analysis::new(src);
        let items = a
            .path_completions(src.rfind("::").unwrap() + 2)
            .expect("Color:: is a path slot");
        let names: Vec<&str> = items.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Red") && names.contains(&"Green") && names.contains(&"Blue"));
    }

    #[test]
    fn path_completions_partial_prefix() {
        let src = "enum Color { Red, Green, Blue }\nfn main() { let c = Color::Re }";
        let a = Analysis::new(src);
        let items = a
            .path_completions(src.rfind("Re").unwrap() + 2)
            .expect("Color::Re is a path slot");
        // Client filters by prefix; we return the full variant set.
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn path_completions_assoc_method_of_struct() {
        let src = "struct P { x: i64 }\nimpl P { fn make() -> i64 { 0 } }\nfn main() { P:: }";
        let a = Analysis::new(src);
        let items = a
            .path_completions(src.rfind("::").unwrap() + 2)
            .expect("P:: is a path slot");
        assert!(items.iter().any(|s| s.name == "make"));
    }

    #[test]
    fn path_completions_unknown_container_is_none() {
        // `Nope` is not a known enum/struct/trait/module → fall back to globals.
        let src = "fn main() { Nope:: }";
        let a = Analysis::new(src);
        assert!(a.path_completions(src.rfind("::").unwrap() + 2).is_none());
    }

    #[test]
    fn path_completions_non_path_position_is_none() {
        let src = "enum Color { Red }\nfn main() { let c = 1; }";
        let a = Analysis::new(src);
        assert!(a.path_completions(src.find("c =").unwrap() + 1).is_none());
    }
}
