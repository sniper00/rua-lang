# Rua IDE / LSP 架构重构方案

> 状态：提案。本文用于指导 Rua LSP 从当前 v3 桥接式实现，重构为 rust-analyzer 风格的 IDE 引擎。
>
> 目标完成度：功能面向 `emmylua-analyzer-rust`，架构边界参考 `rust-lang/rust-analyzer`。
>
> 施工拆分：见 `docs/rua-construction-plan.md`。
>
> Golden 用例清单：见 `docs/rua-golden-cases.md`。

## 1. 背景

当前 Rua LSP 已经具备基础诊断、格式化、hover、go-to-definition、document symbol、completion、references、rename，以及部分跨文件成员访问/成员补全能力。但这些能力主要由三类机制拼出来：

1. `moon-rua-syntax` 持有 rowan CST、`LineIndex`、符号扫描、轻量名称解析。
2. `moon-ruac` 持有编译器 AST、resolve、check、typeck、codegen。
3. LSP/Workspace 在需要语义信息时临时调用 `moon-ruac` 暴露的桥接 API，例如 `member_index`、`binding_types`、`member_completion_src`、`check_diags`。

这套架构能快速点亮 v3 功能，但已经接近上限：

- parser 有两套，但没有明确职责边界和 conformance 网，语义事实需要靠临时 byte span 桥接。
- `moon-rua-syntax::Analysis` 同时做语法、符号、局部语义缓存，职责混杂。
- `moon-ruac::check_diags(src)` 是单文件入口，LSP 诊断不能自然处理 workspace/VFS/open buffer。
- references/rename 依赖同名 token 预过滤再逐个解析，缺少全局 reference index。
- completion、hover、signature、semantic token、inlay hint、code action 等高级能力都需要稳定语义模型；继续追加桥接 API 会越来越脆。

因此如果允许初始阶段完全重构，应把方向从“继续补桥”切到“双 parser 边界清晰 + VFS + IDE analysis DB”。

## 2. 参考对象

### 2.1 rust-analyzer

借鉴点：

- `AnalysisHost` / `Analysis` 快照模型：更新和查询分离。
- VFS/source roots/project model 作为唯一输入，核心分析层不直接做 IO。
- rowan lossless syntax tree 作为语法边界，parser 容错返回 tree + errors。
- HIR 分层：item tree、def map、body、type inference 分开。
- IDE 层和 LSP 层严格隔离：IDE 返回 editor-domain 类型，LSP 只做协议转换。
- 查询按需、增量、可缓存、可取消的思路（rust-analyzer 用 salsa 实现；Rua **不引入 salsa**，用手写 per-file 增量达到同样效果，见 §14）。
- 精度分层：`rustc` 是最终裁判，rust-analyzer 追编译器精度。Rua 也应把 `ruac` 作为 language acceptance / type legality / codegen 的 gold standard，`rua-analysis` 负责 IDE 体验并持续逼近它。
- 共享不是一开始强行合并前端，而是逐步抽取稳定、低依赖的公共内核：token/span/diagnostic code/type representation/member 或 trait solver 等。共享内核不能把 rowan/VFS/LSP 依赖带进编译器。
- IDE 推断不能过度自信：无法证明时宁可返回 `Unknown` / partial result，也不要给出与编译器相反的“精确”结论。

### 2.2 emmylua-analyzer-rust

借鉴点：

- `EmmyLuaAnalysis` 统一承载 compilation、diagnostic、config。
- VFS 管理 `FileId -> text / line index / syntax tree / uri`。
- `DbIndex` 中有 decl/reference/type/module/member/signature/diagnostic/flow 等独立索引。
- `SemanticModel` 是 handler 查询语义的唯一入口。
- LSP handler 按功能拆分，completion/hover/rename/references/semantic_token/inlay/code_actions 都不直接碰底层 parser/typeck 细节。

## 3. 总目标

初始目标架构（4 个 crate，对齐 emmylua：`emmylua_parser` / `emmylua_code_analysis` / `emmylua_ls` + CLI）：

```text
rua-syntax
  lexer + lossless CST + typed AST wrappers + parse errors
  （≈ emmylua_parser）

rua-analysis
  一个 crate 承载全部语义与 IDE 能力（≈ emmylua_code_analysis）：
    vfs/         内存 input store：FileId, SourceRoot, FileText, WorkspaceConfig（文本为唯一真相，无 salsa）
    db_index/    decl / reference / type / module / member 索引（手写 per-file 增量）
    hir/         ItemTree + DefMap + Body lowering + type inference + member lookup
    semantic/    SemanticModel / Semantics —— 查询语义的唯一入口
    ide/         AnalysisHost / Analysis + IDE data types + completion/diagnostics/assists
    diagnostic/  统一 Diagnostic

rua-lsp
  LSP transport, capability registration, handler dispatch, protocol conversion
  （≈ emmylua_ls；只做协议转换，薄）

ruac
  compiler CLI + 可内嵌库；保留简洁 parser / owned AST / check / typeck / codegen
  不依赖 rua-analysis / rowan / LSP
```

后续**按证据**才拆分（不是初始要求）：

```text
rua-analysis -> rua-base-db + rua-hir(-def/-ty) + rua-ide(-db/-completion/-diagnostics/-assists)
```

拆分触发条件：

- crate 编译时间或增量重算成本开始阻碍迭代。
- HIR definition 与 type inference 的 query 边界已经稳定。
- completion/diagnostics/assists 的代码量和测试量足以证明独立 crate 有收益。

在触发之前,`rua-analysis` 内部用**模块**(`vfs`/`hir`/`ide`…)保持边界,而不是用 crate 边界——先证明 VFS、双 parser conformance、reference index 三个核心收益。

仓库边界：

- Rua 语言工具链必须从 `moon_rs` 中剥离，单独建立 git 仓库。仓库名固定为 `rua`，本地路径固定为 `/Users/bruce/GitProjects/rua`。
- 新仓库拥有 `ruac`、LSP、syntax、HIR、IDE analysis、formatter、`.ruai` 标准库/外部库定义、Rua 文档和测试资产。
- 新仓库内 crate/package/binary 命名不使用 `moon` 前缀：使用 `rua-syntax`、`rua-analysis`、`rua-lsp`、`ruac` 等名称。
- `moon_rs` 不再把 `moon-ruac` / `moon-rua-lsp` / `moon-rua-syntax` 作为 workspace members。
- `moon_rs` 只保留 Lua 宿主、runtime、moon API、以及可选的 Rua 产物消费/示例；两边通过发布的 `ruac` binary、generated Lua、`.ruai` 声明或 metadata 交互。
- 仓库剥离是硬前置，必须先完成；后续编译器、LSP、syntax、HIR、IDE 架构重构只在 `/Users/bruce/GitProjects/rua` 新仓库进行。

核心原则：

- parser 有两套：`ruac` 的简洁编译器 parser + `rua-syntax` 的容错 lossless CST parser。
- `ruac` 是独立编译器核心：owned AST、resolve/check/typeck、codegen、零或极少外部依赖，不依赖 `rua-analysis` / rowan / LSP。
- `rua-analysis` 是 IDE/LSP 语义核心：VFS、HIR、SemanticModel、reference index、`.ruai` library。
- `ruac` 是语义精度的 gold standard；`rua-analysis` 的职责是 fast / lazy / incremental / tolerant，并通过测试持续追平 `ruac`。
- 两套 parser 不共享语义实现，通过 conformance tests 保证合法输入接受集、token/text range、diagnostic path semantics 不漂移。
- 编译器和 LSP 不强行共用 HIR/typeck；需要共识的行为用 golden / conformance / type parity 测试约束。
- 如果某块语义逻辑变复杂且稳定（例如 member lookup、trait solver、TypeIR），优先抽成 `ruac` 也能接受的低依赖 shared core，而不是在 `rua-analysis` 和 `ruac` 内长期复制。
- analysis 核心不直接读磁盘。
- LSP 不直接依赖 `ruac` 的临时查询 API。
- `lsp_types` 不进入 HIR/IDE 核心。
- `moon_rs` 与 Rua 工具链没有 Cargo workspace 耦合。
- `ruac` 库可内嵌:零外部依赖、核心不读磁盘(源码经 source-loader 注入)、公共面精简、lib/bin 分离(见 §8.4)。
- 初始 crate 拓扑保持收敛；先证明 VFS、双 parser conformance、reference index 三个核心收益，再细拆 crate。

