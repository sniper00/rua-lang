# Rua Native Analysis 与 LSP 全量迁移施工计划

> 状态：施工中（Step 4B.0-4B.6 已完成；下一步 Step 4B.7）。
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

这是 Phase 4B 中最复杂的一步，需要同时处理三个交叉关注点：pattern narrowing、
closure type inference、和 iterator type inference。当前
`rua-analysis::ide::closure_iterator` 仍通过 `rua_syntax::analysis::Analysis`
调用 compiler-backed 类型查询，本步必须完全替换。

#### 4B.7a：Pattern narrowing 基础设施

改动文件：`crates/rua-analysis/src/hir/infer.rs`（扩），新建
`crates/rua-analysis/tests/pattern_narrowing.rs`。

- 在 `infer.rs` 中新增 `NarrowingContext`：记录当前 branch 中哪些 binding 已被
  narrowing 到更具体类型。
  ```rust
  struct NarrowingContext {
      /// binding -> narrowed type (before entering arm/body)
      narrowed: HashMap<BindingId, Ty>,
      /// bindings that become unavailable (exhausted enum variants)
      exhausted: HashSet<BindingId>,
  }
  ```
- 实现 `narrow_pattern(scrutinee: Ty, pattern: &Pat, ctx: &mut NarrowingContext) -> Vec<(BindingId, Ty)>`。
  对每种 pattern 形态：
  - `Path` pattern（如 `Option::Some(x)`）：scrutinee 是 `Option<T>` 时，
    narrowing 为 `T` 并绑定到 inner binding；同时标记 scrutinee binding
    为 exhausted（该分支不匹配时，后续 `else`/`_` 中 scrutinee 变为 `None`）。
  - `StructVariant` pattern（如 `Shape::Rect { w, h }`）：类似 path，按 variant
    定义 narrow 各 field。
  - `TupleVariant` pattern（如 `Message::Data(a, b)`）：类似，narrow 各位置。
  - `Literal` / `Range` pattern：scrutinee 是 `i64`/`char` 等 primitive 时，
    不做 narrowing（仅用于 exhaustiveness 检查，留给后续 step）。
  - `Wildcard` / `Binding`：全量绑定，无 narrowing。
  - `Or` pattern：每个 sub-pattern 引入相同 binding 集合，类型取 join。
- 在 `infer_expr` 中为 `Match` 接入 narrowing：
  - 推断 scrutinee 类型。
  - 对每个 arm：clone current NarrowingContext，apply `narrow_pattern`，
    在 narrowed context 中推断 arm body。
  - Arm body 类型取所有 arm 的 join。
- 在 `infer_expr` 中为 `IfLet` / `WhileLet` 接入 narrowing：
  - 对 then/loop body 使用 narrowed context。
  - 对 else/后续代码使用 exhausted context。

验证：

```sh
cargo test -p rua-analysis pattern_narrowing
```

fixture 覆盖：
- `match x { Some(v) => v, None => 0 }` 推断 arm 类型。
- `if let Some(v) = opt { v } else { 0 }` narrowing。
- `while let Some(v) = iter.next() { v }` 循环内 narrowing。
- nested pattern `Some(Ok(x))`。
- `match shape { Rect { w, h } => w + h, Circle(r) => r * 2.0 }` 不同 variant
  提供不同 field。
- 不完整 pattern（只有 `Some` arm，缺 `None`）：不 panic，结果为 Unknown。

#### 4B.7b：原生 closure type inference

改动文件：`crates/rua-analysis/src/hir/infer.rs`（扩），
`crates/rua-analysis/tests/closure_type_parity.rs`。

- 在 `infer.rs` 中实现 `infer_closure(expr: ExprId, body: &Body, ...) -> Ty`：
  - 从调用上下文收集 expected parameter types（双向传播）。
  - 推断闭包 body 的返回类型。
  - 构造 `Ty::Callable(CallableTy { params, ret, captures })`。
  - `captures` 记录闭包内引用的外层 local binding id 及其可变性。
- 重写 `crates/rua-analysis/src/ide/closure_iterator.rs` 中的
  `closure_parameters()`：
  - 删除 `rua_syntax::analysis::Analysis::new(&text)` 调用。
  - 改为：parse → find ClosureExpr → lowered Body → `BaseDb::infer()` →
    提取 parameter types 为 `ClosureParameterInfo`。
  - 类型 display 使用 `Ty` 的稳定 `Display` 实现。
- 重写 `semantic_tokens()` 中的 closure parameter 部分：
  - 删除 `rua_syntax::nameres::references_at` 调用。
  - 改为通过 `Semantics::local_references()` 获取闭包参数的 definition +
    references。
  - 保持 `SemanticTokenKind::Parameter` + `DECLARATION` modifier 的
    既有行为。

验证：

```sh
cargo test -p rua-analysis closure_type_parity
cargo test -p rua-analysis --test closure_iterator_ide
```

#### 4B.7c：原生 iterator type inference

改动文件：`crates/rua-analysis/src/hir/infer.rs`（扩），
`crates/rua-analysis/tests/iterator_type_parity.rs`。

- 在 `infer.rs` 中新增 iterator type 推导，**不引入 compiler `IterPlan`**：
  - `.iter()` on `Vec<T>` → `Iterator<Item = &T>`。
  - `.into_iter()` on `Vec<T>` → `Iterator<Item = T>`。
  - `a..b` / `a..=b` → `Iterator<Item = i64>`。
  - `.map(|x| expr)` → `Iterator<Item = typeof(expr)>`。
  - `.filter(...)` → 保持 `Item` 类型不变。
  - `.filter_map(|x| expr)` → 从 expr 提取 `Option<U>` 的 `U`。
  - `.enumerate()` → `Iterator<Item = (i64, T)>`。
  - `.take(n)` / `.skip(n)` → 保持 `Item` 类型不变。
  - `.collect::<Vec<_>>()` → `Vec<Item>`。
  - `.fold(init, |acc, x| expr)` → `typeof(expr)`。
  - `.count()` → `i64`。
  - `.any(|x| expr)` / `.all(|x| expr)` → `bool`。
  - `.find(|x| expr)` → `Option<Item>`。
- Adapter 类型推导规则：
  - 已知 receiver iterator 的 `Item` 类型，从闭包/参数即可算出
    adapter 的新 `Item` 类型。
  - 遇到无法确定的闭包返回类型 → `Unknown`，不阻断 chain 后续推导。
- 每个已知的 builtin iterator method 在 `MemberIndex` 中拥有
  `MemberKind::IteratorAdapter { item_ty: ..., output_ty: ... }` 条目，
  在 4B.6 的 builtin metadata 中补充（本步只需补充调用签名，不需要新 DefId）。

验证：

```sh
cargo test -p rua-analysis iterator_type_parity
```

fixture 覆盖：
- `.iter().map().collect()` 完整 chain 的中间类型。
- `.iter().filter().count()` — consumer 返回 `i64`。
- `.iter().filter_map().collect()` — Option 解包。
- `.iter().enumerate().fold()` — tuple item。
- 闭包内引用外层变量（capture），但仍能推断类型。
- 类型不闭合的 chain（如未知 adaptor）→ 后续 item 为 Unknown。

#### 4B.7d：删除 transition bridge 调用

- `rg "rua_syntax::analysis" crates/rua-analysis/src` 返回空。
- `rg "rua_syntax::nameres" crates/rua-analysis/src` 返回空。
- `cargo test -p rua-analysis --test closure_iterator_ide` 全部通过，
  不再依赖任何 `rua_syntax::analysis` / `rua_syntax::nameres` 路径。

提交拆分建议：
1. `4B.7a: implement pattern narrowing for match/if-let/while-let`
2. `4B.7b: infer closure types natively`
3. `4B.7c: infer iterator adapter and consumer types`
4. `4B.7d: remove rua_syntax analysis bridge from closure_iterator`

建议合并提交：`4B.7: infer closures iterators and narrowed patterns`

### Step 4B.8：实现 native diagnostics 与 reconciliation policy

目标：将 parse errors、name resolution errors、type errors 三条诊断来源合并
为统一的 native diagnostic pipeline，并用 structured `DiagnosticCode` 替代
当前 bare string messages。

#### 4B.8a：稳定 DiagnosticCode 枚举

改动文件：`crates/rua-analysis/src/diagnostic/mod.rs`（扩）。

定义 `DiagnosticCode` 枚举，三组：

```rust
pub enum DiagnosticCode {
    // Parse errors (0001–0099)
    ParseUnexpectedToken = 1,
    ParseUnterminatedString = 2,
    ParseUnterminatedComment = 3,
    ParseExpectedItem = 4,
    ParseMissingDelimiter = 5,

    // Name resolution errors (0100–0199)
    NameUnresolved = 100,
    NameDuplicateDefinition = 101,
    NamePrivateAccess = 102,
    NameModuleNotFound = 103,
    NameAmbiguousImport = 104,

    // Type errors (0200–0299)
    TypeMismatch = 200,
    TypeExpectedBool = 201,
    TypeNotCallable = 202,
    TypeArgumentCount = 203,
    TypeNotIterable = 204,
    TypeInvalidUnary = 205,
    TypeInvalidBinary = 206,
    TypeUnsatisfiedTraitBound = 207,
    TypeUnknownField = 208,
    TypeUnknownMethod = 209,
    TypeMissingMatchArm = 210,
}
```

