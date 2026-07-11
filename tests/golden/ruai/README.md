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

The `rua-analysis` VFS and LSP input layer now support external library roots,
explicit out-of-tree mounts, read-only inputs, and workspace > library > std
priority. Those behaviors have focused integration tests but not a shared
disk-layout golden yet. Accordingly, `library_mount_single_file` still covers
an adjacent `name.ruai` module, while `workspace_shadows_library` covers the
compiler's `.rua`-before-`.ruai` candidate order.