### 3.1 精度契约：`ruac` gold standard，LSP 追精度

rustc / rust-analyzer 的教训不是“IDE 和编译器必须永远分开”，也不是“一开始就共用所有实现”，而是：

- 编译器负责最终正确性，IDE 负责交互速度与容错，两者优化目标不同。
- 一旦 IDE 对类型、trait/member lookup、诊断给出和编译器相反的结论，用户会按编译器结果判断 IDE 错。
- 因此 Rua 要明确精度契约：`ruac` 的 accept/reject、type legality、codegen 是最终裁判；`rua-analysis` 的语义结果必须可被 `ruac` 回归用例校准。

具体策略：

1. `rua-analysis` 可以先实现 IDE 友好的 HIR/type inference，但每个已知分歧都要进入 parity corpus。
2. IDE 无法证明时返回 `Unknown` / partial result，completion/hover 可降级，避免展示“看似精确但与编译器相反”的类型。
3. `ruac check --json` 或等价结构化输出应成为测试和可选 LSP compiler-backed diagnostics 的 oracle。
4. LSP 诊断分两层：`rua-analysis` 给 fast/open-buffer diagnostics；可选后台通过稳定的 `ruac check --json` / facade 给 compiler-exact diagnostics，后者优先级更高，不调用 `ruac` 内部临时 API。
5. 共享实现采用“稳定后抽取”：先用 parity tests 找到重复且易漂移的逻辑，再抽低依赖 shared core。候选包括 token/span、diagnostic code、TypeIR、member lookup、trait/constraint solver。
6. shared core 的依赖规则比功能更重要：不得依赖 rowan、VFS、`lsp_types`、LSP config，也不得迫使 `ruac` 采用 IDE 的容错 CST。

### 3.2 Rust 子集新增语法：闭包与迭代器

闭包和 iterator 是后续 Rua 必须补齐的 Rust 体验核心，但它们要遵守两个约束：

- `ruac` 仍保持简洁 compiler pipeline，不因为闭包/iterator 引入 rowan 或 LSP 依赖。
- iterator 生成 Lua 必须 lazy + fused：能静态看清的链不分配中间 iterator/table，不使用 coroutine，不做每元素动态分发。

闭包语法目标：

```rust
let add = |x| x + 1;
let add2 = |x: i64| -> i64 { x + 2 };
let scale = |x| x * factor;
xs.iter().map(|x| x + 1).filter(|x| x > 10)
```

闭包语义分层：

- Phase A：支持 `|args| expr`、`|args| { block }`、参数/返回类型可选；在调用上下文或 iterator adapter 中做局部推断。
- Phase A：允许只读捕获；`let mut` 捕获只在立即消费的 iterator chain 中先支持，避免过早承诺完整 `FnMut`/`FnOnce`。
- Phase B：支持 `move |...| ...`、闭包作为一等值存储/传参、`Fn`/`FnMut`/`FnOnce` 区分。
- 暂不引入 Rust ownership/borrow/lifetime；捕获语义按 Lua upvalue 实现，但 typeck 仍要阻止明显不安全或未定义的用法。

iterator 语法目标：

```rust
for x in 0..n { ... }
for x in 0..=n { ... }
for x in xs.iter() { ... }
let ys = xs.iter()
    .map(|x| x + 1)
    .filter(|y| y > 10)
    .collect::<Vec<i64>>();
let sum = xs.iter().map(|x| x * 2).fold(0, |acc, x| acc + x);
```

iterator lowering 规则：

- iterator expression 先 lower 成 lazy `IterPlan`，记录 source、adapter chain、closure body、item type、consumer；在知道 consumer 前不生成 Lua。
- `for`、`collect`、`fold`、`count`、`any`、`all`、`find` 等 consumer 触发 codegen。
- range source 直接生成 Lua numeric `for`。
- `Vec<T>` source 直接生成 `for __i = 0, v.n - 1 do local x = v[__i] ... end`，使用 `n`，不用 `#`。
- `map` / `filter` / `filter_map` / `enumerate` / `take` / `skip` / `chain` 等 adapter 尽量 fuse 到同一个 loop。
- 只有当 iterator 被存储、传参、返回或无法静态融合时，才退回小型 runtime pull-iterator 协议。

高效 Lua codegen 示例：

```rust
let ys = xs.iter()
    .map(|x| x + 1)
    .filter(|y| y > 10)
    .collect::<Vec<i64>>();
```

应生成类似：

```lua
local ys = rt.vec()
for __i = 0, xs.n - 1 do
  local x = xs[__i]
  local y = (x + 1)
  if (y > 10) then
    ys:push(y)
  end
end
```

不应生成：

- `map` 后中间 `Vec`。
- 每个 adapter 一个 Lua closure。
- coroutine-based iterator。
- 每个元素调用通用 `next()` 分发，除非 iterator escape。

### 3.3 建议继续支持的 Rust 语法

优先级按“能明显改善 Rua 可写性 / LSP 价值 / codegen 可控性”排序：

P0，闭包和 iterator 同期或紧随其后：

- `if let` / `while let`：配合 `Option` / `Result` / enum pattern。
- `let PAT = expr else { ... };`：错误处理和 early-return 很实用。
- destructuring `let`：tuple、struct、enum destructuring。
- `match` guard：`pat if cond => ...`。
- range syntax：`a..b`、`a..=b`，并复用到 iterator 和 pattern。
- module path：`crate::`、`self::`、`super::`。

P1，中期支持：

- `type Name = ...` type alias。
- `const NAME: T = expr` 常量项。
- `as` casts：先支持 `i64 <-> f64`、数字到 `String` 的显式转换。
- turbofish：`collect::<Vec<i64>>()`、`parse::<i64>()` 这类必要显式类型。
- doc comments 和轻量 attributes：给 LSP hover、`.ruai` 文档、allow/warn 诊断开关使用。
- `pub(crate)` / `pub(super)`：比单纯 `pub` 更接近 Rust module 习惯。

P2，等 type system 稳定后再做：

- `move` closures 与完整 `Fn` / `FnMut` / `FnOnce` 建模。
- associated const / associated type 的有限子集。
- slice/range patterns。
- `impl Trait` 返回值。
- operator trait 更完整映射。

暂不建议近期做：

- lifetimes、borrow checker、unsafe。
- `macro_rules!` / proc macro。
- `async` / `await`，除非先定义清楚它和 moon runtime scheduler 的映射。

## 4. 非目标

第一轮重构不追求：

- 完整复制 rust-analyzer 的 Cargo/proc-macro/macro expansion 复杂度。
- 一次性实现高级 trait solver。
- 一次性实现所有 LSP feature。
- 把 `ruac` 改成 rowan/salsa/HIR-heavy 编译器。
- 一次性消灭双 parser。

允许破坏性变化：

- 收窄 `ruac` 公共 API，清理 IO、诊断、模块可见性和 CLI/bin 边界。
- 调整 crate 依赖方向。
- 将 `moon-rua-syntax::Analysis`、`Workspace` 的职责迁移到新的 IDE/analysis 层。
- 重写 LSP server 结构。
- 从 `moon_rs` workspace 中移除 Rua 编译器与 LSP。

## 5. 规模与性能假设

先用明确假设约束架构复杂度；后续用 benchmark 修正。

初始项目规模假设：

- 常见项目：50-300 个 `.rua` 文件，1-50k LOC。
- 大项目：1k 个文件以内，100-300k LOC。
- 单文件通常小于 2k LOC。
- 模块图主要是单 workspace tree，不需要 Rust/Cargo 级跨 package graph。

交互目标：

- warm completion / hover / goto：p95 < 50ms。
- changed-file diagnostics：p95 < 200ms。
- workspace reference search：普通项目 < 300ms，大项目可后台预索引。
- cold workspace indexing：普通项目 < 2s，大项目 < 10s，并可分批上报进度。

这些数字用于约束两个关键决策：

- 增量策略：已定为手写 per-file 增量，不引入 salsa（理由见 §14）。
- crate 是否细拆：只有当 profiling 显示边界稳定且拆分能降低编译/重算成本时再拆。