每个 code 携带稳定的字符串标识（如 `"E0001"`）和 `DiagnosticSeverity`。

#### 4B.8b：Diagnostic 数据模型增强

改动文件：`crates/rua-analysis/src/diagnostic/mod.rs`（扩）。

- `Diagnostic` 已有 `code`, `severity`, `primary: FileRange`, `message: String`。
  增加 `related: Vec<DiagnosticRelated>`（每个 related 有 `FileRange` + `message`）。
- 增加 `source: DiagnosticOrigin`（`Parse` | `Name` | `Type` | `Structural`）
  用于去重和抑制。
- 定义 `normalize_diagnostics(diags: &mut Vec<Diagnostic>)`：
  - 按 `(file, primary_range.start, code)` 三元组去重。
  - 同一位置同一 code 只保留 severity 最高的。
  - TypeMismatch 跨多个 arm 时，按 `(expected_type, actual_type)` 合并。
- 定义 `suppress_cascade(diags: &mut Vec<Diagnostic>)`：
  - parse error 所在行之后的 name/type error 降级或移除（避免 recovery 噪声）。
  - Unknown 推断之上的 mismatch 抑制（不在 Unknown 上叠加虚假错误）。
- 定义 `reconcile_diagnostics(fast: Vec<Diagnostic>, compiler: Vec<Diagnostic>) -> Vec<Diagnostic>`
  （已有骨架，本步补全具体语义）：
  - compiler diagnostic 优先级高于同位置同 code 的 fast diagnostic。
  - 本函数仅在 parity test 中使用，不进 production hot path。

#### 4B.8c：各层 diagnostic 产出

- **Parse diagnostics**（`crates/rua-analysis/src/diagnostic/mod.rs`）:
  从 `Parse::errors()` 转换为 `Diagnostic`，每个 `ParseError` 已有
  `message` + `range`。映射 `ParseError` 的 message pattern 到对应
  `DiagnosticCode`（如包含 "unterminated" → `ParseUnterminatedString`）。
- **Name diagnostics**（在 `DefMap::build` 中产出）:
  遍历 name resolution 过程，对 unresolved import、duplicate definition、
  private access 产出对应 `Diagnostic`。
  - `DefMap` 构建结果增加 `diagnostics: Vec<Diagnostic>`。
  - 收集入口：`BaseDb::name_diagnostics(file_id) -> Vec<Diagnostic>`。
- **Type diagnostics**（从 `InferenceResult` 提取）:
  `InferenceResult` 已有 `InferenceDiagnostic` 枚举。本步补充
  `InferenceDiagnostic` → `Diagnostic` 的转换（code 映射 + FileRange
  从 BodySourceMap 查询）。
  - 收集入口：`BaseDb::type_diagnostics(def_id) -> Vec<Diagnostic>`。
- **合并入口**：
  `BaseDb::all_diagnostics(file_id) -> Vec<Diagnostic>` =
  parse_diagnostics + name_diagnostics + 该文件所有 def_id 的 type_diagnostics。
  经 `normalize_diagnostics` + `suppress_cascade` 后输出。

#### 4B.8d：Compiler parity 策略

- `ruac::check_diags` 仅在 dev-dependency 测试中使用。
- 新建 `crates/rua-analysis/tests/diagnostic_parity.rs`：
  - 对 `tests/golden/compile-fail/*.rua` 每个文件，同时跑 native 和 compiler
    diagnostics。
  - Normalize 两边的 range/message，比较 diagnostic code 和 primary range。
  - 允许 message 文本差异（写入 expected-difference manifest），但 code 和
    primary range 必须一致。
  - 对 `tests/golden/compile-pass/*.rua`，native 不得产出 error 级别
    diagnostic（warning/hint 允许）。

验证命令：

```sh
cargo test -p rua-analysis diagnostic_parse
cargo test -p rua-analysis diagnostic_name
cargo test -p rua-analysis diagnostic_type
cargo test -p rua-analysis diagnostic_parity
```

退出条件：
- `Analysis::diagnostics(file_id)` 返回 parse + name + type 合并结果。
- 当前 `tests/golden/compile-fail/*.diag.golden` 可被 native diagnostics 覆盖。
- compile-pass corpus 无 native false-positive error。
- production code 不调用 `ruac::check_diags`（仅在 dev/parity 测试中）。
- cascade suppression 在连续语法错误时不产生数十条噪声诊断。

建议提交：`4B.8: produce native structured diagnostics`

### Step 4B.9：完成 Analysis IDE query façade

目标：在 `Analysis` 上暴露所有 LSP handler 所需的 protocol-neutral query，
使 Phase 5 的 handler 迁移可以机械地将 legacy `Workspace` 调用替换为
`Analysis` 调用。当前 `Analysis` 已有 `diagnostics`、`document_symbols`、
`workspace_symbols`、`closure_parameters`、`semantic_tokens` 和底层
`body`/`infer`/`semantics`。本步补齐缺失的 hover、goto、completions、
references、rename。

#### 4B.9a：hover

改动文件：`crates/rua-analysis/src/ide/mod.rs`（扩），可能新建
`crates/rua-analysis/src/ide/hover.rs`。

```rust
impl Analysis {
    pub fn hover(&self, position: ProjectPosition) -> Option<HoverResult>;
}
```

实现策略：
1. 先尝试 `Semantics::find_def_at` → 命中 item/definition，构造 hover：
   - `HoverResult { text: signature_display, range: definition.name_range() }`。
   - Signature display 来自 `ItemSignature` 的 stable formatter（不是 Debug）。
2. 若步骤 1 未命中，尝试 local resolution → 获取 binding 类型 → 显示
   `"let <name>: <type>"`。
3. 若步骤 2 未命中，尝试 member resolution（`.` 左侧有 receiver）：
   - 从 `Semantics::find_def_at` 的 `.` 前导 token 取 receiver 表达式的
     `BodySourceId::NameRef` → infer receiver type → `MemberIndex` 查 field/method。
   - Field hover：显示 `"<name>: <type>"`。
   - Method hover：显示完整 callable signature。
4. Builtin hover（`vec!`、`println!` 等）：从 `BuiltinType` metadata 直接构造。
5. 以上均未命中 → `None`（不返回 "Unknown" 字符串）。

HoverResult 的 `text` 字段为 `String`，上层 LSP adapter 负责转为
`MarkupContent`（当前 legacy 使用 `MarkupKind::PlainText`，Phase 5 时如需
Markdown 只在 adapter 层标记）。

#### 4B.9b：goto_definition

改动文件：`crates/rua-analysis/src/ide/mod.rs`（扩）。

```rust
impl Analysis {
    pub fn goto_definition(&self, position: ProjectPosition) -> Option<NavigationTarget>;
}
```

实现策略：
1. `Semantics::find_def_at` → item/definition → `NavigationTarget { file_id, range }`。
2. 若步骤 1 未命中，尝试 local resolution → binding definition range →
   `NavigationTarget`。
3. 若步骤 2 未命中，尝试 member resolution → member definition range →
   `NavigationTarget`。
4. Builtin/primitive → `None`（无源码定义位置）。
5. 跨文件/`.ruai` target 同样返回：`Definition` 已有完整的 `file_id` + `range`。

#### 4B.9c：completions

改动文件：`crates/rua-analysis/src/ide/mod.rs`（扩），可能新建
`crates/rua-analysis/src/ide/completion.rs`。

```rust
impl Analysis {
    pub fn completions(&self, position: ProjectPosition) -> Vec<CompletionItem>;
}
```

这是最复杂的 query。需要根据光标上下文分派：

1. **Path completion**（光标在 `::` 后，如 `std::|`）：
   - 解析当前 path prefix → 从 `DefMap` 的对应 module 枚举子项 →
     过滤 `pub` 可见性。
2. **Member completion**（光标在 `.` 后，如 `value.|`）：
   - 取 `.` 前的 receiver expression → infer type → `MemberIndex::members_for_type()`。
   - 返回 field + method candidates。
   - Unknown receiver → 空列表。
3. **Scope completion**（光标在 identifier 位置，不在 `::` 或 `.` 后）：
   - 合并 lexical locals + module items + keywords + builtins。
   - 按 `CompletionKind` 分组：`Keyword` > `Local` > `Item` > `Builtin`。
4. **Pattern completion**（在 `match` arm 或 `let` pattern 中）：
   - 如果 cursor 在 enum variant pattern 位置 → 枚举 scrutinee 类型的 variants。
   - 初期可以先返回空，不猜测。

每个 `CompletionItem` 包含：
- `label: String` — 显示文本。
- `kind: CompletionKind` — `Keyword | Local | Item | Field | Method |
  Module | Variant | Builtin`。
- `detail: Option<String>` — 类型签名或文档摘要。
- `insert: CompletionInsert` — `Simple(String)` 直接插入文本 |
  `Snippet(String)` 含 `$0`/`$1` 占位符（snippet 语法只在 LSP adapter 转为
  `InsertTextFormat::Snippet`）。

排序规则：
- 先按 `kind` 优先级排序（已在各分类中固定）。
- 同 kind 内按 `label` 字母序。
- 去重键：`(kind, label)`。

