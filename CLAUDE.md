# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project overview

Rua is a **Rust-syntax-subset language** that transpiles to readable Lua 5.5. The repo holds the compiler (`ruac`), a lossless CST (`rua-syntax`), incremental semantic analysis (`rua-analysis`), and an LSP server (`rua-lsp`).

Key design constraint: no borrow checker, no lifetimes, no ownership — static types are erased at codegen. Rua targets Lua 5.5 as the execution backend.

## Working rules

**Every change must include:**
1. **Tests** — LSP-level features go in `crates/rua-lsp/tests/incremental_stress.rs`; analysis internals go in `crates/rua-analysis/tests/`. No change lands without a test that fails before the fix and passes after.
2. **Documentation** — update relevant docs in `docs/` or doc comments on public APIs when behaviour changes.
3. **Commit** — after tests pass and clippy is clean, commit immediately. Do NOT include `Co-Authored-By` or any other trailer.
   - Use `feat:` prefix for new features and user-facing additions.
   - Use `fix:` prefix for bug fixes and refactors.
   - Use `docs:` for documentation-only changes.


## Workspace crates

| Crate | Purpose |
|---|---|
| `ruac` | Compiler / transpiler: AST → HIR → Lua codegen. Heavy, pre-existing; **not** the focus of current IDE work. |
| `rua-syntax` | Lossless rowan CST: lexer, parser, `AstNode` views, formatter, `LineIndex`. The **IDE-facing** syntax tree. Must have `default-features = false` (disables legacy `ruac` bridge) when consumed by analysis/LSP. |
| `rua-analysis` | Incremental semantic analysis: HIR lowering, name resolution, type inference, and IDE query engine. Protocol-neutral — returns `NavigationTarget`, `HoverResult`, `CompletionItem`, etc. |
| `rua-lsp` | LSP server (stdio JSON-RPC, `lsp-server` + `lsp-types`). Two binaries: `rua-lsp` (requires `--features lsp`) and `rua-fmt` (formatter CLI, no feature gate). |

## Build & test commands

```bash
# Build the LSP server (release)
cargo build --release -p rua-lsp --features lsp

# Run all tests across all crates
cargo test --all

# Run only analysis + LSP tests (excludes ruac)
cargo test -p rua-analysis -p rua-lsp

# Run a single integration test by name
cargo test -p rua-lsp --test incremental_stress -- lint_redundant_mut

# Run analysis unit tests by module
cargo test -p rua-analysis -- hir

# Clippy (only on active crates — ruac has pre-existing warnings)
cargo clippy -p rua-analysis -p rua-lsp --all-targets -- -D warnings

# Format the workspace
cargo fmt --all

# Regenerate golden format snapshots
RUA_UPDATE_GOLDENS=1 cargo test -p rua-syntax --test formatter_goldens update_formatter_goldens -- --ignored --exact
```

**Note**: `cargo clippy --all-targets` will also check `ruac`. To keep cycle times short when only touching IDE crates, scope clippy to `-p rua-analysis -p rua-lsp --features lsp`.

## Analysis pipeline (the "two-tree" design)

The compiler (`ruac`) has its own AST. The IDE/LSP side has a **separate**, lossless rowan CST. These are independent trees that must agree on semantics (verified by conformance tests).

The analysis pipeline flows:

1. **Parse** (`rua-syntax::parse_source_file`) → lossless CST with trivia
2. **ItemTree** (`ItemTree::lower()`) → flat file-level item index (functions, structs, impls, etc.)
3. **DefMap** (`DefMap::build()`) → cross-file name resolution, module graph
4. **MemberIndex** (`MemberIndex::build_shared()`) → field/method → definition lookup (traits + impls + builtin types)
5. **Body lowering** (`lower_fn_body()`) → CST → HIR (`Body` with `Expr`, `Pat`, `NameRef`)
6. **BodyScopes + BodyResolution** → scope tree + name-ref-to-binding resolution
7. **Inference** (`infer_body()`) → type inference, unification, method resolution

