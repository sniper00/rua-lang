# Golden Coverage Matrix

This matrix records repository-level compiler/parser goldens and authoritative
native IDE oracles. The legacy IDE snapshot runner was deleted with the old
semantic facade; the IDE column therefore accepts an active native snapshot or
an exact protocol-neutral/LSP test owned by the production query implementation.

Status meanings:

- `Yes`: direct golden coverage exists for the feature and column.
- `Partial`: related coverage exists, but an important behavior or rejection is
  still missing.
- `No`: no repository-level golden currently covers the feature and column.
- `N/A`: the column does not apply to that feature.

Current inventory:

- Compile pass: 44 `.rua` / `.lua.golden` pairs; every generated artifact is also executed by Lua.
- Compile fail: 44 `.rua` / `.diag.golden` pairs plus shared code/file/range/argument manifests.
- Phase 4A: 13 compile-pass and 8 compile-fail closure/iterator pairs.
- Parser/range: 17 accept, 7 reject with dual-parser diagnostic snapshots, 15 byte-range cases, and a 512-case arbitrary-Unicode property test.
- Formatter: 11 repository-level input/output pairs with parse, lossless, token-preservation, idempotence, and `check_format` invariants.
- File modules: one nested multi-file compile/output/runtime golden.
- `.ruai`: 5 compiler pass, 2 compiler fail, and 4 IDE snapshots.
- General IDE: one active native closure/iterator snapshot plus exact native
  analysis and LSP suites; older snapshots are frozen migration records.

