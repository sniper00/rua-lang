# Rua Native Analysis 与 LSP 全量迁移施工计划

> 状态：施工中（Step 4B.0-4B.5 已完成；下一步 Step 4B.6）。
> 基线：`35f0d8b`（Phase -1 至 Phase 4A 已完成）。
> 架构依据：`docs/rua-ide-architecture.md` Phase 4 / Phase 5。
> 前序计划：`docs/rua-construction-plan.md`。
> 目标：完成 `rua-analysis` 原生 body/type analysis，并让 `rua-lsp` 的全部语义功能只通过长期 `AnalysisHost` 查询。

## 1. 为什么需要这一阶段

前序计划已经完成仓库拆分、golden 基线、双 parser conformance、VFS、
`AnalysisHost` 骨架、ItemTree、DefMap、基础 Semantics，以及闭包/iterator
的编译器和 IDE 垂直切片。但是当前 LSP 仍处于双管线状态：

- `Server.workspace: rua_syntax::workspace::Workspace<DiskLoader>` 仍是打开
  文档、磁盘索引和绝大多数 IDE 查询的主状态。
- `AnalysisInputs` 中的长期 `AnalysisHost` 只接收配置的 `.ruai` library
  roots，尚未承载 workspace/open-buffer 输入和 feature query。
- completion、hover、goto、references、rename、document symbols 仍主要调用
  `rua-syntax::analysis` / `workspace`。
- diagnostics 和 semantic tokens 虽调用 `rua-analysis`，但每次查询都会创建
  临时 `AnalysisHost`，没有复用长期增量数据库。
- `rua-analysis::ide::closure_iterator` 仍通过
  `rua_syntax::analysis::Analysis` 获取 compiler-backed binding types。
- 当前 ItemTree/DefMap 尚未把 field、variant、impl/method 和完整 signature 建成
  可供 body/type/reference cache 使用的稳定 definition identity。
- 当前任一文件变化都会清空全部 DefMap cache，还没有 body/signature/module 级
  依赖失效图。
- external `.ruai` 配置进入一个 handler 不可见的私有 host；物理路径与 logical
  module path 也尚未形成统一 contract。
- LSP 尚未处理 watched-file/workspace-folder 变更，publishDiagnostics 不携带
  document version；VS Code extension 也未贡献 library/mount settings。
- `rua_syntax::lex` 仍通过 transition 调用 compiler tokenizer。
- `rua-syntax` 的普通依赖仍包含 `ruac`，transition-only façade 尚未退出
  production dependency graph。

因此，“LSP 已依赖 `rua-analysis`”不等于架构迁移完成。本阶段必须同时补齐
native body/type/member analysis 和 LSP handler 迁移，否则删除旧 Workspace 会
直接造成类型、成员和局部变量能力回退。

## 2. 最终边界

目标数据流：

```text
disk / VS Code notifications / library configuration
                         |
                         v
          rua-lsp loader + path/file registry
                         |
                         v
                    Change batch
                         |
                         v
                  AnalysisHost (唯一)
                         |
        +----------------+----------------+
        |                |                |
      parse          HIR def/body      inference
        |                |                |
        +----------------+----------------+
                         |
                         v
       protocol-neutral IDE query results
                         |
                         v
              rua-lsp LSP type conversion
```

编译器保持独立：

```text
ruac parser -> owned AST -> resolve/check/typeck -> Lua codegen
```

`ruac` 只在 `rua-analysis` 的 parity/dev tests 中作为 oracle；production LSP
不调用 compiler parser、typeck 或 transition-only IDE façade。

### 2.1 Crate 依赖目标

普通依赖最终应满足：

```text
rua-lsp -> rua-analysis -> rua-syntax -> rowan
    |                            |
    +------ formatter only ------+

ruac -> compiler-owned modules only
```

禁止的普通依赖：

- `rua-syntax -> ruac`
- `rua-analysis -> ruac`
- `ruac -> rua-syntax / rowan / rua-analysis / lsp-types`
- `rua-analysis -> lsp-types / lsp-server`
- `rua-analysis` 核心中的磁盘 IO

`ruac` 可以继续作为 `rua-syntax` / `rua-analysis` 的 dev-dependency，用于
parser、type 和 diagnostic parity；`rua-lsp` 不保留 `ruac`，协议 parity 只比较
LSP adapter 和 `rua-analysis` query result。

## 3. 不变量

每个 Step 都必须保持以下约束：

1. **单一语义状态**：迁移结束后，LSP 只有一个长期 `AnalysisHost`；不得再有
   独立语义 Workspace 或每请求临时数据库。
2. **稳定文件身份**：同一路径在 reload、open/change/close、watcher 事件中保持
   `FileId` 稳定；删除后重新创建的策略必须有测试固定。
3. **核心无 IO**：文件扫描、canonicalization、watcher 和 URI 转换只在 LSP
   adapter；`rua-analysis` 只接收 `Change`。
4. **快照隔离**：一次 request 只使用一个 `Analysis` snapshot；request 执行中
   到达的变更只影响下一次 snapshot。
5. **协议无关**：`rua-analysis` 公共查询不返回 `lsp_types`、URI 或 JSON-RPC
   类型，只返回 `FileId`、`TextRange` 和自有 POD。
6. **容错优先**：不完整代码必须返回 partial result 或 `Unknown`，不能 panic，
   不能为了看起来精确而猜测类型或定义。
7. **确定性输出**：completion、references、symbols、diagnostics 和 edits 必须按
   明确键排序并去重，不能依赖 HashMap 遍历顺序。
8. **只读边界**：Library/Std root 可参与 hover/goto/completion/references，但
   rename/code action 不得生成针对只读文件的 edit。
9. **无静默 fallback**：某 handler 切到新管线后，不允许失败时偷偷调用旧
   Workspace；差异必须成为测试或显式 `Unknown`。
10. **oracle 不进 production**：`ruac` parity 只运行在测试/CI，不进入 LSP
    请求热路径。

## 4. 施工策略

### 4.1 提交规则

- 一个 Step 一个提交，提交标题使用 `<step>: <action>`。
- 每步先新增或更新测试，再切 production 调用点。
- 每步提交说明记录验证命令、已知差异和下一步。
- 不在同一提交中同时实现 native analysis、迁移多个 handler、删除旧代码。
- 不重写历史；旧管线只在所有消费者迁走后删除。