All caches live in `BaseDb` with per-file invalidation. Query pipelines are chained: changing file text invalidates parse → item_tree → def_map → member_index → body → inference.

## Key architectural patterns

### Snapshot isolation: `AnalysisHost` / `Analysis`

`AnalysisHost` owns the mutable `BaseDb`. Call `host.analysis()` to get an immutable `Analysis` snapshot (Rc-shared). The LSP server creates a snapshot once per request cycle; the snapshot stays valid until the next `apply_change()`.

### LSP server dispatch

`crates/rua-lsp/src/lsp.rs` — a single `Server` struct. `main_loop()` reads JSON-RPC messages, dispatches to `handle_*` methods. Each handler:
1. Converts LSP protocol types to `rua_analysis` types (URI → FileId, LSP Position → offset)
2. Calls the corresponding `Analysis` method
3. Converts results back to LSP protocol types

### Integration test pattern

`crates/rua-lsp/tests/incremental_stress.rs` uses a minimal `TestServer` that mirrors the LSP server without protocol overhead:

```rust
let mut srv = TestServer::new();
srv.open(&uri, "fn main() { let x = 42; }");
let pp = srv.pp(&uri, line, col).unwrap();           // ProjectPosition
let hover = srv.snapshot().hover(pp);                // Option<HoverResult>
let diags = srv.snapshot().diagnostics(file_id);      // Vec<Diagnostic>
```

**Every change must have a test**. Tests go in `incremental_stress.rs` for LSP-level features (hover, goto-def, diagnostics), or in `crates/rua-analysis/tests/` for analysis internals (inference, body lowering, member resolution).

### AST-walking for cursor-sensitive queries

For hover, goto-def, and completion on member access (e.g., `p.translate()`), follow **rust-analyzer's pattern**: find the token at cursor offset, then walk **up** the syntax tree via `parent()` to find `FieldExpr` or `MethodCallExpr`. Extract the receiver child, match its range against HIR body expressions. Do NOT iterate `body.exprs()` blindly — it won't find the right receiver.

When using `token_at_offset`, handle `TokenAtOffset::Between(left, right)`: prefer `right` over `left` when `left.kind() == SyntaxKind::Dot` (cursor between `.` and method name).

### `&mut self` and the redundant-mut lint

The `mut` in `&mut self` is about **reference mutability** (mutation through the reference), not binding reassignment. Field writes like `self.x = expr` don't create `LocalUseKind::Write` for the `self` binding. The W0301 lint must additionally check for `Assign` targets whose root `Path` resolves to the binding via a chain of `Field`/`Index` expressions.

## Module layout (non-obvious)

- `crates/rua-analysis/src/hir/body.rs` — HIR expression/binding/pattern types + CST→HIR lowering
- `crates/rua-analysis/src/hir/def_map.rs` — cross-file name resolution, module graph
- `crates/rua-analysis/src/hir/member.rs` — field/method resolution (MemberIndex, trait impls, builtin types)
- `crates/rua-analysis/src/hir/infer.rs` — type inference engine
- `crates/rua-analysis/src/hir/scope.rs` — scope tree, local name resolution, `LocalUseKind`
- `crates/rua-analysis/src/hir/item_tree.rs` — flat file-level item index (ItemTree)
- `crates/rua-analysis/src/diagnostic/mod.rs` — all diagnostics: parse errors, type errors, lints (unused vars, redundant mut, dead code)
- `crates/rua-analysis/src/ide/mod.rs` — `Analysis` query methods (hover, goto_def, completion, references, etc.)
- `crates/rua-analysis/src/ide/completion.rs` — completion engine
- `crates/rua-analysis/src/ide/contract.rs` — protocol-neutral result types
- `crates/rua-analysis/src/ide/closure_iterator.rs` — semantic tokens + closure param detection

## Design documents

- `docs/README.md` - Current documentation index and maintenance rules
- `docs/rua-design.md` - Current language, runtime ABI, module, FFI, and compatibility contracts
- `docs/rua-architecture.md` - Current compiler, dual-parser, analysis, LSP, and validation architecture
- `docs/rua-lsp-features.md` - Current editor and protocol capabilities