## 6. 目标数据流

```text
client / CLI
    |
    v
Change { file text, workspace roots, config }
    |
    v
AnalysisHost::apply_change
    |
    v
VFS / input store (update_file / remove_file)
    |
    +--> parse(FileId) -> Parse<SourceFile>
    +--> item_tree(FileId) -> ItemTree
    +--> def_map(SourceRoot) -> DefMap
    +--> body(DefId) -> Body
    +--> infer(DefId) -> InferenceResult
    +--> diagnostics(FileId) -> Vec<Diagnostic>
    |
    v
Analysis snapshot
    |
    +--> hover
    +--> goto_definition
    +--> completion
    +--> references
    +--> rename
    +--> semantic_tokens
    +--> inlay_hints
    +--> signature_help
```

LSP handler 只做：

```text
LSP params -> FilePosition/FileRange
Analysis query
IDE result -> LSP response
```

## 7. 仓库拆分方案

Rua 工具链应作为独立项目维护。`moon_rs` 是 Rua 的一个重要宿主/使用方，但不应该继续承载 Rua 编译器和 LSP 的源码生命周期。

### 7.1 新仓库内容

新仓库固定位置：

```text
/Users/bruce/GitProjects/rua
```

建议新仓库结构：

```text
rua/
  Cargo.toml
  crates/
    rua-syntax/     # lexer + CST
    rua-analysis/   # vfs + db_index + hir + semantic + ide（一个 crate，内部模块划分）
    rua-lsp/        # 协议层
    ruac/           # 编译器 CLI + 可内嵌库
  lualib/
    rua_rt.lua
    meta/
      *.ruai
  assets/
    example/
    test/
  docs/
    rua-design.md
    rua-ide-architecture.md
    rua-lsp-*.md
    rua-sourcemap.md
  editors/
    vscode/
  xtask/
```

必须迁出的内容：

- `crates/moon-ruac` -> `crates/ruac`。
- `crates/moon-rua-lsp` -> `crates/rua-lsp`。
- `crates/moon-rua-syntax` -> `crates/rua-syntax`。
- 后续新增的 Rua analysis/HIR/IDE crates 使用 `rua-*` 命名。
- Rua 相关 docs。
- Rua examples/tests/golden。
- `lualib/rua_rt.lua` 与 `lualib/meta/*.ruai` 中属于 Rua 语言工具链的部分。
- editor integration 中属于 Rua LSP 的部分。

### 7.2 `moon_rs` 保留内容

`moon_rs` 保留：

- moon runtime。
- Lua host / actor runtime。
- moon API 的 Rust/Lua 实现。
- 需要时保留运行 Rua 产物的示例，但示例应调用外部 `ruac`，不再依赖 workspace 内 crate。
- 可选保留 `lualib` 中纯 moon runtime 必需的 Lua 文件；若 `rua_rt.lua` 只服务 Rua 编译产物，则迁出到 `rua`。

`moon_rs` 不再保留：

- 旧 `moon-ruac` crate。
- 旧 `moon-rua-lsp` crate。
- 旧 `moon-rua-syntax` crate。
- Rua IDE/HIR/analysis crates。
- Rua LSP 编辑器扩展源码。

### 7.3 两仓库交互方式

允许的交互方式：

```text
rua     -> produces ruac (binary + 库 crate) / rua-lsp / generated Lua
moon_rs -> consumes generated Lua, 或调用 ruac binary, 或依赖发布的 ruac 库 crate 在进程内编译
```

推荐顺序：

1. 开发期：`moon_rs` 示例通过环境变量或配置定位本地 `ruac` binary。
2. CI：下载或构建指定版本的 `ruac`。
3. 发布：`rua` 发布 `ruac`/`rua-lsp` binary；`moon_rs` 不需要编译 Rua crates。

允许的例外——进程内内嵌：

- `moon_rs` 可以依赖 `rua` 发布的、版本化的 `ruac` **库 crate**（稳定门面、零依赖、核心无 IO），在进程内即时把 `.rua` 编成 Lua。
- 这不等于 workspace 耦合：用的是发布版本(crates.io / git tag / vendored)，不是 path dependency，也不把 `rua` 拉进 `moon_rs` 的 workspace。

不推荐：

- `moon_rs` 通过 path dependency 依赖新仓库的 **internal** crate（`rua-analysis` / `rua-syntax` 等）。
- `moon_rs` 把新仓库作为 workspace member。
- `rua` 依赖 `moon_rs` 内部 crate。

如果需要共享 moon API 类型定义，使用 `.ruai` 声明文件或生成的 metadata，而不是 Rust crate 互相依赖。

### 7.4 Git 历史迁移

优先保留历史：

```text
git filter-repo / git subtree split
  -> 提取 crates/moon-ruac
  -> 提取 crates/moon-rua-syntax
  -> 提取 crates/moon-rua-lsp
  -> 提取 docs/rua*
  -> 提取 Rua assets/lualib/editor files
```

如果保留完整历史成本过高，允许新仓库初始提交直接导入当前快照，但必须在 commit message 中记录来源：

```text
Imported Rua toolchain from moon_rs at <commit-sha>.
```

迁移后的 `moon_rs` 应有单独提交删除迁出的 workspace members，并更新 README/docs 指向新仓库。

### 7.5 Cargo / CI 要求

新仓库：

- 拥有独立 `Cargo.toml` workspace。
- `cargo test --workspace` 覆盖 compiler/syntax/ide/lsp。
- `cargo clippy --workspace --all-targets`。
- 单独发布 `ruac` 和 `rua-lsp` binary。
- CI 不依赖 `moon_rs` checkout。

`moon_rs`：

- `Cargo.toml` workspace members 移除 Rua crates。
- CI 不构建旧 `moon-ruac` / `moon-rua-lsp`。
- 如果示例需要 Rua 编译，使用预装/下载的 `ruac`。

## 8. Crate 方案

初始只 **4 个 crate**（对齐 emmylua）：

```text
rua-syntax     lexer + CST                      (≈ emmylua_parser)
rua-analysis   vfs + db_index + hir + semantic + ide   (≈ emmylua_code_analysis)
rua-lsp        协议层                            (≈ emmylua_ls)
ruac           编译器 CLI + 可内嵌库
```

`rua-analysis` 内部按**模块**（`vfs` / `db_index` / `hir` / `semantic` / `ide` / `diagnostic`）保持边界，
下面 §8.2 的各 `####` 就是这些内部模块。`rua-base-db` / `rua-hir-def` / `rua-hir-ty` / `rua-ide-*`
只是**将来按 profiling 拆 crate 的目标**，不是 Phase -1 到 Phase 3 的硬要求（拆分触发条件见 §3）。

### 8.1 `rua-syntax`

职责：

- IDE/LSP 专用 lexer/parser。
- rowan CST。
- typed AST wrappers。
- parse error recovery。
- token/text range/AST accessor。

不允许：

- 不做 name resolution。
- 不调用 typeck。
- 不依赖 LSP。
- 不依赖 `ruac`。
- 不作为 `ruac` 编译管线的硬依赖。

核心 API：

```rust
pub struct Parse<T> {
    pub tree: T,
    pub errors: Vec<ParseError>,
}

pub fn parse_source_file(text: &str) -> Parse<ast::SourceFile>;

pub trait AstNode {
    fn can_cast(kind: SyntaxKind) -> bool;
    fn cast(node: SyntaxNode) -> Option<Self>;
    fn syntax(&self) -> &SyntaxNode;
}
```

迁移要求：

- 当前 syntax crate 里的 semantic-ish 模块逐步迁出。
- `Analysis` 不再缓存 `MemberIndex` / `BindingTypes`。
- formatter 可继续依赖 syntax。
- 与 `ruac` parser 建 conformance corpus：合法输入接受集一致；错误恢复只属于 IDE parser，不要求 `ruac` 容错。

### 8.2 `rua-analysis`

一个 crate 承载语义与 IDE 能力（≈ emmylua 的 `emmylua_code_analysis`）。内部用模块划分,
crate 边界只有一条。下面各 `####` 是它的内部模块。

#### `vfs` / input store

职责：

