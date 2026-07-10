//! Member-completion context detection + source repair (`x.` → members).
//!
//! The CST is error-tolerant, so it still locates the receiver and the `.` even
//! when the member is unwritten (`p.`). We turn that into (a) a receiver-end
//! anchor for `ReceiverIndex::at_end`, and (b) an optional sentinel insertion so
//! the (non-tolerant) `ruac` parser can type-check the receiver.

use rowan::{TextSize, TokenAtOffset};

use crate::ast::{AstNode, Expr, FieldExpr, MethodCallExpr, SourceFile};
use crate::kind::SyntaxKind as K;
use crate::{SyntaxNode, SyntaxToken};

/// Placeholder member inserted after a bare `.` so `recv.` parses as `recv.SENTINEL`.
pub const SENTINEL: &str = "__rua_complete";

/// A resolved member-access receiver expression anchor for `ReceiverIndex::at_end`.
#[derive(Debug, Clone)]
pub struct CompletionCtx {
    /// Byte offset of the receiver expression's end (stable across repair).
    pub receiver_end: usize,
    /// Byte offset to insert [`SENTINEL`] (just past the `.`), when needed.
    pub insert_at: usize,
    /// Whether a sentinel must be inserted (bare `.` with no member ident).
    pub needs_sentinel: bool,
    /// Whether a `;` must be appended after the member slot to terminate a
    /// statement whose following code would otherwise fail to parse.
    pub needs_semi: bool,
    /// Byte offset to insert the terminating `;` when [`needs_semi`] and no
    /// sentinel is inserted (just past an already-typed member ident). When a
    /// sentinel is inserted, the `;` follows it directly instead.
    pub semi_at: usize,
    /// Partial member text already typed (for optional server-side filtering).
    pub prefix: String,
}

/// Detect a member-completion context at `offset`. Returns `None` when the
/// cursor is not in a member slot (`recv.` / `recv.par|`).
pub fn completion_context(file: &SourceFile, offset: usize) -> Option<CompletionCtx> {
    let max = usize::from(file.syntax().text_range().end());
    let offset = offset.min(max);
    let pos = TextSize::from(offset as u32);
    let (acc, dot) = locate_member_access(file, pos)?;

    // Must be in the member slot (after the dot), not on the receiver.
    let dot_end = usize::from(dot.text_range().end());
    if offset < dot_end {
        return None;
    }

    let receiver = member_receiver(&acc)?;
    let receiver_end = usize::from(receiver.syntax().text_range().end());

    // `semi_at` defaults to just past the member ident (used only when a
    // partial ident is already present and no sentinel is spliced).
    let (needs_sentinel, prefix, semi_at) = match member_name_token(&acc) {
        Some(name) => {
            let ns = usize::from(name.text_range().start());
            let ne = usize::from(name.text_range().end());
            if offset >= ns && offset <= ne {
                (false, name.text()[..offset - ns].to_string(), ne)
            } else {
                // Cursor before a (possibly glued) member ident → empty prefix,
                // source already parses.
                (false, String::new(), ne)
            }
        }
        None => (true, String::new(), dot_end),
    };

    Some(CompletionCtx {
        receiver_end,
        insert_at: dot_end,
        needs_sentinel,
        needs_semi: stmt_needs_terminator(&acc),
        semi_at,
        prefix,
    })
}

