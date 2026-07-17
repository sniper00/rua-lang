//! Lower the rowan CST to the [`Doc`] IR (P5e-B1, B2, B3).
//!
//! Strategy:
//! - **Structure** (items, blocks, statements, `if`/`match`) is rendered with the
//!   [`Doc`] IR so bodies are multi-line and consistently indented.
//! - **Declaration headers and types** (`fn` signatures, generics, `where`,
//!   struct fields, `use`) are serialized token-by-token with declaration-context
//!   spacing ([`Ser`]). This is safe because those contexts have no unary `-` and
//!   no comparison `<`/`>` (angle brackets there are always generics).
//! - **Expressions** are rendered structurally by [`expr_inline`] so `-x` vs
//!   `a - b` and `a < b` vs `Vec<T>` are never confused. Author-written
//!   parentheses are preserved (the CST keeps `ParenExpr`).
//! - **Comments** (B2) are extracted from the lossless CST via
//!   [`comment::extract_children`] and reattached as leading (own-line) or
//!   trailing (same-line via [`Doc::LineSuffix`]) comments.
//!
//! B3 (long-line wrapping) is partially implemented: arg lists, struct
//! literals, and macro arguments wrap when they exceed the line width.

use super::comment::{self, Comment, Entry};
use super::doc::Doc;
use crate::ast::*;
use crate::{SyntaxKind as K, SyntaxNode, SyntaxToken};

// --- comment helpers --------------------------------------------------------

/// Build the [`Doc`] for a list of leading comments: each comment on its own
/// line, then a hard line break to separate from the following content.
fn leading_doc(comments: &[Comment]) -> Vec<Doc> {
    comments
        .iter()
        .flat_map(|c| [Doc::text(c.text.as_str()), Doc::HardLine])
        .collect()
}

/// Build the [`Doc`] for a list of trailing comments: each is emitted via
/// [`Doc::LineSuffix`] so it sticks to the current line.
fn trailing_doc(comments: &[Comment]) -> Doc {
    if comments.is_empty() {
        return Doc::Nil;
    }
    let suffix = comments
        .iter()
        .map(|c| format!(" {}", c.text))
        .collect::<Vec<_>>()
        .concat();
    Doc::LineSuffix(suffix)
}

/// Try to cast a [`SyntaxNode`] to [`Item`].
fn node_to_item(node: &SyntaxNode) -> Option<Item> {
    Item::cast(node.clone())
}

/// Try to cast a [`SyntaxNode`] to [`Stmt`].
fn node_to_stmt(node: &SyntaxNode) -> Option<Stmt> {
    Stmt::cast(node.clone())
}

// --- entry -----------------------------------------------------------------

/// Lower a whole source file: items separated by a blank line (consecutive
/// `use` declarations stay adjacent). Comments between items are reattached
/// as leading comments for the following item.
pub fn lower_source_file(sf: &SourceFile) -> Doc {
    let entries = comment::extract_children(sf.syntax());
    let mut parts = Vec::new();
    // Track the previous non-comment item for spacing decisions.
    let mut prev_item_kind: Option<SyntaxKind> = None;

    for entry in &entries {
        match entry {
            Entry::Comment(c) => {
                // Standalone comment at file level — emit on its own line.
                if !parts.is_empty() {
                    parts.push(Doc::HardLine);
                }
                parts.push(Doc::text(c.text.as_str()));
            }
            Entry::Node {
                leading,
                trailing,
                node,
                blank_line_before,
            } => {
                let item = node_to_item(node);
                let statement = node_to_stmt(node);
                let first = parts.is_empty();
                if !first {
                    let both_use =
                        prev_item_kind == Some(K::UseDecl) && matches!(item, Some(Item::Use(_)));
                    let touches_item = prev_item_kind.is_some() || item.is_some();
                    parts.push(Doc::HardLine);
                    if touches_item && (!both_use || *blank_line_before) {
                        parts.push(Doc::HardLine);
                    }
                }
                // Leading comments (the item-spacing blank line above already
                // separates them from the previous item).
                parts.extend(leading_doc(leading));
                // Header case: the first item's leading comment separated from
                // it by a blank line (e.g. a file banner) keeps one blank line.
                // (A blank at the very top of the file is dropped since `parts`
                // holds only the comment(s) here.)
                if first && !leading.is_empty() && *blank_line_before {
                    parts.push(Doc::HardLine);
                }
                // The item.
                if let Some(it) = item {
                    parts.push(item_doc(&it));
                    prev_item_kind = Some(it.syntax().kind());
                } else if let Some(statement) = statement {
                    parts.push(stmt_doc(&statement));
                    prev_item_kind = None;
                } else {
                    // Error/recovery node.
                    parts.push(Doc::text(compact(node)));
                    prev_item_kind = None;
                }
                // Trailing comment on same line.
                parts.push(trailing_doc(trailing));
            }
        }
    }
    if parts.is_empty() {
        Doc::Nil
    } else {
        Doc::Concat(parts)
    }
}

