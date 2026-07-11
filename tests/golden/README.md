# Rua Golden Fixtures

This directory is the repository-level oracle corpus shared by the compiler and
future syntax, analysis, and IDE parity tests. Fixtures live outside individual
crates so their paths and expected results remain stable while implementations
change.

The current feature-by-feature status and known gaps are tracked in
[`COVERAGE.md`](COVERAGE.md). Every new language or IDE feature must update that
matrix in the same change as its fixtures.

## Layout

```text
compile-pass/       single-file `.rua` -> `.lua.golden`
compile-fail/       rejected `.rua` -> `.diag.golden`
parser/accept/      sources both parsers must accept
parser/reject/      sources the compiler parser must reject
parser/ranges/      token and text-range snapshots
modules/            multi-file compiler fixtures
ruai/               declaration and external-library fixtures
ide/                completion, hover, navigation, and rename snapshots
phase4a/            active closure and fused-iterator compiler goldens
```

Use lower snake case for case names and keep one primary behavior per case.
Single-file pairs use these names:

```text
compile-pass/<case>.rua
compile-pass/<case>.lua.golden
compile-fail/<case>.rua
compile-fail/<case>.diag.golden
```

Multi-file cases use their own directory with `main.rua` as the entry point.
`.ruai` cases may additionally contain `workspace/`, `library/`, and `std/`
roots. The detailed case matrix is in `docs/rua-golden-cases.md`.

## Running

The default harness is read-only:

```sh
cargo test -p ruac --test golden
```

Focused compiler checks are available as:

```sh
cargo test -p ruac --test golden golden_compile_pass
cargo test -p ruac --test golden golden_compile_fail
cargo test -p ruac --test golden golden_ruai
cargo test -p ruac --test golden phase4a_golden
```

The shared parser corpus and CST byte-range snapshots are checked with:

```sh
cargo test -p rua-syntax --test parser_goldens parser_conformance -- --exact
cargo test -p rua-syntax range_conformance
cargo test -p rua-syntax --test parser_goldens range_golden -- --exact
```

Declaration-file IDE behavior has its own snapshot suite:

```sh
cargo test -p rua-syntax --test ruai_goldens ruai_ide_golden -- --exact
```

The general IDE query snapshots are checked with:

```sh
cargo test -p rua-syntax --test ide_goldens ide_snapshot_golden -- --exact
cargo test -p rua-analysis --test closure_iterator_ide closure_iterator_ide_golden -- --exact
```

Missing expected files and byte mismatches fail the test and print the explicit
update command. Ordinary tests never create or overwrite expected output.

To accept an intentional compiler-output change, review the diff produced by:

```sh
RUA_UPDATE_GOLDENS=1 cargo test -p ruac --test golden update_goldens -- --ignored --exact
```

Range snapshots have a separate guarded update target:

```sh
RUA_UPDATE_GOLDENS=1 cargo test -p rua-syntax --test parser_goldens update_parser_range_snapshots -- --ignored --exact
```

The `.ruai` IDE snapshots use the same guard:

```sh
RUA_UPDATE_GOLDENS=1 cargo test -p rua-syntax --test ruai_goldens update_ruai_ide_snapshots -- --ignored --exact
```

General IDE snapshots also require an explicit guarded update:

```sh
RUA_UPDATE_GOLDENS=1 cargo test -p rua-syntax --test ide_goldens update_ide_snapshots -- --ignored --exact
RUA_UPDATE_GOLDENS=1 cargo test -p rua-analysis --test closure_iterator_ide update_closure_iterator_ide_golden -- --ignored --exact
```

All update tests are ignored and guarded by `RUA_UPDATE_GOLDENS=1`; either
mechanism alone is insufficient to write files.

## Assertions

- Compile-pass output is the byte-exact result of `ruac::compile_path`.
- Compile-fail output is the exact compiler error with the fixture root replaced
  by `<golden>` so snapshots do not depend on an absolute checkout path.
- Parser accept/reject sources must produce the same outcome in the compiler and
  CST parsers; every CST parse must remain lossless, including rejected input.
- Range output records every CST node and non-trivia token with its exact byte
  range. `parser/ranges/<case>.rua` pairs with `<case>.range.golden`.
- `.ruai` compiler fixtures prove declarations participate in checking but are
  skipped by codegen. IDE snapshots cover completion, hover/goto, references,
  and read-only rename behavior.
- General IDE snapshots cover local/member/path completion, local and
  cross-file hover/goto/references/rename, diagnostics, and document symbols.
- The Phase 4A IDE snapshot covers inferred closure parameters, cursor queries,
  semantic tokens for parameters/adapters/ranges, and fast diagnostic stability.
- `COVERAGE.md` records direct golden evidence separately from unit-test
  coverage and keeps unsupported or partially covered behavior explicit.
- A compile-pass case that fails, or a compile-fail case that succeeds, always
  fails even in update mode.
- Golden files are updated only by the documented explicit commands and must be
  reviewed like source code.
