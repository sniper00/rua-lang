//! Rua compiler library.
//!
//! Rua is a Rust-syntax subset language that transpiles to readable Lua 5.5
//! source (see `docs/rua-design.md`). This crate implements the front end
//! (hand-written lexer + recursive-descent parser, structure modeled on the
//! lua-rs parser) and a Lua-source codegen backend.
//!
//! Current scope: P0 + core of P1 (functions, `let`/`let mut`, arithmetic /
//! logic / comparison with precedence, `if`/`else` as expression and statement,
//! `while`/`loop`/`break`/`continue`, blocks, calls, `return`).
//!
//! # API stability
//!
//! The IDE-facing functions [`check_diags`], [`member_index`], [`type_members`],
//! [`binding_types`], [`member_index_path`], [`member_index_src`],
//! [`member_completion`], and [`member_completion_src`] are transition-only
//! compatibility bridges for the current `rua-syntax` and `rua-lsp` crates.
//! They are not stable compiler APIs and must not gain new consumers. Phase 5
//! moves their queries into `rua-analysis`; Phase 6 then removes or privatizes
//! these facades as part of narrowing `ruac`'s public API.

pub mod ast;
pub mod check;
pub mod codegen;
pub mod diag;
pub mod lexer;
pub mod parser;
pub mod reader;
pub mod resolve;
pub mod token;
pub mod tokenize;
pub mod typeck;

use std::path::Path;

use crate::diag::Diag;

/// Compile Rua source text to Lua 5.5 source text. File modules (`mod name;`)
/// are not available here (no base directory); use [`compile_path`] for those.
pub fn compile_str(src: &str) -> Result<String, String> {
    let mut program = parser::parse(src)?;
    // File id 0 with an empty path: diagnostics fall back to `line: msg`.
    let mut files = vec![String::new()];
    resolve::resolve_modules(&mut program.items, None, &mut files)?;
    resolve::resolve_uses(&mut program);
    check::check(&program, &files)?;
    let info = typeck::check(&program, &files)?;
    reject_pending_closure_codegen(&info, &files)?;
    Ok(codegen::generate(&program, &info))
}

/// Compile a Rua source file (resolving `mod name;` file modules relative to the
/// file's directory) to Lua 5.5 source text.
pub fn compile_path(path: &Path) -> Result<String, String> {
    let (program, files) = parse_and_resolve(path)?;
    check::check(&program, &files)?;
    let info = typeck::check(&program, &files)?;
    reject_pending_closure_codegen(&info, &files)?;
    Ok(codegen::generate(&program, &info))
}

fn reject_pending_closure_codegen(info: &typeck::TypeInfo, files: &[String]) -> Result<(), String> {
    let (span, message) = if let Some(span) = info.first_closure() {
        (span, "closure codegen is not implemented yet")
    } else if let Some(span) = info.pending_iter_codegen() {
        (span, "iterator codegen is not implemented yet")
    } else {
        return Ok(());
    };
    let diagnostic = Diag::new(
        span.file,
        span.start,
        span.len,
        span.line,
        message.to_string(),
    );
    Err(diag::render_all(&[diagnostic], files))
}

/// Parse a file and splice in its file modules, without semantic checks. Returns
/// the merged program plus the file registry (index = file id) for diagnostics.
pub fn parse_and_resolve(path: &Path) -> Result<(ast::Program, Vec<String>), String> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| format!("reading {}: {}", path.display(), e))?;
    let mut program = parser::parse(&src).map_err(|e| format!("{}: {}", path.display(), e))?;
    // The root file is id 0; child files are appended during resolution.
    let mut files = vec![path.display().to_string()];
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    resolve::resolve_modules(&mut program.items, Some(dir), &mut files)?;
    resolve::resolve_uses(&mut program);
    Ok((program, files))
}