- 定义 `FileId`、`SourceRootId`、`Edition/Config`。
- 提供内存 input store（file text / source root / workspace config），不使用 salsa。
- VFS：文本为唯一真相，编辑器 buffer 覆盖磁盘。
- 不做具体 IDE feature。

核心类型：

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId(u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceRootId(u32);

pub struct Change {
    pub file_changes: Vec<FileChange>,
    pub root_changes: Vec<SourceRootChange>,
    pub config: Option<WorkspaceConfig>,
}
```

核心接口（纯内存 store + 手写缓存，非 salsa query）：

```rust
impl BaseDb {
    // 写入路径（loader / didOpen / didChange）——文本为唯一真相
    fn set_file_text(&mut self, file: FileId, text: Arc<str>);
    fn remove_file(&mut self, file: FileId);

    // 读取路径
    fn file_text(&self, file: FileId) -> Arc<str>;
    fn source_root(&self, root: SourceRootId) -> &SourceRoot;

    // parse 结果按 FileId 缓存；set_file_text / remove_file 使该文件缓存失效
    fn parse(&self, file: FileId) -> Arc<Parse<SourceFile>>;
}
```

#### `hir` — definition 层（item tree / def map）

职责：

- 从 CST lower 到 HIR definition 层。
- 建立 item tree。
- 建立 module tree / def map。
- 处理 `mod`、`use`、visibility、`.ruai`。
- 产生 `DefId` / `ModuleId` / `LocalModuleId`。

核心分层：

```text
ItemTree
  单文件 item 摘要，不包含函数体细节。

DefMap
  workspace/module 级名称解析。

Body
  单个函数/const/initializer 的表达式与局部绑定。
```

重要不变量：

- 修改函数体不应导致 `ItemTree` 大面积失效。
- 修改一个函数体不应导致全局 module/name resolution 重算。
- `mod foo;` 只解析到 `FileId`，不直接读磁盘。

核心 ID：

```rust
pub struct DefId {
    pub file_id: FileId,
    pub local_id: LocalDefId,
}

pub enum DefKind {
    Function,
    Struct,
    Enum,
    Trait,
    Impl,
    Field,
    Variant,
    Module,
    ExternFn,
    TypeAlias,
}
```

#### `hir` — type inference

职责：

- 类型表示 `Ty`。
- 表达式类型推断。
- 函数签名、泛型、trait bound。
- method/member lookup。
- enum pattern/Option/Result flow narrowing。
- type diagnostics。

核心类型：

```rust
pub enum Ty {
    Unknown,
    Never,
    Bool,
    I64,
    F64,
    String,
    Unit,
    Vec(Arc<Ty>),
    HashMap(Arc<Ty>, Arc<Ty>),
    Option(Arc<Ty>),
    Result(Arc<Ty>, Arc<Ty>),
    Adt(AdtId, Substitution),
    Function(SignatureId),
    Ref { mutable: bool, inner: Arc<Ty> },
}

pub struct InferenceResult {
    pub expr_types: ArenaMap<ExprId, Ty>,
    pub pat_types: ArenaMap<PatId, Ty>,
    pub binding_types: ArenaMap<BindingId, Ty>,
    pub method_resolutions: ArenaMap<ExprId, MethodResolution>,
    pub diagnostics: Vec<TypeDiagnostic>,
}
```

核心查询：

```rust
fn infer(db: &dyn HirTyDb, def: DefWithBodyId) -> Arc<InferenceResult>;
fn type_of_expr(db: &dyn HirTyDb, file: FileId, offset: TextSize) -> Option<Ty>;
fn lookup_members(db: &dyn HirTyDb, ty: Ty, scope: ModuleId) -> Vec<Member>;
fn resolve_call_signature(db: &dyn HirTyDb, call: CallId) -> Option<Signature>;
```

#### `semantic` — SemanticModel facade

职责：

- 对外隐藏 `hir` 内部结构。
- 提供稳定 semantic facade（查询语义的唯一入口）。
- IDE/LSP 都通过这里拿语义；`ruac` 不通过这里编译。

示例 API：

```rust
pub struct Semantics<'db> {
    db: &'db dyn HirDb,
}

impl<'db> Semantics<'db> {
    pub fn find_def_at(&self, pos: FilePosition) -> Option<Definition>;
    pub fn type_at(&self, pos: FilePosition) -> Option<Ty>;
    pub fn resolve_path(&self, file: FileId, path: AstPtr<Path>) -> Option<Definition>;
    pub fn resolve_member(&self, file: FileId, offset: TextSize) -> Option<Member>;
}
```

#### `ide` — 通用数据结构（ide-db）

职责：

- IDE 通用数据结构。
- `LineIndex`。
- `FilePosition` / `FileRange`。
- `NavigationTarget`。
- `SourceChange`。
- text edit / assist edit builders。
- symbol search index。

不允许：

- 不引入 `lsp_types`。
- 不直接做 socket/stdio。

#### `ide` — AnalysisHost / Analysis

职责：

- 提供 `AnalysisHost` / `Analysis`（`rua-analysis` 的对外主入口）。
- 聚合 completion/diagnostic/assist 等 feature 模块。
- 是 LSP 和测试最常用入口。

核心 API：

```rust
pub struct AnalysisHost {
    db: RootDatabase,
}

impl AnalysisHost {
    pub fn new() -> Self;
    pub fn apply_change(&mut self, change: Change);
    pub fn analysis(&self) -> Analysis;
}

pub struct Analysis {
    db: RootDatabaseSnapshot,
}

impl Analysis {
    pub fn diagnostics(&self, file_id: FileId) -> Vec<Diagnostic>;
    pub fn hover(&self, pos: FilePosition) -> Option<HoverResult>;
    pub fn goto_definition(&self, pos: FilePosition) -> Option<Vec<NavigationTarget>>;
    pub fn completion(&self, pos: FilePosition) -> Vec<CompletionItem>;
    pub fn references(&self, pos: FilePosition) -> Vec<FileRange>;
    pub fn rename(&self, pos: FilePosition, new_name: &str) -> Result<SourceChange, RenameError>;
    pub fn semantic_tokens(&self, file_id: FileId) -> Vec<SemanticToken>;
    pub fn inlay_hints(&self, file_id: FileId, range: Option<TextRange>) -> Vec<InlayHint>;
    pub fn signature_help(&self, pos: FilePosition) -> Option<SignatureHelp>;
}
```

### 8.3 `rua-lsp`

职责：

- LSP capability registration。
- JSON-RPC transport。
- file watcher / workspace folder / configuration。
- 外部 library `.ruai` 配置、命令、watcher。
- request cancellation。
- 将 LSP type 转为 IDE type。

目录建议：

```text
crates/rua-lsp/src/
  main.rs
  server.rs
  global_state.rs
  dispatch.rs
  convert.rs
  handlers/
    completion.rs
    hover.rs
    goto_definition.rs
    references.rs
    rename.rs
    diagnostics.rs
    semantic_tokens.rs
    inlay_hints.rs
    signature_help.rs
    code_actions.rs
    document_symbol.rs
    workspace_symbol.rs
    formatting.rs
