# Rua Golden Fixtures

This directory is the repository-level oracle corpus shared by the compiler and
future syntax, analysis, and IDE parity tests. Fixtures live outside individual
crates so their paths and expected results remain stable while implementations
change.

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
```

Missing expected files and byte mismatches fail the test and print the explicit
update command. Ordinary tests never create or overwrite expected output.

To accept an intentional compiler-output change, review the diff produced by:

```sh
RUA_UPDATE_GOLDENS=1 cargo test -p ruac --test golden update_goldens -- --ignored --exact
```

The update test is both ignored and guarded by `RUA_UPDATE_GOLDENS=1`; either
mechanism alone is insufficient to write files.

## Assertions

- Compile-pass output is the byte-exact result of `ruac::compile_path`.
- Compile-fail output is the exact compiler error with the fixture root replaced
  by `<golden>` so snapshots do not depend on an absolute checkout path.
- A compile-pass case that fails, or a compile-fail case that succeeds, always
  fails even in update mode.
- Golden files are updated only by the explicit command above and must be
  reviewed like source code.
