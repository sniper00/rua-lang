# Rua Annotation

Rua annotation 是具有 declaration identity、target 集合、参数 schema 和 retention
策略的结构化 metadata。它参与 compiler artifact、LSP 语义查询以及可选的 Lua
runtime registry，不执行 token rewrite 或 AST rewrite。

## 1. 声明与使用

```rua
#[targets(function, struct)]
#[retention(runtime)]
pub annotation Route(method: String, path: String);

#[Route(method = "GET", path = "/health")]
pub fn health() -> String { "ok" }
```

annotation declaration 必须包含 `#[targets(...)]`。schema attribute：

| Attribute | 含义 |
|---|---|
| `targets(...)` | 合法 target 集合 |
| `retention(source)` | 只存在于 source semantic view |
| `retention(build)` | 进入 compiler `AnnotationIndex` 和 metadata artifact |
| `retention(runtime)` | 进入 artifact 与 Lua runtime registry |
| `repeatable` | 同一 target 可重复使用 |

未写 `retention` 时使用 `build`。未写 `repeatable` 时，同一 annotation 在同一 target
上最多出现一次。

合法 target 名称为 `struct`、`enum`、`function`、`method`、`field`、`variant` 和
`extern_function`。runtime retention 要求 target 是 public 且具有稳定 backend
locator；function、struct、enum、variant、struct/enum method、field 和 variant field
可进入 runtime registry。

## 2. 参数

schema 参数支持：

- `String`、`bool`、`i64`、`f64`。
- 用户 enum 类型及其 variant。
- `Vec<T>` / `List<T>` 同构列表。

参数按名称传递，全部为必需参数：

```rua
enum Format { Json, MessagePack }

#[targets(struct)]
annotation Codec(formats: Vec<Format>);

#[Codec(formats = [Format::Json, Format::MessagePack])]
pub struct Message {}
```

参数值必须是可序列化的 compile-time value。function call、closure、map、用户对象和
运行时变量不属于 annotation value。compiler 与 native analysis 检查缺失参数、未知
参数、重复参数、类型不匹配、target 不匹配和重复使用。

## 3. cfg 顺序

attribute 处理顺序为：

1. 解析 meta item。
2. 展开 `cfg_attr`。
3. 求值 `cfg` 并构造 active view。
4. 解析 active annotation identity。
5. 校验 schema、target 与参数。
6. 建立 `AnnotationIndex`。

inactive target 不进入 compiler/LSP annotation index，也不产生 runtime metadata。
条件语法与 project 配置见 [Attribute 与条件编译](rua-attributes.md)。

## 4. Compiler artifact 与 CLI

resolved `AnnotationIndex` 以 `DefId`/`AnnotationTarget` 建立双向索引，记录 canonical
annotation name、validated arguments 和 source order。IO-free host 直接从 compiler
artifact 查询；CLI 可以输出 TOML：

```bash
ruac metadata src/main.rua --format toml
ruac metadata src/main.rua \
  --annotation moon::web::Route --format toml
```

metadata 输出不依赖 terminal diagnostic 文本，也不执行第三方 processor。

## 5. Lua runtime registry

modules 输出为整个构建生成一个聚合文件：

```text
dist/main.lua
dist/api/users.lua
dist/rua_annotations.lua
```

`rua_annotations.lua` 按 source order 保存 canonical locator：

```lua
local entries = {
    {
        annotation = "moon::web::Route",
        target = {
            module = "api.users",
            kind = "function",
            path = { "list_users" },
        },
        args = { method = "GET", path = "/users" },
    },
}

local M = require("rua_std").annotations
M.set_entries(entries)
return M
```

加载 registry 只注册 locator，不执行应用 module。`Annotations::load` 使用 locator 的
module/path 惰性加载 target，并遵守 Lua `require` cache。没有 runtime-retained
instance 时不生成 `rua_annotations.lua`。

bundle 输出把等价记录直接注册到 `rua_std.annotations`，target locator 可以持有同一
chunk 中的 Lua value。两种输出包含相同的 canonical name、参数与 source order。

Rua 查询 API：

```rua
let routes = Annotations::find("moon::web::Route");
for route in routes {
    let handler = Annotations::load(route);
    install_route(route, handler);
}
```

## 6. IDE

native analysis 为 annotation declaration、application 和 schema parameter 建立稳定
identity。LSP 支持 target-aware completion、参数 completion、hover、goto definition、
references、rename 和参数诊断；inactive annotation 使用 cfg semantic modifier。

annotation declaration 的文档、targets、retention 和 repeatable 状态进入 hover 与
completion resolve。查找与重命名按 identity 工作，不扫描同名文本。