```

LSP 层不允许：

- 不直接调用 `ruac::check_diags`。
- 不直接遍历 rowan。
- 不直接做 type inference。
- 不直接扫描 workspace references。

### 8.4 `ruac`

`ruac` 分为**库**（`ruac` lib）与**可执行**（`ruac` bin）两层。
库必须可被宿主程序直接内嵌（例如 `moon_rs` runtime 想在进程内即时把 `.rua` 编成 Lua），
因此库的依赖、IO、公共面都要按"可嵌入"标准约束。

现状（起点，已经不错）：

- **零外部依赖**：`moon-ruac` 没有 `[dependencies]`，纯 std。这条要作为硬约束保持
  （建议 CI 断言 `cargo tree` 为空；与 `rua-analysis` 做 parity 时不得把 rowan/salsa 拉回 `ruac` 核心）。
- **Cargo 层已分 lib/bin**：`[lib]` + `[[bin]]` 已存在，`main.rs` 很薄。

需要修的两处（内嵌阻碍）：

- **核心自己读磁盘**：多文件 `mod name;` 由 `resolve` 直接 `fs::read_to_string`；`compile_str`
  纯但不支持 `mod`。核心必须改为通过调用方提供的 source-loader 取源码，磁盘 IO 只留在便利封装里。
- **内部模块全 `pub`**：等于把实现细节当成公共 API，且 bin 会伸手进内部（现在 `main.rs` 直接调
  `check::check`/`typeck::check`）。要收窄成一个精简 facade。

#### 库 `ruac`（lib）

职责：

- parse -> owned AST -> resolve/check/typeck -> codegen -> Lua。
- 对外只暴露精简、稳定的 facade；内部模块 `pub(crate)`。
- 结构化诊断（`Vec<Diagnostic>`）为主 API，字符串渲染仅作便利。

约束：

- 零外部依赖，纯 std。
- 核心不做磁盘 IO：源文本由 source-loader / VFS 注入。
- 不 `process::exit`、不 `eprintln!`；错误经 `Result` / 结构化诊断返回。
- 不依赖 `lsp_types`、不依赖 rowan、不依赖 `rua-analysis`。
- 不为了 LSP 功能扩展临时 semantic 查询 API；LSP 能力由 `rua-analysis` 承担。

公共面（示意，内部一律 `pub(crate)`）：

```rust
// 纯核心：无 IO，源码由 loader 提供
pub fn compile(entry: FileId, loader: &mut dyn SourceLoader) -> Result<Compiled, Vec<Diagnostic>>;
pub fn check(entry: FileId, loader: &mut dyn SourceLoader) -> Vec<Diagnostic>;

// 单文件便利：无 mod、无 IO
pub fn compile_str(src: &str) -> Result<String, Vec<Diagnostic>>;

// std fs 便利封装：feature = "fs"（默认开），内部就是 FsLoader 包一层
#[cfg(feature = "fs")]
pub fn compile_path(path: &Path) -> Result<String, Vec<Diagnostic>>;
```

其中 `FileId`、`Diagnostic`、`Compiled` 是 `ruac` 自己的极小公共类型，不复用 `rua-analysis` 的 IDE 类型。

source-loader 缝（宿主内嵌的关键抽象，也正是 §9 VFS 注入的同一条规则）：

```rust
pub trait SourceLoader {
    /// 把 `mod name;`（相对某源单元）解析并返回其文本 + 稳定 FileId。
    fn load_module(&mut self, from: FileId, name: &str) -> Result<(FileId, Arc<str>), String>;
    fn file_text(&self, file: FileId) -> Arc<str>;
}
```

- CLI 传 `FsLoader`（读磁盘）；宿主传自己的内存 / VFS / 打包资源 loader。
- LSP 的 `AnalysisHost` 可以设计类似的 loader/VFS 注入规则，但不直接复用 `ruac` loader 类型，避免 crate 依赖倒挂。

#### 可执行 `ruac`（bin）

职责：

- CLI：`build`、`check`、`fmt`、`trace`。
- 参数解析、`FsLoader` 装配、把结构化诊断渲染成 `path:line: msg`、`fs::write` 输出、`ExitCode`。
- **只调用库的公共 facade**，不碰内部模块（这条本身就是"边界干净"的验收测试）。

迁移后管线：

```text
CLI args -> FsLoader
  -> ruac::compile(entry, loader)   // 库核心，无 IO
  -> Lua output -> fs::write
```

保留策略：

- 保留 `ruac` 自有 parser / owned AST / typeck / codegen 作为编译器核心。
- 重构重点放在 facade 收窄、IO 外置、结构化诊断、模块可见性和 golden 稳定性。
- 与 `rua-syntax` / `rua-analysis` 通过 conformance、golden Lua 输出、诊断快照、type parity 测试对齐。

## 9. VFS / Project Model

Rua analysis 核心不能直接读磁盘。所有文件内容由外层注入：

```text
LSP file watcher / didOpen / didChange
  -> VFS collects file text
  -> AnalysisHost::apply_change

IDE tests / standalone analysis tools
  -> construct in-memory Change
  -> AnalysisHost::apply_change
```

`ruac build/check` 不走 `AnalysisHost`；它走 `ruac::SourceLoader` 和自己的编译器 pipeline。两边都遵守“核心不直接 IO”的规则，但类型和依赖边界分开。

VFS 需要支持：

- file URI。
- non-file URI，如 untitled buffer。
- `.rua`。
- `.ruai`。
- std/prelude/library roots。
- LSP 动态新增的外部库定义文件/目录，语义类似 TypeScript 的 `.d.ts`。
- open buffer 覆盖 disk text。
- workspace roots。
- file version。

Module resolution 不读磁盘，只在已知 source roots 中查：

```text
mod foo;
  -> current_dir/foo.rua
  -> current_dir/foo/mod.rua
  -> current_dir/foo.ruai
  -> current_dir/foo/mod.ruai
```

如果文件不存在，产生 resolve diagnostic，而不是尝试 IO。

### 9.1 外部库定义文件

LSP 必须支持把外部库定义文件加入 analysis，就像 TypeScript 把 `.d.ts` 加入项目一样。Rua 的声明文件统一使用 `.ruai`。

目标能力：

- 用户可以在编辑器配置中声明外部库定义文件或目录。
- LSP 可以在运行时新增、移除、刷新这些定义文件，不需要重启 server。
- 外部定义参与 hover、completion、goto definition、references、type check、signature help。
- 外部定义不参与 codegen。
- 外部定义默认只读：goto 可以跳转，rename/code action 不应修改 library root 内文件。

建议配置形态：

```json
{
  "rua.library": [
    "/path/to/moon/meta",
    "/path/to/vendor/foo.ruai"
  ],
  "rua.libraryMounts": {
    "moon": "/path/to/moon/meta",
    "foo": "/path/to/vendor/foo.ruai"
  }
}
```

说明：

- `rua.library`：普通 library roots。目录下的 `name.ruai` / `name/mod.ruai` 暴露为模块 `name`。
- `rua.libraryMounts`：显式挂载，适合一个文件或目录要绑定到指定模块名。
- LSP 通过 `workspace/didChangeConfiguration` 接收变更，重新扫描 library roots，并以 `Change` 提交到 `AnalysisHost`。
- `ruac check --lib <path>` / `ruac build --lib <path>` 是编译器侧可选能力；若实现，必须通过 `ruac` 自己的 loader/声明解析完成，并用 parity tests 与 LSP `.ruai` 语义对齐。

LSP 命令：

```text
rua.addLibrary(path, mount?)
rua.removeLibrary(path_or_mount)
rua.reloadLibraries()
```

这些命令只修改 server 侧当前 session 的 library roots；是否持久化到编辑器配置由客户端扩展决定。server 必须把命令结果转成同一套 `Change`，避免出现“配置路径”和“命令路径”两套状态。

SourceRoot 类型：

```rust
pub enum SourceRootKind {
    Workspace,
    Library,
    Std,
    Virtual,
}

pub enum FileKind {
    Source,
    Declaration,
}
```

解析优先级：

```text
workspace .rua
workspace .ruai
library .ruai
std/prelude .ruai
```

优先级规则：

- workspace 文件优先于外部 library。
- `.rua` 实现优先于 `.ruai` 声明。
- library root 中的 `.ruai` 只提供类型/签名/文档，不生成 Lua。
- library root 可以被 file watcher 监听；变更后只 invalidates 依赖它的模块/typeck。
- library diagnostics 默认只报告 parse/resolve/type 这类阻塞性错误，不报告 unused/style。

外部定义示例：

```rust
// moon.ruai
extern "lua" {
    pub fn log(msg: String);
    pub fn sleep(ms: i64);
}

pub mod uuid {
    pub fn new_v4() -> String;
}
```

使用：

```rust
mod moon;