/// Produce the repaired source `ruac` should type-check. When a member
/// ident already exists the source is returned unchanged; otherwise [`SENTINEL`]
/// is spliced in just past the `.` (after the receiver, so `receiver_end` is
/// unchanged).
pub fn repair(src: &str, ctx: &CompletionCtx) -> String {
    if ctx.needs_sentinel {
        // Splice `SENTINEL` (and, if the statement needs it, a trailing `;`)
        // just past the `.`; `receiver_end` sits before this, so it's stable.
        let mut s = String::with_capacity(src.len() + SENTINEL.len() + 1);
        s.push_str(&src[..ctx.insert_at]);
        s.push_str(SENTINEL);
        if ctx.needs_semi {
            s.push(';');
        }
        s.push_str(&src[ctx.insert_at..]);
        return s;
    }
    if ctx.needs_semi {
        // A partial member ident is already present; terminate the statement by
        // inserting `;` just past it so following statements still parse.
        let mut s = String::with_capacity(src.len() + 1);
        s.push_str(&src[..ctx.semi_at]);
        s.push(';');
        s.push_str(&src[ctx.semi_at..]);
        return s;
    }
    src.to_string()
}

// --- helpers ----------------------------------------------------------------

/// Find the `(FieldExpr|MethodCallExpr, dot_token)` the cursor is completing.
fn locate_member_access(file: &SourceFile, pos: TextSize) -> Option<(SyntaxNode, SyntaxToken)> {
    let tok = pick_token(file, pos)?;

    // Case 1: cursor on the member ident (`p.par|`).
    if tok.kind() == K::Ident
        && let Some(parent) = tok.parent()
        && matches!(parent.kind(), K::FieldExpr | K::MethodCallExpr)
        && let Some(dot) = dot_child(&parent)
    {
        return Some((parent, dot));
    }

    // Case 2: cursor right after a bare `.` (`p.|`).
    if tok.kind() == K::Dot
        && let Some(parent) = tok.parent()
        && matches!(parent.kind(), K::FieldExpr | K::MethodCallExpr)
    {
        return Some((parent, tok));
    }

    None
}

/// The token anchoring the cursor: the member ident it's inside, or the `.` it
/// sits right after. Trivia resolves to the previous token (`p. |`).
fn pick_token(file: &SourceFile, pos: TextSize) -> Option<SyntaxToken> {
    match file.syntax().token_at_offset(pos) {
        TokenAtOffset::Single(t) => {
            if t.kind().is_trivia() {
                t.prev_token()
            } else {
                Some(t)
            }
        }
        TokenAtOffset::Between(left, right) => {
            if right.kind() == K::Ident {
                Some(right)
            } else if left.kind() == K::Dot || left.kind() == K::Ident {
                Some(left)
            } else if left.kind().is_trivia() {
                left.prev_token()
            } else {
                None
            }
        }
        TokenAtOffset::None => None,
    }
}

fn dot_child(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == K::Dot)
}

fn member_receiver(acc: &SyntaxNode) -> Option<Expr> {
    match acc.kind() {
        K::FieldExpr => FieldExpr::cast(acc.clone())?.base(),
        K::MethodCallExpr => MethodCallExpr::cast(acc.clone())?.receiver(),
        _ => None,
    }
}

fn member_name_token(acc: &SyntaxNode) -> Option<SyntaxToken> {
    match acc.kind() {
        K::FieldExpr => FieldExpr::cast(acc.clone())?.field_name(),
        K::MethodCallExpr => MethodCallExpr::cast(acc.clone())?.method_name(),
        _ => None,
    }
}

/// Whether the member access needs a trailing `;` to keep the (non-tolerant)
/// `ruac` parser happy: it sits *directly* as a statement expression
/// (`ExprStmt`) or a `let` initializer (`LetStmt`) that has no terminating `;`.
/// Without it, a following statement (`stack.` then `while ...`) is a parse
/// error and type-checking bails, yielding no completions.
///
/// Expression positions (call args, index, macro args, …) have a different
/// parent and are excluded — inserting `;` there would itself be a syntax error.
fn stmt_needs_terminator(acc: &SyntaxNode) -> bool {
    let Some(parent) = acc.parent() else {
        return false;
    };
    if !matches!(parent.kind(), K::ExprStmt | K::LetStmt) {
        return false;
    }
    // Already terminated → don't double up.
    !parent
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .any(|t| t.kind() == K::Semi)
}
