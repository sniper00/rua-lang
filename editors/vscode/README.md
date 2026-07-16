# Rua Language — VS Code extension

Editor support for **Rua** (Rust-like syntax that transpiles to Lua 5.5), backed
by the `rua-lsp` language server from this workspace.

## Features

Provided by the `rua-lsp` server (see `crates/rua-lsp`):

- Live diagnostics (type/parse errors)
- Hover, go-to-definition, find references, rename (cross-file)
- Document symbols / outline
- Completion: members (`x.`), paths (`Type::`), globals + keywords
- Document formatting

Plus TextMate syntax highlighting and basic editing config (comments, brackets,
auto-close) from this extension.

## Prerequisites: build the tools

The extension launches `rua-lsp` for editor features and `ruac` for **Rua:
Build File**. Build both first:

The `rua-lsp` binary is behind the `lsp` feature, so pass `--features lsp`:

```bash
# from the rua workspace root
cargo build -p rua-lsp --bin rua-lsp --features lsp             # debug   -> target/debug/rua-lsp
cargo build -p rua-lsp --bin rua-lsp --features lsp --release   # release -> target/release/rua-lsp
cargo build -p ruac                                             # debug   -> target/debug/ruac
cargo build -p ruac --release                                   # release -> target/release/ruac
```

Then point the extension at them, e.g.:

```json
{
  "rua.server.path": "${workspaceFolder}/target/debug/rua-lsp",
  "rua.compiler.path": "${workspaceFolder}/target/debug/ruac"
}
```

If both tools are on `PATH`, the defaults work without configuration.

## Develop / debug

```bash
cd editors/vscode
npm install
npm run compile      # or: npm run watch
npm run test-extension
```

Press **F5** ("Run Rua Extension") to launch an Extension Development Host, then
open any `.rua` / `.ruai` file.

## Package a .vsix

```bash
npm run package      # produces rua-lang-<version>.vsix
```

## Settings

| Setting | Default | Description |
|---|---|---|
| `rua.server.path` | `rua-lsp` | Path to the server (absolute, `${workspaceFolder}`-relative, or on PATH). |
| `rua.server.args` | `[]` | Extra args passed to the server. |
| `rua.compiler.path` | `ruac` | Path to the compiler used by **Rua: Build File**. |
| `rua.compiler.args` | `[]` | Extra args appended to `ruac build <file>`. |
| `rua.trace.server` | `off` | Trace JSON-RPC traffic (`off`/`messages`/`verbose`). |

To make the context-menu command emit a project module tree when it is invoked
on the root source file:

```json
{
  "rua.compiler.args": ["--emit", "modules", "--out-dir", "dist/modules"]
}
```

Configure runtime search directories in `.ruarc.toml`; `ruac` writes them into
the generated root module's `package.path`:

```toml
[runtime]
lua_path = ["./dist/modules", "./lua"]
```

Alternatively set `LUA_PATH` in the host process or append repeated
`--lua-path <dir>` compiler arguments. Child source modules are not independent
compiler entry points; use the command on the project root file.

Restart waits for the server child process `close` event before disposing its
output channel and watcher.

Project libraries should normally be stored in `.ruarc.toml` at the workspace
root so `ruac` and `rua-lsp` consume the same inputs:

```toml
[workspace]
library = ["./types"]

[[workspace.lua_library]]
root = "../moon_rs/lualib"

[workspace.library_mounts]
host = "../host/host.ruai"

[runtime]
std_path = "./std"
lua_path = ["./dist/modules", "./lua"]
```

`workspace.lua_library` is the preferred bulk configuration for ordinary Lua
libraries. With colocated files, `moon/http/client.ruai` automatically describes
`require("moon.http.client")`. For separate trees, use one entry with
`declaration_root` and `runtime_root`; no per-module mapping is needed.

All project fields use snake_case. Library and standard-library inputs are
configured only in `.ruarc.toml`; VS Code settings only control the compiler
and language-server processes plus protocol tracing. The extension watches
`.ruarc.toml` and asks the server to reload it after changes.

## Commands

- **Rua: Build File** (`rua.buildFile`): saves and builds the selected `.rua`
  file. Available from Explorer and editor context menus.
- **Rua: Restart Language Server** (`rua.restartServer`)
