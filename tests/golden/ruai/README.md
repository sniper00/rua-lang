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

External library-root configuration, explicit out-of-tree mounts, and a
separate library-over-std priority layer are not implemented yet. These
fixtures deliberately do not simulate those layers inside the test harness.
Accordingly, `library_mount_single_file` currently covers an adjacent
`name.ruai` module, while `workspace_shadows_library` covers the implemented
`.rua`-before-`.ruai` candidate order rather than source-root precedence.
