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

## Prerequisites: build the server

The extension launches the `rua-lsp` binary; build it first:

The `rua-lsp` binary is behind the `lsp` feature, so pass `--features lsp`:

```bash
# from the rua workspace root
cargo build -p rua-lsp --bin rua-lsp --features lsp             # debug   -> target/debug/rua-lsp
cargo build -p rua-lsp --bin rua-lsp --features lsp --release   # release -> target/release/rua-lsp
```

Then point the extension at it via the `rua.server.path` setting, e.g.:

```json
{ "rua.server.path": "${workspaceFolder}/target/debug/rua-lsp" }
```

If `rua-lsp` is on your `PATH`, the default (`rua-lsp`) just works.

## Develop / debug

```bash
cd editors/vscode
npm install
npm run compile      # or: npm run watch
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
| `rua.trace.server` | `off` | Trace JSON-RPC traffic (`off`/`messages`/`verbose`). |

## Commands

- **Rua: Restart Language Server** (`rua.restartServer`)
