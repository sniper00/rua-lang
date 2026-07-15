# Rua 工具链架构

Rua 将 batch compiler 与交互式 IDE 分成两条独立流水线。两者共享语言基础设施和可验证契约，但针对不同工作负载保留各自的 AST、解析策略和语义实现。

## 1. Workspace 边界

```text
rua-core      stable IDs, ranges, diagnostics, language contracts
rua-lex       shared lossless token stream
rua-project   IO-free project, logical path and source-provider model
rua-resources versioned std.toml, declarations and embedded resources
   |                              |
   v                              v
ruac                           rua-syntax
strict compiler               tolerant Rowan CST + formatter
                                  |
                                  v
                              rua-analysis
                              incremental HIR and IDE queries
                                  |
                                  v
                                rua-lsp
                              protocol and workspace adapter
```

长期约束：

- `ruac` 不依赖 Rowan、analysis 或 LSP，可以嵌入不提供磁盘和 CWD 的 host。
- `rua-syntax` / `rua-analysis` production 不调用 compiler semantic API 作为 fallback。
- `rua-analysis` 不持有 URI、LSP 类型或磁盘扫描策略。
- `rua-lsp` 不重做 name resolution、type inference 或 semantic fallback。
- language item、diagnostic code、source range、stable identity 和 runtime ABI 只在中立 crate 定义一次。

这些边界由 `scripts/check-boundaries.sh` 持续验证。

## 2. 双 parser

双 parser 是长期设计：

| | `ruac` strict parser | `rua-syntax` IDE parser |
|---|---|---|
| 目标 | batch compile、host embedding | 编辑中的不完整源码 |
| 输出 | owned AST | lossless Rowan CST |
| 错误策略 | fail-fast | error recovery + error node |
| trivia | 只保留 API documentation | 完整保留 whitespace/comment |
| 资源控制 | token / nesting budget | lossless、range-safe property |

两者共享 `rua-lex` token/range、`rua-core` contract、`rua-project` model，以及 accept/reject、range 和 semantic corpus；不共享 AST、recovery 或 type system。这样 `ruac` 保持小而可嵌入，IDE 同时获得稳定的增量语法树。

## 3. Compiler 数据流

```text
shared tokens
  -> owned AST preserving chunk order
  -> module and declaration collection
  -> resolved HIR
  -> structural checks and ID-keyed type facts
  -> BackendLayout
  -> structured Lua IR
  -> deterministic printer + source map
```

collection 先分配 module/item identity，再解析 import、path 和 body，因此支持前向引用与递归。成功的 use site 在 codegen 前已经是 `LocalId`、`DefId`、`ModuleId`、`BuiltinId` 或其他稳定 target；type facts 同样以 identity 为 key。

`BackendLayout` 唯一负责把 semantic identity 分配到 Lua place，并处理关键字、Unicode、保留前缀和清洗冲突。codegen 只消费 resolved HIR、type facts 和 layout，不按 AST 字符串、span 或未限定名称重新猜目标。

root free function 按 resolved dependency 排序并直接输出带 EmmyLua 注解的 `local function`；直接递归沿用该形式，只有包含多个函数的强连通依赖环才生成独立的 Lua 前向声明。

标准库也是输入，而不是散落在 compiler/LSP 中的成员表。`rua-resources` 用同一 schema 加载内嵌资源或显式目录：`std.toml` 列出 `.ruai` declaration、Option/Result language item、Lua runtime 包、导出子表、局部别名和可选 ABI。声明与单文件 `rua_std.lua` 位于同一目录。analysis 从 declaration 构建类型、成员、文档与 definition identity；compiler resolve 后才把标准定义 identity 连接到 runtime export。codegen 在 chunk 顶部最多输出一次 `require("rua_std")` 和一次包级 ABI 检查，再为实际使用的 export 生成 local；普通 `.ruai` library module 使用同一 import registry，但仍可映射到独立 Lua 包。只有 manifest 指定的 `Option` 与 `Result` 具有语言级表示，用户声明的同名类型仍走普通类型、trait 和 method 规则。

用户 method call 同样消费 type checker 记录的 dispatch identity：具体 receiver 直接调用 owner method，泛型与 trait object 从对象 metatable 动态分派，避免实例字段遮蔽同名方法。trait/operator impl 和公开 Lua ABI 类型保留实例 metatable；私有 inherent impl 静态分派，不为实例附加 metatable。私有且无 runtime member 的 struct/enum 只生成类型注解，不分配空 class table。

无 guard 且直接返回的 match 生成 `if/elseif/else`；identity 已证明穷尽时最后一臂直接作为 `else`，多分支 enum 只读取一次 tag，通用 guard match 才保留 matched state。融合 iterator 使用 identity-keyed closure substitution、直接 accumulator destination 和嵌套 filter guard，不分配闭包参数别名或恒真 active flag。未使用 initializer 只有在 type facts 证明无副作用且不会抛错时才消除；未知调用、用户 operator、索引、`?` 和潜在除零继续保留。String 长度、常量整数除余以及非负 range induction variable 对正数取余等已知 primitive operation 直接使用 Lua 表达式。