### 4.2 双管线期规则

双管线只用于迁移验证：

- 建 test-only parity adapter，同一 fixture 同时调用 legacy 和 native query。
- 对 path、range、排序和展示文本做 normalization 后比较。
- 可接受差异必须写入命名明确的 expected-difference fixture，并注明删除 Step。
- production handler 一旦迁移，只调用 native query。
- 不增加用户可配置的 legacy/native 开关，不长期维护两套行为。

### 4.3 每步通用验证

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p rua-analysis
cargo test -p rua-lsp --features lsp
cargo test -p rua-syntax
cargo test -p ruac
git diff --check
```

修改 shared query contract、VFS 或依赖边界时额外运行：

```sh
cargo test --workspace --all-features
cargo clippy -p rua-analysis --all-targets --no-deps -- -D warnings
cargo clippy -p rua-lsp --features lsp --all-targets --no-deps -- -D warnings
cargo clippy -p rua-syntax --all-targets --no-deps -- -D warnings
cargo tree -p rua-lsp --features lsp -e normal
cargo tree -p rua-syntax -e normal
```

## 5. Phase 4B：Native HIR Body 与 Type Analysis

目标：让 `rua-analysis` 自己回答局部作用域、类型、成员、诊断和 IDE 查询，
不再借 `rua-syntax::analysis` 间接调用 `ruac`。

### Step 4B.0：冻结迁移 parity 基线

改动范围：

- 为现有 completion、hover、goto、references、rename、symbols、diagnostics、
  semantic tokens 建统一 query fixture 清单。
- 复用 `tests/golden/ide`、`tests/golden/ruai` 和 Phase 4A fixtures。
- 新增 test-only legacy/native result normalizer；native 尚未实现的查询先返回
  `Unsupported`，不伪造结果。
- 记录当前 legacy 输出、排序、range、readonly 和错误行为。

验证命令：

```sh
cargo test -p rua-analysis migration_baseline
cargo test -p rua-syntax --test ide_goldens
cargo test -p rua-syntax --test ruai_goldens
cargo test -p rua-lsp --features lsp
```

退出条件：

- 所有当前用户可见 LSP 能力都有可复用 oracle。
- parity harness 能区分 equal、expected difference 和 unsupported。
- 本步不改变 production LSP 行为。

建议提交：`4B.0: freeze native analysis migration baseline`

### Step 4B.1：定义 protocol-neutral IDE 数据模型

改动范围：

- 在 `rua-analysis::ide` 定义并统一：
  - `ProjectId` / `ProjectPosition` / `QueryContext`
  - `FilePosition` / `FileRange`
  - `NavigationTarget`
  - `HoverResult`
  - `CompletionItem` / `CompletionKind`
  - `ReferenceResult`
  - `TextEdit` / `SourceChange`
  - `RenameError`
  - `Diagnostic` / `DiagnosticCode`
  - `SemanticToken`
- 所有 range 使用 UTF-8 byte offset；UTF-16 转换只在 LSP。
- 纯文件内 parse/source-map helper 可用 `FilePosition`；任何可能跨文件、依赖
  module/root priority 或使用 shared library 的 semantic query 必须接收
  `ProjectPosition` 或显式 `ProjectId`。
- 为所有列表结果规定稳定排序和去重键。
- 不暴露 rowan node/token、compiler AST 或 LSP protocol type。

验证命令：

```sh
cargo test -p rua-analysis ide_contract
cargo clippy -p rua-analysis --all-targets --no-deps -- -D warnings
```

退出条件：

- 后续 handler 所需结果都能由 protocol-neutral 类型表达。
- public API 不含 `lsp_types`、URI、rowan tree handle 或 `ruac` 类型。

建议提交：`4B.1: define protocol-neutral IDE query types`

### Step 4B.2：稳定 project、definition 与 signature 模型

改动范围：

- 先固定跨 cache/snapshot 可引用的 definition identity；`DefId(u32)` 不能依赖
  每次重建 DefMap 的偶然插入顺序。可采用 root/module/local 分层 ID 或 db-owned
  interner，但必须明确 identity、equality 和失效语义。
- 定义 `ProjectId`（或等价 project model），为每个 workspace 建显式 root file、
  workspace roots 和有序 dependency roots；query 必须携带 project/root context，
  module resolution 不得遍历数据库中的全部 roots，也不得跨 multi-root 按
  basename 全局猜测。
- 扩充 ItemTree/DefMap，完整记录：
  - struct fields、enum variants
  - trait/impl blocks、methods、receiver forms
  - extern declarations
  - generic params、bounds、where predicates
  - function/method signatures 和 declaration/source kind
- 定义 signature hash/fingerprint；body 文本变化不改变 definition identity 或
  signature，公开签名变化才传播到依赖者。
- 建 definition/source map，所有 Def/field/variant/method 都能回到 FileRange。
- `.rua` / `.ruai` 和 workspace/library/std 中的同名定义按 root/candidate priority
  产生确定结果。
- module candidate 必须按每个 root 的 logical module base 计算，不能把 workspace
  importer 的物理/相对目录直接套到 library root；用内存 fixture 固定
  `workspace/src/main.rua` 可见 `library/foo.ruai` 的约定。

验证命令：

```sh
cargo test -p rua-analysis definition_identity
cargo test -p rua-analysis item_signature
cargo test -p rua-analysis member_definition_map
cargo test -p rua-analysis multi_root_identity
```

退出条件：

- DefId/MemberId 可安全作为 Body、Inference 和 ReferenceIndex cache key。
- field/variant/impl/method/extern 不再从 definition layer 丢失。
- body-only edit 不改变 item identity；signature edit 有明确依赖失效信号。
- multi-root 和 source-root shadowing 有测试固定。
- workspace A 不会看到 workspace B，除非 project dependency 显式声明。

建议提交：`4B.2: stabilize project definitions and signatures`

### Step 4B.3：实现 HIR Body 与 SourceMap

改动范围：

- 新增 `hir::body`，定义 `BodyId`、`ExprId`、`PatId`、`BindingId` 和 arena。
- lower function、method、closure、block、statement、expression 和 pattern。
- 建 `BodySourceMap`：HIR ID <-> `FileId + TextRange`。
- recovered/missing syntax lower 为显式 Missing 节点，不 panic。
- `BaseDb::body(DefId)` 做 per-owner semantic cache；`BodySourceMap` 绑定当前 file
  revision，文本长度/range 变化时必须刷新，即使 semantic body 可复用。
- 不在本步实现类型推断。

最低 fixture：

- let/mut、params、return/tail
- block/if/while/loop/for
- call/path/field/method/index
- match 和所有已支持 pattern
- closure params/body/capture syntax
- iterator/range expression
- incomplete expression 和缺失 delimiter

验证命令：

```sh
cargo test -p rua-analysis body_lowering
cargo test -p rua-analysis body_source_map
cargo test -p rua-syntax parser_conformance
```

退出条件：

- 可编译 corpus 的函数/方法/闭包 body 可完整 lower。
- 不完整代码能生成 partial Body 和稳定 SourceMap。
- 修改一个 body 不重算无关文件 parse/ItemTree。

建议提交：`4B.3: lower syntax into cached HIR bodies`

### Step 4B.4：实现 native local scopes 与局部 name resolution

改动范围：

- 为 Body 建 lexical scope tree。
- 支持 fn/method/closure params、let、for、if let、while let、match arm、
  tuple/struct/enum pattern binding。
- 正确处理 shadowing、声明前不可见、arm/branch/closure 边界。
- 局部 use 解析到 `BindingId`；item/path 继续通过 DefMap/Semantics。
- member name 在本步保持 unresolved，留给 type/member lookup。
- 建本文件 binding-use index，支持 definition/references/rename 的基础数据。
- 不调用 `rua_syntax::nameres`。

验证命令：

```sh
cargo test -p rua-analysis local_scope
cargo test -p rua-analysis local_resolution
cargo test -p rua-analysis local_reference_index
```

退出条件：

- local goto/references/rename parity fixture 与 legacy 一致。
- 闭包参数和 pattern binding 不再依赖 `rua-syntax::analysis`。
- malformed body 不产生跨作用域错误绑定。

建议提交：`4B.4: resolve locals in native body scopes`

### Step 4B.5：实现类型模型与基础 inference

改动范围：

- 定义 analysis-owned `Ty`：primitive、named ADT、tuple、function、closure、
  Vec、HashMap、Option、Result、iterator、generic param、Unknown、Never。
- 建 expected-type propagation、兼容性、unification 和最小 generic substitution。
- 推断 literal、annotation、binding、unary/binary、assignment、call、return、
  block、if、while、loop、for 和 index。
- `InferenceResult` 保存 expr/pat/binding/call 的类型和局部 diagnostic facts。
- 所有不确定路径降级 `Unknown`；`Unknown` 不触发二次 mismatch 噪声。
- `BaseDb::infer(BodyId)` 缓存并记录依赖。

验证命令：

```sh
cargo test -p rua-analysis inference_primitives
cargo test -p rua-analysis inference_control_flow
cargo test -p rua-analysis inference_calls
cargo test -p rua-analysis type_parity
```

退出条件：

- local/parameter/return hover 所需基础类型来自 native inference。
- 已覆盖合法 corpus 不出现 false-positive type error。
- 已覆盖非法 corpus 的核心 mismatch 与 `ruac` 同类；无法证明时为 Unknown。

建议提交：`4B.5: infer native body types`

### Step 4B.6：实现 ADT、generic、trait 与 member lookup

改动范围：

- 从 ItemTree/DefMap 建 struct/enum/trait/impl/type member index。
- 支持字段、associated fn、`self` / `&self` / `&mut self` method。
- 支持 generic ADT/function/method、bounds 和 `where` 的已实现子集。
- 支持 trait default method 和 inherent/trait impl 合并，歧义时不猜测。
- 建 analysis-owned builtin metadata，覆盖 Vec/HashMap/String/Option/Result；
  不从 compiler completion façade复制运行时查询结果。
- `.ruai` declaration 与 workspace source 使用同一 DefMap/type/member path。
- 输出 member resolution、call signature 和 completion candidates。

验证命令：

```sh
cargo test -p rua-analysis adt_inference
cargo test -p rua-analysis member_lookup
cargo test -p rua-analysis generic_trait_inference
cargo test -p rua-analysis ruai_member_lookup
cargo test -p rua-analysis member_parity
```

退出条件：

- field/method hover、goto 和 completion 不调用 compiler bridge。
- workspace > library > std 的定义和成员优先级一致。
- ambiguous/unsupported receiver 返回空或 Unknown，不返回错误成员。

建议提交：`4B.6: resolve native type members and signatures`

### Step 4B.7：实现 narrowing、closure 与 iterator inference

改动范围：

- 支持 enum/Option/Result match、if let、while let 的 pattern narrowing。
- 原生推断 closure params、return、read capture、fused mutable capture 边界。
- 原生推断 range/Vec iterator source、所有 Phase 4A adapters/consumers 的 item
  和 result type；IDE 只建类型事实，不复制 compiler codegen `IterPlan`。
- semantic tokens 的 closure definition/use 绑定来自 native BindingId。
- 删除 `rua-analysis::ide::closure_iterator` 对
  `rua_syntax::analysis::Analysis` 的调用。

验证命令：

```sh
cargo test -p rua-analysis pattern_narrowing
cargo test -p rua-analysis closure_type_parity
cargo test -p rua-analysis iterator_type_parity
cargo test -p rua-analysis --test closure_iterator_ide
```

退出条件：

- Phase 4A IDE snapshot 完全由 native analysis 生成。
- closure/iterator 已覆盖类型与 `ruac` 一致或显式 Unknown。
- `rg "rua_syntax::analysis" crates/rua-analysis/src` 无结果。

建议提交：`4B.7: infer closures iterators and narrowed patterns`

### Step 4B.8：实现 native diagnostics 与 reconciliation policy

改动范围：

- 合并 parse、name、type diagnostics，定义稳定 analysis diagnostic code。
- 保留 primary `FileRange`，必要时支持 related ranges。
- 对 recovery cascade 做去重和抑制，Unknown 不产生推测性错误。
- 建 compiler parity policy：
  - compiler 接受的 golden 不得出现 analysis error；
  - compiler 拒绝的已覆盖 golden应有同类 diagnostic；
  - message 可不同，但 code/category 和 primary range 必须稳定。
- `ruac::check_diags` 只在 dev/parity tests 中调用。

验证命令：

```sh
cargo test -p rua-analysis diagnostic_parse
cargo test -p rua-analysis diagnostic_name
cargo test -p rua-analysis diagnostic_type
cargo test -p rua-analysis diagnostic_parity
```

退出条件：

- 当前 LSP diagnostic golden 可由 native pipeline 重建。
- 无 production compiler fallback。
- 同一事实不会被 fast/compiler 两套来源重复发布。

建议提交：`4B.8: produce native structured diagnostics`

### Step 4B.9：完成 Analysis IDE query façade

改动范围：

- 在 `Analysis` 上提供完整 query：
  - `diagnostics(ProjectId, FileId)`
  - `hover(ProjectPosition)`
  - `goto_definition(ProjectPosition)`
  - `completions(ProjectPosition)`
  - `references(ProjectPosition, include_declaration)`
  - `rename(ProjectPosition, new_name)`
  - `document_symbols(ProjectId, FileId)`
  - `workspace_symbols(ProjectId, query)`
  - `semantic_tokens(ProjectId, FileId)`
- 查询内部只访问 BaseDb/HIR/inference/index，不接受 source string 临时重建。
- rename 返回跨文件 `SourceChange`，并在 readonly target 上返回专用错误。
- hover/completion detail 使用稳定 display 层，不把 debug string 暴露给 LSP。

验证命令：

```sh
cargo test -p rua-analysis ide_hover
cargo test -p rua-analysis ide_navigation
cargo test -p rua-analysis ide_completion
cargo test -p rua-analysis ide_references_rename
cargo test -p rua-analysis ide_symbols_tokens
```

退出条件：

- legacy Workspace 的全部语义查询都有 native 对应入口。
- query result 跨文件、`.ruai`、readonly、Unknown 和排序语义有测试。
- 多个 `Analysis` snapshot 可同时保留，后续 Change 不改变旧结果；这不表示
  snapshot 可以跨线程发送。

建议提交：`4B.9: expose complete native IDE queries`

### Step 4B.10：补齐缓存、依赖失效与性能基线

改动范围：

- 为 parse、ItemTree、DefMap、Body、Inference、member index、reference index
  定义 revision/dependency 和失效范围；替换当前“任一文件变化清空全部 DefMap”
  的粗粒度策略。
- 拆分 revision-local source map 与可复用 semantic cache：parse、ItemTree、
  DefSourceMap、BodySourceMap 必须反映当前文本 revision；稳定 Def identity、
  signature graph 和 inference 可按 fingerprint 复用。
- 区分 trivia/body/signature/module/root revision：在文件前部插入注释或改变前一个
  body 长度时，所有受影响 range/source map 必须刷新；semantic fingerprint 未变
  时不得重算 Def graph/inference。body semantic 变化只失效对应 owner，
  signature/mod/use/`.ruai` 变化才沿依赖图传播。
- 增加 test-only query counters，证明无关文件修改不重算 body/inference。
- root/module/public signature 改变时正确失效依赖文件。
- body-only 改变不重建无关 module tree。
- 建 synthetic workspace benchmark/smoke：多文件首次索引、单文件连续编辑、
  completion/hover 热查询。
- 先记录基线和结构性断言；没有 profiling 证据时不引入 salsa 或并行 runtime。

验证命令：

```sh
cargo test -p rua-analysis cache_invalidation
cargo test -p rua-analysis dependency_invalidation
cargo test -p rua-analysis snapshot_isolation
cargo test -p rua-analysis large_workspace_smoke -- --ignored
```

退出条件：

- 单文件 body edit 不重算无关文件 body/inference，但当前文件 parse/source maps
  必须刷新并返回新 byte range。
- trivia/range-only edit 不重算 semantic graph/inference，goto/rename/token range
  仍与当前文本完全一致。
- public/module/library change 会失效所有实际依赖者。
- 热查询复用缓存，不每次 lower/infer 全文件。

建议提交：`4B.10: bound native analysis invalidation`

### Step 4B.11：建立 native analysis 边界门禁

改动范围：

- 禁止 `rua-analysis/src` 调用 `rua_syntax::analysis`、`workspace`、`nameres`
  或 transition types。
- 允许使用 `rua-syntax` parse、typed AST、token/range 和纯 structural helper。
- `ruac` 只保留在 `rua-analysis` dev-dependencies。
- CI 增加 dependency/source boundary check。

验证命令：

```sh
! rg -n "rua_syntax::(analysis|workspace|nameres)" crates/rua-analysis/src
! cargo tree -p rua-analysis -e normal | rg "ruac"
cargo test -p rua-analysis
cargo clippy -p rua-analysis --all-targets --no-deps -- -D warnings
```

退出条件：

- native analysis 不再通过任何旧语义 façade 借用 compiler 结果。
- Phase 5 可以迁移 handler，不需要再扩 transition API。

建议提交：`4B.11: enforce native analysis dependency boundary`

### Step 4B.12：替换 syntax lexer transition bridge

改动范围：

- 当前 `rua_syntax::lex` 仍调用 `transition::lex -> ruac tokenizer`；实现
  syntax-owned trivia-aware lexer，或提取仅含 tokenization 的低依赖 `rua-lex`。
- 保持 rowan parser 所需 token kinds、gap-free byte coverage、comments/trivia、
  UTF-8 boundary 和 recovery 行为。
- 扩充 compiler/syntax lexical conformance，覆盖所有 token、错误字节、嵌套注释、
  string/numeric edge cases 和非 ASCII。
- 本步只迁 lexer；legacy semantic façade 等所有 LSP consumer 迁走后再在 5.10
  删除整个 transition 模块。

验证命令：

```sh
cargo test -p rua-syntax lexer
cargo test -p rua-syntax --test conformance
cargo test -p rua-syntax parser_conformance
cargo test -p ruac
```

退出条件：

- `rua_syntax::lex` 不调用 `ruac` 或 transition。
- token/range/parser corpus 无行为漂移。
- 最终移除 transition 时不再有 lexical blocker。

建议提交：`4B.12: own CST lexing in rua-syntax`

## 6. Phase 5：LSP Feature 全量迁移

目标：让长期 `AnalysisHost` 成为 `rua-lsp` 唯一语义状态，逐项迁移所有
handler，最后删除 legacy Workspace 和 production compiler dependency。

### Step 5.0：建立 protocol-level LSP parity harness

改动范围：

- 基于 `lsp_server::Connection::memory` 建完整 request/notification harness。
- 支持 initialize、didOpen、didChange、didClose、watched file、configuration。
- 对 response/notification JSON 做 URI、顺序和 version normalization。
- 为所有已声明 capability 建至少一个协议级 snapshot。
- test-only 可并行调用 legacy/native adapter，但 production 不增加切换开关。

验证命令：

```sh
cargo test -p rua-lsp --features lsp protocol_parity
cargo test -p rua-lsp --features lsp capability_contract
```

退出条件：

- handler 迁移可在真实 LSP request/response 层比较，不只测内部 helper。
- 没有声明但不实现的 capability，也没有实现但未声明的核心 capability。

建议提交：`5.0: add protocol-level LSP migration harness`

### Step 5.1：实现 LSP workspace loader 与稳定 FileId registry

改动范围：

- 将磁盘扫描和路径身份集中到 `rua-lsp::workspace_loader`。
- registry 分别保存原始 URI、canonical disk path 和 root-relative logical
  `VfsPath`，不能把物理路径直接当模块路径。
- 映射 normalized path/URI <-> stable FileId；为每个 workspace folder、每个
  library/mount、std 和 virtual 输入分配独立 SourceRootId。
- registry 能从 request URI 得到 `(ProjectId, FileId)`；shared library FileId 的
  查询语义由发起请求的 ProjectId 决定，不能反向猜一个全局 project。
- 支持 multi-root workspace、`.rua`、`.ruai`、nested modules、symlink
  canonicalization 和 `untitled:` virtual document。
- 支持 `workspace/didChangeWorkspaceFolders` 的 add/remove，不要求重启 server。
- 定义 open overlay 生命周期：
  - open：磁盘文件 text 被 buffer 覆盖；
  - change：只更新相同 FileId text/version，拒绝或忽略倒退 version；
  - close：存在磁盘文件时恢复磁盘 text，否则移除 virtual file；
  - create/change/delete watcher：批量生成 Change。
- library reload 保持未删除路径的 FileId 稳定，移除 stale root/file。

验证命令：

```sh
cargo test -p rua-lsp --features lsp workspace_loader
cargo test -p rua-lsp --features lsp document_lifecycle
cargo test -p rua-lsp --features lsp multi_root
```

退出条件：

- loader 是 LSP 唯一磁盘 IO/路径注册入口。
- open/change/close/watcher/config 的 FileId 和 text 行为有完整测试。
- 同一路径不会同时出现在 legacy/new registry 中形成双身份。

建议提交：`5.1: build stable LSP workspace inputs`

### Step 5.2：建立 authoritative AnalysisHost 与临时 legacy mirror

改动范围：

- `Server` 持有 `AnalysisHost`、file/root registry 和 document versions。
- initialize/index、didOpen/change/close、watcher、configuration 全部提交
  `Change` batch。
- 每个 native request 开头获取一次 snapshot，并传给 query/adapter。
- semantic tokens 和 diagnostics 删除 per-request 临时 host，改用长期 host；其
  feature 的最终行为切换仍分别在 5.7/5.8 验收。
- 5.3–5.8 尚未迁移的 production handlers 可暂时使用 `LegacyQueryMirror`：
  - mirror 由同一个 file/root registry 和输入事件喂入；
  - 使用 no-IO loader，不自行扫描磁盘、分配 FileId 或决定 root priority；
  - 只为尚未迁移的 handler 保留旧 query cache；
  - 每迁移一个 handler 即删除对应 mirror 调用，5.10 物理删除 mirror。
- 将 `AnalysisInputs` 的私有 AnalysisHost/FileId allocator 在本步删除；library
  scanner 暂时降为只输出 physical/logical source specs 的 loader helper，所有
  ID 和 Change 都由统一 registry/host 管理。
- 本阶段保持单线程 host 所有权。当前 `Rc<BaseDb>` 和 rowan red node 不是 Send；
  如以后引入 worker，必须把整个 host 固定在单一 worker 线程，通过 Change 和
  protocol-neutral POD 通信，禁止跨线程发送 Analysis snapshot/red node。

验证命令：

```sh
cargo test -p rua-lsp --features lsp analysis_host_lifecycle
cargo test -p rua-lsp --features lsp snapshot_per_request
cargo test -p rua-lsp --features lsp unsaved_buffer
```

退出条件：

- production `Server` 的每个文件变更只提交一次 authoritative AnalysisHost；
  legacy mirror 只消费同一输入事件，不拥有独立 IO/identity/config policy。
- request 不从磁盘重新读取已打开文档。
- semantic/diagnostic 热查询复用 BaseDb cache。
- 不存在把 non-Send snapshot 临时塞进后台线程的实现。
- 尚未迁移的现有 handlers 继续可用，中间提交不造成功能缺失。

建议提交：`5.2: establish authoritative LSP analysis state`

### Step 5.3：迁移 document/workspace symbols

改动范围：

- document symbols 使用 `Analysis::document_symbols`。
- 实现/接入 `workspace/symbol`，使用 `Analysis::workspace_symbols`。
- 保持 hierarchy、container、detail、docs 和 range parity。
- formatting 继续调用纯 `rua-syntax` formatter，不迁入 semantic analysis。

验证命令：

```sh
cargo test -p rua-lsp --features lsp symbol_migration
cargo test -p rua-analysis document_symbols
cargo test -p rua-analysis workspace_symbols
```

退出条件：

- symbol handler 不访问 legacy Workspace。
- current document symbol golden 无回退；workspace symbol capability 有快照。

建议提交：`5.3: migrate LSP symbols to native analysis`

### Step 5.4：迁移 goto definition 与 hover

改动范围：

- goto/hover 统一调用 native `FilePosition` query。
- 覆盖 local、item、path、module、field、method、builtin、cross-file 和 `.ruai`。
- builtin 无源码定义时 hover 可返回结果，goto 明确返回 None。
- LSP adapter 只负责 FileId/URI/range/MarkupContent 转换。

验证命令：

```sh
cargo test -p rua-lsp --features lsp navigation_migration
cargo test -p rua-lsp --features lsp hover_migration
cargo test -p rua-analysis ide_navigation
```

退出条件：

- goto/hover production path 不访问 `rua_syntax::workspace`。
- cross-file、member、closure param 和 `.ruai` parity snapshots 通过。

建议提交：`5.4: migrate LSP navigation and hover`

### Step 5.5：迁移 references、prepare rename 与 rename

改动范围：

- references 使用 native binding/definition reference index。
- includeDeclaration 行为、排序和去重固定。
- prepare rename 与 rename 共用同一 native target validation。
- rename 输出跨 FileId SourceChange，由 LSP 转 WorkspaceEdit。
- Library/Std target 返回明确 readonly error；member rename 只有在定义集合可证明
  完整时开放，否则显式拒绝。

验证命令：

```sh
cargo test -p rua-lsp --features lsp references_migration
cargo test -p rua-lsp --features lsp rename_migration
cargo test -p rua-analysis ide_references_rename
```

退出条件：

- local/item/cross-file/closure param references 和 rename parity 通过。
- `.ruai` rename 使用专用 readonly 错误，不再复用 InvalidName。
- 不生成重叠、重复或只读文件 edit。

建议提交：`5.5: migrate LSP references and rename`

### Step 5.6：迁移 completion

改动范围：

- completion context 由 syntax structural context + native semantic facts组成。
- 支持 lexical locals、items、keywords、builtins、module path、enum variant、
  associated item、field/method 和 `.ruai` member。
- 保持 member/path context 抑制无关 globals 的现有 UX。
- detail/docs/sort/filter/insert text 由 protocol-neutral candidate 转成 LSP item；
  snippet 只在 LSP adapter 添加。
- Unknown receiver 返回空 member result，不回退猜测 globals。

验证命令：

```sh
cargo test -p rua-lsp --features lsp completion_migration
cargo test -p rua-analysis ide_completion
cargo test -p rua-syntax --test ide_goldens ide_snapshot_golden -- --exact
```

退出条件：

- 当前 local/member/trait/module/closure/`.ruai` completion snapshots 通过。
- completion handler 不调用 compiler member completion façade。
- candidate 顺序确定且无重复。

建议提交：`5.6: migrate LSP completion to native analysis`

### Step 5.7：迁移 diagnostics

改动范围：

- publish diagnostics 只调用当前 Analysis snapshot 的 native diagnostics。
- 正确携带 document version；close 清空，change 只发布最新 version。
- module/library change 后重算并发布受影响的已打开文件。
- 删除 `reconciled_diagnostics_for` 和 production `ruac::check_diags`。
- compiler parity 留在 `rua-analysis` tests。

验证命令：

```sh
cargo test -p rua-lsp --features lsp diagnostics_migration
cargo test -p rua-lsp --features lsp diagnostic_versions
cargo test -p rua-analysis diagnostic_parity
```

退出条件：

- 无重复 fast/compiler diagnostic。
- stale request/change 不覆盖新版本 diagnostics。
- LSP 普通依赖不再需要 `ruac` 诊断 API。

建议提交：`5.7: publish native analysis diagnostics`

### Step 5.8：迁移 semantic tokens

改动范围：

- semantic tokens 直接使用长期 Analysis snapshot。
- token facts 来自 native definition/type resolution，不扫描 source string 临时建库。
- 保持 closure parameter、method、range 的既有 token；逐步扩 item/type/field/
  enum/trait 等类别时同步 legend 和 snapshot。
- full token result 保持稳定排序；是否增加 resultId/delta 由本步 profiling 决定，
  不作为迁移阻塞条件。

验证命令：

```sh
cargo test -p rua-lsp --features lsp semantic_token_migration
cargo test -p rua-analysis semantic_tokens
cargo test -p rua-analysis --test closure_iterator_ide
```

退出条件：

- semantic token request 不创建新 AnalysisHost。
- UTF-8 byte range -> UTF-16 token length/column 有多字节测试。
- token legend 与实际 token_type index 一致。

建议提交：`5.8: serve semantic tokens from native snapshots`

### Step 5.9：合并 external library 配置与 watcher

改动范围：

- 将 `AnalysisInputs` 的 library scanner 合并到 workspace loader/root registry。
- 本步合并 5.2 暂留的 library config/scanner helper；不得重新引入私有 host 或
  FileId allocator。
- 每个 configured directory/single-file mount 保留独立 physical base、logical
  module base 和 SourceRootId；不再把所有 declaration 塞进一个共享 root。
- 明确定义跨 root module candidate：workspace importer 的目录相对查找不能错误
  地套用到 library root。至少覆盖 `workspace/src/main.rua` +
  `library/foo.ruai`、nested library module、single-file named mount。
- 固定 workspace > library > std root priority，以及同一 root 内 `.rua` >
  `.ruai` candidate priority。
- VS Code extension contributes `rua.library` 和 `rua.libraryMounts` settings。
- LanguageClient 初始化和 didChangeConfiguration 明确发送 Rua settings。
- 为配置的目录/单文件 mount 建 watcher；create/change/delete 形成增量 Change。
- library/std 文件保持 readonly，可被所有 query 使用。
- 配置 reload 不清空 workspace/open documents。

验证命令：

```sh
cargo test -p rua-lsp --features lsp library_configuration
cargo test -p rua-lsp --features lsp library_watcher
cargo test -p rua-analysis module_resolution
cd editors/vscode && npm run check-types
```

退出条件：

- 外部 `.ruai` 可在真实 LSP flow 中 completion/hover/goto/references。
- workspace `src/` 布局和 library root logical path 不会错位。
- 文件变更无需重启 server 即生效。
- rename 不编辑 library/std。

建议提交：`5.9: integrate external library roots and watchers`

### Step 5.10：删除 legacy Workspace 与 transition production path

改动范围：

- 删除 `LegacyQueryMirror`，包括旧
  `Server.workspace: rua_syntax::workspace::Workspace<DiskLoader>`。
- 删除或合并 5.9 后残留的 `AnalysisInputs` config/scanner helper；不得保留第二个
  host、registry 或 file text cache。
- LSP production source 不再引用 `rua_syntax::analysis/workspace/nameres`。
- 删除 `rua-syntax::analysis` / `workspace` 中无消费者的 semantic façade；纯 syntax
  helper 可保留并改名到明确模块。
- 删除 `rua-syntax::transition` 和所有 production compiler bridge。
- 将 `ruac` 从 `rua-syntax` 普通 dependencies 移到 parity 所需的
  dev-dependencies；从 `rua-lsp` dependencies/dev-dependencies 完全删除。
- 增加 source/dependency boundary CI gate。

验证命令：

```sh
! rg -n "rua_syntax::(analysis|workspace|nameres)" crates/rua-lsp/src crates/rua-analysis/src
! rg -n "crate::transition|mod transition" crates/rua-syntax/src
! cargo tree -p rua-syntax -e normal | rg "ruac"
! cargo tree -p rua-lsp --features lsp -e normal | rg "ruac"
cargo tree -p ruac -e normal
cargo test --workspace --all-features
```

退出条件：

- LSP 只有一个 AnalysisHost 和一个 file/root registry。
- `rua-syntax` 普通依赖不含 `ruac`。
- `rua-lsp` 普通依赖不含 `ruac`，也不调用旧 semantic Workspace。
- `ruac` 仍不依赖 rowan、analysis 或 LSP crates。

建议提交：`5.10: remove legacy LSP analysis pipeline`

### Step 5.11：增量、压力与错误恢复验收

改动范围：

- rapid didChange：连续版本只发布最新诊断/semantic result。
- unsaved sibling/module edit 会影响另一文件的 hover/completion/goto/diagnostics。
- large workspace：首次索引和热查询不重复全库 parse/infer。
- multi-root：同名 module/root priority 不串 workspace。
- malformed edit：每个 cursor query 不 panic，修复文本后结果恢复。
- library reload、file delete/recreate、symlink 和 untitled 生命周期。
- workspace folder 动态 add/remove 不污染其他 root。
- server shutdown/restart 无遗留进程或阻塞 request。
- 在固定 synthetic fixture 和 release build 上记录 p95：warm hover/completion
  < 50 ms、changed-file diagnostics < 200 ms、普通 workspace references < 300 ms。
  CI 以结构性 query counter 为硬门禁；时间阈值作为可重复 benchmark/发布门禁，
  避免共享 runner 抖动造成随机失败。

验证命令：

```sh
cargo test -p rua-lsp --features lsp incremental_stress
cargo test -p rua-lsp --features lsp malformed_edit_recovery
cargo test -p rua-lsp --features lsp multi_root
cargo test -p rua-analysis large_workspace_smoke -- --ignored
```

退出条件：

- 无 stale diagnostics、FileId 漂移、跨 root 污染或 panic。
- query counters 证明热路径复用缓存。
- 性能若相对 Step 4B.10 基线或上述发布阈值退化，必须有 profiling 记录和修复提交。

建议提交：`5.11: validate incremental LSP behavior`

### Step 5.12：VS Code E2E、CI 与文档收尾

改动范围：

- 构建 `rua-lsp` 和 extension，增加可重复 VS Code Extension Host smoke。
- 使用真实 Extension Host test runner（例如 `@vscode/test-electron`），不以纯
  TypeScript unit test 代替编辑器集成验证。
- 在 extension package scripts 中增加 `test:e2e`；Linux CI 使用 xvfb 运行。
- 覆盖 activation、open/change/save/close、diagnostics、completion、hover、goto、
  references、rename、format、semantic tokens、library settings、restart command。
- CI 执行 Rust workspace tests、严格 Clippy、TypeScript typecheck/build；可用环境下
  通过 headless Extension Host 跑 smoke。
- 覆盖 `rua.library` / `rua.libraryMounts` 配置同步、watched-file notification 和
  workspace-folder add/remove。
- 更新 extension README、settings、troubleshooting 和测试步骤。
- 更新 `tests/golden/COVERAGE.md`、架构状态和本计划完成记录。

验证命令：

```sh
cargo build -p rua-lsp --bin rua-lsp --features lsp
cargo test --workspace --all-features
cargo clippy -p rua-analysis --all-targets --no-deps -- -D warnings
cargo clippy -p rua-lsp --features lsp --all-targets --no-deps -- -D warnings
cargo clippy -p rua-syntax --all-targets --no-deps -- -D warnings
cd editors/vscode
npm ci
npm run check-types
npm run compile
npm run package
npm run test:e2e
# Linux CI:
xvfb-run -a npm run test:e2e
```

退出条件：

- Extension Development Host 中所有现有功能通过 smoke checklist。
- `.vsix` 可构建，内含 extension bundle，不内嵌平台错误的 server binary。
- CI 对 dependency boundary、Rust、TypeScript 和 golden drift 都有门禁。
- Phase 4B/5 文档状态改为完成。

建议提交：`5.12: complete native analysis LSP migration`

## 7. Feature 迁移矩阵

每迁移一行，必须同时满足 native unit、parity、protocol 和 golden 四层中适用的
测试；不能只把 handler 编译通过。除纯 formatting 外，所有 semantic query 都
必须携带显式 ProjectId/ProjectPosition。

| Feature | Native query | Legacy oracle | Protocol snapshot | Required edge cases |
| --- | --- | --- | --- | --- |
| Diagnostics | `Analysis::diagnostics` | compiler/golden test only | publishDiagnostics | version、clear、dependency change |
| Hover | `Analysis::hover` | legacy hover golden | Hover response | local、item、member、builtin、ruai |
| Goto | `Analysis::goto_definition` | legacy goto golden | Location | local、cross-file、member、ruai |
| Completion | `Analysis::completions` | legacy completion golden | CompletionList/Array | local、path、member、Unknown |
| References | `Analysis::references` | legacy references golden | Location[] | includeDeclaration、cross-file |
| Rename | `Analysis::rename` | legacy rename golden | WorkspaceEdit/error | readonly、invalid name、overlap |
| Document symbols | `Analysis::document_symbols` | existing symbol golden | nested symbols | docs、container、range |
| Workspace symbols | `Analysis::workspace_symbols` | Phase 3 tests | workspace/symbol | query filter、multi-root |
| Semantic tokens | `Analysis::semantic_tokens` | Phase 4A snapshot | full tokens | UTF-16、legend、declaration |
| Formatting | `rua-syntax` formatter | formatter golden | TextEdit[] | parse error、idempotence |

## 8. 总体验收门禁

Phase 5 只有同时满足以下条件才算完成：

### 8.1 行为

- 当前 VS Code 用户可见能力无静默回退。
- local/item/member/cross-file/`.ruai`/closure/iterator 查询均走 native analysis。
- Unknown 和 malformed source 行为有明确 snapshot。
- library/std readonly 行为贯穿 references/rename/code action 边界。

### 8.2 架构

- LSP 只有一个长期 `AnalysisHost`。
- LSP adapter 是唯一磁盘、URI、watcher 和 LSP type 所有者。
- `rua-analysis` 无磁盘 IO、LSP protocol type 和普通 `ruac` 依赖。
- `rua-syntax` 无普通 `ruac` 依赖和 transition production 模块。
- `ruac` 保持 rowan-free / analysis-free / LSP-free。

### 8.3 增量

- file identity 在完整 document lifecycle 中稳定。
- 无关文件编辑不重算无关 body/inference。
- module/public/library 变化正确失效依赖者。
- request 使用一致 snapshot，无 stale version 输出。

### 8.4 质量

- `cargo test --workspace --all-features` 通过。
- 三个迁移 crate 的严格 Clippy 通过。
- parser/type/diagnostic parity 通过或只有命名的 expected differences。
- golden、protocol snapshots、VS Code typecheck/build/package 通过。
- worktree clean，每个 Step 有独立提交和验证记录。

## 9. 风险与应对

| 风险 | 表现 | 应对 |
| --- | --- | --- |
| native typeck 覆盖不足 | completion/hover 比 legacy 少 | Unknown 优先；先补 parity fixture，再扩规则，不调用旧 façade fallback |
| 双 parser range 漂移 | goto/token range 偏移 | 继续复用 parser/range conformance；SourceMap 只认当前 source byte range |
| 双状态生命周期错误 | unsaved text 与磁盘结果不一致 | Step 5.2 前不迁 handler；完成后 production 只保留 AnalysisHost |
| FileId 不稳定 | close/reload 后 references 丢失 | path registry 独立测试，配置 reload 复用现有 ID |
| definition ID 依赖插入顺序 | cache 重建后引用指向另一项 | Body 前先稳定 root/module/item identity 和 signature fingerprint |
| reference index 全库扫描 | 大 workspace rename 卡顿 | 建按 name/DefId 索引和依赖失效计数，先测再优化 |
| false-positive diagnostics | 编辑中持续报错 | Missing/Unknown 抑制 cascade；compiler accept corpus 为硬门禁 |
| `.ruai` 可查不可改边界泄漏 | rename 编辑依赖声明 | SourceRoot readonly 在 SourceChange 构造层统一检查 |
| multi-root 污染 | 同名模块跳到错误 workspace | 每个 query 显式 root_file/root_id；禁止全局 basename lookup |
| semantic token UTF-16 错位 | 非 ASCII 文本颜色错位 | byte range 与 UTF-16 转换分层，多字节 protocol snapshot |
| 一次删除过多 legacy | 难定位功能回退 | handler 逐项迁移，每项迁移后禁止 fallback，最后单独删除旧管线 |
| non-Send red node 被跨线程传递 | 编译受阻或引入不安全共享 | Phase 5 保持单线程 host；未来 worker 拥有完整 host，只传 POD/Change |

## 10. 明确非目标

本阶段不做：

- 不合并 `ruac` parser 与 rowan parser。
- 不重写 `ruac` codegen 或改变 Phase 4A fused iterator contract。
- 不因“可能更快”直接引入 salsa、异步 runtime 或多线程 query executor。
- 不在本阶段加入后台 compiler oracle；compiler 只参与测试 parity。
- 不实现完整 Rust borrow checker、Fn/FnMut/FnOnce 或 iterator escape runtime。
- 不把 formatter 移入 `rua-analysis`。
- 不实现 source map/trace。
- 不把 inlay hints、code actions、signature help 等新功能混进迁移 Step；它们在
  native query 和 LSP 单状态稳定后单独规划。
- 不在 `.vsix` 中默认捆绑未经多平台发布流程验证的 `rua-lsp` binary。

## 11. 关键路径

必须按以下依赖顺序推进：

```text
native: 4B.0 -> 4B.1 -> 4B.2 -> 4B.3 -> 4B.4 -> 4B.5 -> 4B.6
                                                            |
         4B.7 -> 4B.8 -> 4B.9 -> 4B.10 -> 4B.11 ----------+
                                                            |
