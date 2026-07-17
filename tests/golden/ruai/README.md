# `.ruai` Golden Fixtures

Each case keeps project sources under `workspace/`. Compiler cases pair
`workspace/main.rua` with `main.lua.golden` or `main.diag.golden`; IDE cases
store their query result in `result.ide.golden` at the case root.

The production loader maps declarations from their path without source-level
module declarations. These layouts represent the same logical module:

```text
name.rua
name/mod.rua
name.ruai
name/mod.ruai
```

Declaration files may use an empty `{}` for function and method signatures.
Non-empty bodies and executable file statements are rejected with
`E0108` by both the compiler and native analysis.

The `rua-analysis` VFS and LSP input layer support external library roots,
explicit out-of-tree mounts, read-only inputs, and workspace > library > std
priority. Exact integration and protocol tests own those behaviors.
`library_mount_single_file` covers an adjacent `name.ruai` module, while
`workspace_shadows_library` covers workspace source precedence over a library
declaration with the same logical path.