#### 4B.9d：references

改动文件：`crates/rua-analysis/src/ide/mod.rs`（扩）。

```rust
impl Analysis {
    pub fn references(
        &self,
        position: ProjectPosition,
        include_declaration: bool,
    ) -> Vec<ReferenceResult>;
}
```

实现策略：
1. `Semantics::find_def_at` → `Definition` → 从 reference index 查所有 use
   site → 每个 use site 为 `ReferenceResult { file_id, range, kind }`。
2. 若步骤 1 未命中 → `Semantics::local_references_at` → `FileRange` 列表 →
   `ReferenceResult`（`kind = ReferenceKind::Local`）。
3. 若步骤 2 未命中 → 尝试 member reference index（如果 member 定义可追踪）。
   初期 member reference 可以返回空，不猜测。
4. `include_declaration` 控制是否包含定义位置。
5. 结果按 `(file_id, range.start)` 排序并去重。
6. `ReferenceKind` 枚举：`Local | Item | FieldAccess | MethodCall`。

#### 4B.9e：rename

改动文件：`crates/rua-analysis/src/ide/mod.rs`（扩）。

```rust
impl Analysis {
    pub fn prepare_rename(&self, position: ProjectPosition) -> Option<RenameTarget>;
    pub fn rename(&self, position: ProjectPosition, new_name: &str) -> Result<SourceChange, RenameError>;
}
```

实现策略：
- `prepare_rename`：验证 cursor 在可重命名目标上。
  - 返回 `RenameTarget { range, name, kind }`，kind 为 `Local | Item | Member`。
  - LSP 用 range 做高亮，name 做默认输入。
  - readonly file → 返回 `None`（不做 prepare）。
- `rename`：
  1. `prepare_rename` → 确定 target。
  2. 检查 new_name 有效性（非空、非关键字、合法标识符）→ 否则
     `RenameError::InvalidName`。
  3. 检查 target 所在 file 是否 readonly → `RenameError::ReadOnly`。
  4. `references(position, include_declaration=true)` 获取所有 reference。
  5. 对每个 reference 构造 `FileEdit { file_id, edits: vec![TextEdit { range, text: new_name }] }`。
  6. 合并同 file 的 edits → `SourceChange`。
  7. 检查所有受影响的 file 是否为 readonly → 任一 readonly 即返回
     `RenameError::ReadOnly`（不部分编辑）。

`RenameError` 枚举：
```rust
pub enum RenameError {
    InvalidName(String),
    ReadOnly(String),
    Unsupported { reason: String },
}
```

#### 4B.9f：semantic_tokens 完善

本步不改变 `semantic_tokens` 的调用签名（已在 `Analysis` 上），但确保
4B.7 中重写的 `semantic_tokens()` 已完全不依赖 `rua_syntax::nameres`。
本步增加以下 token 类别（逐步扩，不要求全部一次完成）：

- `SemanticTokenKind::Function` — fn 定义和调用。
- `SemanticTokenKind::Struct` — struct 定义和构造表达式。
- `SemanticTokenKind::Enum` — enum 定义。
- `SemanticTokenKind::Interface` — trait 定义。
- `SemanticTokenKind::Type` — type 引用（参数类型、返回类型等）。

本步仅要求 legend 与实际 token 一致，不要求所有类别全覆盖。

验证命令：

```sh
cargo test -p rua-analysis ide_hover
cargo test -p rua-analysis ide_navigation
cargo test -p rua-analysis ide_completion
cargo test -p rua-analysis ide_references_rename
cargo test -p rua-analysis ide_symbols_tokens
```

退出条件：
- `Analysis` 上的 `hover`、`goto_definition`、`completions`、`references`、
  `prepare_rename`、`rename` 六个方法全部可用。
- 所有六个方法的返回值都是 protocol-neutral POD（无 LSP types、无 rowan node、
  无 `ruac` types）。
- 每个方法在 `FilePosition` 不存在/offset 越界/文件未加载时返回 `None`/空
  而非 panic。
- `completions` 结果排序确定且去重。
- `rename` 在 readonly file 上返回 `RenameError::ReadOnly`。
- 现有 `document_symbols`、`workspace_symbols`、`semantic_tokens` 继续可用。

建议提交：`4B.9: expose complete native IDE queries`

### Step 4B.10：补齐缓存、依赖失效与性能基线

这是决定 LSP 实际可用性的关键一步。当前 `invalidate_file` 对任何文件变更
都会全量清空 `DefMap` 和 `MemberIndex` 缓存。本步必须在测量基础上做
细粒度失效，而不是提前优化。

#### 4B.10a：建立性能测量基线

改动文件：`crates/rua-analysis/tests/cache_invalidation.rs`（新建），
`crates/rua-analysis/tests/dependency_invalidation.rs`（新建）。

- 建 synthetic workspace fixture：100 个 `.rua` 文件，每文件 5 个函数，
  含 struct/enum/trait/impl 定义和跨文件模块引用。
- 使用 test-only query counter（在所有 `BaseDb` cache 查询处增加
  `Cell<u64>` counter，或在测试 harness 中注入统计）记录：
  - 首次索引的 parse count、ItemTree lower count、DefMap build count、
    Body lower count、Inference count。
  - 单文件 body edit 后的各项重算 count。
  - trivia-only edit（插入注释/空格）后的各项重算 count。
  - signature change（修改函数返回类型）后的各项重算 count。
  - 模块增删后的各项重算 count。
- 不要求达到绝对时间阈值（CI 环境不稳定），但要求结构性断言：
  - body-only edit：只重算当前文件 parse/ItemTree/BodySourceMap + 当前
    def_id 的 body/inference，不重算其他文件的任何 cache。
  - trivia edit：不重算任何 DefMap/ItemTree/Body/Inference（仅 parse +
    source maps 刷新）。
  - signature change：沿依赖图传播到 import 该定义的文件的 DefMap，
    但不触发无关文件的 body/inference 重算。

#### 4B.10b：实现细粒度失效

改动文件：`crates/rua-analysis/src/db.rs`。

核心设计——用 **revision stamp** 替代全量清除：

```rust
struct BaseDb {
    // ... existing caches ...

    /// Monotonically increasing stamp bumped on any VFS write.
    global_revision: u64,

    /// file_id -> current file revision (bumped on set_file_text / remove_file).
    file_revisions: RefCell<HashMap<FileId, u64>>,

    /// Incremental parse: file_id -> (revision, parse). Replaced when revision changes.
    parse_cache: RefCell<HashMap<FileId, (u64, Arc<Parse<SourceFile>>)>>,

    /// Incremental ItemTree: same pattern.
    item_tree_cache: RefCell<HashMap<FileId, (u64, Arc<ItemTree>)>>,

    /// DefMap requires more nuance:
    ///   (root_file_id, root_revision) -> Arc<DefMap>
    /// Cleared only when root_file_id's file revision changes OR a dependency's
    /// public signature changes.
    def_map_cache: RefCell<HashMap<(FileId, u64), Arc<DefMap>>>,

    /// Body cache: DefId -> (file_revision, body, source_map).
    /// Keyed by file_revision because source ranges must match current text.
    body_cache: RefCell<HashMap<DefId, BodyCacheEntry>>,

    /// Inference cache: DefId -> InferenceCacheEntry.
    /// Depends on body semantic equality + resolution equality — not revision.
    /// Validated via Arc::ptr_eq on body + resolution + def_map + member_index.
    inference_cache: RefCell<HashMap<DefId, InferenceCacheEntry>>,

    /// MemberIndex: coupled to DefMap identity (Arc::ptr_eq).
    member_index_cache: RefCell<HashMap<(FileId, u64), MemberIndexCacheEntry>>,
}
```

失效规则表：

| 变更类型 | Parse | ItemTree | DefMap | Body | Inference | MemberIndex |
|----------|-------|----------|--------|------|-----------|-------------|
| trivia edit（仅空格/注释） | 重算本文件 | 不变（signature text 未变时指纹一致） | 不变 | source_map 重算（range 偏移），body 不变 | 不变（body Arc::ptr_eq） | 不变 |
| body-only edit（函数体） | 重算本文件 | 不变（item signature 不变） | 不变（无新公开签名） | 重算该 def_id | 重算该 def_id | 不变 |
| signature edit（函数返回类型/参数） | 重算本文件 | 重算本文件（item tree 中的 signature row 变了） | 依赖者重算 | 重算该 def_id | 重算该 def_id | 重算依赖 root |
| 新增/删除 item | 重算本文件 | 重算本文件 | 依赖者重算 | 仅新增的 lower | 仅新增的 infer | 重算依赖 root |
| 新增/删除 file module | 重算本文件 | 重算本文件 | 所有依赖 root 重算 | N/A | N/A | 所有依赖 root 重算 |
| `.ruai` 变更 | 重算本文件 | 重算本文件 | 所有 import 该 root 的 DefMap 重算 | N/A | N/A | 重算相关 root |
| 跨 root module add/remove | N/A | N/A | 重算 project DefMap | N/A | N/A | 重算 project |

关键实现细节：