// --- token serializer (declaration context) --------------------------------

/// Serializes a token run with spacing rules appropriate for declarations and
/// types. Never used for expression operators (see module docs).
struct Ser {
    out: String,
    prev: Option<K>,
}

impl Ser {
    fn new() -> Self {
        Ser {
            out: String::new(),
            prev: None,
        }
    }

    fn push(&mut self, t: &SyntaxToken) {
        let k = t.kind();
        if let Some(p) = self.prev
            && Self::space(p, k)
        {
            self.out.push(' ');
        }
        self.out.push_str(t.text());
        self.prev = Some(k);
    }

    fn space(prev: K, cur: K) -> bool {
        use K::*;
        // No space *after* these.
        if matches!(
            prev,
            ColonColon | LParen | LBracket | LBrace | Amp | Lt | Dot | Hash
        ) {
            return false;
        }
        // No space *before* these.
        if matches!(
            cur,
            ColonColon
                | Comma
                | Semi
                | Colon
                | RParen
                | RBracket
                | RBrace
                | Lt
                | Gt
                | Dot
                | LBracket
        ) {
            return false;
        }
        // Call/index/generic-close directly followed by `(` — no space.
        if cur == LParen && matches!(prev, Ident | Gt | RParen | RBracket | KwSelf) {
            return false;
        }
        // `path::{ .. }` use-group.
        if cur == LBrace && prev == ColonColon {
            return false;
        }
        true
    }

    fn finish(self) -> String {
        self.out
    }
}

fn tok_text(t: SyntaxToken) -> String {
    t.text().to_string()
}

/// Serialize every non-trivia token of a node (used for types and simple decls).
fn ser_node(n: &SyntaxNode) -> String {
    let mut s = Ser::new();
    for t in n.descendants_with_tokens().filter_map(|e| e.into_token()) {
        if !t.kind().is_trivia() && (n.kind() == K::Attribute || !token_in_attribute(n, &t)) {
            s.push(&t);
        }
    }
    s.finish()
}

/// Serialize the tokens of a node that come before its body's opening `{`
/// (the declaration "header": `pub fn name<..>(..) -> T where ..`).
fn decl_header(n: &SyntaxNode) -> String {
    let brace = n
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == K::LBrace)
        .map(|t| t.text_range().start());
    let mut s = Ser::new();
    for t in n.descendants_with_tokens().filter_map(|e| e.into_token()) {
        if t.kind().is_trivia() || token_in_attribute(n, &t) {
            continue;
        }
        if let Some(b) = brace
            && t.text_range().start() >= b
        {
            break;
        }
        s.push(&t);
    }
    s.finish()
}

fn token_in_attribute(node: &SyntaxNode, token: &SyntaxToken) -> bool {
    node.children().any(|child| {
        child.kind() == K::Attribute && child.text_range().contains_range(token.text_range())
    })
}

fn with_attributes(node: &SyntaxNode, body: Doc) -> Doc {
    let attributes = node
        .children()
        .filter(|child| child.kind() == K::Attribute)
        .collect::<Vec<_>>();
    if attributes.is_empty() {
        return body;
    }
    let mut parts = Vec::with_capacity(attributes.len() * 2 + 1);
    for attribute in attributes {
        parts.push(Doc::text(ser_node(&attribute)));
        parts.push(Doc::HardLine);
    }
    parts.push(body);
    Doc::Concat(parts)
}

