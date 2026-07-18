# Rua Artifact

`ruac` can persist generated Lua together with the source map required to
translate Lua diagnostics back to Rua locations. The artifact format is JSON,
versioned, and independent of the compiler process.

## Bundle

```bash
ruac build src/main.rua -o dist/main.lua
```

This writes:

```text
dist/main.lua
dist/main.lua.rua-map.json
```

The sidecar records the schema version, compiler version, `rua_std` ABI,
generated-source hash, source-file table, and generated-to-Rua mappings.

## Modules

```bash
ruac build src/main.rua --emit modules --out-dir dist/modules
```

This writes one Lua file per runtime module and one shared manifest:

```text
dist/modules/main.lua
dist/modules/presentation/console.lua
dist/modules/rua-artifact.json
```

The manifest uses paths relative to its own directory. A runtime must reject
an artifact when its schema or runtime ABI is unknown, when a path escapes the
artifact directory, or when a generated Lua file no longer matches its stored
hash.

## Host loading

Hosts should use the unified `RuaArtifact`/manifest contract rather than
reimplementing bundle and modules handling. `moon_rs` automatically discovers
`<entry>.rua-map.json` or `rua-artifact.json` when launched with a generated
`.lua` entry. A plain Lua file without either marker keeps the existing Lua
filesystem loader.

The source map is intended for diagnostics and future debugger mappings. It is
not an authenticity signature; artifact distribution still needs the host's
normal trust and integrity controls.
