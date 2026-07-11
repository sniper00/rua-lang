# IDE Snapshot Fixtures

Single-file cases pair `<case>.rua` with `<case>.snap`. Cross-file cases keep
their sources under `<case>/workspace/` while the snapshot remains at the IDE
fixture root. Snapshots contain relative paths and byte ranges only.

These queries exercise the public `Analysis` and `Workspace` APIs consumed by
the LSP. The `rua-lsp` test suite separately protects conversion to LSP ranges,
completion items, symbols, locations, and workspace edits.

The `.ruai` hover/goto/completion/references/rename snapshots live under
`tests/golden/ruai/` and are reused as the declaration-file part of the IDE
matrix.

`closure_iterator.snap` is produced by `rua-analysis`; it records inferred
closure parameter types, goto/completion/references/rename, and protocol-neutral
semantic tokens for closure parameters, adapter methods, and range operators.