// --- items -----------------------------------------------------------------

fn item_doc(it: &Item) -> Doc {
    match it {
        Item::Annotation(annotation) => with_attributes(
            annotation.syntax(),
            Doc::text(ser_node(annotation.syntax())),
        ),
        Item::Fn(f) => fn_doc(f),
        Item::Struct(s) => struct_doc(s),
        Item::Enum(e) => enum_doc(e),
        Item::Trait(t) => trait_doc(t),
        Item::Impl(i) => impl_doc(i),
        Item::Extern(x) => extern_doc(x),
        Item::Use(u) => with_attributes(u.syntax(), Doc::text(ser_node(u.syntax()))),
    }
}

fn fn_doc(f: &FnDecl) -> Doc {
    let body = match f.body() {
        Some(b) => Doc::concat([
            Doc::text(decl_header(f.syntax())),
            Doc::text(" "),
            block_doc(&b),
        ]),
        None => Doc::text(format!("{};", decl_header(f.syntax()))),
    };
    with_attributes(f.syntax(), body)
}

fn struct_doc(s: &StructDecl) -> Doc {
    match s.field_list() {
        Some(fl) => {
            let fields = lower_container(
                fl.syntax(),
                |node| {
                    if let Some(fd) = FieldDecl::cast(node.clone()) {
                        Doc::concat([field_doc(&fd), Doc::text(",")])
                    } else {
                        Doc::text(compact(node))
                    }
                },
                false,
            );
            with_attributes(
                s.syntax(),
                Doc::concat([
                    Doc::text(decl_header(s.syntax())),
                    Doc::text(" "),
                    brace_block(fields),
                ]),
            )
        }
        // Unit struct `struct S;`.
        None => with_attributes(s.syntax(), Doc::text(ser_node(s.syntax()))),
    }
}

fn enum_doc(e: &EnumDecl) -> Doc {
    let variants = match e.variant_list() {
        Some(vl) => lower_container(
            vl.syntax(),
            |node| {
                if let Some(v) = EnumVariant::cast(node.clone()) {
                    Doc::concat([variant_doc(&v), Doc::text(",")])
                } else {
                    Doc::text(compact(node))
                }
            },
            false,
        ),
        None => Vec::new(),
    };
    with_attributes(
        e.syntax(),
        Doc::concat([
            Doc::text(decl_header(e.syntax())),
            Doc::text(" "),
            brace_block(variants),
        ]),
    )
}

fn variant_doc(v: &EnumVariant) -> Doc {
    let name = v.name_text().unwrap_or_default();
    let body = if let Some(fl) = v.field_list() {
        let fields: Vec<String> = fl.fields().map(|fd| ser_node(fd.syntax())).collect();
        if fields.is_empty() {
            format!("{name} {{}}")
        } else {
            format!("{name} {{ {} }}", fields.join(", "))
        }
    } else {
        let tys: Vec<String> = v.tuple_types().map(|t| ser_node(t.syntax())).collect();
        if tys.is_empty() {
            name
        } else {
            format!("{name}({})", tys.join(", "))
        }
    };
    with_attributes(v.syntax(), Doc::text(body))
}

fn field_doc(field: &FieldDecl) -> Doc {
    with_attributes(field.syntax(), Doc::text(ser_node(field.syntax())))
}

fn trait_doc(t: &TraitDecl) -> Doc {
    let methods = lower_container(
        t.syntax(),
        |node| {
            if let Some(tm) = TraitMethod::cast(node.clone()) {
                trait_method_doc(&tm)
            } else {
                Doc::text(compact(node))
            }
        },
        true,
    );
    with_attributes(
        t.syntax(),
        Doc::concat([
            Doc::text(decl_header(t.syntax())),
            Doc::text(" "),
            brace_block_spaced(methods),
        ]),
    )
}

fn trait_method_doc(tm: &TraitMethod) -> Doc {
    let body = match tm.default_body() {
        Some(b) => Doc::concat([
            Doc::text(decl_header(tm.syntax())),
            Doc::text(" "),
            block_doc(&b),
        ]),
        None => Doc::text(ser_node(tm.syntax())),
    };
    with_attributes(tm.syntax(), body)
}

