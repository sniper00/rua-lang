# Rua 文档

本目录描述 Rua 当前语言、编译器、运行时和 IDE 契约。

## 文档入口

- [语言与运行时设计](rua-design.md)：执行模型、类型表示、表达式、模块、extern、标准库与 Lua 输出。
- [Attribute 与条件编译](rua-attributes.md)：`#[cfg]`、`#[cfg_attr]`、project 配置和 inactive 语义。
- [Annotation](rua-annotations.md)：schema、target、retention、compiler index、聚合 runtime registry 和 IDE 能力。
- [工具链架构](rua-architecture.md)：crate 边界、双 parser、compiler、增量 analysis、LSP 生命周期和验证门禁。
- [LSP 功能](rua-lsp-features.md)：编辑器协议能力、语义保证、workspace 行为和 VS Code 配置。
- [Lua 堆栈转换](rua-stacktrace.md)：Lua traceback 解析、source map 映射和未映射 runtime frame 处理。
- [Rua Artifact](rua-artifact.md)：版本化 source-map sidecar、bundle/modules manifest 和跨进程加载契约。

测试 fixture 的规则和覆盖矩阵：

- [Golden 测试说明](../tests/golden/README.md)
- [Golden 覆盖矩阵](../tests/golden/COVERAGE.md)

## 当前特点

- Rua 使用静态类型和 Rust 风格结构化语法，运行模型采用 Lua 风格可执行 chunk。
- 源文件没有特殊 `main`；声明与顶层语句按源码顺序组合。
- 文件路径直接定义 module identity；纯目录只形成 namespace。
- modules 输出使用普通 Lua `require`，完整路径生成 PascalCase 依赖别名。
- 单一公开 `struct`/`enum` 模块直接返回类型 table，多公开类型模块返回 module table。
- `Result<T, E>` 使用 `{ is_ok, payload }` tagged value；`Option<T>` 使用 nullable value。
- 原生 `[value, ...]` 构造 `Vec<T>`，`#{ key: value }` 构造 `HashMap<K, V>`。
- `#[cfg]`、`#[cfg_attr]` 和用户 annotation 共享结构化 attribute model。
- runtime annotation 聚合到一个 `rua_annotations.lua`，target 按 locator 惰性加载。
- `.ruai` 是 declaration-only 接口，标准库声明与 Lua runtime 由 `std.toml` 连接。
- compiler 与 IDE 使用两条 parser/semantic 流水线，共享 lexer、project contract 与 conformance corpus。
- LSP 支持跨文件导航、类型与成员 completion、文档 hover、inlay hints、rename、hierarchy 和 formatting。

## 维护规则

- 文档只描述仓库中可验证的当前行为。
- 语言语义写入 `rua-design.md`，内部数据流写入 `rua-architecture.md`，协议能力写入 `rua-lsp-features.md`。
- 测试数量与 case 清单由测试目录维护。
- 未实现的设计使用 issue 或 PR，不进入当前功能文档。
