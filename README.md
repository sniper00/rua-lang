# Rua

Rua 是一门采用 Rust 风格语法、编译到可读 Lua 5.5 源码的静态类型脚本语言。它保留 `struct`、`enum`、`trait`、泛型、模式匹配、闭包和 iterator，但不实现所有权、借用检查或生命周期。

```bash
cargo build --release -p ruac -p rua-lsp --features lsp
target/release/ruac build app.rua
```

生成物面向 Lua 5.5。标准运行时集中在单个 [rua_std.lua](crates/rua-resources/resources/std/rua_std.lua)；bundle codegen 最多生成一次 `require("rua_std")`，modules codegen 依赖 Lua 的 `require` cache 共享同一个 runtime package。

## 语言语义

Rua 文件本身就是可执行 chunk，没有特殊入口函数。顶层语句按源码顺序执行；名为 `main` 的函数只是普通函数，不会被编译器自动调用。

```rua
fn answer() -> i64 { 42 }

let value = answer();
println!("answer = {}", value);
```

`Result<T, E>` 是一等 tagged value，而不是 Lua multi-return。`Ok(value)` 和 `Err(error)` 在赋值、参数传递、字段、容器和闭包捕获后仍保留状态；`Ok(nil)` 与 `Err(nil)` 也可区分。`?`、模式匹配和普通返回使用同一表示。

```text
Ok(v)  -> { true,  v }
Err(e) -> { false, e }
```

第一个数组槽是 `is_ok`，第二个是 payload。boolean tag 与 payload 分离，所以
`Ok(nil)` 和 `Err(nil)` 仍可区分，同时避免为每个 Result 分配命名 hash 字段。

只有显式 extern/FFI adapter 可以把 tagged Result 转换成宿主约定。`Option<T>` 当前使用 `T | nil`，因此 `Some(nil)` 不属于可表达状态。

```rua
fn load(path: String) -> Result<String, String> {
    if path == "" { Err("empty path") } else { Ok("config") }
}

let config = load("app.rua")?;
```

常用脚本表达式保持静态类型，并生成显式 Lua 控制流：

```rua
struct Profile {
    city: String,
}

fn choose_city(profile: Option<Profile>, cities: Vec<String>) -> String {
    let mut attempts = 0;
    attempts += 1;

    let city = profile?.city ?? "unknown";
    let aliases: HashMap<String, String> = #{ "sh": "Shanghai" };
    let supported = city in cities && "sh" in aliases;

    loop {
        if supported { break city; }
        break "fallback";
    }
}
```

复合赋值的复杂左值只求值一次；`??` 仅在 `None` 时求值右侧，因而不会把
`Some(false)` 当成缺省；`?.` 的 method 参数也惰性求值。`in` 支持 `Vec`、
`HashMap` key、`String` 子串和 `Iter`/range，`#{...}` 构造有统一键值类型的
`HashMap`。`loop` 可用 `break value` 返回值，`while`/`for` 只允许裸 `break`。

其他核心映射：

| Rua | Lua 运行时表示 |
|---|---|
| `i64` / `f64` | Lua integer / number |
| `bool` | boolean |
| `String` | string |
| `Option<T>` | `T` / `nil` |
| `Result<T, E>` | `{ is_ok, payload }` tagged array table |
| `Vec<T>` | 0-based table，加 `n` 长度 |
| `HashMap<K, V>` | runtime map table |
| `struct` | table + class metatable |
| `enum` | tagged table |

## 模块

Rua 的模块身份直接来自文件路径，不需要也不接受源码级 `mod` 声明。以
`src/main.rua` 为入口时，`src` 是 source root：

```rua
use math::add;

let total = add(3, 4);
```

`src/math.rua` 和 `src/math/mod.rua` 都映射到 `math`，但不能同时存在；
`src/domain/order.rua` 映射到 `domain::order`，中间目录自动形成 namespace。
`.ruai` 使用同一规则。workspace `.rua` 优先于同路径 declaration 和外部库，
同一优先级的重复映射会报歧义，不会静默选取。

`.ruai` 允许空 `{}` 作为 function/method declaration body，但任何非空实现或
顶层 executable statement 都以 `E0108` 拒绝。compiler 与 LSP 共享
`rua-project` 的路径映射；解析阶段为 module、定义、local、type、trait、variant
和 use site 分配稳定 identity，Lua codegen 只消费这些结果。

## Compiler

CLI：

```bash
ruac build src/main.rua                         # 写入 src/main.lua
ruac build src/main.rua -o dist/app.lua
ruac build src/main.rua --emit modules --out-dir dist \
  --lua-path dist --lua-path /path/to/rua_std
ruac check src/main.rua
ruac build src/main.rua --std-path ./path/to/standard-library
ruac build src/main.rua -c ./path/to/.ruarc.toml
```

默认 `--emit bundle` 把完整程序写入一个 Lua 文件。`--emit modules` 为每个
resolved runtime module 生成一个文件：入口保留 root 文件名，
`domain::order` 写入 `domain/order.lua`。每个输出在顶部使用普通 Lua
`require("domain.order")` 加载依赖，并直接返回自己的 module table。可用重复的
`--lua-path <dir>` 把搜索目录写入入口的 `package.path`：