- **DefMap 按 root file revision 缓存**：`DefMapKey::Implicit(root_file)`
  的 cache key 变为 `(root_file, file_revision(root_file))`。
  仅在 root file 的 revision 或该 root file 下的任何 dependency file
  的 revision 变化时重算。具体策略：
  - `DefMap::build` 过程中记录所有访问过的 `file_id`。
  - 当任何被依赖 file 的 file_revision 改变 → 清空对应的 `(root_file, _)` key。
  - 初期简化：只要 root file 或其任一被依赖文件 revision 变化，就重算
    整个 DefMap；不实现 module 粒度失效（成本在 10B.10d 中度量，若
    DefMap 重算时间在 100 文件以下 < 5ms，则无需进一步优化）。
- **Body source map 绑定 file_revision**：`BodySourceMap` 必须精确
  反映当前文本 byte offset。当 file_revision 变化时，即使 body
  semantic 未变，source map 也必须重算。
- **Inference 用 Arc::ptr_eq 验 semantic equality**：当前实现已使用
  此策略。本步验证它在 body semantic 不变时确实不重算。
- **MemberIndex 绑定 DefMap Arc identity**：当前实现已使用此策略。
  保持不变。

#### 4B.10c：snapshot 隔离测试

- 验证多个 `Analysis` snapshot 的独立性：
  - 创建 host，apply `Change A`，获取 `snapshot1`。
  - Apply `Change B` 到 host，获取 `snapshot2`。
  - `snapshot1.hover(pos)` 的结果仍反映 Change A 的状态。
  - `snapshot2.hover(pos)` 反映 Change B 的状态。
  - 同一个 `Arc<Parse>` 在 snapshot 间安全共享。

#### 4B.10d：smoke benchmark（被忽略的测试）

新建 `crates/rua-analysis/tests/large_workspace_smoke.rs`：

- 构造 100 文件、500 函数的 synthetic workspace（纯内存，无 IO）。
- 首次索引耗时记录（不计入 CI 硬门禁，仅人为对比）。
- hot query（连续 100 次 hover 在同一个解析完成的 workspace 上）的
  query counter 断言。
- 单文件连续 50 次编辑的 incremental 重算 count 断言。

验证命令：

```sh
cargo test -p rua-analysis cache_invalidation
cargo test -p rua-analysis dependency_invalidation
cargo test -p rua-analysis snapshot_isolation
cargo test -p rua-analysis large_workspace_smoke -- --ignored
```

退出条件：
- 单文件 body edit 不重算无关文件 body/inference。
- trivia-only edit 不重算 Def graph/inference，但 source maps 返回
  新 byte range。
- public/module/library change 会失效实际依赖者的 DefMap/MemberIndex。
- hot query 复用缓存（通过 query counter 断言）。

建议提交：`4B.10: bound native analysis invalidation`

### Step 4B.11：建立 native analysis 边界门禁

本步是纯验证+门禁步骤，不新增功能。目标是锁定 4B.7-4B.10 达成的边界。

改动范围：

- `crates/rua-analysis/Cargo.toml`：确保 `ruac` 在 `[dev-dependencies]` 中
  （parity tests），不在 `[dependencies]` 中。
- 新增 CI 脚本或 Makefile target 执行以下边界检查：
  ```sh
  # 禁止从 rua-analysis/src 调用旧 semantic façade
  ! rg -n "rua_syntax::(analysis|workspace|nameres)" crates/rua-analysis/src

  # 禁止 rua-analysis 普通依赖 ruac
  ! cargo tree -p rua-analysis -e normal --depth 0 | rg "ruac"

  # 禁止 rua-analysis 引入 lsp_types / lsp_server
  ! cargo tree -p rua-analysis -e normal --depth 0 | rg "lsp"
  ```
- `crates/rua-analysis/src/` 的 `mod` / `use` 声明审查：
  - 允许：`rua_syntax::{Parse, AstNode, SyntaxKind, SyntaxNode, SyntaxToken,
    parse_source_file, lex, ...}`（纯 structural / parsing）。
  - 允许：`rua_syntax::ast::{FnDecl, Block, Expr, Pattern, ...}`（typed
    AST view）。
  - 禁止：`rua_syntax::analysis`、`rua_syntax::workspace`、
    `rua_syntax::nameres`、`rua_syntax::symbols`（旧 semantic façade）。
  - 禁止：`rua_syntax::transition`（compiler bridge）。
  - 禁止：`ruac::*`（production 路径）。
- `cargo test -p rua-analysis` 全绿。
- `cargo test --workspace --all-features` 全绿。
- 确认 `rua-analysis` 中没有任何模块在 production 路径中依赖 compiler type。

退出条件：
- `rg "rua_syntax::(analysis|workspace|nameres)" crates/rua-analysis/src` 无输出。
- `cargo tree -p rua-analysis -e normal | rg "ruac"` 无输出（生产依赖）。
- Phase 5 可以迁移 handler，不再需要扩 transition API。

建议提交：`4B.11: enforce native analysis dependency boundary`

### Step 4B.12：替换 syntax lexer transition bridge

这是 Phase 4B 的最后一步，完成后 `rua-syntax` 的生产依赖不再需要 `ruac`。

改动文件（新建）：`crates/rua-syntax/src/lexer.rs`（已有骨架，需补全 native 实现）。

#### 4B.12a：分析当前 transition 调用点

当前 `rua_syntax/src/lexer.rs:25`：
```rust
pub fn lex(text: &str) -> Vec<LexToken> {
    crate::transition::lex(text)
}
```

当前 `crates/rua-syntax/src/transition.rs` 将 `ruac` tokenizer 的结果
转换为 `LexToken`，并在真实 token 之间补回 whitespace/comments trivia
以保证 gap-free byte coverage。

#### 4B.12b：实现 syntax-owned lexer

两种方案：

**方案 A（推荐）：在 `rua-syntax` 内自持 lexer。**
- 复制 `ruac` 的 `reader.rs` + `tokenize.rs` + `token.rs` 的核心逻辑到
  `rua-syntax` 内部（约 800 行），去掉 `ruac` 的 `SourceRange` 依赖，
  直接输出 `LexToken { kind: SyntaxKind, start: u32, len: u32 }`。
- 优点：彻底去除 `ruac` 依赖，lexer 不再涉 transition。
- 缺点：两套 lexer 的代码重复。通过 lexer conformance tests 防止漂移。

**方案 B：提取共享 `rua-lex` crate。**
- 将 `ruac` 的 `reader.rs` + `tokenize.rs` + `token.rs` 提取为新的
  workspace member `crates/rua-lex`，不含任何 `ruac` 特有类型。
  `ruac` 和 `rua-syntax` 均依赖 `rua-lex`。
- `rua-lex` 只有词法功能：reader → tokenize → `(RuaTokenKind, byte_range)`。
- 优点：零代码重复。
- 缺点：新增一个 crate，变更范围大。

本步采用方案 A（减少变更半径），方案 B 可作为后续优化在 Phase 6 单独评估。

#### 4B.12c：native lexer 要求

- 支持 `ruac` tokenizer 的全部 token kind：所有关键字、标识符、整数字面量、
  浮点字面量、字符串字面量、char 字面量、运算符、分隔符、行注释、嵌套块注释。
- Gap-free byte coverage：trivia token（whitespace、line comment、block
  comment）必须在 token 流中出现，且所有 token 的 byte range 必须连续覆盖
  整个源文本（开区间 `[0, text.len())`）。
- 错误恢复：非 UTF-8 字节、未终止的字符串/块注释/char 字面量必须产生 error
  token 并继续 lex，不能返回 `Err`。
- 保持 `SyntaxKind` 与 token type 的一一映射。

#### 4B.12d：Conformance 验证

- 对 `tests/golden/parser/accept/` 中的每个 `.rua` 文件：
  - `ruac` lex → `Vec<RuaTokenKind>` + ranges。
  - native `lex` → `Vec<SyntaxKind>` + ranges。
  - 验证：两种 lexer 产生的非 trivia token 序列的 kind 列表和 byte range
    完全一致（trivia 允许 whitespace/comment 的 range 差异，只要两者都
    gap-free 且真实 token 的 range 一致）。
- 对 `tests/golden/parser/reject/` 中的每个文件：验证两者产生相同的 error token
  数量和位置。
- 新增 non-ASCII / emoji / right-to-left 字符的 lexer fixture。
- 验证 `parse_flat(src).text() == src`（lossless invariant）。

验证命令：

```sh
cargo test -p rua-syntax lexer
cargo test -p rua-syntax --test conformance
cargo test -p rua-syntax parser_conformance
cargo test -p ruac
```

退出条件：
- `rua_syntax::lex` 不调用 `ruac` 或 `transition`。
- `cargo test -p rua-syntax` 全部通过。
- `parse_flat` 的 lossless invariant 对所有 fixtures 保持。
- CST parser 对 `tests/golden/parser/accept/` 的输出（green tree 结构）
  无变化。

注意：本步完成后，`rua-syntax` 的生产依赖仍然包含 `ruac`（因为
`analysis.rs` / `workspace.rs` 仍通过 `transition` 调用 compiler
semantic API）。这个依赖要等到 Phase 5.10 才最终删除。本步仅保证
lexer 不再经 transition。

建议提交：`4B.12: own CST lexing in rua-syntax`

## 6. Phase 5：LSP Feature 全量迁移

