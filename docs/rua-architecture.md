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

依赖约束：

- `ruac` 不依赖 Rowan、analysis 或 LSP，可以嵌入不提供磁盘和 CWD 的 host。
- `rua-syntax` / `rua-analysis` production 不调用 compiler semantic API 作为 fallback。
- `rua-analysis` 不持有 URI、LSP 类型或磁盘扫描策略。
- `rua-lsp` 不重做 name resolution、type inference 或 semantic fallback。
- language item、diagnostic code、source range、stable identity 和 runtime ABI 只在中立 crate 定义一次。

这些边界由 `scripts/check-boundaries.sh` 持续验证。

## 2. 双 parser

Rua 使用两套 parser：

| | `ruac` strict parser | `rua-syntax` IDE parser |
|---|---|---|
| 目标 | batch compile、host embedding | 编辑中的不完整源码 |
| 输出 | owned AST | lossless Rowan CST |
| 错误策略 | fail-fast | error recovery + error node |
| trivia | 只保留 API documentation | 完整保留 whitespace/comment |
| 资源控制 | token / nesting budget | lossless、range-safe property |

两者共享 `rua-lex` token/range、`rua-core` contract、`rua-project` model，以及 accept/reject、range 和 semantic corpus；不共享 AST、recovery 或 type system。这样 `ruac` 保持小而可嵌入，IDE 同时获得稳定的增量语法树。

grammar 扩展必须先进入 shared lexer，再分别进入 strict AST 与 Rowan CST。复合赋值、
`loop` value、`??`、`?.`、`in` 和 `#{...}` 由同一 accept/format corpus 校验两条
parser 流水线；compiler execution golden 与 native inference/LSP cursor test 再校验
两套独立 semantic 实现没有漂移。

模块图不由任一 parser 解析。`rua-project::module_path_from_relative_file` 把
`domain/order.rua` 和 `domain/order/mod.rua` 规范化为同一 logical path；`ruac`
扫描 filesystem 或 `ProjectSpec` 后构造 compiler-internal module node，analysis 则
从 VFS/project root 构造相同 DefMap。源码级 `mod` 在两个 parser 中都被拒绝。

## 3. Compiler 数据流

```text
shared tokens
  -> owned AST preserving chunk order
  -> cfg_attr expansion + active declaration/member view
  -> module and declaration collection
  -> resolved HIR
  -> annotation identity/schema index
  -> structural checks and ID-keyed type facts
  -> BackendLayout
  -> structured Lua IR
  -> deterministic printer + source map
```

collection 先分配 module/item identity，再解析 import、path 和 body，因此支持前向引用与递归。成功的 use site 在 codegen 前已经是 `LocalId`、`DefId`、`ModuleId`、`BuiltinId` 或其他稳定 target；type facts 同样以 identity 为 key。

attribute 先解析为结构化 meta item。`cfg_attr` 展开和 `cfg` 求值使用 project 的
`CfgOptions`，只把激活的声明与成员交给 collection；codegen 不再次检查 attribute
字符串。resolve 后建立的 `AnnotationIndex` 保存 annotation identity、validated arguments、
retention 和目标 identity，metadata 与 runtime registry 都从该索引投影。

`BackendLayout` 唯一负责把 semantic identity 分配到 Lua place，并处理关键字、Unicode、保留前缀和清洗冲突。bundle layout 把 module 映射到同一 lexical chunk 中的 table path；modules layout 把当前 module 映射到具名本地 table，把跨 module identity 映射到由普通 `require` 绑定的稳定 local alias。依赖 alias 由完整逻辑路径转成 PascalCase，例如 `application.checkout` 对应 `ApplicationCheckout`；路径折叠重名时追加 identity，用户局部变量同名时由 layout 改名。仅含一个公开 `struct`/`enum` 的源模块把 module identity 与该类型 table 合并；声明模块保留配置的外部 ABI。codegen 只消费 resolved HIR、type facts 和 layout，不按 AST 字符串、span 或未限定名称重新猜目标。

root free function 按 resolved dependency 排序并直接输出带 EmmyLua 注解的 `local function`；直接递归沿用该形式，只有包含多个函数的强连通依赖环才生成独立的 Lua 前向声明。