Lua 5.5 table allocation 按可证明的容量生成：随后填充的 module/type 方法表使用 `table.create(0, nrec)`；只对保持精确长度的 Vec iterator `collect` 预分配 sequence capacity，`filter`/`filter_map` 不使用输入长度作为容量。静态 table constructor 由 Lua 自身的 `NEWTABLE` hint 负责，codegen 不额外引入函数调用。

Lua IR 结构化表示 expression、place、table、call、function、statement 和 block。printer 独占括号、优先级、缩进和文本输出；source map 使用 HIR source anchor，不从生成字符串反推。

## 4. Native analysis 数据流

```text
file text/path/root/project/config/standard-library revision
  -> tolerant parse
  -> ItemTree
  -> project DefMap
  -> Body + Scope + BodySourceMap
  -> BodyResolution + Inference
  -> MemberIndex + ReferenceIndex
  -> protocol-neutral IDE results
```

文件和 inline module 的顶层语句 lower 到 synthetic chunk body，所以顶层 binding 参与 scope、inference、diagnostics、hover、references 和 rename。

definition identity 携带 project context。enum variant、field、method、trait item、standard declaration 和 inline macro 都是可导航的 semantic target。`ReferenceIndex` 由 resolved occurrence 构建，区分 declaration、read、write、call、capture 和 member use；references、rename、hierarchy 与 unused diagnostics 不扫描同名文本。

cache 以 file revision、project/root/config identity 为 key。public signature、private body、文件删除、project 删除和 library reload 分别触发受控失效；取消或基于旧 generation 的结果不进入 cache。

## 5. 文档与诊断契约

`Documentation` 是 protocol-neutral semantic record。只有 `///`、`//!`、`/** */` 和 `/*! */` 附着为 API 文档；普通注释和被空行隔开的注释不会进入 hover。

function、method、trait item、extern、`.ruai` declaration、field、enum variant 与 inline macro 从同一记录提供 hover、completion resolve 和 signature help。

`DiagnosticCode` 是 compiler 与 analysis 共用的稳定分类。machine contract 使用 code、file、byte range 和命名参数；CLI message 只是 presentation。LSP 直接发布 native analysis diagnostic，不启动 compiler 再解析终端文字。

## 6. LSP project 与并发

production server 维护一个长期 `AnalysisHost`，adapter state 分开记录：

- canonical path、stable `FileId`、`SourceRootId` 和 `ProjectId`。
- workspace root、readonly library root 和 project-scoped mount。
- disk text/revision 与 open overlay/version。
- configuration revision、watcher 和 scan generation。

`didOpen` 建立 overlay；`didChange` 只接受递增 version；`didClose` 恢复最新 disk text；只有 watcher delete 删除磁盘 identity。multi-root workspace 的 dependency 和 library 设置不会跨 project 泄漏。

未在 `.ruarc.toml` 配置 `runtime.std_path` 时，LSP 使用内嵌标准库，并把同版本资源物化到只读临时目录以提供可打开的 definition URI。自定义路径必须包含有效 `std.toml`；manifest 与所有 `.ruai` 会在替换当前索引前完整校验。一个 server 实例中的 workspace folder 必须使用相同标准库根，避免同一 semantic database 出现互相冲突的 language item。

目录扫描会处理 ignore 文件、跳过常见构建目录并防止 symlink cycle。昂贵只读查询和扫描运行在 bounded worker 上，支持 `$/cancelRequest`；输入 generation 改变后旧结果以 `ContentModified` 失败，不能覆盖新状态。URI/path 转换与 UTF-8 byte offset 到 UTF-16 position 的转换集中在 adapter 层。

## 7. 验证门禁

```bash
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets --all-features
cargo test --locked --workspace --all-features
cargo clippy --locked --workspace --all-targets --all-features --no-deps -- -D warnings
bash scripts/check-boundaries.sh
(cd editors/vscode && npm run check-types && npm run test-extension)
```

CI 固定校验 Lua 5.5.0 source archive。专项测试覆盖双 parser conformance 与任意 Unicode、结构化 compile-fail、每个 compile-pass 的真实 Lua execution、cross-file source map、incremental invalidation、multi-root/library lifecycle、cancellation/stale rejection、URI/UTF-16、stdio protocol lifecycle、formatter atomic write 和真实 VS Code Extension Host。

fixture 约定与实际覆盖以 [Golden 测试说明](../tests/golden/README.md)和[覆盖矩阵](../tests/golden/COVERAGE.md)为准。