目标：让长期 `AnalysisHost` 成为 `rua-lsp` 唯一语义状态，逐项迁移所有
handler，最后删除 legacy Workspace 和 production compiler dependency。

当前 LSP 状态（Phase 4B.6 结束时）：

```
Server {
    workspace: Workspace<DiskLoader>,     // 主语义状态（文件扫描、索引、查询）
    analysis_host: AnalysisHost,          // 仅用于 library/declaration 文件
    analysis_inputs: AnalysisInputs,      // 自有 host + 独立 FileId allocator
}
```

目标状态（Phase 5.10 结束时）：

```
Server {
    host: AnalysisHost,                   // 唯一语义状态
    registry: FileRegistry,               // 路径/URI -> FileId 映射
    document_versions: HashMap<FileId, i32>,  // LSP document version tracking
}
```

### Step 5.0：建立 protocol-level LSP parity harness

改动文件：`crates/rua-lsp/tests/protocol_parity.rs`（新建）。

本步不修改 `Server`，只搭建测试基础设施。

- 使用 `lsp_server::Connection::memory` 创建完整 LSP 协议循环 harness。
- 实现 `TestClient`：封装 memory connection，提供 `request<T: lsp_types::request::Request>()`
  和 `notify<N: lsp_types::notification::Notification>()` 方法。
- 实现 `initialize()`：发送标准 `InitializeParams` + `initialized` notification，
  返回 `ServerCapabilities`。
- 实现 `Fixture`：
  ```
  Fixture::new()
    .with_workspace_folder("/root")
    .with_open_document("/root/main.rua", "fn main() { let x = 1; }")
    .with_library("/lib/meta.ruai", "extern \"lua\" { pub fn log(msg: String); }")
  ```
- 为每个 LSP capability 建至少一个 protocol-level snapshot test：
  - 发送 request → 接收 response → normalize JSON（排序 keys、规范化 URI、
    移除 null 字段）→ snapshot。
- Test-only parity adapter：
  ```rust
  enum Backend { Legacy, Native }
  fn run_request(fixture: &Fixture, backend: Backend) -> serde_json::Value;
  ```
  同一 fixture 可分别跑 legacy 和 native 后端，比较 JSON 输出差异。

验证命令：

```sh
cargo test -p rua-lsp --features lsp protocol_parity
cargo test -p rua-lsp --features lsp capability_contract
```

退出条件：
- 所有已声明 LSP capability 都有对应 protocol snapshot test。
- 没有声明但不 response 的 capability，也没有 response 但未声明的 capability。
- Test harness 可以在 `< 500ms` 内完成一个完整的 request/response 循环。

建议提交：`5.0: add protocol-level LSP migration harness`

### Step 5.1：实现 LSP workspace loader 与稳定 FileId registry

这是 Phase 5 最关键的基础设施步骤。当前 `Server` 有三个并行的文件身份系统：
(1) `Workspace<DiskLoader>` 的 path → file 映射，(2) `AnalysisInputs` 的
`file_ids: BTreeMap<PathBuf, FileId>`，(3) LSP diagnostics 中的 URI → path 映射。
本步统一为一个 `FileRegistry`。

改动文件：新建 `crates/rua-lsp/src/file_registry.rs`。

#### 5.1a：FileRegistry 数据模型

```rust
/// Unified path/URI/document identity registry.
struct FileRegistry {
    /// Normalized disk path → stable FileId.
    /// Populated on initialize/didOpen/workspace-folder-add.
    /// Key is the canonical (realpath) path, not the logical path.
    disk_files: HashMap<PathBuf, FileId>,

    /// Document URI → FileId (includes untitled: URIs).
    uri_index: HashMap<Uri, FileId>,

    /// FileId → current registration info.
    files: HashMap<FileId, FileInfo>,

    /// SourceRootId → root metadata.
    roots: HashMap<SourceRootId, RootInfo>,

    /// Next FileId to allocate.
    next_file_id: u32,

    /// Next SourceRootId to allocate.
    next_root_id: u32,
}

struct FileInfo {
    uri: Uri,
    disk_path: Option<PathBuf>,       // None for untitled:
    root_id: SourceRootId,
    vfs_path: VfsPath,                // root-relative logical path
    kind: FileKind,
}

struct RootInfo {
    kind: SourceRootKind,
    root_dir: Option<PathBuf>,        // physical base (None for virtual)
    logical_base: VfsPath,            // module resolution base
}
```

#### 5.1b：关键约束

1. **同一物理文件的 FileId 必须稳定**：
   - 首次 opened/workspace-scanned → 分配 FileId。
   - didClose 后重新 didOpen → 复用同一 FileId。
   - workspace-folder remove → 移除 FileId 映射但不回收（悬空引用无害，
     因为 Analysis snapshot 持有 `Arc` 保持旧数据存活）。
2. **URI &lt;→ FileId 映射**：
   - `file:///` URI → canonicalize path → lookup disk_files。
   - `untitled:` URI → 分配 virtual FileId（无 disk_path）。
3. **路径规范化**：
   - macOS：必须处理 `/var` vs `/private/var` 的 symlink canonicalization。
   - 所有路径在 registry 中存储为 canonical form。
   - 对大小写不敏感文件系统（macOS 默认 APFS）做额外测试。
4. **Multi-root 隔离**：
   - 每个 workspace folder 分配独立 `SourceRootId`。
   - 每个 library/config mount 分配独立 `SourceRootId`。
   - Module resolution 必须显式带 SourceRootId/ProjectId，不能全局搜索。
5. **Open overlay 生命周期**：
   ```rust
   enum DocumentState {
       /// Disk file, currently open with possibly-unsaved buffer text.
       Open { text: String, version: i32 },
       /// Disk file, not open (text is in VFS only).
       Closed,
       /// Virtual file (untitled:), only exists while open.
       Virtual { text: String, version: i32 },
   }
   ```
   - didOpen：如果 disk path 已存在 → Open(state)；否则分配新 FileId（virtual）。
   - didChange：更新 Open text/version。拒绝倒退的 version。
   - didClose：存在 disk path → Closed（恢复磁盘 text 进 VFS）；否则删除 FileId
     映射。
   - Watcher file create/change/delete → 刷新 Closed 状态文件的 VFS text；
     Open 文件的 watcher event 忽略（buffer 为准）。

#### 5.1c：IO 隔离

- `FileRegistry` **不直接读磁盘**。
- 磁盘扫描和 watcher 在 `crates/rua-lsp/src/loader.rs`（新建）中实现，
  作为 `FileRegistry` 的输入源。
- `loader::scan_workspace(root: &Path) -> Vec<(PathBuf, VfsPath, String)>`
  返回 canonical path + logical path + file text。
- `loader::scan_library(config: &LibraryConfig) -> Vec<ScannedLibraryFile>`
  复用现有 `analysis_inputs::scan_configured_files` 逻辑（本步之后删除
  `AnalysisInputs` 的文件扫描代码）。

验证命令：

```sh
cargo test -p rua-lsp --features lsp workspace_loader
cargo test -p rua-lsp --features lsp document_lifecycle
cargo test -p rua-lsp --features lsp multi_root
```

fixture 覆盖：
- 同一文件 didOpen → didClose → didOpen 保持 FileId 稳定。
- unsaved buffer 内容在 hover/goto 中反映。
- didClose 后恢复磁盘内容作为后续查询来源。
- watcher 删除文件后，已打开文件的未保存 buffer 仍可用。
- multi-root 下同名模块不跨 workspace 污染。
- symlink canonicalization（macOS `/tmp` → `/private/tmp`）。
- `untitled:` virtual document。
- backwards version 被忽略（client bug 防御）。

退出条件：
- `FileRegistry` 是 LSP 内唯一路径/URI → FileId 的映射。
- 旧 `Workspace<DiskLoader>` 的路径扫描逻辑不再用于 FileId 分配。
- open/change/close/watcher/config 的 FileId 和 text 行为有完整测试。

建议提交：`5.1: build stable LSP workspace inputs`

### Step 5.2：建立 authoritative AnalysisHost 与临时 legacy mirror

本步将 `FileRegistry` 和 `AnalysisHost` 连接为权威数据流，并建立一个
不会独立分配 FileId 的 legacy mirror 让尚未迁移的 handler 继续工作。

改动文件：`crates/rua-lsp/src/lsp.rs`（主修改），
`crates/rua-lsp/src/analysis_inputs.rs`（部分删除）。

#### 5.2a：Server 新状态结构

```rust
struct Server {
    connection: Connection,
    /// 唯一权威语义状态
    host: AnalysisHost,
    /// 统一路径/文件注册
    registry: FileRegistry,
    /// 文档版本跟踪 (FileId → LSP version)
    document_versions: HashMap<CoreFileId, i32>,
    /// 打开文档状态 (FileId → DocumentState)
    document_states: HashMap<CoreFileId, DocumentState>,
    /// 临时 legacy mirror（5.3-5.8 逐步删除）
    legacy: Option<LegacyQueryMirror>,
}
```

#### 5.2b：Change 提交流程

所有输入事件统一经过以下路径后提交到 `host`：

1. **initialize**：遍历 workspace folders → scanner → 为每个 root 分配
   SourceRootId → 为每个文件分配 FileId → construct `Change` batch →
   `host.apply_change(change)`。