fn impl_doc(i: &ImplDecl) -> Doc {
    let methods = lower_container(
        i.syntax(),
        |node| {
            if let Some(f) = FnDecl::cast(node.clone()) {
                fn_doc(&f)
            } else {
                Doc::text(compact(node))
            }
        },
        true,
    );
    with_attributes(
        i.syntax(),
        Doc::concat([
            Doc::text(decl_header(i.syntax())),
            Doc::text(" "),
            brace_block_spaced(methods),
        ]),
    )
}

fn extern_doc(x: &ExternBlock) -> Doc {
    let fns = lower_container(
        x.syntax(),
        |node| {
            if let Some(ef) = ExternFn::cast(node.clone()) {
                with_attributes(ef.syntax(), Doc::text(ser_node(ef.syntax())))
            } else {
                Doc::text(compact(node))
            }
        },
        false,
    );
    with_attributes(
        x.syntax(),
        Doc::concat([
            Doc::text(decl_header(x.syntax())),
            Doc::text(" "),
            brace_block(fns),
        ]),
    )
}

// --- blocks & braces -------------------------------------------------------

fn brace_block(entries: Vec<Doc>) -> Doc {
    if entries.is_empty() {
        Doc::text("{}")
    } else {
        Doc::concat([
            Doc::text("{"),
            Doc::indent(Doc::concat([
                Doc::HardLine,
                Doc::join(Doc::HardLine, entries),
            ])),
            Doc::HardLine,
            Doc::text("}"),
        ])
    }
}

/// Like [`brace_block`] but with a blank line between entries (fns/methods).
fn brace_block_spaced(entries: Vec<Doc>) -> Doc {
    if entries.is_empty() {
        Doc::text("{}")
    } else {
        Doc::concat([
            Doc::text("{"),
            Doc::indent(Doc::concat([
                Doc::HardLine,
                Doc::join(Doc::concat([Doc::HardLine, Doc::HardLine]), entries),
            ])),
            Doc::HardLine,
            Doc::text("}"),
        ])
    }
}

/// Walk a container node's direct children with [`comment::extract_children`],
/// calling `lower` for each child node. Returns the resulting [`Doc`] entries
/// (including standalone comment entries), ready for wrapping in braces.
///
/// When `spaced` is false (statement blocks, struct fields, enum variants),
/// entries whose author wrote a blank line before them (`blank_line_before`)
/// get a leading [`Doc::HardLine`] prepended. Combined with [`brace_block`]'s
/// single-`HardLine` join this produces a blank line between those entries.
/// When `spaced` is true (methods in impl/trait, items in mod), blank lines
/// between every entry are already the default via [`brace_block_spaced`], so
/// `blank_line_before` is ignored to avoid triple-newline stacking.
fn lower_container(
    container: &SyntaxNode,
    lower: impl Fn(&SyntaxNode) -> Doc,
    spaced: bool,
) -> Vec<Doc> {
    let entries = comment::extract_children(container);
    let mut docs = Vec::new();
    let mut first_node = true;
    for entry in &entries {
        match entry {
            Entry::Comment(c) => {
                docs.push(Doc::text(c.text.as_str()));
            }
            Entry::Node {
                leading,
                trailing,
                node,
                blank_line_before,
            } => {
                let mut parts: Vec<Doc> = Vec::new();
                // In non-spaced blocks, a blank line before this node adds an
                // extra HardLine; the brace_block HardLine join turns that into
                // a blank line.  In spaced blocks the join already supplies the
                // blank line, so we skip this to avoid stacking. The first node
                // never gets a leading blank (no blank line right after `{`).
                if *blank_line_before && !spaced && !first_node {
                    parts.push(Doc::HardLine);
                }
                parts.extend(leading_doc(leading));
                parts.push(lower(node));
                parts.push(trailing_doc(trailing));
                docs.push(Doc::Concat(parts));
                first_node = false;
            }
        }
    }
    docs
}

