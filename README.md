# Rua

Rua is the standalone repository for the Rua compiler, syntax tooling, IDE
analysis, LSP server, runtime declarations, and language fixtures.

This repository was split from `moon_rs`. The migration follows
`docs/rua-construction-plan.md`, with the IDE/LSP target architecture described
in `docs/rua-ide-architecture.md`.

Initial crate import source: `moon_rs` at
`96a40b9ae8bce122fa7a2b32b745b7f3a51bd516`, including the Rua-related working
tree changes present at import time.

The repository split, analysis architecture baseline, and Phase 4A closure and
fused-iterator implementation in `docs/rua-construction-plan.md` are complete.