2. **didOpen**：registry 记录 Open 状态 → `Change::set_file_text(file_id, text)` →
   `host.apply_change(change)`。
3. **didChange**：registry 更新 version → `Change::set_file_text(file_id, text)` →
   `host.apply_change(change)`。
4. **didClose**：registry 恢复 Closed 状态 → 如果文件有磁盘内容：
   `Change::set_file_text(file_id, disk_text)` 否则 `Change::remove_file(file_id)` →
   `host.apply_change(change)`。
5. **didChangeConfiguration**：reload library config → registry 更新 library roots →
   `Change` batch → `host.apply_change(change)`。
6. **didChangeWatchedFiles** / **didChangeWorkspaceFolders**：类似。
7. **每个 request handler 开头**：`let analysis = host.analysis();` — 获取当前
   一致 snapshot。

#### 5.2c：LegacyQueryMirror

在 5.3-5.8 尚未迁移的 handler 需要使用旧的 semantic query，但不能再通过
`Workspace<DiskLoader>` 独立扫描磁盘和分配 FileId。

```rust
struct LegacyQueryMirror {
    workspace: Workspace<NoOpLoader>,
}
```

- `NoOpLoader`：实现 `FileLoader` trait，不从磁盘读文件。`load()` 永远返回
  `Ok(None)`。所有的 file text 由 `Server` 在每次 request 前通过 registry
  注入 `Workspace`。
- 注入方式：对每个尚未迁移的 handler，在调用 `legacy.workspace.*` 之前：
  - 检查该 request 需要的磁盘文件是否已通过 `host` 的 VFS 加载。
  - 若未加载，临时从磁盘读取并同时更新 `host`（保证单一状态）。
  - 这不是优雅方案，仅是迁移过渡手段；每个 handler 迁走后删除对应调用。
- `LegacyQueryMirror` 在 5.10 物理删除。

#### 5.2d：删除 AnalysisInputs 的私有 host

- `analysis_inputs.rs` 中的 `AnalysisHost` 和 `FileId` allocator 删除。
- Library scanning 逻辑（`scan_configured_files`）迁移到 `loader.rs`。
- Library 文件的 FileId 由 `FileRegistry` 统一分配，SourceRootId 由
  `FileRegistry` 统一管理。
- `AnalysisInputs` 结构体本步降级为纯 helper：其 `reload_from_settings` 只
  输出 `Vec<(ScannedFile, SourceRootId)>`，由 `FileRegistry::apply_library_reload`
  消费并生成 `Change`。

#### 5.2e：不变式

- 整个 LSP 中只有一个 `AnalysisHost`。
- `host.apply_change(change)` 是 file text 进入分析引擎的唯一入口。
- Request 不再从磁盘重新读取已打开文档。
- Semantic tokens 和 diagnostics 不再创建临时 `AnalysisHost`（早已在
  `Analysis::semantic_tokens` / `Analysis::diagnostics` 上直接从
  snapshot 查询）。
- `Rc<BaseDb>` 和 rowan red node 只在主线程使用（保持 `!Send` 约束）。

验证命令：

```sh
cargo test -p rua-lsp --features lsp analysis_host_lifecycle
cargo test -p rua-lsp --features lsp snapshot_per_request
cargo test -p rua-lsp --features lsp unsaved_buffer
```

退出条件：
- 启动后 `Server` 只有一个 `AnalysisHost`。
- 所有 file text 变更（open/change/close/configuration/watcher）都经由
  `FileRegistry` → `Change` → `AnalysisHost::apply_change`。
- 尚未迁移的 handler 通过 `LegacyQueryMirror` 继续可用，不出现功能回退。
- 请求不重复读取磁盘文件（已打开 buffer 优先）。

建议提交：`5.2: establish authoritative LSP analysis state`

### Step 5.3：迁移 document/workspace symbols

这是最简单的迁移（symbols 只依赖 DefMap，已在 4B.2 完成），
作为 5.3 优先迁移以建立 handler 迁移模式。

改动文件：`crates/rua-lsp/src/lsp.rs`。

每个 handler 迁移遵循相同的四步模式：
1. 新增 `native_<handler>` 函数：接收 `&Analysis, Request`，返回 protocol-native 结果。
2. 在 dispatch 中替换调用：将旧 `workspace.xxx()` 替换为 `native_xxx(&analysis, ...)`。
3. 添加 protocol-level parity test：同一 fixture 分别经 legacy/native handler，比较 JSON。
4. 删除 LegacyQueryMirror 中的对应调用（不删除 mirror 本身）。

```rust
fn native_document_symbols(
    analysis: &Analysis,
    registry: &FileRegistry,
    params: &DocumentSymbolParams,
) -> Option<DocumentSymbolResponse> {
    let file_id = registry.file_id_for_uri(&params.text_document.uri)?;
    let symbols = analysis.document_symbols(file_id, file_id);
    Some(DocumentSymbolResponse::Nested(
        symbols.into_iter().map(document_symbol_to_lsp).collect()
    ))
}
```

Workspace symbols 从 registry 获取当前 active project，遍历所有 open workspace folder 的 roots。Query string 在 `Analysis` 层做前缀/子串匹配，结果按 `(SymbolKind, name)` 排序。

验证命令：

```sh
cargo test -p rua-lsp --features lsp symbol_migration
```

退出条件：
- document/workspace symbol handler 不访问旧 `Workspace`。
- 现有 `ide_goldens` snapshot 无回退。
- workspace symbol 查询在 multi-root 下不跨 workspace 泄漏。

提交：`5.3: migrate LSP symbols to native analysis`

### Step 5.4：迁移 goto definition 与 hover

这两项共享同一个底层 `Semantics::find_def_at` 路径，合并迁移。

改动文件：`crates/rua-lsp/src/lsp.rs`。

注意：当前 legacy `goto_definition` 返回的是 **hover text**（一个 bug）。迁移时修正为返回 `Location` / `GotoDefinitionResponse`。

```rust
fn native_goto_definition(
    analysis: &Analysis,
    registry: &FileRegistry,
    params: &GotoDefinitionParams,
) -> Option<GotoDefinitionResponse> {
    let position = registry.file_position(params)?;
    let target = analysis.goto_definition(position)?;
    let uri = registry.uri_for_file(target.file_id)?;
    let range = text_range_to_lsp_range(target.range, analysis, target.file_id)?;
    Some(GotoDefinitionResponse::Scalar(Location { uri, range }))
}

fn native_hover(
    analysis: &Analysis,
    registry: &FileRegistry,
    params: &HoverParams,
) -> Option<Hover> {
    let position = registry.file_position(params)?;
    let result = analysis.hover(position)?;
    let range = text_range_to_lsp_range(result.range, analysis, position.file_id)?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::PlainText,
            value: result.text,
        }),
        range: Some(range),
    })
}
```

fixture 覆盖：local/item/cross-file/member/closure param/`.ruai`/builtin goto 和 hover；Unknown receiver → None 不 panic。

验证命令：

```sh
cargo test -p rua-lsp --features lsp navigation_migration
cargo test -p rua-lsp --features lsp hover_migration
```

退出条件：goto/hover handler 不调用旧 `Workspace`；匹配或超过 legacy 能力。

提交：`5.4: migrate LSP navigation and hover`

### Step 5.5：迁移 references、prepare rename 与 rename

改动文件：`crates/rua-lsp/src/lsp.rs`。

```rust
fn native_references(
    analysis: &Analysis,
    registry: &FileRegistry,
    params: &ReferenceParams,
) -> Option<Vec<Location>> {
    let position = registry.file_position(params)?;
    let results = analysis.references(position, params.context.include_declaration);
    Some(results.into_iter()
        .filter_map(|r| {
            let uri = registry.uri_for_file(r.file_id)?;
            let range = text_range_to_lsp_range(r.range, analysis, r.file_id)?;
            Some(Location { uri, range })
        })
        .collect())
}

fn native_prepare_rename(
    analysis: &Analysis,
    registry: &FileRegistry,
    params: &TextDocumentPositionParams,
) -> Option<PrepareRenameResponse> {
    let position = registry.file_position(params)?;
    let target = analysis.prepare_rename(position)?;
    let range = text_range_to_lsp_range(target.range, analysis, position.file_id)?;
    Some(PrepareRenameResponse::Range(range))
}

fn native_rename(
    analysis: &Analysis,
    registry: &FileRegistry,
    params: &RenameParams,
) -> Result<Option<WorkspaceEdit>, lsp_server::Error> {
    let position = registry.file_position(params)?;
    match analysis.rename(position, &params.new_name) {
        Ok(source_change) => {
            let mut changes = HashMap::new();
            for file_edit in source_change.edits {
                let uri = registry.uri_for_file(file_edit.file_id)
                    .ok_or_else(|| lsp_server::Error { code: 0, message: "missing URI".into() })?;
                let text_edits: Vec<TextEdit> = file_edit.edits.into_iter()
                    .filter_map(|edit| {
                        let range = text_range_to_lsp_range(edit.range, analysis, file_edit.file_id)?;
                        Some(TextEdit { range, new_text: edit.text.clone() })
                    })
                    .collect();
                changes.insert(uri, text_edits);
            }
            Ok(Some(WorkspaceEdit { changes: Some(changes), ..Default::default() }))
        }
        Err(RenameError::ReadOnly(msg)) =>
            Err(lsp_server::Error { code: 0, message: msg }),
        Err(RenameError::InvalidName(msg)) =>
            Err(lsp_server::Error { code: 1, message: msg }),
        Err(RenameError::Unsupported { reason }) =>
            Err(lsp_server::Error { code: 2, message: reason }),
    }
}
```

