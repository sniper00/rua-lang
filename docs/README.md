# Rua 文档

本目录只维护当前实现契约。已完成的审查、施工计划、迁移日志和阶段 RFC 不再随代码保留；需要追溯时使用 Git 历史。

## 文档入口

- [语言与运行时设计](rua-design.md)：语言定位、chunk 语义、类型表示、模块、FFI 和兼容边界。
- [工具链架构](rua-architecture.md)：crate 边界、双 parser、compiler、增量 analysis、LSP 生命周期和测试门禁。
- [LSP 功能](rua-lsp-features.md)：编辑器当前暴露的协议能力和语义保证。

测试 fixture 的规则和实际覆盖由以下文件维护：

- [Golden 测试说明](../tests/golden/README.md)
- [Golden 覆盖矩阵](../tests/golden/COVERAGE.md)

## 当前特点

- Rua 使用 Rust 风格语法和静态类型，但不引入所有权、借用或生命周期。
- 源文件是可执行 chunk，没有特殊 `main`；生成物是可读的 Lua 5.5。
- `Result<T, E>` 在 Rua 内部始终是一等 tagged value，Lua multi-return 只出现在显式 FFI adapter。
- compiler 和 IDE 长期保留两套 parser，共享 lexer、稳定 ID、project contract 和 conformance corpus。
- codegen 只消费 resolved identity、typed facts、backend layout 和结构化 Lua IR，不按字符串猜语义。
- `.ruai` 是严格的 declaration-only 接口文件；workspace 与 library mount 都具有 project identity。
- LSP 使用独立增量语义引擎，支持跨文件导航、文档 hover、enum variant identity、原子 rename 和可取消查询。
- compiler API 返回结构化诊断与 generated-to-Rua source map；golden 生成物由真实 Lua 执行验证。

## 维护规则

- 文档用现在时描述已经存在的行为，不把计划写成能力。
- 稳定语义写入 `rua-design.md`，内部数据流写入 `rua-architecture.md`，协议面写入 `rua-lsp-features.md`。
- 测试数量和 case 清单只在测试目录维护，避免多份统计漂移。
- 临时设计讨论使用 issue 或 PR；完成后更新现行契约，而不是新增带日期的历史文档。
