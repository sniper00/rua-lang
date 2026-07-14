# Rua

Rua 是一门采用 Rust 风格语法、编译到可读 Lua 源码的静态类型脚本语言。它保留 `struct`、`enum`、`trait`、泛型、模式匹配、闭包和 iterator，但不实现所有权、借用检查或生命周期。

```bash
cargo build --release -p ruac -p rua-lsp --features lsp
target/release/ruac build app.rua
```

生成物面向 Lua 5.4/5.5，并在使用运行时能力时依赖 [rua_rt.lua](lualib/rua_rt.lua)。

## 语言语义

Rua 文件本身就是可执行 chunk，没有特殊入口函数。顶层语句按源码顺序执行；名为 `main` 的函数只是普通函数，不会被编译器自动调用。

```rua
fn answer() -> i64 { 42 }

let value = answer();
println!("answer = {}", value);
```

`Result<T, E>` 是一等 tagged value，而不是 Lua multi-return。`Ok(value)` 和 `Err(error)` 在赋值、参数传递、字段、容器和闭包捕获后仍保留状态；`Ok(nil)` 与 `Err(nil)` 也可区分。`?`、模式匹配和普通返回使用同一表示。

```text
Ok(v)  -> { __rua_result = true, tag = "ok",  value = v }
Err(e) -> { __rua_result = true, tag = "err", value = e }
```

只有显式 extern/FFI adapter 可以把 tagged Result 转换成宿主约定。`Option<T>` 当前使用 `T | nil`，因此 `Some(nil)` 不属于可表达状态。

```rua
fn load(path: String) -> Result<String, String> {
    if path == "" { Err("empty path") } else { Ok("config") }
}

let config = load("app.rua")?;
```

其他核心映射：

| Rua | Lua 运行时表示 |
|---|---|
| `i64` / `f64` | Lua integer / number |
| `bool` | boolean |
| `String` | string |
| `Option<T>` | `T` / `nil` |
| `Result<T, E>` | 带 `tag` 与 `value` 的 runtime value |
| `Vec<T>` | 0-based table，加 `n` 长度 |
| `HashMap<K, V>` | runtime map table |
| `struct` | table + class metatable |
| `enum` | tagged table |

## 模块

Rua 支持 inline module、文件 module、`.ruai` declaration 和可见性检查。

```rua
mod math;
use math::add;

let total = add(3, 4);
```

文件 module 按确定顺序查找：`name.rua`、`name/mod.rua`、`name.ruai`、`name/mod.ruai`。同时存在多个候选会报歧义，不会静默选取。`.ruai` 允许空 `{}` 作为 function/method declaration body，但任何非空实现或顶层 executable statement 都以 `E0108` 拒绝。解析阶段为 module、定义、local、type、trait、variant 和 use site 分配稳定 identity；Lua codegen 只消费这些解析结果。

## Compiler

CLI：

```bash
ruac build src/main.rua                         # 写入 src/main.lua
ruac build src/main.rua -o dist/app.lua
ruac check src/main.rua
ruac build src/main.rua --builtins-dir ./sysroot
```

库 API：

```rust
let lua = ruac::compile_str("let value = 42;")?;
let lua = ruac::compile_path(path)?;
let lua = ruac::compile_project(&project_spec, &source_provider)?;
let artifact = ruac::compile_project_with_diagnostics(
    &project_spec,
    &source_provider,
)?;
```

`compile_project_with_diagnostics` 是完整 host 集成入口，成功值包含 Lua 与 generated-to-Rua source map，失败值 `CompileFailure` 包含 diagnostic code、文件和 byte range。`compile_str`、`compile_path`、`compile_project` 与 artifact convenience API 同样返回结构化失败；只有 CLI 负责渲染展示文字。`compile_path_artifact` 为文件系统 host 保留 generated-to-Rua source map。`ProjectSpec` 提供稳定 `FileId`、逻辑路径、source root 和 library mount；`SourceProvider` 提供源码。project API 使用内嵌 sysroot，不读取磁盘、不依赖 CWD；`compile_path` 才是文件系统 adapter。

编译器主链：

```text
rua-lex tokens
  -> strict parser / owned AST
  -> module collection + resolved HIR identities
  -> structural check + ID-keyed type facts
  -> backend layout
  -> structured Lua IR
  -> Lua printer
```

## IDE

Rua 长期保留两套 parser：

- `ruac` 使用 fail-fast strict parser，适合编译器和嵌入 host。
- `rua-syntax` 使用 error-tolerant Rowan parser，保留 trivia 和错误节点，适合编辑中的源码。

两者共享 `rua-lex`、`rua-core`、`rua-project`、grammar corpus 和 range conformance 测试，但不共享 AST、错误恢复或语义实现。`rua-analysis` 是独立的增量语义引擎，不调用 compiler AST/typeck 作为 production fallback。

`rua-lsp` 支持 hover、goto definition/implementation、completion、references、atomic rename、call/type hierarchy、inlay hints、diagnostics、semantic tokens、code actions、symbols、folding 和 formatting。函数、方法、trait method、extern、`.ruai` declaration 与 builtin inline macro 的文档来自统一 semantic record。enum variant 在构造、限定路径、alias 和 pattern 中使用同一 identity。

VS Code 设置：

- `rua.library`: `.ruai` 文件或目录列表。
- `rua.libraryMounts`: logical module name 到 `.ruai` 文件/目录的映射。
- `rua.sysroot`: 可选 sysroot 路径。
- `rua.server.path`, `rua.server.args`, `rua.trace.server`。

workspace/library 扫描支持 `.gitignore`、`.ignore` 和 `.ruaignore`，默认排除 `.git`、`target`、`node_modules`。

## Workspace

```text
crates/rua-core      stable IDs, diagnostic and builtin contracts
crates/rua-lex       shared lossless token stream
crates/rua-project   IO-free project/source-provider model
crates/ruac          strict compiler and Lua backend
crates/rua-syntax    Rowan CST parser and formatter
crates/rua-analysis  incremental semantic database and IDE queries
crates/rua-lsp       stdio LSP adapter and formatter CLI
lualib/rua_rt.lua    versioned runtime ABI
```

## 验证

```bash
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets --all-features
cargo test --locked --workspace --all-features
cargo clippy --locked --workspace --all-targets --all-features --no-deps -- -D warnings
bash scripts/check-boundaries.sh
(cd editors/vscode && npm run check-types && npm run test-extension)
```

CI 固定构建并校验 Lua 5.5.0；compiler compile-pass golden 在比较 Lua snapshot 后还会真实执行生成物，compile-fail golden 另行锁定结构化 code/file/range/arguments。两个 parser 通过 arbitrary-Unicode property test 验证无 panic、lossless CST 和合法错误 range。

文档入口见 [docs/README.md](docs/README.md)，其中包括[语言与运行时设计](docs/rua-design.md)、[工具链架构](docs/rua-architecture.md)和[当前 LSP 功能](docs/rua-lsp-features.md)。

## License

MIT