fixture 覆盖：local/item/cross-file/closure param references（含/不含 declaration）；rename 生成跨文件 WorkspaceEdit；Library/Std rename 返回 readonly error；重叠 edit 合并；空名和关键字名被拒绝。

验证命令：

```sh
cargo test -p rua-lsp --features lsp references_migration
cargo test -p rua-lsp --features lsp rename_migration
```

退出条件：references/rename handler 不调用旧 `Workspace`；`.ruai` rename 返回专用 readonly error；不生成重叠或重复 edit。

提交：`5.5: migrate LSP references and rename`

### Step 5.6：迁移 completion

这是最复杂的 handler 迁移。将 completion context detection 和 candidate generation 全部迁到 `rua-analysis`。

改动文件：`crates/rua-lsp/src/lsp.rs`。

```rust
fn native_completion(
    analysis: &Analysis,
    registry: &FileRegistry,
    params: &CompletionParams,
) -> Option<CompletionResponse> {
    let position = registry.file_position(params)?;
    let items = analysis.completions(position);
    if items.is_empty() {
        return Some(CompletionResponse::Array(Vec::new()));
    }
    let lsp_items: Vec<CompletionItem> = items.into_iter()
        .filter_map(|item| completion_to_lsp(item, analysis, position.file_id))
        .collect();
    Some(CompletionResponse::Array(lsp_items))
}
```

`CompletionKind` → `lsp_types::CompletionItemKind` 映射：Keyword→KEYWORD, Local/Item→VARIABLE, Field→FIELD, Method→METHOD, Module→MODULE, Variant→ENUM_MEMBER, Builtin→FUNCTION。

Completion context detection（`::` 后 → path completion，`.` 后 → member completion，否则 → scope completion）在 `rua-analysis` 中实现等价的 syntax token 分析。`rua-syntax/src/completion.rs` 保留但标记 deprecated，5.10 删除。

fixture 覆盖：顶层/`.`/`::` 三种上下文；struct fields/methods；`.ruai` declared type member；闭包参数在 body 内的 completion；Unknown receiver 返回空；排序确定无重复。

验证命令：

```sh
cargo test -p rua-lsp --features lsp completion_migration
cargo test -p rua-syntax --test ide_goldens ide_snapshot_golden -- --exact
```

退出条件：completion handler 不调用 compiler member completion façade；现有 ide_goldens completion snapshot 通过或记录 named expected difference；候选顺序确定无重复。

提交：`5.6: migrate LSP completion to native analysis`

### Step 5.7：迁移 diagnostics

当前 diagnostics 使用 native analysis 但以 per-request 临时 `AnalysisHost`。本步确保只用长期 host snapshot，正确处理 document version，依赖文件变更后发布受影响已打开文件的 diagnostics。

改动文件：`crates/rua-lsp/src/lsp.rs`。

推送模型：`publish_diagnostics_for_file(analysis, registry, connection, file_id)` → 获取 `analysis.diagnostics(file_id)` → 转 LSP Diagnostic → publish 带 version。

触发时机：didOpen（首次）、didChange（最新 version）、didClose（空 diagnostics 清空）、didChangeConfiguration/watcher（受影响已打开文件重算）。依赖文件变更时重算所有引用该定义的已打开文件的 diagnostics。

删除 `reconciled_diagnostics_for` 和 production `ruac::check_diags` 调用。`diagnostic_parity` 测试保留在 `rua-analysis` dev-tests。

验证命令：

```sh
cargo test -p rua-lsp --features lsp diagnostics_migration
cargo test -p rua-lsp --features lsp diagnostic_versions
```

退出条件：无重复 fast/compiler diagnostic；同一文件不会被同一变更触发两次 publish；stale version 不覆盖最新 diagnostics。

提交：`5.7: publish native analysis diagnostics`

### Step 5.8：迁移 semantic tokens

本步确保 semantic tokens 从长期 `Analysis` snapshot 服务。semantic tokens 的核心改写（移除 `rua_syntax::nameres` 调用）已在 4B.7b 完成，本步仅做 LSP adapter 切换。

改动文件：`crates/rua-lsp/src/lsp.rs`。

```rust
fn native_semantic_tokens(
    analysis: &Analysis,
    registry: &FileRegistry,
    params: &SemanticTokensParams,
) -> Result<Option<SemanticTokensResult>, lsp_server::Error> {
    let file_id = registry.file_id_for_uri(&params.text_document.uri)?;
    let tokens = analysis.semantic_tokens(file_id);
    let data = encode_semantic_tokens(&tokens);
    Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
        result_id: None,
        data,
    })))
}
```

UTF-16 列号转换：LSP semantic token positions 要求 UTF-16 code units。`LineIndex` 已提供 UTF-8 → UTF-16 转换。必须新增测试覆盖多字节字符（如希腊字母 `α`）的 token column 正确性。

验证命令：

```sh
cargo test -p rua-lsp --features lsp semantic_token_migration
cargo test -p rua-analysis --test closure_iterator_ide
```

退出条件：semantic token request 不创建临时 AnalysisHost；token legend 与实际 token_type index 一致；非 ASCII 文本 tokens 的 UTF-8→UTF-16 转换正确。

提交：`5.8: serve semantic tokens from native snapshots`

### Step 5.9：合并 external library 配置与 watcher

本步完成 5.2 暂留的 library config 整合，消除 `AnalysisInputs` 残存的私有
host/FileId allocator。完成后 library scanner 仅输出文件规格，所有 ID 和
Change 都由统一的 `FileRegistry` + `AnalysisHost` 管理。

改动文件：`crates/rua-lsp/src/file_registry.rs`（扩），新建
`crates/rua-lsp/src/library_loader.rs`。

#### 5.9a：library loader 纯化

- 将 `analysis_inputs.rs` 中的磁盘扫描逻辑（`scan_configured_files`,
  `scan_path`, `scan_directory`, `load_declaration`）提取到
  `library_loader.rs`。
- `LibraryLoader` 只输出：
  ```rust
  struct LibraryFileSpec {
      disk_path: PathBuf,
      logical_path: VfsPath,
      text: String,
      mount_name: Option<String>,  // for named mounts like `clock::`
  }
  ```
- `LibraryLoader::load(config: &LibraryConfig) -> (Vec<LibraryFileSpec>, Vec<String>)`
  返回文件规格和 warnings。
- 不做 FileId 分配，不创建 `Change`，不拥有 `AnalysisHost`。

#### 5.9b：FileRegistry 整合 library roots

- `FileRegistry::apply_library_load(specs: &[LibraryFileSpec]) -> Change`：
  - 为每个配置的 library directory/mount 分配独立的 `SourceRootId`（不再
    把所有 declaration 塞进一个共享 `LIBRARY_ROOT_ID`）。
  - Stable FileId：同一个 `(disk_path, mount_name)` 在 reload 中保持
    相同 FileId（通过持久化 `HashMap<(PathBuf, Option<String>), FileId>`）。
  - 清除不再存在于新配置中的 stale files/roots。
  - 返回 `Change` batch（由 `Server` 提交到 `AnalysisHost`）。
- 固定 root priority：Workspace > Library（按 mount order）> Std。
  同一 root 内：`.rua` > `.ruai`。
- 跨 root module candidate：workspace importer 的基于目录的模块查找
  仅在自己 root 内进行；library module 通过显式 root logical base 查找。

#### 5.9c：watcher 集成

- 为每个 configured library directory 和 mount file 建文件系统 watcher。
- create/change/delete → 增量更新 Change → `host.apply_change`。
- watcher 去抖（debounce）：同一文件 100ms 内的多次事件合并。
- library/std 文件标记 readonly，rename/code action 边界检查复用
  `SourceRootKind::Library | SourceRootKind::Std`。

#### 5.9d：VS Code extension 配置

- 在 `editors/vscode/package.json` 中 contributes：
  ```json
  "rua.library": {
    "type": "array",
    "items": { "type": "string" },
    "description": "Directories or files containing .ruai type declarations"
  },
  "rua.libraryMounts": {
    "type": "object",
    "description": "Named module path → file/directory mapping for .ruai declarations"
  }
  ```
- `LanguageClient` 初始化时发送 `workspace/configuration` 获取上述配置。
- `didChangeConfiguration` 触发 library reload。

#### 5.9e：删除 AnalysisInputs

- `analysis_inputs.rs` 在本步完全删除。其测试迁移到
  `crates/rua-lsp/tests/library_loader.rs`。

验证命令：

```sh
cargo test -p rua-lsp --features lsp library_configuration
cargo test -p rua-lsp --features lsp library_watcher
cargo test -p rua-analysis module_resolution
cd editors/vscode && npm run check-types
```

