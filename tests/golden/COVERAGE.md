# Golden Coverage Matrix

This matrix records repository-level golden coverage, not all unit-test
coverage. A feature marked `No` may have focused unit tests but still lacks a
shared oracle fixture under `tests/golden/`.

Status meanings:

- `Yes`: direct golden coverage exists for the feature and column.
- `Partial`: related coverage exists, but an important behavior or rejection is
  still missing.
- `No`: no repository-level golden currently covers the feature and column.
- `N/A`: the column does not apply to that feature.

Current inventory:

- Compile pass: 43 `.rua` / `.lua.golden` pairs.
- Compile fail: 42 `.rua` / `.diag.golden` pairs.
- Parser/range: 15 accept, 6 reject, and 15 byte-range cases.
- `.ruai`: 5 compiler pass, 1 compiler fail, and 4 IDE snapshots.
- General IDE: 14 snapshots across completion, navigation, references, rename,
  diagnostics, and symbols.

| Feature | Compile pass | Compile fail | Parser/range | IDE snapshot | Notes |
| --- | --- | --- | --- | --- | --- |
| Lexing, comments, literals | Partial | No | Yes | Partial | `comments_whitespace_stability`; parser comments, escapes, numeric, keyword-boundary cases. No dedicated lexical compile-fail matrix. |
| Expressions and operators | Yes | Yes | Yes | N/A | `expr_*`, binary/unary type errors, call/field/index/path ranges. |
| Bindings, mutability, assignment | Yes | Partial | Partial | Yes | Let annotation mismatch is covered; assignment-target/type diagnostics and a dedicated let range snapshot are missing. |
| Blocks, if, while, loop | Yes | Yes | Yes | Yes | Includes if-expression lowering, break/continue, non-bool conditions, block ambiguity, and fast diagnostics. |
| Functions, returns, recursion | Yes | Yes | Yes | Yes | Zero/typed args, explicit/tail returns, recursion, arity/type errors, fn ranges, hover and symbols. |
| Struct declarations and literals | Yes | Yes | Yes | Yes | Fields, literals, methods, missing/extra fields, struct ranges, member completion and symbols. |
| Enums, match, patterns | Yes | Yes | Yes | Yes | All variant forms, match bindings, constructor/pattern shape errors, match-arm ranges, enum symbols. |
| `Option<T>` | Yes | Yes | Partial | No | `Some`/`None` and constructor arity covered; no dedicated IDE snapshot or Option-specific parser range. |
| `Result<T, E>` and `?` | Yes | Partial | Partial | No | `Ok`/`Err` and successful `?` lowering covered; invalid `?` receiver/error propagation mismatch diagnostics are missing. |
| `Vec<T>` | Yes | No | Yes | No | Basic codegen and index/type syntax covered; no shared element-mismatch diagnostic or Vec completion snapshot. |
| `HashMap<K, V>` | Yes | No | No | No | Basic codegen only; key/value mismatch goldens and dedicated parser/IDE snapshots are missing. |
| Std macros and runtime calls | Yes | No | Partial | No | `println!`, `format!`, and macro nodes appear in range fixtures; misuse diagnostics are missing. |
| Numeric ranges and `for` | Yes | No | Yes | No | Both range forms now have compiler Lua coverage; invalid-bound diagnostics and IDE coverage are still missing. |
| Closures | No | No | Yes | No | Both parsers accept expression/typed/block closures with range snapshots; typecheck/codegen are not implemented yet. |
| Iterator adapters and fusion | No | No | Partial | No | Adapter-call closure syntax has parser/range coverage; iterator typing and fused codegen are not implemented yet. |
| Inline modules and `use` | Yes | Yes | Yes | Yes | Inline/nested modules, aliases/grouped imports, private imports, use ranges, module-path completion and symbols. |
| File modules (`.rua`) | No | Yes | No | Yes | Missing-module rejection and IDE cross-file queries exist; compiler pass/codegen and dedicated parser/range cases are absent. |
| Visibility (`pub`/private) | Yes | Yes | Partial | N/A | Public and same-module private access plus cross-module/private import errors; no dedicated visibility parser range. |
| Extern Lua ABI and variadics | Yes | No | Yes | Partial | Extern blocks/ranges and `.ruai` signature hover exist; wrong-ABI/builtin misuse diagnostics are absent. |
| Generic functions and types | Yes | Partial | Yes | No | Identity and generic ADTs covered; rejection coverage concentrates on bounds rather than inference conflicts and no generic-specific IDE snapshot exists. |
| Traits, bounds, and `where` | Yes | Yes | Yes | Yes | Trait impl/default methods, generic method bounds, unknown/unsatisfied bounds, trait/impl ranges and member completion. |
| Methods and receiver forms | Yes | Yes | Yes | Yes | Associated/self/mut-self methods, call errors, receiver parsing, method ranges and completion. |
| Adjacent `.ruai` modules | Yes | Yes | Partial | Yes | Single-file/directory loading, declaration typecheck, codegen skip, hover/goto/completion/references/readonly rename. |
| External `.ruai` library roots | No | No | No | No | `--lib`, configured out-of-tree mounts, workspace/library/std source-root precedence and watchers are not implemented. |
| `.ruai` declaration restrictions | Partial | No | Partial | Yes | Declaration modules are skipped by codegen, but bodies are currently accepted and no invalid-body diagnostic golden exists. |
| Parse/name/type diagnostics | N/A | Yes | Yes | Yes | Exact compiler messages plus parser recovery and fast IDE diagnostics are covered. |
| Diagnostic codes and precise ranges | N/A | Partial | Partial | Partial | Compiler goldens preserve path/line/message; stable codes and parser byte ranges/columns are not available end to end. |
| Completion | N/A | N/A | N/A | Yes | Local, struct member, trait/default member, module path and `.ruai` member completion snapshots. |
| Hover and goto definition | N/A | N/A | N/A | Yes | Local type/function signature, cross-file targets, and `.ruai` signatures. |
| References and rename | N/A | N/A | N/A | Yes | Local/cross-file edits plus `.ruai` declaration references and readonly rejection. |
| Document symbols and docs | N/A | N/A | N/A | Yes | Struct/enum/trait/impl/module hierarchy, ranges, signatures and leading docs. |
| Semantic tokens | N/A | N/A | N/A | No | Not implemented and no snapshot exists. |
| Inlay hints | N/A | N/A | N/A | No | Not implemented and no snapshot exists. |
| Formatter/comment stability | Yes | N/A | Yes | N/A | Shared comment/whitespace compile golden plus parser trivia corpus; formatter also has crate-local goldens. |
| Lua source maps and trace | No | No | N/A | No | Tracked separately in `docs/rua-sourcemap.md`; no source-map golden is present. |