/// Lower a block, reattaching comments from the CST.
fn block_doc(b: &Block) -> Doc {
    // Find the tail expression node (last ExprStmt without trailing `;`).
    let tail_expr = b.tail();
    let tail_stmt_range = tail_expr
        .as_ref()
        .and_then(|e| e.syntax().parent())
        .map(|p| p.text_range());

    let docs = lower_container(
        b.syntax(),
        |node| {
            if let Some(stmt) = node_to_stmt(node) {
                let is_tail = tail_stmt_range.is_some_and(|r| r == stmt.syntax().text_range());
                if is_tail
                    && let Stmt::Expr(es) = &stmt
                    && let Some(e) = es.expr()
                {
                    return expr_doc(&e);
                }
                stmt_doc(&stmt)
            } else {
                Doc::text(compact(node))
            }
        },
        false,
    );

    brace_block(docs)
}

// --- statements ------------------------------------------------------------

fn is_multiline_expr(e: &Expr) -> bool {
    matches!(
        e,
        Expr::If(_) | Expr::Match(_) | Expr::Loop(_) | Expr::Block(_)
    )
}

fn stmt_doc(s: &Stmt) -> Doc {
    match s {
        Stmt::Let(l) => {
            let mut head = String::from("let ");
            if l.is_mut() {
                head.push_str("mut ");
            }
            head.push_str(&l.name().map(tok_text).unwrap_or_default());
            if let Some(ty) = l.ty() {
                head.push_str(": ");
                head.push_str(&ser_node(ty.syntax()));
            }
            match l.init() {
                Some(init) if is_multiline_expr(&init) => Doc::concat([
                    Doc::text(format!("{head} = ")),
                    expr_doc(&init),
                    Doc::text(";"),
                ]),
                Some(init) => Doc::concat([
                    Doc::text(format!("{head} = ")),
                    expr_inline(&init),
                    Doc::text(";"),
                ]),
                None => Doc::text(format!("{head};")),
            }
        }
        Stmt::Return(r) => match r.value() {
            Some(v) if is_multiline_expr(&v) => {
                Doc::concat([Doc::text("return "), expr_doc(&v), Doc::text(";")])
            }
            Some(v) => Doc::concat([Doc::text("return "), expr_inline(&v), Doc::text(";")]),
            None => Doc::text("return;"),
        },
        Stmt::Expr(es) => match es.expr() {
            Some(e) if is_multiline_expr(&e) => expr_doc(&e),
            Some(e) => Doc::concat([expr_inline(&e), Doc::text(";")]),
            None => Doc::Nil,
        },
        Stmt::While(w) => {
            let body = w
                .body()
                .map(|b| block_doc(&b))
                .unwrap_or_else(|| Doc::text("{}"));
            let head: Doc = if w.is_while_let() {
                let pat = w.let_pattern().map(|p| pat_str(&p)).unwrap_or_default();
                let ex = w.condition().map(|c| expr_inline(&c)).unwrap_or(Doc::Nil);
                Doc::concat([Doc::text(format!("while let {pat} = ")), ex, Doc::text(" ")])
            } else {
                let cond = w.condition().map(|c| expr_inline(&c)).unwrap_or(Doc::Nil);
                Doc::concat([Doc::text("while "), cond, Doc::text(" ")])
            };
            Doc::concat([head, body])
        }
        Stmt::Loop(l) => {
            let body = l
                .body()
                .map(|b| block_doc(&b))
                .unwrap_or_else(|| Doc::text("{}"));
            Doc::concat([Doc::text("loop "), body])
        }
        Stmt::For(f) => {
            let var = f.var().map(tok_text).unwrap_or_default();
            let it = f.iter().map(|e| expr_inline(&e)).unwrap_or(Doc::Nil);
            let body = f
                .body()
                .map(|b| block_doc(&b))
                .unwrap_or_else(|| Doc::text("{}"));
            Doc::concat([
                Doc::text(format!("for {var} in ")),
                it,
                Doc::text(" "),
                body,
            ])
        }
        Stmt::Break(statement) => match statement.value() {
            Some(value) => Doc::concat([Doc::text("break "), expr_inline(&value), Doc::text(";")]),
            None => Doc::text("break;"),
        },
        Stmt::Continue(_) => Doc::text("continue;"),
    }
}

// --- expressions -----------------------------------------------------------