fixture 覆盖：
- 外部 `.ruai` 在 completion/hover/goto/references 中可用。
- library reload 保持 stable FileId。
- workspace `src/main.rua` + `library/foo.ruai` 模块 candidate 不串位。
- nested library subdirectory module。
- single-file named mount（如 `"clock" → "/lib/clock.ruai"`）。
- watcher delete/create 使变更立即生效。
- rename 正确拒绝 library/std 文件。
- 空配置/无效配置不 panic。

退出条件：
- 外部 `.ruai` 可在真实 LSP flow 中 completion/hover/goto/references。
- workspace 与 library root 的 logical path 不会错位。
- 文件变更无需重启 server 即生效。
- rename 不编辑 library/std。

建议提交：`5.9: integrate external library roots and watchers`

### Step 5.10：删除 legacy Workspace 与 transition production path

这是迁移的最终清理步骤。执行需谨慎——每个删除点都必须在 5.3-5.9
验证通过后才安全。

#### 5.10a：删除顺序

1. **删除 `LegacyQueryMirror`**：
   - 删除 `Server.legacy: Option<LegacyQueryMirror>` 字段。
   - 删除 `crates/rua-lsp/src/legacy_mirror.rs`（如果 5.2 时将其提取为独立模块）。
2. **删除旧 `Server.workspace`**：
   - 删除 `Server.workspace: rua_syntax::workspace::Workspace<DiskLoader>`。
   - 删除 `DiskLoader` 的使用。
3. **清理 `rua-syntax`**：
   - 删除 `rua-syntax/src/analysis.rs`（整个模块）—— 所有消费者已迁走。
   - 删除 `rua-syntax/src/workspace.rs`（整个模块）。
   - 删除 `rua-syntax/src/nameres.rs` 中的 semantic-query 函数（`references_at`
     等不再被 `rua-analysis` 调用的）。保留纯 syntax-level name resolution
     helper 如有需要。
   - 删除 `rua-syntax/src/transition.rs`（整个模块）。
   - 从 `rua-syntax/src/lib.rs` 删除 `mod transition;` 和 `pub mod analysis;`、
     `pub mod workspace;` 等声明。
   - 将 `ruac` 从 `[dependencies]` 移到 `[dev-dependencies]`。
4. **清理 `rua-lsp`**：
   - 从 `Cargo.toml` 的 `[dependencies]` 和 `[dev-dependencies]` 中
     完全删除 `ruac`。
   - 删除 `use rua_syntax::analysis::*`、`use rua_syntax::workspace::*`
     等导入。
5. **清理 `rua-analysis`**：
   - 确保 `ruac` 仅在 `[dev-dependencies]` 中。

#### 5.10b：保留项

- `rua-syntax` 中纯 structural 模块保留：`ast.rs`、`parser.rs`、`lexer.rs`、
  `kind.rs`、`format/`、`line_index.rs`、`symbols.rs`（纯 CST symbol
  collection）。
- `rua-syntax/src/nameres.rs` 中纯 syntax-level name resolution 保留
  （如果 `rua-analysis` 仍在 dev 测试中使用），但不应通过
  `rua_syntax::nameres` 被 production 路径调用。

#### 5.10c：CI 边界门禁

在 CI 中增加以下硬门禁（`scripts/check-boundaries.sh`）：

```sh
#!/bin/bash
set -euo pipefail

# rua-analysis must not depend on ruac in production
! cargo tree -p rua-analysis -e normal --depth 1 | rg "ruac"

# rua-syntax must not depend on ruac in production
! cargo tree -p rua-syntax -e normal --depth 1 | rg "ruac"

# rua-lsp must not depend on ruac at all
! cargo tree -p rua-lsp --features lsp -e normal --depth 2 | rg "ruac"

# No transition module in rua-syntax
! rg -n "mod transition|crate::transition" crates/rua-syntax/src/

# No legacy semantic facade imported in production code
! rg -n "rua_syntax::(analysis|workspace|nameres)" crates/rua-lsp/src/ crates/rua-analysis/src/
```

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
- LSP 只有一个 `AnalysisHost` 和一个 `FileRegistry`。
- `rua-syntax` 普通依赖不含 `ruac`。
- `rua-lsp` 普通依赖不含 `ruac`，也不调用旧 semantic Workspace。
- `ruac` 仍不依赖 rowan、analysis 或 LSP crates。
- `cargo test --workspace --all-features` 通过。

建议提交：`5.10: remove legacy LSP analysis pipeline`

### Step 5.11：增量、压力与错误恢复验收

本步不新增功能，仅添加系统级回归测试和性能门禁。所有测试在 `rua-lsp`
的 `tests/` 目录下。

改动文件：`crates/rua-lsp/tests/incremental_stress.rs`（新建），
`crates/rua-lsp/tests/malformed_edit_recovery.rs`（新建），
`crates/rua-lsp/tests/multi_root.rs`（新建）。

#### 测试清单

1. **rapid didChange**：20 次连续 `textDocument/didChange` → 验证只有最终
   version 的 diagnostics 被 publish（旧 version 不覆盖新 version）。
2. **unsaved sibling edit**：打开 `a.rua` 和 `b.rua`，在 `b.rua` 中修改
   一个被 `a.rua` 引用的函数签名 → hover/goto/completion/diagnostics
   在 `a.rua` 中反映 `b.rua` 的 unsaved 变更。
3. **multi-root isolation**：两个 workspace folder，各自有 `src/main.rua`
   和 `mod utils` → workspace A 的 goto 不跳到 workspace B 的 `utils`。
4. **malformed edit recovery**：连续输入 `fn main() { let x`（未完）→
   不 panic → 输入 `= 1; }` 完成 → diagnostics 正确更新。
5. **library reload**：修改 `.ruai` 文件 → 已打开文档的 diagnostics/hover
   反映新声明 → stable FileId 保持不变。
6. **file delete/recreate**：删除后重建同路径文件 → FileId 稳定 →
   references 不丢失。
7. **symlink**：workspace root 经 symlink 访问 → canonical path 一致。
8. **`untitled:` document**：未保存文件支持 completion/hover/diagnostics。
9. **workspace folder add/remove**：添加第二个 folder → 索引其内容 →
   移除 → 不再索引。其他 root 不受影响。
10. **query counter 断言**（测试端使用 `cfg(debug_assertions)` 的计数器）：
    - warm hover（连续第 10 次查询同一位置）：仅触发 0-1 次 body lowering。
    - 一个文件 edit 后：无关文件的 body/inference cache 命中率 100%。
    - 不做绝对时间测量（CI 环境噪声大），仅做 query count 结构性断言。

#### 性能基线记录

- 在 release build 下执行 synthetic workspace（100 文件、500 函数）的
  首次索引，记录 `instant::Instant` 耗时到 console（不参与 CI pass/fail）。
- 作为发布门禁（非 CI 门禁），手动对比 sequential version 的耗时变化。

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
- 上述 stress 测试全部通过。

建议提交：`5.11: validate incremental LSP behavior`

### Step 5.12：VS Code E2E、CI 与文档收尾

改动文件：`editors/vscode/`（全部），`.github/workflows/ci.yml`（如有），
`docs/`（更新）。

#### 5.12a：VS Code Extension Host 测试

- `editors/vscode/package.json` 增加 `test:e2e` script：
  ```json
  "scripts": {
    "test:e2e": "node ./out/test/runTest.js"
  }
  ```
- 使用 `@vscode/test-electron`（或等效）下载 VS Code 并启动 Extension Host。
- smoke checklist（在 Extension Development Host 中验证）：
  1. Extension activates on `.rua` file open。
  2. Open `.rua` file → diagnostics published。
  3. Hover over variable → type info shown。
  4. Go-to-definition (F12) → jumps to definition。
  5. Completion (Ctrl+Space) → items shown。
  6. References (Shift+F12) → all references listed。
  7. Rename (F2) → all references renamed，library file refused。
  8. Format (Shift+Alt+F) → file formatted。
  9. Change `.ruai` config → reload → new declarations available。
  10. Semantic tokens → syntax highlighting correct。

#### 5.12b：CI pipeline

- Rust CI：
  ```yaml
  - cargo test --workspace --all-features
  - cargo clippy -p rua-analysis --all-targets --no-deps -- -D warnings
  - cargo clippy -p rua-lsp --features lsp --all-targets --no-deps -- -D warnings
  - cargo clippy -p rua-syntax --all-targets --no-deps -- -D warnings
  - bash scripts/check-boundaries.sh
  ```
- TypeScript CI：
  ```yaml
  - cd editors/vscode && npm ci && npm run check-types && npm run compile
  ```
- E2E（Linux）：
  ```yaml
  - cd editors/vscode && xvfb-run -a npm run test:e2e
  ```

#### 5.12c：文档收尾

- 本文件状态从 "施工中" 改为 "已完成"。
- 更新 `docs/rua-ide-architecture.md` 中的架构状态，反映 Phase 4B/5 完成。
- 如 `tests/golden/COVERAGE.md` 存在，更新 coverage 矩阵。
- `README.md` 中添加开发指南链接。

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
- `.vsix` 构建成功，不内嵌平台错误的 server binary。
- CI 对 dependency boundary、Rust、TypeScript 和 golden drift 都有门禁。
- 本计划文档状态标记为完成。

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