## Known Gaps

- Closures and iterator adapters have parser fixtures plus registered Phase 4A
  TODOs, but remain absent from type checking and executable codegen goldens.
- Range/`for` syntax and Lua lowering are covered; invalid-bound behavior is not
  protected by a repository golden.
- `Vec` and `HashMap` mismatch behavior has unit tests but no shared
  compile-fail oracle.
- File-based `.rua` module compilation lacks positive Lua goldens under
  `tests/golden/modules/`.
- External `.ruai` roots, explicit mounts, std/prelude priority, and library
  watchers have no production API; tests must not simulate them in a harness.
- Compiler diagnostics do not yet expose stable diagnostic codes, and parse or
  bare diagnostics lack end-to-end byte/column ranges.
- Conservative name/call checking intentionally leaves unresolved external
  names, non-callable values, and unknown methods without diagnostics.
- `.ruai` files currently accept function bodies; declaration-only syntax rules
  are not enforced.
- `.ruai` rename rejection currently reuses `RenameError::InvalidName` instead
  of a dedicated readonly/library error.
- Semantic tokens, inlay hints, iterator performance-shape assertions, and
  source-map snapshots are absent.

## Merge Gate

Every new language or IDE feature must update this matrix in the same change:

- New syntax: parser accept/reject plus compile pass or compile fail.
- New type checking: compile pass, compile fail, and a parity note.
- New code generation: byte-exact Lua golden.
- New IDE behavior: at least one cursor-query snapshot.
- New `.ruai` behavior: compiler and IDE goldens.

Changing an existing `Yes` to `Partial` or `No` requires an explicit rationale
in the change description.
