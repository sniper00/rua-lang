# `.ruai` Golden Fixtures

Each case keeps project sources under `workspace/`. Compiler cases pair
`workspace/main.rua` with `main.lua.golden` or `main.diag.golden`; IDE cases
store their query result in `result.ide.golden` at the case root.

The current production loader resolves declaration modules adjacent to the
workspace through `mod name;`, using this order:

```text
name.rua
name/mod.rua
name.ruai
name/mod.ruai
```

Declaration files may use an empty `{}` for function and method signatures.
Non-empty bodies and executable file/module statements are rejected with
`E0108` by both the compiler and native analysis.

The `rua-analysis` VFS and LSP input layer now support external library roots,
explicit out-of-tree mounts, read-only inputs, and workspace > library > std
priority. Those behaviors are owned by exact integration/protocol tests rather
than the retired legacy IDE snapshot runner. Accordingly, `library_mount_single_file` covers
an adjacent `name.ruai` module, while `workspace_shadows_library` covers the
compiler's `.rua`-before-`.ruai` candidate order.