```bash
lua dist/main.lua
```

也可通过 `LUA_PATH` 设置运行环境，或在 `.ruarc.toml` 的
`runtime.lua_path` 中持久配置目录。modules 模式遵循 Lua `require` 的深度优先
初始化顺序；无法用普通 `require` 安全表示的循环模块依赖会在编译期报错，此类工程
可消除依赖环或继续使用 bundle。`--out-dir` 必须显式指定；编译器覆盖本次生成的
文件，但不删除目录中的其他内容。

`ruac` 默认从输入文件目录向上查找最近的 `.ruarc.toml`。项目配置与 LSP
共享，字段使用 snake_case：

```toml
[workspace]
library = ["./types", "../host/moon.ruai"]

[[workspace.lua_library]]
root = "../moon_rs/lualib"

[workspace.library_mounts]
host = "../host/actual-name.ruai"

[runtime]
std_path = "./std"
lua_path = ["./dist", "/opt/rua/lib"]
```

普通 Lua 库优先使用 `workspace.lua_library`。共置目录用 `root`；声明和
runtime 分离时写一次 root pair：

```toml
[[workspace.lua_library]]
declaration_root = "../moon_rs/ruai"
runtime_root = "../moon_rs/lualib"
```

声明按相对路径自动映射：`moon/http/client.ruai` 对应 Rua
`moon::http::client` 和 Lua `require("moon.http.client")`。每个文件型 `.ruai`
是独立 Lua module。compiler 与 LSP 都递归索引 declaration root；runtime root
自动加入生成入口的 `package.path`，因此无需逐模块配置或源码声明。

`workspace.library` 和 `library_mounts` 保留给 declaration-only 路径及特殊逻辑
名称映射。命令行 `--library`、
`--library-mount name=path`、`--std-path` 和可重复使用的
`--lua-path dir` 可以覆盖或补充项目配置。`lua_path` 设置 bundle/modules 生成
入口的 Lua 运行时搜索目录，不参与 Rua/`.ruai` 的编译期 module 解析。

库 API：

```rust
let lua = ruac::compile_str("let value = 42;")?;
let lua = ruac::compile_path(path)?;
let lua = ruac::compile_path_with_std(path, std_root)?;
let lua = ruac::compile_project(&project_spec, &source_provider)?;
let artifact = ruac::compile_project_with_diagnostics(
    &project_spec,
    &source_provider,
)?;
let modules = ruac::compile_project_modules_artifact(&project_spec, &source_provider)?;
```

`compile_project_with_diagnostics` 是完整 host 集成入口，成功值包含 Lua 与 generated-to-Rua source map，失败值 `CompileFailure` 包含 diagnostic code、文件和 byte range。`compile_str`、`compile_path`、`compile_project` 与 artifact convenience API 同样返回结构化失败；只有 CLI 负责渲染展示文字。默认入口使用内嵌标准库；`compile_*_with_std` 和 `--std-path` 显式加载包含 `std.toml` 的目录。`compile_path_artifact` 为文件系统 host 保留 generated-to-Rua source map。`ProjectSpec` 提供稳定 `FileId`、逻辑路径、source root 和 library mount；`SourceProvider` 提供源码。

标准库的 `.ruai` 签名、文档、成员、Lua 包、导出子表、局部别名和可选 ABI 由 `std.toml` 统一描述。声明文件与 `rua_std.lua` 放在同一资源目录；`Vec`、`HashMap`、`Iter`、`String`、`Result`、格式化和整数运算是同一 Lua 包的独立导出。编译器只对 manifest 指定的 `Option` 与 `Result` language item 保留特殊表示。同名用户类型仍是普通 `struct`/`enum`，不会误入标准库 lowering。部署时只需提供一个 `rua_std.lua`。

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

- `rua.server.path`, `rua.server.args`, `rua.compiler.path`,
  `rua.compiler.args`, `rua.trace.server`。

library、module mount 和标准库路径只写入 workspace 根目录的 `.ruarc.toml`，
让 `ruac` 与 `rua-lsp` 使用同一份项目输入。VS Code 设置只控制 compiler、
language server 进程与协议 trace。Explorer 或编辑器中右键 `.rua` 文件可执行
**Rua: Build File**。

workspace/library 扫描支持 `.gitignore`、`.ignore` 和 `.ruaignore`，默认排除 `.git`、`target`、`node_modules`。

## Workspace

```text
crates/rua-core      stable IDs, diagnostics and language contracts
crates/rua-lex       shared lossless token stream
crates/rua-project   IO-free project/source-provider model
crates/rua-resources versioned standard-library manifest, declarations and Lua runtime
crates/ruac          strict compiler and Lua backend
crates/rua-syntax    Rowan CST parser and formatter
crates/rua-analysis  incremental semantic database and IDE queries
crates/rua-lsp       stdio LSP adapter and formatter CLI
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
