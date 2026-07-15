# Rua 语言与运行时设计

Rua 是一门采用 Rust 风格语法、编译到可读 Lua 5.5 源码的静态类型脚本语言。它提供结构化数据、模式匹配、trait、泛型、闭包和 iterator，但不实现所有权、借用检查和生命周期。

本文描述当前语言和对外实现契约。工具链内部结构见[工具链架构](rua-architecture.md)。

## 1. 执行模型

Rua 文件本身就是可执行 chunk，没有特殊入口函数。声明与语句可以在文件和 inline module 中按源码顺序混排：

```rua
fn answer() -> i64 { 42 }

let value = answer();
println!("answer = {}", value);
```

名为 `main` 的函数只是普通函数，不会自动执行。compiler 先为声明分配 identity，再按源码顺序执行初始化，因此前向引用和互递归不会改变可观察副作用的顺序。root chunk 完成后返回 public export table。

## 2. 类型与数据

| Rua 类型 | Lua 表示 |
|---|---|
| `i64` / `f64` | integer / number |
| `bool` | boolean |
| `String` | string |
| `()` | `nil` / no value |
| tuple | positional table |
| `Option<T>` | `T` 或 `nil` |
| `Result<T, E>` | tagged runtime value |
| `Vec<T>` | 0-based table，加显式长度 `n` |
| `HashMap<K, V>` | runtime map |
| struct | table + class metatable |
| enum | tagged table |
| closure / function | Lua function |

类型在编译期检查，在 Lua 后端擦除。Rua 支持 mutable binding、struct field、unit/tuple/struct enum variant、trait/impl、泛型约束、模式匹配、闭包捕获和 iterator adapter；这些能力不隐含 Rust 的 move、borrow 或 lifetime 语义。

### 2.1 Result ABI

`Result<T, E>` 在 Rua 内部始终只产生一个 Lua value：

```text
Ok(v)  = { __rua_result = true, tag = "ok",  value = v }
Err(e) = { __rua_result = true, tag = "err", value = e }
```

赋值、参数、返回值、字段、容器、闭包、嵌套 Result、`?` 和 pattern 都使用同一表示。tag 与 payload 分离，所以 `Ok(nil)` 和 `Err(nil)` 可区分。用户定义的同名 `Result` 不会触发 builtin lowering。

`Option<T>` 则使用 `T | nil`，`Some(v)` 擦除为 `v`，`None` 为 `nil`。它与 Result 是两个独立 ABI。

## 3. 函数、闭包与 iterator

函数和闭包都可以作为一等值传递。闭包支持表达式体、block body、参数/返回类型和 lexical capture：

```rua
let offset = 2;
let add = |x: i64| -> i64 { x + offset };
```

iterator 是惰性、可组合的运行时协议。range、`Vec` 和 adapter 可以进入 `for`，并支持 `map`、`filter`、`fold`、`find`、`any`、`all`、`count` 和 `collect`。compiler 可以融合已知 adapter 链，但优化前后必须保持相同的可观察语义。

标准 iterator 同时是一等 runtime value：它可以赋值、返回、传参和分阶段组合。`String::chars` 按 UTF-8 Unicode scalar 迭代，不按字节拆分。

## 4. 模块与接口文件

Rua 支持 inline module、文件 module、`use`、可见性和只读 library mount。文件 module 的候选顺序固定为：

1. `name.rua`
2. `name/mod.rua`
3. `name.ruai`
4. `name/mod.ruai`

同时存在多个候选会产生歧义错误，不会静默选择。logical path 不得逃离 project source root。

`.ruai` 是 declaration-only 接口文件。函数和 method 可用空 `{}` 表示声明，trait signature 也可使用 `;`；以下内容产生 `E0108`：

- 非空函数、impl method 或 trait method body。
- 文件顶层 executable statement。
- inline module 中的 executable statement。

compiler module loader、IO-free project API 和 native analysis 对该规则保持一致。

### 4.1 标准库配置

默认标准库随 `rua-resources` 内嵌。自定义标准库目录必须包含 `std.toml`，其中显式列出 declaration 文件、runtime source、language item、Lua 包、导出子表、局部别名和可选 ABI。`.ruai` 是类型签名、文档、completion、hover 和 goto definition 的唯一来源；runtime binding 只决定已解析标准定义如何连接到 Lua 导出。

`Option` 的 nullable 表示和 `Result` 的 tagged 表示需要 compiler 参与，因此由 `[lang_items]` 指定。`Vec`、`HashMap`、`Iter` 与 `String` 没有专用构造器表或成员表，它们通过 declaration 和单个 `rua_std` Lua 包的普通导出实现；Result 的 tagged 构造也由 `result` 导出提供。codegen 对该包只执行一次 `require` 和 ABI 检查，并按实际引用生成 `vec`、`map` 等局部别名。用户类型不需要修改 `std.toml`：直接声明 `struct`、`enum`、`trait` 和 `impl` 即可；名称恰好相同也不会获得 language item 语义。

## 5. Extern 与宿主边界

普通 Lua 函数使用单值 ABI：

```rua
extern "lua" {
    fn clock() -> f64;
}
```

Lua 常见的 `(value, nil)` / `(nil, error)` 约定必须显式声明为 `lua-result`：

```rua
extern "lua-result" {
    fn read_value(key: String) -> Result<String, String>;
}
```

adapter 在边界把 multi-return 转成 Rua tagged Result，并把 Result 参数反向展开。该 ABI 要求 function 非 variadic，且返回类型解析到 builtin `Result<T, E>` identity。普通 `extern "lua"` 不会根据类型名称猜测转换方式。

`std.toml` 中 runtime module 的可选 `abi` 是 compiler 与该 Lua 模块的硬契约。生成物只 require 实际使用的模块，并逐模块检查 `ABI_VERSION`；自定义库未声明 `abi` 时只生成 require。

## 6. Compiler API

推荐的宿主入口是 IO-free project API：

```rust
let artifact = ruac::compile_project_with_diagnostics(
    &project_spec,
    &source_provider,
)?;
```

`ProjectSpec` 提供 root、source root、library mount、stable `FileId` 和 logical path；`SourceProvider` 提供源码。成功 artifact 包含 Lua source 和 generated-to-Rua source map，失败 `CompileFailure` 包含稳定 diagnostic code、文件、byte range 和命名参数。

`compile_str`、`compile_path`、`compile_project` 与 artifact convenience API 使用同一结构化失败类型。只有 CLI adapter 把诊断渲染为终端文字；`compile_path_with_std` 和 `--std-path` 是显式标准库文件系统入口，未指定时使用内嵌标准库。

## 7. 稳定契约

以下行为属于兼容边界：

- runtime ABI version、Result/Option/container 表示和 FFI adapter。
- public export key、module 初始化顺序和顶层副作用顺序。
- diagnostic code 与 source range 的语义。
- source map 对 generated span、source file 和 byte range 的关联。

human diagnostic wording、Lua 临时变量名和 printer 的非语义排版可以演进。任何 ABI 或运行时表示变更都必须增加真实 Lua execution test；任何 grammar 变更都必须同时覆盖 strict parser、IDE parser 和 range conformance。
