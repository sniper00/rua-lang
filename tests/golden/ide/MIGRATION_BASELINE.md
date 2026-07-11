# Native Analysis Migration Baseline

This file records the user-visible behavior frozen by Step 4B.0. The executable
fixture inventory and three-state normalizer live in
`crates/rua-analysis/tests/migration_baseline.rs` and its test-only support
module. Existing snapshot runners remain the owners of snapshot updates.

## Native status

| Query | Legacy oracle | Step 4B.0 native status | Removal step |
| --- | --- | --- | --- |
| Completion | local/member/path, `.ruai`, closure snapshots | `Unsupported` | 4B.9 |
| Hover | item/local and `.ruai` signature snapshots | `Unsupported` | 4B.9 |
| Go to definition | local/cross-file/`.ruai`/closure snapshots | `Unsupported` | 4B.9 |
| References | local/cross-file/`.ruai`/closure snapshots | `Unsupported` | 4B.9 |
| Rename | local/cross-file/readonly `.ruai`/closure snapshots | `Unsupported` | 4B.9 |
| Document symbols | `document_symbols.snap` | `ExpectedDifference(native-document-symbol-members)` | 4B.9 |
| Diagnostics | compiler IDE, `.ruai`, and Phase 4A oracles | `Unsupported` for type diagnostics | 4B.8 |
| Semantic tokens | closure/iterator snapshot | `Equal` | n/a |

`Unsupported` is a typed test result, not an empty list or `None`. An expected
difference must name both its reason and the step that removes it. Any other
difference fails the migration baseline.

## Frozen output contract

- All stored paths are workspace-relative and all stored ranges are UTF-8 byte
  ranges. UTF-16 conversion belongs to the LSP adapter.
- Completion has strict member, path, then global priority. A recognized member
  or path context suppresses global results even when its result is empty.
- Shared member-completion snapshots sort all candidates by name. The LSP's
  broader built-in member path may group fields before methods; protocol tests
  freeze that adapter-specific order. Other completion output preserves its
  documented source/fixed order and uses first-result-wins de-duplication.
- References are sorted by relative file and byte start, then de-duplicated.
  Rename edits are normalized by relative file and byte start.
- Document symbols preserve source traversal order. Their full range covers the
  declaration and their selection range covers the name token.
- Diagnostics and semantic tokens preserve deterministic byte-range order.
  Semantic tokens are de-duplicated by range and kind.
- `.ruai` definitions participate in hover, navigation, completion, and
  references. Rename is rejected if any edit touches a declaration file.
- The current readonly rename snapshot exposes legacy `InvalidName`. This is a
  deliberately frozen defect; Step 4B.9 replaces it with a dedicated readonly
  error and updates the oracle explicitly.

Formatting remains owned by `rua-syntax` and is protected by its byte-exact
formatter goldens. Prepare-rename is part of the rename protocol adapter; the
LSP baseline test fixes its current range-or-null behavior, and Step 5.0 adds
the complete protocol snapshot.

The Phase 4A inventory contributes 12 compiler-accepted sources and 9 rejected
sources. Accepted closure/iterator type cases remain unsupported until Step
4B.7; rejected diagnostic cases remain unsupported until Step 4B.8.