fn main() {
    moon::log("hello");
    let id = moon::uuid::new_v4();
}
```

LSP 查询结果：

- `moon::log` hover 显示 `.ruai` 签名和文档。
- completion 在 `moon::` 后显示 `log`、`sleep`、`uuid`。
- go-to-definition 跳到外部 `.ruai` 文件。
- rename `moon::log` 被拒绝，因为目标在 `SourceRootKind::Library`。

## 10. IDE Feature 目标

### 10.1 第一批迁移

先迁移当前已存在能力：

- diagnostics。
- formatting。
- hover。
- goto definition。
- document symbol。
- completion。
- references。
- rename。

要求：

- 迁移后行为不能明显退化。
- 当前 member access/member completion 用例要保留。
- 跨文件 `mod`、`.ruai`、open buffer 都必须走 VFS。
- 外部 library `.ruai` 必须能通过 LSP 配置注入，并参与补全/跳转/诊断。

### 10.2 EmmyLua 级补齐

之后补：

- semantic tokens。
- inlay hints。
- signature help。
- workspace symbol。
- document highlight。
- code actions。
- completion resolve。
- module path completion。
- auto import。
- type definition。
- implementation。
- folding range。
- selection range。
- document link。
- diagnostics pull model。

Completion providers 拆分：

```text
keyword_provider
scope_provider
module_path_provider
member_provider
type_provider
field_provider
variant_provider
function_provider
macro_provider
auto_import_provider
postfix_provider
snippet_provider
```

## 11. Diagnostics 设计

诊断分层：

```text
ParseDiagnostic
ResolveDiagnostic
VisibilityDiagnostic
TypeDiagnostic
FlowDiagnostic
UnusedDiagnostic
StyleDiagnostic
```

IDE 对外统一：

```rust
pub struct Diagnostic {
    pub range: FileRange,
    pub severity: Severity,
    pub code: Option<DiagnosticCode>,
    pub message: String,
    pub fixes: Vec<Assist>,
}
```

LSP push/pull 都消费同一数据。

## 12. Rename / References 设计

不能再用“同名 token 预过滤 + goto 校验”作为主路径。

目标：

```text
ReferenceIndex
  DefId -> Vec<FileRange>
  FileId -> Vec<(NameRef, ResolvedDef)>
```

references 查询：

```text
pos -> Definition -> DefId -> ReferenceIndex lookup
```

rename 查询：

```text
pos -> Definition -> DefId
validate new name
collect references
filter declaration files / generated files
produce SourceChange
```

成员 rename 必须基于 resolved member id，而不是字段名文本。

## 13. Type / Member / Signature 设计

成员访问：

```text
receiver expr -> Ty -> lookup_members(scope, Ty)
```

成员补全：

```text
completion context detects receiver
Semantics::type_of_expr(receiver)
lookup_members
return fields/methods/assoc items
```

signature help：

```text
cursor -> CallExpr
callee type -> callable signatures
active parameter by comma count
```

hover：

```text
name/path/member/literal/expression -> semantic info -> markdown
```

inlay hints：

```text
binding type hints
parameter hints
return type hints
chaining/intermediate type hints, optional
```

## 14. Incremental 策略

**不引入 salsa。** 采用 emmylua 式手写 per-file 增量：`DbIndex` 上按文件删除 + 重建索引
（`remove_index(file_ids)` / `update_index(file_ids)`），配合文件级脏标记、模块依赖图和
item signature hash 决定连带失效范围。

理由：

- Rua 规模小（见 §5）、无宏、类型系统简单，整项目或按文件重索引本就在毫秒级；salsa 的收益
  换不回它的学习成本、API 变动风险和内存/调试开销。emmylua 作为完整 Lua LSP（Lua 5.1–5.5、
  40+ 诊断、flow narrowing）就没有用 salsa，是最直接的佐证。
- salsa 最核心的 backdating 收益可以廉价近似：比较文件导出 item signature 的 hash，没变就
  不连带重算依赖者。
- 手写增量能 `println!` 调试、能单步；salsa 的重算是隐式的，难排查。
- 把查询边界收敛到 `Semantics` / `SemanticModel` facade 后面；若将来真做到超大 workspace
  需要 salsa，替换点只在 facade 内部，不污染 LSP 与 codegen。

索引 / 缓存粒度（都是普通 Rust 数据结构 + 手写失效，不是 salsa query）：

```text
parse(FileId)            -> 按 FileId 缓存，set_file_text 时失效
item_tree(FileId)        -> 按 FileId 缓存
def_map(SourceRootId)    -> 按 source root 缓存
body(DefWithBodyId)      -> 按 def 缓存
infer(DefWithBodyId)     -> 按 def 缓存
diagnostics(FileId)      -> 按 FileId 缓存
symbol_index(SourceRootId)
reference_index          -> DefId -> Vec<FileRange>
```

文件变更处理（emmylua 式）：

```text
update_file(file):
    invalidate parse / item_tree / body / infer / diagnostics(file)
    db.remove_index([file]); db.update_index([file])
    if item signature hash 变化:
        recompute def_map(affected root) + dependents（沿 module graph）