标准库也是输入，而不是散落在 compiler/LSP 中的成员表。`rua-resources` 用同一 schema 加载内嵌资源或显式目录：`std.toml` 列出 `.ruai` declaration、Option/Result language item、Lua runtime 包、导出子表、局部别名和可选 ABI。声明与单文件 `rua_std.lua` 位于同一目录。analysis 从 declaration 构建类型、成员、文档与 definition identity；compiler resolve 后才把标准定义 identity 连接到 runtime export。bundle codegen 在 chunk 顶部最多输出一次 runtime import；modules codegen 为实际使用 runtime 的输出单元分别声明 import，Lua cache 保证同一包只加载一次。普通 `.ruai` library module 使用同一 import registry，但仍可映射到独立 Lua 包。只有 manifest 指定的 `Option` 与 `Result` 具有语言级表示，用户声明的同名类型仍走普通类型、trait 和 method 规则。

普通 Lua library 不复制标准库的逐模块 manifest。`rua-project` 将每个
`workspace.lua_library` 规范化为 declaration/runtime root pair，并分别合并到
只读语义输入与 Lua 搜索路径。LSP 递归扫描 declaration root；compiler 按同一
logical path 解析。codegen 把每个 file-backed declaration `ModuleId` 视为独立
require boundary，完整 module path 转为点分 Lua package name；没有实体文件的目录
只形成路径 namespace。

用户 method call 同样消费 type checker 记录的 dispatch identity：具体 receiver 直接调用 owner method，泛型与 trait object 从对象 metatable 动态分派，避免实例字段遮蔽同名方法。trait/operator impl 和公开 Lua ABI 类型保留实例 metatable；私有 inherent impl 静态分派，不为实例附加 metatable。私有且无 runtime member 的 struct/enum 只生成类型注解，不分配空 class table。

无 guard 且直接返回的 match 生成 `if/elseif/else`；identity 已证明穷尽时最后一臂直接作为 `else`，多分支 enum 只读取一次 tag，通用 guard match 才保留 matched state。融合 iterator 使用 identity-keyed closure substitution、直接 accumulator destination 和嵌套 filter guard，不分配闭包参数别名或恒真 active flag。未使用 initializer 只有在 type facts 证明无副作用且不会抛错时才消除；未知调用、用户 operator、索引、`?` 和潜在除零继续保留。String 长度、常量整数除余以及非负 range induction variable 对正数取余等已知 primitive operation 直接使用 Lua 表达式。

脚本表达式也只消费 type facts：复合赋值先把复杂 place 的 base/index materialize
一次；`loop` value 写入 compiler-owned destination；`??` 与 `?.` 生成显式 nil guard，
不能用会混淆 `false` 的 Lua `or`；`in` 根据 `ContainsKind` 选择 Vec element、map key、
string substring 或 iterator consumer；typed map literal 调用 `map.new(nrec)` 后按顺序
insert。codegen 不根据变量名或表形状猜测其中任何一种操作。

builtin Result 使用 ABI v2 `{ is_ok, payload }` array table。codegen 通过统一的
Result tag/payload helper 生成 `[1]`/`[2]` 访问；普通 enum 继续使用命名 `tag`，两种
表示不会因表面形状相似而混用。FFI adapter 是唯一在 Rua Result 与 Lua
multi-return 之间转换的边界。

Lua 5.5 table allocation 按可证明的容量生成：随后填充的 module/type 方法表使用 `table.create(0, nrec)`；只对保持精确长度的 Vec iterator `collect` 预分配 sequence capacity，`filter`/`filter_map` 不使用输入长度作为容量。静态 table constructor 由 Lua 自身的 `NEWTABLE` hint 负责，codegen 不额外引入函数调用。

Lua IR 结构化表示 expression、place、table、call、function、statement 和 block。printer 独占括号、优先级、缩进和文本输出；source map 使用 HIR source anchor，不从生成字符串反推。

modules backend 只为 root 和具有实体 `.rua` source 的 runtime `ModuleId` 生成独立
Lua IR 和 source map。纯目录 namespace 没有输出文件或 runtime table；跨过 namespace
的引用直接 require 最终实体模块。每个单元在顶部直接 require identity-resolved
dependency。仅有一个公开 `struct`/`enum` 时，类型 table 同时作为 module 返回值，
跨模块调用直接使用返回的类型 table；多个公开类型或纯函数模块以文件名末段的
PascalCase 声明 module table。EmmyLua class identity 使用完整路径，例如
`presentation.Console` 或 `domain.Product`。单元随后定义、初始化并返回对应 table；
`.ruai` 只形成 runtime import，不产生生成文件。`runtime.lua_path`/`--lua-path`
只负责在 root 输出中前置 Lua `package.path`。依赖图必须无环，循环依赖在生成阶段
诊断；初始化顺序采用 Lua require 的深度优先语义。