lexer:  4B.0 -------------------------------> 4B.12 -------+
                                                            v
LSP:                                  5.0 -> 5.1 -> 5.2
                                                      |
                         5.3 -> 5.4 -> 5.5 -> 5.6 -> 5.7 -> 5.8
                                                      |
                                    5.9 -> 5.10 -> 5.11 -> 5.12
```

允许在不共享 production 文件的前提下并行：

- 4B.3 body lowering 与 5.0 protocol harness。
- 4B.6 member fixtures 与 4B.8 diagnostic parity harness。
- 4B.12 syntax lexer 与 4B.8 native diagnostics。
- 5.3 symbols adapter 与 5.1 loader edge-case tests。
- 5.11 stress fixtures 与 5.12 VS Code test harness 骨架。

禁止并行：

- 4B.4/4B.5 对同一 Body/Inference 数据结构的定义。
- 5.2 Server state 改造与任一 feature handler migration。
- 5.10 legacy 删除与尚未完成的 handler migration。

## 12. 每步完成记录

每个提交说明或施工日志使用：

```text
Step:
Scope:
Changed files:
Native tests:
Parity tests:
Protocol/golden tests:
Dependency checks:
Known differences:
Next step:
```

第一步从 **4B.0** 开始。不要先修改 `Server` 字段；在 native query contract、
body/type capability 和协议 parity baseline 就绪之前，替换主状态只会把架构迁移
变成功能回退调试。