/// Run parse + resolve + structural-check + typeck on an in-memory source string
/// and return every diagnostic with its byte-offset span (if available), plus the
/// file registry (index = file id, for resolving `Diag.file` to a path).
///
/// This is the entry point for LSP live diagnostics: the returned diagnostics can
/// be converted to `lsp_types::Diagnostic` via `rua_syntax::LineIndex`.
///
/// Parse errors and resolve errors (which currently return `String`) are wrapped
/// as [`Diag::bare`] — they carry the message but no span. When the parser is
/// enhanced to emit structured errors with byte offsets (I2), those will flow
/// through as properly-positioned diagnostics.
///
/// # Transition-only
///
/// Compatibility bridge for the current LSP diagnostics path. New IDE code
/// must use the planned `rua-analysis` diagnostics query instead of extending
/// this single-file compiler entry point. Phase 5 removes its IDE consumers.
pub fn check_diags(src: &str) -> (Vec<Diag>, Vec<String>) {
    let mut diags: Vec<Diag> = Vec::new();

    // 1. Parse
    let mut program = match parser::parse(src) {
        Ok(p) => p,
        Err(e) => {
            diags.push(Diag::bare(format!("parse error: {e}")));
            return (diags, vec![String::new()]);
        }
    };

    // 2. Resolve (file modules will fail for in-memory sources — that's OK).
    // File id 0 with an empty path: diagnostics fall back to `line: msg`.
    let mut files = vec![String::new()];
    if let Err(e) = resolve::resolve_modules(&mut program.items, None, &mut files) {
        diags.push(Diag::bare(format!("resolve error: {e}")));
        // Continue with what we have — structural/type checks may still find
        // issues in the rest of the program.
    }
    resolve::resolve_uses(&mut program);

    // 3. Structural checks.
    diags.extend(check::collect_diags(&program));

    // 4. Type-checking.
    diags.extend(typeck::collect_diags(&program));

    (diags, files)
}

/// Type-check in-memory `src` (single-file view) and return the member-access
/// resolution table (`x.field` / `x.method()` → definition span + hover detail)
/// for the LSP.
///
/// Single-file view: file modules (`mod name;`) are not loaded, so member
/// accesses whose receiver type is defined in another file resolve to nothing
/// (returned index simply omits them — zero false positives). Parse/resolve
/// errors yield whatever partial index type-checking could still produce.
///
/// # Transition-only
///
/// Compatibility bridge for the current `rua-syntax` analysis cache. New IDE
/// code must use the planned `rua-analysis` type-inference/member query. Phase 5
/// removes this bridge from the IDE path.
pub fn member_index(src: &str) -> typeck::MemberIndex {
    let mut program = match parser::parse(src) {
        Ok(p) => p,
        Err(_) => return typeck::MemberIndex::default(),
    };
    let mut files = vec![String::new()];
    let _ = resolve::resolve_modules(&mut program.items, None, &mut files);
    resolve::resolve_uses(&mut program);
    typeck::member_index(&program)
}

/// Type-check in-memory `src` (single-file view) and return the member-completion
/// catalog (`type name → fields + methods`). Mirrors [`member_index`]; file
/// modules are not loaded — cross-file completion uses the `_src` variant (C1).
///
/// # Transition-only
///
/// Compatibility bridge for the current member-completion implementation. New
/// IDE code must use the planned `rua-analysis` completion queries. Phase 5
/// removes this bridge from the IDE path.
pub fn type_members(src: &str) -> typeck::TypeMembers {
    let mut program = match parser::parse(src) {
        Ok(p) => p,
        Err(_) => return typeck::TypeMembers::default(),
    };
    let mut files = vec![String::new()];
    let _ = resolve::resolve_modules(&mut program.items, None, &mut files);
    resolve::resolve_uses(&mut program);
    typeck::type_members(&program)
}

/// Type-check in-memory `src` (single-file view) and return the binding-type
/// index (`let` / `for` / parameter name span → inferred-type hover text) for
/// LSP local-variable hover. Mirrors [`member_index`]; file modules are not
/// loaded (locals are always resolved within their own file, so file id 0
/// matches the LSP's single-file view).
///
/// # Transition-only
///
/// Compatibility bridge for the current local-hover implementation. New IDE
/// code must use the planned `rua-analysis` body-inference query. Phase 5
/// removes this bridge from the IDE path.
pub fn binding_types(src: &str) -> typeck::BindingTypes {
    let mut program = match parser::parse(src) {
        Ok(p) => p,
        Err(_) => return typeck::BindingTypes::default(),
    };
    let mut files = vec![String::new()];
    let _ = resolve::resolve_modules(&mut program.items, None, &mut files);
    resolve::resolve_uses(&mut program);
    typeck::binding_types(&program)
}