dependency alias 由完整 module path 转为 PascalCase，例如 `application.checkout`
对应 `ApplicationCheckout`。`BackendLayout` 统一处理路径折叠冲突、用户 local 遮蔽和
runtime alias 冲突。模块头按搜索路径、标准库 ABI、module require、标准库 export
alias 分组；Lua IR 中的 blank entry 负责类型、函数、初始化与最终 return 的空行。

runtime annotation 的 modules artifact 包含一个 `rua_annotations.lua`。codegen 按
resolved annotation identity 与 backend place 生成 canonical locator；registry 加载只
注册 metadata，`Annotations::load` 负责 target module 的惰性 require。CLI 仅清理带
ruac metadata header 的 compiler-owned metadata 文件。

## 4. Native analysis 数据流

```text
file text/path/root/project/config/standard-library revision
  -> tolerant parse
  -> project-filtered ItemTree
  -> project DefMap
  -> Body + Scope + BodySourceMap
  -> BodyResolution + Inference
  -> MemberIndex + ReferenceIndex
  -> protocol-neutral IDE results
```

每个源文件的顶层语句 lower 到 synthetic chunk body，所以顶层 binding 参与 scope、inference、diagnostics、hover、references 和 rename。目录 namespace 没有独立 chunk。

native inference 独立推断 `loop` break join、Option chain/coalesce、membership 与 map
key/value，并把可选链 receiver 解包后交给 `MemberIndex`。因此 `value?.member` 与
`value.member` 共享同一个 declaration identity，而表达式结果仍按 Option 规则包装。

analysis 使用与 compiler 相同的 `CfgOptions` 构造 project-filtered ItemTree。未激活
声明不参与 resolve、completion 或 diagnostics，但原始 syntax token 带 inactive
modifier，并可 hover 查看未满足的 cfg。annotation declaration 和 application 则以
definition identity 进入 completion、hover、definition、references 与 rename。

definition identity 携带 project context。enum variant、field、method、trait item 和 standard declaration 都是可导航的 semantic target。`ReferenceIndex` 由 resolved occurrence 构建，区分 declaration、read、write、call、capture 和 member use；references、rename、hierarchy 与 unused diagnostics 不扫描同名文本。

cache 以 file revision、project/root/config identity 为 key。public signature、private body、文件删除、project 删除和 library reload 分别触发受控失效；取消或过期 generation 的结果不进入 cache。

## 5. 文档与诊断契约

`Documentation` 是 protocol-neutral semantic record。只有 `///`、`//!`、`/** */` 和 `/*! */` 附着为 API 文档；普通注释和被空行隔开的注释不会进入 hover。

function、method、trait item、extern、`.ruai` declaration、field 与 enum variant 从同一记录提供 hover、completion resolve 和 signature help。标准函数由 `.ruai` 派生索引提供相同能力，不维护宏特判。

`DiagnosticCode` 是 compiler 与 analysis 共用的稳定分类。machine contract 使用 code、file、byte range 和命名参数；CLI message 只是 presentation。LSP 直接发布 native analysis diagnostic，不启动 compiler 再解析终端文字。

## 6. LSP project 与并发

production server 维护一个长期 `AnalysisHost`，adapter state 分开记录：

- canonical path、stable `FileId`、`SourceRootId` 和 `ProjectId`。
- workspace root、readonly library root 和 project-scoped mount。
- disk text/revision 与 open overlay/version。
- configuration revision、watcher 和 scan generation。

`didOpen` 建立 overlay；`didChange` 只接受递增 version；`didClose` 恢复最新 disk text；只有 watcher delete 删除磁盘 identity。multi-root workspace 的 dependency 和 library 设置不会跨 project 泄漏。

未在 `.ruarc.toml` 配置 `runtime.std_path` 时，LSP 使用内嵌标准库，并把同版本资源物化到只读临时目录以提供可打开的 definition URI。自定义路径必须包含有效 `std.toml`；manifest 与所有 `.ruai` 会在替换当前索引前完整校验。一个 server 实例中的 workspace folder 必须使用相同标准库根，避免同一 semantic database 出现互相冲突的 language item。

目录扫描会处理 ignore 文件、跳过常见构建目录并防止 symlink cycle。昂贵只读查询和扫描运行在 bounded worker 上，支持 `$/cancelRequest`；输入 generation 改变后过期结果以 `ContentModified` 失败，不能覆盖新状态。URI/path 转换与 UTF-8 byte offset 到 UTF-16 position 的转换集中在 adapter 层。

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