| Feature | Compile pass | Compile fail | Parser/range | IDE oracle | Notes |
| --- | --- | --- | --- | --- | --- |
| Lexing, comments, literals | Partial | No | Yes | Partial | `comments_whitespace_stability`; parser comments, escapes, numeric, keyword-boundary cases. No dedicated lexical compile-fail matrix. |
| Expressions and operators | Yes | Yes | Yes | Yes | `expr_*`, binary/unary errors plus exact native inference for the script operators. |
| Bindings, mutability, assignment | Yes | Partial | Yes | Yes | Compound assignment codegen and single-evaluation runtime behavior are covered; invalid assignment targets still lack a shared compile-fail. |
| Blocks, if, while, loop | Yes | Yes | Yes | Yes | Includes loop values, compatible `break value`, rejection in `while`, break/continue and fast diagnostics. |
| Option `??` and `?.` | Yes | Yes | Yes | Yes | False-safe/lazy runtime execution plus optional member completion, hover, goto and inference. |
| Membership `in` | Yes | Yes | Yes | Yes | Vec/map/String/range execution, element mismatch, precedence and native bool inference. |
| Typed map literal `#{...}` | Yes | Yes | Yes | Yes | Capacity-aware codegen, mixed-value rejection, formatting and native key/value inference. |
| Functions, returns, recursion | Yes | Yes | Yes | Yes | Zero/typed args, explicit/tail returns, recursion, arity/type errors, fn ranges, hover and symbols. |
| Struct declarations and literals | Yes | Yes | Yes | Yes | Fields, literals, methods, missing/extra fields, struct ranges, member completion and symbols. |
| Enums, match, patterns | Yes | Yes | Yes | Yes | All variant forms, match bindings, constructor/pattern shape errors, match-arm ranges, enum symbols. |
| `Option<T>` | Yes | Yes | Yes | Yes | Constructors, `?`, `??`, `?.`, `expect`, declaration navigation and optional member cursor queries. |
| `Result<T, E>` and `?` | Yes | Partial | Partial | Yes | ABI v2 array tag, `Ok(nil)`/`Err(nil)`, `expect`, methods, map, storage, FFI, hover/goto and successful `?` lowering are covered; error propagation mismatch diagnostics remain incomplete. |
| `Vec<T>` | Yes | Yes | Yes | Yes | Construction/indexing plus `in` execution, mismatch rejection and inference. |
| `HashMap<K, V>` | Yes | Yes | Yes | Yes | Constructor/method and typed literal codegen, key/value inference, `in` and mismatch rejection. |
| Std macros and runtime calls | Yes | No | Partial | No | `println!`, `format!`, and macro nodes appear in range fixtures; misuse diagnostics are missing. |
| Numeric ranges and `for` | Yes | No | Yes | Partial | Both range forms have compiler Lua coverage and the range operator has a semantic-token snapshot; invalid-bound diagnostics and dedicated `for` IDE coverage are missing. |
| Closures | Yes | Yes | Yes | Yes | Expression/typed/block closures, read/fused mutable capture, inference diagnostics, ranges, cursor queries, and semantic tokens are covered. |
| Iterator adapters and fusion | Yes | Yes | Yes | Yes | All Phase 4A sources/adapters/consumers are type/codegen tested; exact Lua goldens enforce fused loops and the IDE snapshot covers item types and adapter tokens. |
| Imports and path modules | Yes | Yes | Yes | Yes | `use`, aliases/grouping, path-derived namespaces and explicit rejection of legacy `mod` syntax. |
| File modules (`.rua`) | Yes | Yes | Yes | Yes | Nested/sibling files, bundle/modules output, runtime execution, source maps and IDE queries. |
| Visibility (`pub`/private) | Yes | Yes | Partial | N/A | Public and same-module private access plus cross-module/private import errors; no dedicated visibility parser range. |
| Extern Lua ABI and variadics | Yes | Partial | Yes | Partial | Plain extern and explicit `lua-result` runtime behavior, parser ranges and `.ruai` hover exist; invalid adapter diagnostics are unit-tested rather than shared goldens. |
| Generic functions and types | Yes | Partial | Yes | No | Identity and generic ADTs covered; rejection coverage concentrates on bounds rather than inference conflicts and no generic-specific IDE snapshot exists. |
| Traits, bounds, and `where` | Yes | Yes | Yes | Yes | Trait impl/default methods, generic method bounds, unknown/unsatisfied bounds, trait/impl ranges and member completion. |
| Methods and receiver forms | Yes | Yes | Yes | Yes | Associated/self/mut-self methods, call errors, receiver parsing, method ranges and completion. |
| Adjacent `.ruai` modules | Yes | Yes | Partial | Yes | Single-file/directory loading, declaration typecheck, codegen skip, hover/goto/completion/references/readonly rename. |
| External `.ruai` library roots | No | No | No | Yes | VFS precedence, configured mounts, project scoping, reload and watcher behavior have exact integration/protocol tests; no compiler golden applies to external LSP configuration. |
| `.ruai` declaration restrictions | Yes | Yes | Partial | Yes | Declaration modules skip codegen; non-empty function/method/trait bodies and executable chunks produce `E0108` in compiler and analysis. |
| Parse/name/type diagnostics | N/A | Yes | Yes | Yes | Exact text plus structured code/file/byte-range/argument manifests, parser recovery and fast IDE diagnostics. |
| Diagnostic codes and precise ranges | N/A | Yes | Yes | Yes | Compiler manifests lock stable codes and ranges; parser range goldens and IDE diagnostic tests cover both pipelines. |
| Completion | N/A | N/A | N/A | Yes | Local, struct member, trait/default member, module path and `.ruai` member completion snapshots. |
| Hover and goto definition | N/A | N/A | N/A | Yes | Local type/function signature, cross-file targets, and `.ruai` signatures. |
| References and rename | N/A | N/A | N/A | Yes | Local/cross-file edits plus `.ruai` declaration references and readonly rejection. |
| Document symbols and docs | N/A | N/A | N/A | Yes | Struct/enum/trait/impl/module hierarchy, ranges, signatures and leading docs. |
| Semantic tokens | N/A | N/A | N/A | Yes | Closure parameter definitions/uses, adapter methods, and range operators have an exact protocol-neutral snapshot plus LSP conversion tests. |
| Inlay hints | N/A | N/A | N/A | Yes | Exact analysis/LSP tests cover primitive, aggregate, tuple, branch and clickable type hints. |
| Formatter/comment stability | Yes | N/A | Yes | N/A | Shared comment/whitespace compile golden, parser trivia corpus, and 11 repository-level formatter goldens. |
| Lua source maps and trace | Yes | No | N/A | N/A | A cross-file golden locks generated slices to exact Rua `FileId` and byte ranges; trace rendering remains tracked separately. |

## Known Gaps

- Range/`for` syntax and Lua lowering are covered; invalid-bound behavior is not
  protected by a repository golden.
- External `.ruai` roots are LSP configuration rather than compiler inputs;
  exact multi-root, mount, reload and watcher protocol tests own this contract.
- Conservative name/call checking intentionally leaves unresolved external
  names, non-callable values, and unknown methods without diagnostics.
- `.ruai` rename rejection currently reuses `RenameError::InvalidName` instead
  of a dedicated readonly/library error.

## Merge Gate

Every new language or IDE feature must update this matrix in the same change:

- New syntax: parser accept/reject plus compile pass or compile fail.
- New type checking: compile pass, compile fail, and a parity note.
- New code generation: byte-exact Lua golden.
- New IDE behavior: at least one cursor-query snapshot.
- New `.ruai` behavior: compiler and IDE goldens.

Changing an existing `Yes` to `Partial` or `No` requires an explicit rationale
in the change description.