```

重要失效边界：

- 改注释/空白：parse 变化，但 item signature hash 不变 → 不连带重算 def_map/dependents。
- 改函数体：只重算 body + infer + diagnostics(file)。
- 改 item signature：重算 item tree + 受影响 def map scope + dependents。
- 改 `mod/use`：重算 def map / module graph。
- 改 `.ruai`：重算依赖它的 source roots / typeck。

## 15. 测试策略

现有 `tests/fixtures/examples/*.rua` 更像 smoke/examples，不能作为完整 oracle。重构前必须新增系统化 golden corpus，放到 `tests/golden/` 或等价目录：

```text
tests/golden/
  compile-pass/       .rua -> .lua.golden
  compile-fail/       .rua -> .diag.golden
  parser/accept/      parser accept corpus
  parser/reject/      parser reject corpus
  parser/ranges/      key token/range snapshots
  modules/            multi-file module fixtures
  ruai/               external declaration/library fixtures
  ide/                LSP/IDE expect snapshots
```

最低覆盖要求：

- compile-pass golden 至少 30 个，覆盖 expressions、statements、functions、closures、iterators、structs/enums、Option/Result、containers、modules/use/pub、extern/std、generics/traits、`.ruai`。
- compile-fail diagnostic golden 至少 30 个，覆盖 parse、name、call、type、closure inference/capture、iterator adapter、struct/enum、module visibility、trait/generic、Option/Result、`.ruai` 错误。
- parser/range golden 至少 20 个，覆盖 lexical、item ranges、expression ranges、closure pipe/range/adapters、generic/block ambiguity、error recovery。
- 每个新增语言特性必须先补 golden 或 snapshot，再改实现。
- golden 更新必须显式运行，不允许普通测试自动覆盖 expected output。

### 15.1 Syntax

- lexer golden。
- parser round-trip。
- parser error recovery。
- AST accessor coverage。

### 15.2 HIR

- item tree snapshots。
- def map snapshots。
- name resolution snapshots。
- visibility tests。
- module resolution tests。

### 15.3 Typeck

- expression type snapshots。
- closure param/return inference tests。
- closure capture tests。
- iterator item type and adapter chain tests。
- member lookup tests。
- method resolution tests。
- generic tests。
- Option/Result/match/if-let narrowing tests。

### 15.4 IDE

使用 cursor marker 测试：

```text
fn main() {
    let p = Point { x: 1 };
    p.$0
}
```

测试输出用 expect snapshot：

- completion labels。
- hover markdown。
- goto target。
- references ranges。
- rename edits。
- diagnostics。
- semantic tokens。
- inlay hints。
- external library `.ruai` completion/goto/hover/rename-readonly。

### 15.5 Compiler

- `tests/golden/compile-pass/*.rua` 的 byte-exact Lua golden 输出。
- `tests/golden/compile-fail/*.rua` 的 diagnostic code/range/message golden 输出。
- iterator lazy/fused codegen golden：range/Vec/map/filter/enumerate/fold/collect 不产生中间 Vec 或 coroutine。
- 现有 `tests/fixtures/examples/*.rua` 继续作为 smoke corpus，但不是唯一 oracle。
- CLI build/check 行为。
- `.ruai` 声明模块。
- `--lib` 注入外部 `.ruai`，与 LSP `rua.library` 解析一致。
- sourcemap。
- runtime examples。

## 16. 迁移阶段

### Phase -1：独立仓库剥离

目标：

- 先把 Rua 工具链从 `moon_rs` 中物理剥离。
- 建立独立 git 仓库，后续所有 Rua 编译器/LSP/HIR/IDE 重构都在新仓库完成。

任务：

1. 创建新仓库 `/Users/bruce/GitProjects/rua`。
2. 迁移并重命名：`crates/moon-ruac` -> `crates/ruac`，`crates/moon-rua-syntax` -> `crates/rua-syntax`，`crates/moon-rua-lsp` -> `crates/rua-lsp`。
3. 迁移 Rua 相关 `docs/rua*`、examples/tests/golden、`lualib/rua_rt.lua`、`lualib/meta/*.ruai`、editor integration。
4. 在新仓库建立独立 workspace、CI、README。
5. 在 `moon_rs` 中移除 Rua crates 的 workspace membership 和 path dependency。
6. 更新 `moon_rs` 文档：Rua 编译器/LSP 已迁移到独立仓库，`moon_rs` 如需 Rua 只调用外部 `ruac`。
7. 记录源仓库 commit SHA，必要时用 `git subtree split` / `git filter-repo` 保留历史。

退出条件：

- 新仓库 `cargo test --workspace` 能单独运行。
- `moon_rs` 不再构建旧 `moon-ruac` / `moon-rua-lsp` / `moon-rua-syntax`。
- `moon_rs` CI 不依赖 Rua workspace crates。
- Rua LSP 和 `ruac` 的后续开发只发生在新仓库。

### Phase 0：冻结与基线

目标：

- 冻结 v3 行为。
- 补足当前 LSP/ruac/syntax 关键测试。
- 建立 golden baseline。

任务：

1. 记录新仓库基线：`cargo test -p ruac -p rua-syntax -p rua-lsp --features rua-lsp/lsp` 状态。
2. 为已有 member access/member completion/rename/references 建 snapshot 或 golden。
3. 标记旧架构 API：`member_index`、`binding_types`、`member_completion_src` 为 transition-only。
4. 建 `ruac` oracle corpus：accept/reject、diagnostic range/code、golden Lua output，后续 `rua-analysis` parity 都以此为基准。

退出条件：

- 当前行为有足够测试保护，可以开始破坏性迁移。
- 基线测试在新仓库内记录，而不是依赖 `moon_rs` workspace。
- `ruac` 作为 gold standard 的测试入口明确。

### Phase 1：双 parser 基线与 conformance

目标：

- 明确 `rua-syntax` 与 `ruac` parser 的职责边界。
- `rua-syntax` 作为 IDE/LSP parser，提供 lossless CST、容错、typed AST wrappers。
- `ruac` 保留简洁 compiler parser / owned AST，不为 LSP 功能继续扩桥。
- 建立 parser conformance 网，防止双 parser 行为漂移。

任务：

1. 清理 `rua-syntax` 对 `ruac` 的依赖。
2. `rua-syntax` parse API 统一为 `Parse<SourceFile>`。
3. 补齐 AST wrappers，覆盖 IDE/LSP 所需语法。
4. 建 parser accept/reject conformance：合法 corpus 上 `rua-syntax` 与 `ruac` 接受集一致。
5. 建 token/text range conformance：关键 token、ident、path、item range 在两边可稳定映射。
6. 错误恢复只要求 `rua-syntax` 支持；`ruac` 对错误输入保持简洁失败模型。
7. 如果共享 lexer/token 会显著降低漂移，可以提取极小 `rua-lex` 模块；否则先用 conformance 测试约束，不额外拆 crate。

退出条件：

- `rua-syntax` 可独立承担 IDE/LSP parse。
- `ruac` parser 仍保持 rowan-free / analysis-free。
- 双 parser conformance 测试进入 CI。
- 旧 ruac 测试仍绿。

### Phase 2：rua-analysis + VFS + AnalysisHost

目标：

- 引入 root database。
- LSP 和 IDE 测试工具都能向 AnalysisHost 提交文件。
- LSP 能注入外部 library `.ruai` source roots。

任务：

1. 新建 `rua-analysis`（先建 `vfs` / `db_index` 模块）。
2. 定义 `FileId`、`SourceRootId`、`Change`。
3. 引入内存 root database（input store + 手写 per-file 缓存失效，无 salsa）。
4. 实现 `parse(FileId)` 缓存。
5. 在 `rua-analysis` 的 `ide` 模块加 `AnalysisHost / Analysis` skeleton。
6. 支持 `SourceRootKind::Workspace/Library/Std/Virtual`。
7. 支持 `FileKind::Source/Declaration`。
8. LSP 配置 `rua.library` / `rua.libraryMounts` 可转成 `Change`。

退出条件：

- LSP 能通过 AnalysisHost 获取 parse tree/diagnostics stub。
- 核心 analysis 不读磁盘。
- 外部 `.ruai` 文件变更可被重新载入并触发相关缓存/索引失效。

### Phase 3：HIR def

目标：

- 在 `rua-analysis` 中建立 IDE 专用 item/module/name resolution；以当前 `ruac::resolve` 行为作为 parity baseline。

任务：

1. 在 `rua-analysis` 的 `hir` 模块实现 definition/name-resolution；将来必要时才拆为 `rua-hir-def`。
2. 实现 `ItemTree`。
3. 实现 `DefMap`。
4. 实现 `mod` / `use` / visibility。
5. 支持 `.ruai`。
6. 提供 `find_def_at` 基础能力。
7. 增加 module/name resolution parity tests，对齐 `ruac` 对可编译项目的解析结果。

退出条件：

- goto definition 不依赖旧 `rua-syntax::nameres`。
- document symbol/workspace symbol 可从 HIR 输出。
- `rua-analysis` 与 `ruac` 在 module/name resolution 的已覆盖场景无分歧。

### Phase 4：HIR body + typeck

目标：

- 在 `rua-analysis` 中建立 IDE 专用 type inference 和 member lookup；以当前 `ruac::typeck` 行为作为 parity baseline。

任务：

1. lower function body 到 HIR body。
2. 实现基础类型推断。
3. 实现 ADT/enum/struct/trait/impl。
4. 实现 member lookup。
5. 实现 call signature。
6. 实现 Option/Result/pattern narrowing。
7. 实现闭包类型推断：参数、返回值、捕获、闭包作为 iterator adapter 参数。
8. 实现 iterator item type 推断：range、Vec、`.iter()`、`.map()`、`.filter()`、`.enumerate()`、`.fold()`、`.collect()`。
9. 类型不确定时保留 `Ty::Unknown` / partial inference，不向 IDE 暴露过度精确的假结果。
10. 增加 type/member/diagnostic parity tests，对齐 `ruac` 的 type legality。

退出条件：

- hover type、member access、member completion、signature help 能从新 typeck 得到结果。
- 主要 typeck diagnostics 可从新层输出。
- 闭包和 iterator 的已覆盖场景可显示可靠 item/return type。
- 对已覆盖语法，IDE type/member 结果要么与 `ruac` 一致，要么显式降级为 `Unknown`。

### Phase 5：IDE feature 迁移

目标：

- 现有 LSP 功能全部迁到 `rua-analysis` 的 `ide` 模块。

任务：

1. diagnostics。
2. hover。
3. goto definition。
4. completion。
5. references。
6. rename。
7. document symbol。
8. formatting 保持 syntax/formatter。

退出条件：

- LSP 不再调用 `ruac::check_diags/member_index/binding_types/member_completion_src`。
- 当前 v3 用户可见能力不退化。

### Phase 6：ruac 精简化与行为对齐

目标：

- 保持 `ruac` 简洁编译器 pipeline：parser -> owned AST -> resolve/check/typeck -> codegen。
- 完成 `ruac` 可内嵌化：核心无 IO、公共 facade 精简、结构化诊断、lib/bin 边界干净。
- 与 `rua-analysis` 行为对齐，但不依赖 `rua-analysis`。

任务：

1. 收窄 `ruac` 公共 API：内部 parser/resolve/typeck/codegen 模块改为 `pub(crate)`。
2. 引入 `SourceLoader`，把 `mod` 解析所需 IO 从核心挪到 CLI / 宿主 loader。
3. 结构化 `Diagnostic` / `FileId` / `TextRange`，字符串渲染只放在 CLI。
4. 对齐并扩充 golden Lua 输出，保护 codegen 行为。
5. 支持 sourcemap（如果进入近期需求）。
6. 若 `ruac` 支持 `.ruai` / `--lib`，只在声明解析和 check 阶段使用，codegen 跳过 declaration file。
7. 增加与 `rua-syntax` 的 parser conformance、与 `rua-analysis` 的诊断/type parity 测试。
8. 提供稳定 `ruac check --json`（或等价结构化 API）作为 LSP/CI oracle。
9. 为 iterator chain 增加 lazy `IterPlan` / 等价内部表示；consumer 确定前不发 Lua。
10. 为 range/Vec/map/filter/enumerate/take/skip/fold/collect 生成 fused Lua loops；仅在 iterator escape 时退回 runtime iterator。

退出条件：

- `ruac` 可作为独立库 crate 被宿主内嵌，不拉入 rowan / `rua-analysis` / `lsp_types`。
- `ruac` bin 只调用 lib facade，不触碰内部模块。
- compiler 与 LSP 的关键行为通过 conformance/parity/golden 测试对齐。
- compiler-backed diagnostics 有稳定机器可读输出。
- iterator golden 证明简单链无中间 Vec、无 coroutine、无 per-item adapter closure。

### Phase 7：EmmyLua 级功能补齐

目标：

- 补齐高级 LSP 能力。

任务：

1. semantic tokens。
2. inlay hints。
3. signature help。
4. code actions。
5. workspace symbol。
6. document highlight。
7. folding/selection range。
8. completion resolve。
9. auto import/module path completion。
10. type definition/implementation。

退出条件：

- 功能矩阵接近 EmmyLua。
- 每项功能有 snapshot tests。

### Phase 8：LSP 工程化

目标：

- 大 workspace 可用。

任务：

1. handler 模块化。
2. cancellation。
3. debounce diagnostics。
4. file watcher。
5. library root watcher。
6. configuration reload。
7. workspace progress。
8. background indexing。
9. pull diagnostics。
10. 可选 compiler-backed diagnostics worker：后台运行 `ruac check --json`，将 compiler-exact diagnostics 与 fast IDE diagnostics 合并展示。

退出条件：

- 大项目编辑不卡主 loop。
- 诊断和索引可后台更新。
- 如果启用 compiler-backed diagnostics，`ruac` 结果优先于 IDE 近似诊断。

## 17. 风险与对策

| 风险 | 对策 |
| --- | --- |
| 重构跨度过大，长时间不可用 | phase-by-phase 保持旧能力可跑；LSP 新 analysis 先 shadow mode |
| 双 parser 行为漂移 | parser accept/reject corpus + token/range conformance + CI 必跑 |
| IDE HIR/type inference 与 `ruac` 编译语义漂移 | 诊断/type parity tests；`ruac` codegen 继续由 golden Lua 输出保护 |
| IDE 给出过度精确但错误的类型 | `Ty::Unknown` / partial inference 降级；hover/completion 标记不确定来源；compiler-backed diagnostics 优先 |
| shared core 把 IDE 依赖带进 `ruac` | shared core 只能依赖 std/极小基础类型；禁止 rowan/VFS/`lsp_types`/LSP config 进入 |
| compiler-backed diagnostics 与 open buffer 不同步 | 明确 fast diagnostics 与 compiler-exact diagnostics 来源；保存前使用 analysis，保存/快照后使用 `ruac check` |
| 拆仓库后 `moon_rs` 示例/CI 断裂 | 先定义外部 `ruac` 调用路径；迁移提交中同步更新 CI 和文档 |
| 两仓库出现循环依赖 | `rua` 不依赖 `moon_rs`；共享信息通过 `.ruai`/metadata/binary，而不是 Rust crate |
| 手写增量把小改动放大成全量重算 | 文件级脏标记 + 模块依赖图 + item signature hash backdating；索引按文件 remove/re-add（emmylua 式，见 §14） |
| `.ruai` / std lib / open buffer 语义复杂 | VFS/source root 先设计清楚，禁止核心层 IO |
| 外部库定义文件导致解析优先级混乱 | 明确 workspace > library > std，`.rua` 实现优先于 `.ruai` 声明 |
| completion 体验回退 | 迁移前固定 v3 completion snapshot |
| rename/reference 错改 | 必须基于 `DefId`/`MemberId`，禁止纯文本替换 |

## 18. 决策点

需要尽快确定：

1. 增量策略（已定）：手写 per-file 增量，不引入 salsa。
2. parser 策略（已定）：双 parser。`rua-syntax` 服务 IDE/LSP，`ruac` 保留简洁 compiler parser。
3. 双 parser conformance 范围：accept/reject corpus、token/range 映射、诊断路径、type parity 覆盖到什么粒度。
4. `ruac` oracle 输出格式：`check --json` 的 diagnostic code/range/severity/schema。
5. shared core 抽取标准：哪些逻辑稳定后可以共享，依赖上限是什么。
6. compiler-backed diagnostics 是否默认开启、何时触发、如何和 open buffer diagnostics 合并。
7. `.ruai` 是否纳入和 `.rua` 同一 HIR 表示，只在 analysis/codegen 边界标记 declaration。
8. 标准库/prelude 是内置虚拟文件，还是随 `lualib/meta` 注入 VFS。
9. 外部 library `.ruai` 的配置 schema 和模块挂载规则。
10. 独立仓库名称、发布节奏、`moon_rs` 如何获取 `ruac` binary。
11. 是否必须保留 Rua 相关目录的 git 历史。
12. `ruac` 库内嵌:宿主(如 `moon_rs`)是否允许直接依赖发布的 `ruac` **库 crate** 在进程内编译 `.rua`。

建议决策：

- 新仓库命名为 `rua`，本地路径为 `/Users/bruce/GitProjects/rua`。
- 不引入 salsa；采用 emmylua 式手写 per-file 增量（`remove_index`/`update_index` + 模块依赖图 + signature-hash backdating）。将来超大 workspace 再评估，替换点限制在 `Semantics` facade 内。
- 采用双 parser：`rua-syntax` 是 IDE/LSP parser，`ruac` 是 compiler parser。
- 不删除 `ruac` parser；只收窄它的公共 API，并通过 conformance/parity tests 控制漂移。
- `ruac` 是 gold standard；`rua-analysis` 精度追 `ruac`，不能证明时降级为 `Unknown`，不展示虚假的精确类型。
- shared core 只在逻辑稳定后抽取，且必须保持 `ruac` 友好：低依赖、无 rowan/VFS/LSP 类型。
- LSP 可增加 compiler-backed diagnostics worker；`ruac` 结果优先于 IDE fast diagnostics。
- `.ruai` 进入同一 HIR，只加 `is_decl` / `FileKind::Declaration` 标记。
- std/prelude 以虚拟 source root 注入 VFS。
- LSP 支持 `rua.library` 与 `rua.libraryMounts`，外部定义作为 `SourceRootKind::Library` 加入 VFS。
- library root 只读，rename/code action 不修改外部声明文件。
- `ruac` 库按可内嵌标准维护:零依赖、核心无 IO、公共面精简、lib/bin 分离(见 §8.4)。
- `moon_rs` 与 Rua 的默认交互仍是外部 `ruac` binary 或 generated Lua;但**允许**宿主直接依赖发布的 `ruac` **库 crate**(版本化,来自新仓库发布物)在进程内即时编译。这与"不依赖 Rua *internal* crates / 不做 workspace path 耦合"不冲突:被依赖的是稳定的 `ruac` 库门面,不是 HIR/IDE 内部 crate,也不是 workspace 成员。

## 19. 最小可交付切片

第一轮实际施工建议只做到 Phase -1 到 Phase 3：

```text
Phase -1 repository split
Phase 0 baseline
Phase 1 dual parser conformance baseline
Phase 2 rua-analysis + VFS + AnalysisHost
Phase 3 HIR def/name resolution
```

交付后应具备：

- 新 architecture skeleton。
- `rua` 独立仓库已建立在 `/Users/bruce/GitProjects/rua`，`ruac` 和 LSP 已从 `moon_rs` workspace 剥离。
- 双 parser 边界确定，conformance 测试进入 CI。
- LSP 可通过 `AnalysisHost` 查询 parse/name resolution。
- LSP 可配置外部 `.ruai` library root，并在 goto/completion 中可见。
- goto definition/document symbol/workspace symbol 有新管线原型。
- 旧 LSP/ruac 仍能跑。

这能把最大的不确定性先压下去；typeck/codegen 迁移再进入第二轮。