/// Multi-file variant of [`member_index`]: parse `path`, splice in its `mod name;`
/// file modules (relative to the file's directory), then build the member table.
///
/// Unlike [`member_index`], receiver types defined in child files resolve, and
/// each hit's `member_file` / `target_file` are real file ids into the returned
/// registry (index = file id → path). File id 0 is `path` itself.
///
/// Parse / resolve / type errors are tolerated: whatever partial index could be
/// produced is returned (never panics, never hard-errors), so the LSP degrades
/// gracefully while the user is mid-edit.
///
/// # Transition-only
///
/// Compatibility bridge for path-based member lookup. It is limited to disk
/// state and must not become a workspace API. Phase 5 replaces it with
/// `rua-analysis` queries backed by the VFS and open buffers.
pub fn member_index_path(path: &Path) -> (typeck::MemberIndex, Vec<String>) {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return (typeck::MemberIndex::default(), vec![path.display().to_string()]),
    };
    member_index_src(&src, path)
}

/// Like [`member_index_path`] but the root file's text is supplied directly
/// (`root_src`) instead of read from disk, while `mod name;` **child** files are
/// still loaded from disk relative to `root_path`'s directory.
///
/// This is the LSP entry point: the active buffer (with unsaved edits) drives the
/// root offsets — so a member use-site span matches the editor's cursor — while
/// types defined in sibling/child files resolve from their on-disk contents.
///
/// Limitation: unsaved edits in **child** files are not reflected (they load from
/// disk). File id 0 is `root_path`.
///
/// # Transition-only
///
/// Compatibility bridge for cross-file member lookup. New IDE code must use
/// the planned VFS-backed `rua-analysis` query, which sees all open buffers.
/// Phase 5 removes this bridge from the IDE path.
pub fn member_index_src(root_src: &str, root_path: &Path) -> (typeck::MemberIndex, Vec<String>) {
    let mut program = match parser::parse(root_src) {
        Ok(p) => p,
        Err(_) => return (typeck::MemberIndex::default(), vec![root_path.display().to_string()]),
    };
    let mut files = vec![root_path.display().to_string()];
    let dir = root_path.parent().unwrap_or_else(|| Path::new("."));
    let _ = resolve::resolve_modules(&mut program.items, Some(dir), &mut files);
    resolve::resolve_uses(&mut program);
    (typeck::member_index(&program), files)
}

/// Single-file member completion: `(catalog, receiver index)`. Mirrors
/// [`member_index`]; file modules not loaded (use [`member_completion_src`]).
///
/// # Transition-only
///
/// Compatibility bridge for the current single-file completion path. New IDE
/// code must use the planned `rua-analysis` completion query. Phase 5 removes
/// this bridge from the IDE path.
pub fn member_completion(src: &str) -> (typeck::TypeMembers, typeck::ReceiverIndex) {
    let mut program = match parser::parse(src) {
        Ok(p) => p,
        Err(_) => {
            return (
                typeck::TypeMembers::default(),
                typeck::ReceiverIndex::default(),
            );
        }
    };
    let mut files = vec![String::new()];
    let _ = resolve::resolve_modules(&mut program.items, None, &mut files);
    resolve::resolve_uses(&mut program);
    typeck::member_completion(&program)
}

/// Multi-file member completion: root text from the buffer, `mod` children
/// from disk. Mirrors [`member_index_src`]. Returns the file registry too so the
/// caller can map receiver `recv_file` ids (0 = root) if needed.
///
/// # Transition-only
///
/// Compatibility bridge for the current cross-file completion path. New IDE
/// code must use the planned VFS-backed `rua-analysis` completion query. Phase 5
/// removes this bridge from the IDE path.
pub fn member_completion_src(
    root_src: &str,
    root_path: &std::path::Path,
) -> (typeck::TypeMembers, typeck::ReceiverIndex, Vec<String>) {
    let mut program = match parser::parse(root_src) {
        Ok(p) => p,
        Err(_) => {
            return (
                typeck::TypeMembers::default(),
                typeck::ReceiverIndex::default(),
                vec![root_path.display().to_string()],
            );
        }
    };
    let mut files = vec![root_path.display().to_string()];
    let dir = root_path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let _ = resolve::resolve_modules(&mut program.items, Some(dir), &mut files);
    resolve::resolve_uses(&mut program);
    let (tm, ri) = typeck::member_completion(&program);
    (tm, ri, files)
}

#[cfg(test)]
mod tests;