/// Render an expression as a Doc: block-bearing forms (`if`/`match`/block) go
/// multi-line; everything else is a wrapping-aware inline [`Doc`].
fn expr_doc(e: &Expr) -> Doc {
    match e {
        Expr::If(i) => if_doc(i),
        Expr::Match(m) => match_doc(m),
        Expr::Loop(loop_expr) => Doc::concat([
            Doc::text("loop "),
            loop_expr
                .body()
                .map(|body| block_doc(&body))
                .unwrap_or_else(|| Doc::text("{}")),
        ]),
        Expr::Block(b) => block_doc(b),
        _ => expr_inline(e),
    }
}

fn if_doc(i: &IfExpr) -> Doc {
    let cond = i.condition().map(|c| expr_inline(&c)).unwrap_or(Doc::Nil);
    let head: Doc = if i.is_if_let() {
        let pat = i.let_pattern().map(|p| pat_str(&p)).unwrap_or_default();
        Doc::concat([Doc::text(format!("if let {pat} = ")), cond])
    } else {
        Doc::concat([Doc::text("if "), cond])
    };
    let then = i
        .then_block()
        .map(|b| block_doc(&b))
        .unwrap_or_else(|| Doc::text("{}"));
    let mut parts = vec![head, Doc::text(" "), then];
    if let Some(eb) = i.else_block() {
        parts.push(Doc::text(" else "));
        parts.push(block_doc(&eb));
    } else if let Some(ei) = i.else_if() {
        parts.push(Doc::text(" else "));
        parts.push(if_doc(&ei));
    }
    Doc::Concat(parts)
}

fn match_doc(m: &MatchExpr) -> Doc {
    let scrut = m.scrutinee().map(|s| expr_inline(&s)).unwrap_or(Doc::Nil);
    let arms: Vec<Doc> = m.arms().map(|a| arm_doc(&a)).collect();
    let inner = if arms.is_empty() {
        Doc::Nil
    } else {
        Doc::indent(Doc::concat([Doc::HardLine, Doc::join(Doc::HardLine, arms)]))
    };
    Doc::concat([
        Doc::text("match "),
        scrut,
        Doc::text(" {"),
        inner,
        Doc::HardLine,
        Doc::text("}"),
    ])
}

fn arm_doc(a: &MatchArm) -> Doc {
    let pats: Vec<String> = a.patterns().map(|p| pat_str(&p)).collect();
    let head_str = pats.join(" | ");
    let mut head = Doc::text(head_str);
    if let Some(g) = a.guard() {
        head = Doc::concat([head, Doc::text(" if "), expr_inline(&g)]);
    }
    match a.body() {
        Some(b) if is_multiline_expr(&b) => {
            Doc::concat([head, Doc::text(" => "), expr_doc(&b), Doc::text(",")])
        }
        Some(b) => Doc::concat([head, Doc::text(" => "), expr_inline(&b), Doc::text(",")]),
        None => Doc::concat([head, Doc::text(" =>")]),
    }
}

