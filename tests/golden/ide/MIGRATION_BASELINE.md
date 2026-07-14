# Native Analysis Coverage

The legacy `rua-syntax` semantic facade and its three-state migration runner
were deleted after the native vertical path became authoritative. Current
coverage is owned by protocol-neutral `rua-analysis` tests and `rua-lsp`
adapter/protocol tests; no query may fall back to a compiler-backed workspace.

## Query Ownership

| Query | Native owner | Coverage |
| --- | --- | --- |
| Completion | `Analysis::completions` | local, member, path, enum, builtin macro, `.ruai` |
| Hover | `Analysis::hover` | local, function, method, extern, docs, macro, enum variant |
| Go to definition | `Analysis::goto_definition` | local, cross-file, member, enum, readonly `.ruai` |
| References | `ReferenceIndex` | local, item, member, cross-file, declaration inclusion |
| Rename | `Analysis::rename` | atomic edits, conflicts, readonly rejection, enum variants |
| Symbols | native DefMap/ItemTree | document and multi-project workspace symbols |
| Diagnostics | native parse/HIR/inference | structured code/range/origin |
| Semantic tokens | native body/definition model | declarations, locals, members, closures, macros |

## Output Contract

- Core ranges are UTF-8 byte ranges; UTF-16 conversion belongs to the LSP adapter.
- Project-sensitive queries require an explicit `ProjectId` context.
- Results are deterministically sorted and de-duplicated by their protocol-neutral identities.
- `.ruai` definitions participate in semantic queries and are read-only rename targets.
- Completion resolve, hover, goto, references, rename, and hierarchy consume the same
  definition/member/reference identities rather than rebuilding name tables.
- The closure/iterator snapshot is generated entirely from native analysis.

Compiler parity remains in the direct `ruac` oracle tests under
`crates/rua-analysis/tests/{parity,inference,member_inference}.rs`. Parser and
range parity remain in `crates/rua-syntax/tests/{parser_goldens,range_conformance}.rs`.