/// Collapse a subtree to a single whitespace-normalized line. Only used as a
/// fallback for block-bearing expressions nested inside an inline context.
fn compact(n: &SyntaxNode) -> String {
    n.text()
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build a wrapping, comma-separated list: stays flat if it fits on one line,
/// or indented one-item-per-line with trailing comma when broken.
///
/// When `spaced` is true (braces), the delimiters get a space in flat mode
/// (e.g. `{ a, b }`); when false (parens/brackets), they don't (e.g. `(a, b)`).
fn wrap_list(open: &str, items: Vec<Doc>, close: &str, spaced: bool) -> Doc {
    if items.is_empty() {
        return Doc::text(format!("{open}{close}"));
    }
    let (open_break, close_break): (&Doc, &Doc) = if spaced {
        (&Doc::Line, &Doc::Line)
    } else {
        (&Doc::SoftLine, &Doc::SoftLine)
    };
    Doc::concat([
        Doc::text(open),
        Doc::group(Doc::concat([
            Doc::indent(Doc::concat([
                open_break.clone(),
                Doc::join(Doc::concat([Doc::text(","), Doc::Line]), items),
                Doc::if_break(Doc::text(","), Doc::Nil),
            ])),
            close_break.clone(),
        ])),
        Doc::text(close),
    ])
}

fn arg_list_doc(args: Option<ArgList>) -> Doc {
    match args {
        Some(a) => {
            let items: Vec<Doc> = a.args().map(|e| expr_inline(&e)).collect();
            if items.is_empty() {
                Doc::Nil
            } else {
                Doc::group(Doc::concat([
                    Doc::indent(Doc::concat([
                        Doc::SoftLine,
                        Doc::join(Doc::concat([Doc::text(","), Doc::Line]), items),
                        Doc::if_break(Doc::text(","), Doc::Nil),
                    ])),
                    Doc::SoftLine,
                ]))
            }
        }
        None => Doc::Nil,
    }
}

/// Render an expression to a [`Doc`]. Simple expressions produce flat text;
/// compound expressions (calls, struct literals, macros) produce wrapping
/// groups that break across lines when too wide.
fn expr_inline(e: &Expr) -> Doc {
    match e {
        Expr::Literal(l) => Doc::text(l.value().map(tok_text).unwrap_or_default()),
        Expr::Path(p) => Doc::text(p.segments().map(tok_text).collect::<Vec<_>>().join("::")),
        Expr::Paren(pe) => Doc::concat([
            Doc::text("("),
            pe.inner().map(|i| expr_inline(&i)).unwrap_or(Doc::Nil),
            Doc::text(")"),
        ]),
        Expr::Unary(u) => Doc::concat([
            Doc::text(u.op().map(tok_text).unwrap_or_default()),
            u.operand().map(|o| expr_inline(&o)).unwrap_or(Doc::Nil),
        ]),
        Expr::Bin(b) => Doc::concat([
            b.lhs().map(|x| expr_inline(&x)).unwrap_or(Doc::Nil),
            Doc::text(format!(" {} ", b.op().map(tok_text).unwrap_or_default())),
            b.rhs().map(|x| expr_inline(&x)).unwrap_or(Doc::Nil),
        ]),
        Expr::Assign(a) => Doc::concat([
            a.target().map(|x| expr_inline(&x)).unwrap_or(Doc::Nil),
            Doc::text(format!(
                " {} ",
                a.op().map(tok_text).unwrap_or_else(|| "=".to_string())
            )),
            a.value().map(|x| expr_inline(&x)).unwrap_or(Doc::Nil),
        ]),
        Expr::Range(r) => Doc::concat([
            r.start().map(|x| expr_inline(&x)).unwrap_or(Doc::Nil),
            Doc::text(if r.is_inclusive() { "..=" } else { ".." }),
            r.end().map(|x| expr_inline(&x)).unwrap_or(Doc::Nil),
        ]),
        Expr::Try(t) => Doc::concat([
            t.expr().map(|x| expr_inline(&x)).unwrap_or(Doc::Nil),
            Doc::text("?"),
        ]),
        Expr::Call(c) => {
            let callee = c.callee().map(|x| expr_inline(&x)).unwrap_or(Doc::Nil);
            let args = arg_list_doc(c.arg_list());
            Doc::concat([callee, Doc::text("("), args, Doc::text(")")])
        }
        Expr::MethodCall(m) => {
            let recv = m.receiver().map(|x| expr_inline(&x)).unwrap_or(Doc::Nil);
            let method = m.method_name().map(tok_text).unwrap_or_default();
            let args = arg_list_doc(m.arg_list());
            Doc::concat([
                recv,
                Doc::text(if m.is_optional() { "?." } else { "." }),
                Doc::text(method),
                Doc::text("("),
                args,
                Doc::text(")"),
            ])
        }
        Expr::Field(f) => Doc::concat([
            f.base().map(|x| expr_inline(&x)).unwrap_or(Doc::Nil),
            Doc::text(if f.is_optional() { "?." } else { "." }),
            Doc::text(f.field_name().map(tok_text).unwrap_or_default()),
        ]),
        Expr::Index(i) => Doc::concat([
            i.base().map(|x| expr_inline(&x)).unwrap_or(Doc::Nil),
            Doc::text("["),
            i.index().map(|x| expr_inline(&x)).unwrap_or(Doc::Nil),
            Doc::text("]"),
        ]),
        Expr::Array(array) => wrap_list(
            "[",
            array
                .elements()
                .map(|element| expr_inline(&element))
                .collect(),
            "]",
            false,
        ),
        Expr::StructLit(s) => struct_lit_doc(s),
        Expr::Map(map) => map_doc(map),
        Expr::Closure(closure) => Doc::text(ser_node(closure.syntax())),
        Expr::If(if_expr) => if_doc(if_expr),
        Expr::Match(match_expr) => match_doc(match_expr),
        Expr::Loop(loop_expr) => Doc::concat([
            Doc::text("loop "),
            loop_expr
                .body()
                .map(|body| block_doc(&body))
                .unwrap_or_else(|| Doc::text("{}")),
        ]),
        Expr::Block(block) => block_doc(block),
    }
}

fn struct_lit_doc(s: &StructLitExpr) -> Doc {
    let path = s
        .path_segments()
        .map(tok_text)
        .collect::<Vec<_>>()
        .join("::");
    let fields: Vec<Doc> = s
        .fields()
        .map(|fi| {
            let name = fi.name().map(tok_text).unwrap_or_default();
            match fi.value() {
                Some(v) => Doc::concat([Doc::text(format!("{name}: ")), expr_inline(&v)]),
                None => Doc::text(name),
            }
        })
        .collect();
    if fields.is_empty() {
        Doc::text(format!("{path} {{}}"))
    } else {
        Doc::concat([
            Doc::text(path),
            Doc::text(" "),
            wrap_list("{", fields, "}", true),
        ])
    }
}

fn map_doc(map: &MapExpr) -> Doc {
    let entries = map
        .entries()
        .map(|entry| {
            Doc::concat([
                entry.key().map(|key| expr_inline(&key)).unwrap_or(Doc::Nil),
                Doc::text(": "),
                entry
                    .value()
                    .map(|value| expr_inline(&value))
                    .unwrap_or(Doc::Nil),
            ])
        })
        .collect();
    Doc::group(wrap_list("#{", entries, "}", true))
}

// --- patterns --------------------------------------------------------------

fn pat_str(p: &Pattern) -> String {
    match p.kind() {
        PatternKind::Missing => compact(p.syntax()),
        PatternKind::Wildcard => "_".into(),
        PatternKind::Binding => p.binding_name().map(tok_text).unwrap_or_else(|| "_".into()),
        PatternKind::Literal => p
            .syntax()
            .descendants_with_tokens()
            .filter_map(|e| e.into_token())
            .filter(|t| !t.kind().is_trivia())
            .map(|t| t.text().to_string())
            .collect::<String>(),
        PatternKind::Range => match p.range() {
            Some(range) => format!(
                "{}{}{}",
                range
                    .start()
                    .map(|literal| literal.text())
                    .unwrap_or_default(),
                range.operator_token().text(),
                range
                    .end()
                    .map(|literal| literal.text())
                    .unwrap_or_default()
            ),
            None => compact(p.syntax()),
        },
        PatternKind::Path => p
            .path_segments()
            .map(tok_text)
            .collect::<Vec<_>>()
            .join("::"),
        PatternKind::TupleVariant => {
            let path = p
                .path_segments()
                .map(tok_text)
                .collect::<Vec<_>>()
                .join("::");
            let elems: Vec<String> = p.sub_patterns().map(|sp| pat_str(&sp)).collect();
            format!("{path}({})", elems.join(", "))
        }
        PatternKind::StructVariant => {
            let path = p
                .path_segments()
                .map(tok_text)
                .collect::<Vec<_>>()
                .join("::");
            let mut fields: Vec<String> = p
                .pattern_fields()
                .map(|field| match field.pattern() {
                    Some(pattern) => format!("{}: {}", field.name().text(), pat_str(&pattern)),
                    None if field.is_shorthand() => field.name().text().to_string(),
                    None => format!("{}:", field.name().text()),
                })
                .collect();
            if p.rest_token().is_some() {
                fields.push("..".into());
            }
            if fields.is_empty() {
                format!("{path} {{}}")
            } else {
                format!("{path} {{ {} }}", fields.join(", "))
            }
        }
    }
}
