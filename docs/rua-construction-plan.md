# Rua 拆仓与 IDE/LSP 施工计划

> 状态：已完成（2026-07-11）。Phase -1 至 Phase 4A 的各 Step 均已实现、验证并提交。
> `moon_rs` 的 workspace/doc 收尾分别记录在 `e126f2f` 和 `c5cca65`。
> 架构依据：`docs/rua-ide-architecture.md`。
> Golden 用例清单：`docs/rua-golden-cases.md`。
> 固定目标仓库：`/Users/bruce/GitProjects/rua`。

## 1. 施工原则

Phase 是里程碑，Step 是一次可验证提交。实际施工按 Step 执行，不按 Phase 一口气推进。

每个 Step 必须满足：

- 只解决一个问题：迁移、重命名、编译修复、测试补齐、API 收窄不能混在同一步。
- 有明确验证命令；如果验证失败，记录失败原因和下一步，不把失败藏进后续重构。
- 不在迁仓阶段做架构重写。Phase -1 只搬家、改名、修边界；Phase 0 以后才开始新架构施工。
- 不删除 `ruac` 自有 parser。`ruac` 保持简洁 compiler pipeline；LSP/IDE 走 `rua-analysis`。
- `ruac` 是 gold standard；`rua-analysis` 的精度通过 conformance/parity/golden 测试追 `ruac`。
- Golden 覆盖按 `docs/rua-golden-cases.md` 执行；现有 `tests/fixtures/examples/*.rua` 只作为 smoke corpus，不足以作为完整 oracle。
- 新仓库 crate/package/binary 不使用 `moon` 前缀。
- `moon_rs` 与 `rua` 不形成 workspace 或 path dependency 耦合。

每个 Step 的提交说明建议格式：

```text
<phase-step>: <short action>

Example:
-1.3: import ruac crate into rua repo
```

## 2. 执行边界

第一轮只做到 Phase -1 到 Phase 3：

```text
Phase -1  repository split
Phase 0   baseline and oracle
Phase 1   dual parser conformance
Phase 2   rua-analysis + VFS + AnalysisHost
Phase 3   HIR def/name resolution
```

第一轮不做：

- 不重写 `ruac` parser/typeck/codegen。
- 不把 `ruac` 接到 rowan / `rua-analysis`。
- 不做完整 type inference。
- 不在拆仓阶段实现闭包/lambda 与 iterator chain；这些进入 Phase 4A。
- 不补齐全部 LSP feature。
- 不删除 `moon_rs` 中非 Rua runtime/API 代码。

## 3. Phase -1：独立仓库剥离

目标：先把 Rua 工具链从 `moon_rs` 中物理剥离到 `/Users/bruce/GitProjects/rua`。这一阶段只处理仓库、路径、命名、编译边界。

### Step -1.1：迁移范围盘点

目标：

- 生成待迁移清单，确认哪些属于 Rua 工具链，哪些继续留在 `moon_rs`。

改动范围：

- 只读盘点；不移动文件。
- 可以在施工记录中写下源仓库 commit SHA。

建议检查：

```sh
git status --short
git rev-parse HEAD
rg -n "moon-ruac|moon-rua-syntax|moon-rua-lsp|ruac|rua-lsp|rua-syntax" Cargo.toml crates docs lualib editors
find crates/moon-ruac crates/moon-rua-syntax crates/moon-rua-lsp -maxdepth 2 -type f
```

退出条件：

- 明确迁移清单：`moon-ruac`、`moon-rua-syntax`、`moon-rua-lsp`、Rua docs、Rua examples/tests/golden、`lualib/rua_rt.lua`、`lualib/meta/*.ruai`、editor integration。
- 明确保留清单：`moon-runtime`、moon API、Lua 宿主能力继续留在 `moon_rs`。

### Step -1.2：创建新仓库骨架

目标：

- 创建 `/Users/bruce/GitProjects/rua`，初始化独立 git 仓库和 Cargo workspace。

改动范围：

- 只创建新仓库骨架：`Cargo.toml`、`README.md`、`crates/`、`docs/`、`tests/`。
- 暂不迁移任何 crate 源码。

建议文件：

```text
/Users/bruce/GitProjects/rua/
  Cargo.toml
  README.md
  crates/
  docs/
  tests/
```

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo metadata --format-version 1
git status --short
```

退出条件：

- 新仓库存在。
- `cargo metadata` 能运行。
- 初始 commit 可提交。

### Step -1.3：迁移 `moon-ruac` 为 `ruac`

目标：

- 把编译器 crate 迁到新仓库，并去掉 `moon` 前缀。

改动范围：

- 从 `moon_rs/crates/moon-ruac` 复制到 `rua/crates/ruac`。
- 修改 package/lib/bin 名称为 `ruac`。
- 修复 crate 内部路径和测试引用。
- 不改 parser/typeck/codegen 行为。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p ruac
cargo tree -p ruac
```

退出条件：

- `cargo test -p ruac` 通过，或失败原因被记录为迁移 blocker。
- `ruac` 不依赖 rowan、`rua-analysis`、`lsp_types`。
- 新仓库 commit。

### Step -1.4：迁移 `moon-rua-syntax` 为 `rua-syntax`

目标：

- 把 syntax/formatter/IDE parser 相关 crate 迁到新仓库，并去掉 `moon` 前缀。

改动范围：

- 从 `moon_rs/crates/moon-rua-syntax` 复制到 `rua/crates/rua-syntax`。
- 修改 package/lib 名称为 `rua-syntax`。
- 修复对 `moon-ruac` 的路径引用；必要时先保留 transition API，但不能新增对 `ruac` 的长期依赖。
- 不重构 parser。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p rua-syntax
cargo test -p ruac
```

退出条件：

- `rua-syntax` 可单独测试。
- `ruac` 仍可单独测试。
- 新仓库 commit。

### Step -1.5：迁移 `moon-rua-lsp` 为 `rua-lsp`

目标：

- 把 LSP crate 迁到新仓库，并去掉 `moon` 前缀。

改动范围：

- 从 `moon_rs/crates/moon-rua-lsp` 复制到 `rua/crates/rua-lsp`。
- 修改 package/bin 名称为 `rua-lsp`。
- 修复对 `rua-syntax` / `ruac` 的 package 引用。
- 允许先用 feature gate 或 TODO 标记保留旧桥接，不在本步重写 LSP 架构。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p rua-lsp --features lsp
cargo test -p rua-syntax
cargo test -p ruac
```

退出条件：

- `rua-lsp` 能测试或编译；若失败，失败原因必须明确记录。
- 没有 `moon-*` package 名残留在新仓库目标 crate 名称中。
- 新仓库 commit。

### Step -1.6：迁移文档、声明文件和测试资产

目标：

- 把 Rua 工具链相关资产迁到新仓库。

改动范围：

- 迁移 `docs/rua*`。
- 迁移 Rua examples/tests/golden。
- 迁移 `lualib/rua_rt.lua`。
- 迁移 `lualib/meta/*.ruai`。
- 迁移 editor integration。
- 不迁移 `moon-runtime` 的实现代码。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
find docs -maxdepth 2 -type f
find . -name "*.ruai" -o -name "*.rua"
cargo test --workspace
```

退出条件：

- 新仓库拥有编译器、LSP、syntax、docs、`.ruai`、测试资产。
- `cargo test --workspace` 状态被记录。
- 新仓库 commit。

### Step -1.7：新仓库命名清理

目标：

- 去掉新仓库内对 `moon` 前缀的 crate/package/binary 命名依赖。

改动范围：

- 只清理新仓库里的名字、README、Cargo metadata、命令名。
- 保留必要的历史说明，例如 “imported from moon_rs”。

建议检查：

```sh
cd /Users/bruce/GitProjects/rua
rg -n "moon-ruac|moon-rua-syntax|moon-rua-lsp|moon_ruac|moon_rua" .
cargo metadata --format-version 1
```

退出条件：

- 目标 crate/package/binary 名只使用 `ruac`、`rua-syntax`、`rua-lsp`、`rua-analysis`。
- 新仓库 commit。

### Step -1.8：新仓库 workspace 基线

目标：

- 让新仓库可以独立构建和测试。

改动范围：

- 只修 workspace/Cargo/test 配置。
- 不做功能重构。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test --workspace
cargo clippy --workspace --all-targets
```

退出条件：

- `cargo test --workspace` 通过，或有明确 blocker 列表。
- `cargo clippy` 状态记录。
- 新仓库 commit。

### Step -1.9：从 `moon_rs` 移除 Rua workspace 成员

目标：

- `moon_rs` 不再构建 Rua 编译器/LSP/syntax crates。

改动范围：

- 修改 `moon_rs/Cargo.toml` workspace members。
- 移除对 `crates/moon-ruac`、`crates/moon-rua-syntax`、`crates/moon-rua-lsp` 的 path dependency。
- 不删除用户未迁移确认的文件；先让 workspace 解耦。

验证命令：

```sh
cd /Users/bruce/GitProjects/moon_rs
cargo metadata --format-version 1
cargo test --workspace
```

退出条件：

- `moon_rs` workspace 不再包含 Rua crates。
- `moon_rs` 不需要新仓库 path dependency 才能构建。
- `moon_rs` commit。

### Step -1.10：更新 `moon_rs` 文档入口

目标：

- 让 `moon_rs` 文档说明 Rua 编译器/LSP 已迁出。

改动范围：

- 更新 README 或相关 docs。
- 指向 `/Users/bruce/GitProjects/rua` 或后续远端仓库。
- 说明 `moon_rs` 如需 Rua，使用外部 `ruac` binary、generated Lua、`.ruai` metadata，或发布版 `ruac` library facade。

验证命令：

```sh
cd /Users/bruce/GitProjects/moon_rs
rg -n "moon-ruac|moon-rua-syntax|moon-rua-lsp|ruac|rua-lsp" README.md docs Cargo.toml
```

退出条件：

- 文档不再暗示 Rua crates 仍在 `moon_rs` workspace 内维护。
- `moon_rs` commit。

## 4. Phase 0：冻结与基线

目标：迁仓后先固定现有行为，建立 `ruac` oracle 和 LSP snapshot，避免后续重构时漂移。

Golden 用例的具体 case id、文件名和覆盖点见 `docs/rua-golden-cases.md`。本节只描述施工顺序和验收条件。

### Step 0.1：记录迁仓后测试状态

改动范围：

- 只新增测试记录文档或 CI baseline。
- 不修功能。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p ruac
cargo test -p rua-syntax
cargo test -p rua-lsp --features lsp
cargo test --workspace
```

退出条件：

- 每条命令状态被记录。
- 已知失败有 issue/TODO。

### Step 0.2：建立 golden 目录规范和 harness

改动范围：

- 新增 `tests/golden/` 或等价目录。
- 先建立目录、命名、更新命令、断言策略。
- 不改 `ruac` 行为。

建议目录：

```text
tests/golden/
  compile-pass/       # .rua -> .lua.golden
  compile-fail/       # .rua -> .diag.golden
  parser/accept/      # parser accept corpus
  parser/reject/      # parser reject corpus
  parser/ranges/      # key token/range snapshots
  modules/            # multi-file module fixtures
  ruai/               # .ruai library fixtures
  ide/                # completion/hover/goto/reference/rename snapshots
```

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p ruac
```

退出条件：

- golden harness 能发现缺失 golden、输出 mismatch，并给出明确更新命令。
- golden 更新命令必须显式运行，不能在普通测试中自动覆盖。
- 后续 `rua-analysis` parity 可复用这些 fixture。

### Step 0.3：补齐 compile-pass Lua golden

改动范围：

- 为可编译输入新增 `.rua` 和 `.lua.golden`。
- 覆盖 codegen 稳定性，不改变当前 Lua 输出。
- 每个用例尽量短小，避免一个大示例覆盖太多行为而定位困难。

最低用例矩阵：

| 类别 | 用例 |
| --- | --- |
| expressions | precedence, unary, bool ops, comparison, nested call, field access, method call, indexing |
| closures | `|x| x + 1`, typed closure, block closure, read capture, immediate mutable capture |
| iterators | range for, inclusive range, Vec iteration, map/filter/enumerate/take/skip/fold/collect lazy chain |
| statements | let/mut assignment, tail return, explicit return, block tail, if expression temp, while, break, continue |
| functions | zero arg, typed params, return type, recursion, mutual recursion predeclare |
| structs | declaration, literal, missing optional none if supported, field access, associated fn, `&self` method |
| enums | unit/tuple/struct variants, construction, match bind, wildcard, or-pattern |
| Option/Result | `Some`/`None`, `Ok`/`Err`, `?`, match on Option/Result |
| containers | `Vec<T>`, `HashMap<K,V>` if supported, `.len()`, `.get()`, literal/macros |
| modules | inline `mod`, nested `mod`, file `mod`, sibling module call, `use`, `use as`, grouped `use` |
| visibility | public item access, private same-module access |
| extern/std | `extern "lua"`, variadic extern if supported, `println!`, `format!` |
| generics/traits | generic fn, generic struct/enum, trait impl, bounded generic, `where`, method-level generic |
| `.ruai` | declaration module used from `.rua`, declaration skipped by codegen |

建议文件名：

```text
compile-pass/expr_precedence.rua
compile-pass/control_if_while.rua
compile-pass/function_recursion.rua
compile-pass/struct_methods.rua
compile-pass/enum_match.rua
compile-pass/option_result_try.rua
compile-pass/containers_vec_hashmap.rua
compile-pass/module_inline_use.rua
modules/file_mod_basic/main.rua
modules/file_mod_nested/main.rua
compile-pass/extern_lua_std.rua
compile-pass/generic_trait_bounds.rua
ruai/library_decl_basic/main.rua
```

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p ruac golden_compile_pass
```

退出条件：

- 至少 30 个 compile-pass golden；如果闭包/iterator 进入实现范围，必须额外补对应 golden。
- 旧 `tests/fixtures/examples/*.rua` 继续作为 smoke corpus，但不再是唯一 golden 来源。
- 每个新增语言特性必须先补 compile-pass golden，再改实现。

### Step 0.4：补齐 compile-fail diagnostic golden

改动范围：

- 为拒绝输入新增 `.rua` 和 `.diag.golden`。
- 断言 diagnostic code、message、primary range、file path。
- 先允许 line/column 级 range；后续 `check --json` 稳定后升级到 byte range。

最低用例矩阵：

| 类别 | 用例 |
| --- | --- |
| parse | missing brace, bad item start, bad generic list, bad `where`, bad pattern |
| names | unresolved name, duplicate fn, duplicate field, duplicate variant, ambiguous variant |
| call | wrong arity, non-callable callee, method not found, associated fn used as method if invalid |
| types | assignment mismatch, return mismatch, branch type mismatch, invalid binary op, invalid field type |
| closures | closure param cannot infer, return mismatch, invalid mutable capture, unsupported escaping closure |
| iterators | non-iterable source, filter predicate not bool, map/filter arg not closure, collect type mismatch |
| structs/enums | missing field, extra field, wrong variant constructor form, variant pattern arity mismatch |
| modules | missing file module, private item access, invalid `use`, import private item |
| traits/generics | unknown trait bound, trait method missing in impl, trait bound not satisfied, method-level generic mismatch |
| Option/Result | `?` on non-Result, `Ok`/`Err` wrong arity, incompatible `Result<T,E>` return |
| `.ruai` | declaration file with body if forbidden, rename/codegen attempt against declaration, unresolved library mount |

建议文件名：

```text
compile-fail/parse_missing_brace.rua
compile-fail/name_duplicate_item.rua
compile-fail/type_return_mismatch.rua
compile-fail/call_wrong_arity.rua
compile-fail/struct_missing_field.rua
compile-fail/enum_bad_variant_form.rua
compile-fail/module_private_access.rua
compile-fail/generic_bound_unsatisfied.rua
compile-fail/result_try_on_non_result.rua
ruai/library_decl_invalid_body/main.rua
```

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p ruac golden_compile_fail
```

退出条件：

- 至少 30 个 compile-fail golden。
- 每类 diagnostic 至少有一个稳定 golden。
- `ruac` 作为 gold standard 的输入/输出用例可重复运行。

### Step 0.5：补齐 parser/range golden

改动范围：

- 为双 parser conformance 准备 corpus。
- 覆盖 parser accept/reject 和关键 token/text range。

最低用例矩阵：

| 类别 | 用例 |
| --- | --- |
| lexical | comments, string escapes, integer/floating literals, keywords vs identifiers |
| item ranges | fn, struct, enum, trait, impl, extern, mod, use |
| expression ranges | path, qualified path, call, method call, field, index, closure, range, struct literal, enum literal |
| generic ambiguity | `Vec<T>`, nested generics, comparison near generic syntax |
| closure/iterator ambiguity | `|x|`, `||`, `|` in patterns if supported, `..` / `..=`, `.map(|x| ...)` |
| block ambiguity | `Ident { .. }` struct literal vs `if cond { .. }` / `match x { .. }` |
| recovery | incomplete fn, incomplete call, missing comma, missing `}` |

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p rua-syntax parser_conformance
cargo test -p rua-syntax range_golden
```

退出条件：

- 至少 20 个 parser/range golden。
- Phase 1 conformance 可直接复用这些文件。

### Step 0.6：补齐 `.ruai` / external library golden

改动范围：

- 建 library root fixtures。
- 覆盖 workspace > library > std 优先级。
- 覆盖 `.rua` 实现优先于 `.ruai` 声明。

最低用例矩阵：

| 类别 | 用例 |
| --- | --- |
| library root | directory root, single-file mount, nested module mount |
| priority | workspace source shadows library declaration, library shadows std |
| IDE behavior | hover from `.ruai`, completion from `.ruai`, goto declaration, references include declaration |
| readonly | rename/code action must not edit library root |
| compiler behavior | declaration used for check, declaration skipped by codegen |

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p ruac golden_ruai
cargo test -p rua-syntax
cargo test -p rua-lsp --features lsp
```

退出条件：

- `.ruai` 不只是 smoke test；至少覆盖 library mount、priority、readonly、codegen skip。

### Step 0.7：固定现有 LSP/Syntax 行为快照

改动范围：

- 为 completion、hover、goto definition、references、rename、document symbol 增加最小 snapshot。
- 优先覆盖已有功能，不追求新能力。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p rua-syntax
cargo test -p rua-lsp --features lsp
```

退出条件：

- 现有用户可见能力有回归保护。

### Step 0.8：建立 golden 覆盖缺口清单

改动范围：

- 新增 `tests/golden/COVERAGE.md` 或等价文档。
- 标记每个语言能力是否已有 compile-pass、compile-fail、parser/range、IDE snapshot。
- 不要求一次补满所有远期功能，但不能没有清单。

建议表头：

```text
Feature | compile-pass | compile-fail | parser/range | IDE snapshot | Notes
```

退出条件：

- 已知缺口显式列出。
- 新 feature 合并前必须更新覆盖矩阵。

### Step 0.9：标记 transition-only API

改动范围：

- 给 `member_index`、`binding_types`、`member_completion_src`、`check_diags` 等桥接 API 加注释或 deprecated 标记。
- 不删除 API。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test --workspace
```

退出条件：

- 旧桥接 API 的临时性质明确。
- 后续 Phase 5 有清理目标。

## 5. Phase 1：双 parser 基线与 conformance

目标：明确 `ruac` compiler parser 与 `rua-syntax` IDE parser 的边界，建立双 parser 漂移预警。

### Step 1.1：断开 `rua-syntax` 到 `ruac` 的长期依赖

改动范围：

- 清理 `rua-syntax` 对 `ruac` 内部 AST/typeck/resolve 的依赖。
- 如短期无法断开，集中封装到 `transition` 模块并标记删除计划。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo tree -p rua-syntax
cargo test -p rua-syntax
```

退出条件：

- `rua-syntax` 不依赖 `ruac` 内部模块。

### Step 1.2：稳定 `rua-syntax` parse API

改动范围：

- 统一 `Parse<SourceFile>`、parse errors、typed AST wrapper 入口。
- 不做 name resolution。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p rua-syntax
```

退出条件：

- IDE parser 的公共入口稳定。

### Step 1.3：建立 parser accept/reject conformance

改动范围：

- 新增合法/非法 corpus。
- 同一输入分别跑 `rua-syntax` 与 `ruac` parser。
- 合法输入接受集必须一致；错误输入只要求 `rua-syntax` 能容错，不要求与 `ruac` 错误恢复一致。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p rua-syntax parser_conformance
cargo test -p ruac
```

退出条件：

- conformance 测试进入 CI。

### Step 1.4：建立 token/text range conformance

改动范围：

- 对 ident、path、item、function、field/member access 做 range 对齐测试。
- 不要求 AST 结构相同，只要求关键 source range 可映射。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p rua-syntax range_conformance
```

退出条件：

- rename/goto/reference 需要的基础 range 有回归保护。

## 6. Phase 2：`rua-analysis` + VFS + AnalysisHost

目标：建立 IDE analysis skeleton，先能吃文件、缓存 parse、注入 `.ruai` library root。

### Step 2.1：新增 `rua-analysis` 空 crate

改动范围：

- 新增 `crates/rua-analysis`。
- 只建模块骨架：`vfs`、`db_index`、`hir`、`semantic`、`ide`、`diagnostic`。
- 暂不实现 feature。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p rua-analysis
cargo test --workspace
```

退出条件：

- `rua-analysis` 进入 workspace。
- LSP 尚不依赖它也可以。

### Step 2.2：实现 VFS 基础 ID 和 Change

改动范围：

- 定义 `FileId`、`SourceRootId`、`FileKind`、`SourceRootKind`、`Change`。
- 不做磁盘 IO。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p rua-analysis vfs
```

退出条件：

- 能在内存中新增、更新、删除 file text。

### Step 2.3：实现 parse cache

改动范围：

- `BaseDb::parse(FileId)` 调 `rua-syntax`。
- `set_file_text` 只失效对应文件缓存。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p rua-analysis parse_cache
```

退出条件：

- parse 结果来自 VFS text。
- 修改一个文件不会重算无关文件 parse。

### Step 2.4：实现 AnalysisHost / Analysis skeleton

改动范围：

- 新增 `AnalysisHost::new/apply_change/analysis`。
- 新增 `Analysis` snapshot。
- 先只提供 `diagnostics(file)` stub 和 `parse(file)` 测试辅助。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p rua-analysis analysis_host
```

退出条件：

- LSP/测试能通过 `AnalysisHost` 提交文件并获取 snapshot。

### Step 2.5：支持 `.ruai` library roots

改动范围：

- 支持 `SourceRootKind::Workspace/Library/Std/Virtual`。
- 支持 `FileKind::Source/Declaration`。
- 实现只读 library root 标记。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p rua-analysis library_root
```

退出条件：

- `.ruai` 能进入 analysis input。
- library root 文件默认只读。

### Step 2.6：LSP 配置接入 AnalysisHost

改动范围：

- `rua-lsp` 读取 `rua.library` / `rua.libraryMounts`。
- 转成 `Change` 提交给 `AnalysisHost`。
- 不实现完整 goto/completion，只打通输入链路。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p rua-lsp --features lsp
cargo test -p rua-analysis
```

退出条件：

- 外部 `.ruai` 配置能变成 analysis source root。

## 7. Phase 3：HIR def / name resolution

目标：实现 IDE 专用 definition/name-resolution 原型，支持 goto/document symbol/workspace symbol 的新管线。

### Step 3.1：实现 ItemTree skeleton

改动范围：

- 从 `rua-syntax` CST lower 顶层 item 摘要。
- 只记录名称、kind、range、visibility stub。
- 不 lower 函数体。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p rua-analysis item_tree
```

退出条件：

- 顶层 function/struct/enum/mod/type alias 能进入 ItemTree。

### Step 3.2：实现 module file resolution

改动范围：

- 支持 `mod foo;` -> `foo.rua` / `foo/mod.rua` / `foo.ruai` / `foo/mod.ruai`。
- 只在 VFS/source roots 查找，不读磁盘。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p rua-analysis module_resolution
```

退出条件：

- module resolution 不做 IO。
- workspace > library > std 优先级有测试。

### Step 3.3：实现 DefMap skeleton

改动范围：

- 建 `ModuleId`、`DefId`、`DefKind`。
- 建单 workspace module tree。
- 不做复杂 visibility。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p rua-analysis def_map
```

退出条件：

- 能从 module/path 查到基础定义。

### Step 3.4：实现 find_def_at 原型

改动范围：

- `Semantics::find_def_at(FilePosition)`。
- 覆盖 simple name、path、module item。
- 不做 type/member lookup。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p rua-analysis find_def_at
```

退出条件：

- goto definition 原型不依赖旧 `rua-syntax::nameres`。

### Step 3.5：实现 document/workspace symbol 原型

改动范围：

- 从 ItemTree / DefMap 输出 document symbol。
- 从全局 index 输出 workspace symbol。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p rua-analysis symbol
```

退出条件：

- document symbol / workspace symbol 可走新 analysis。

### Step 3.6：module/name parity tests

改动范围：

- 用 Phase 0 的 `ruac` oracle corpus 对齐可编译项目。
- 对差异建立 expected failure 或 TODO，不让分歧静默存在。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p rua-analysis parity
cargo test -p ruac
```

退出条件：

- 已覆盖 module/name resolution 场景与 `ruac` 无分歧，或差异被记录。

## 8. Phase 4A：闭包与高效 iterator

目标：新增 Rust 风格闭包/lambda 与 iterator chain，同时保证生成 Lua 是 lazy + fused 的高效代码。此阶段在 Phase 3 之后进行，不阻塞拆仓。

### Step 4A.1：闭包与 iterator 语法 RFC + golden

改动范围：

- 只补文档与 golden fixture。
- 明确支持范围：`|x| expr`、`|x: T| -> U { ... }`、range、`for x in iter`、`.iter()`、`.map()`、`.filter()`、`.enumerate()`、`.take()`、`.skip()`、`.fold()`、`.collect::<Vec<_>>()`。
- 明确暂不支持范围：完整 `Fn/FnMut/FnOnce`、`move` closure、iterator escape 的所有高级组合。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p ruac golden_compile_pass
cargo test -p ruac golden_compile_fail
```

退出条件：

- 闭包与 iterator golden 先失败但已入库，或标记 ignored/todo。
- 语义边界明确。

### Step 4A.2：`ruac` parser 支持闭包与 range

改动范围：

- `ruac` parser 新增 closure expr：`|args| expr`、`|args| { block }`、typed args、optional return type。
- 新增 range expr：`a..b`、`a..=b`。
- 不做 typeck/codegen。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p ruac parser_closure
cargo test -p ruac parser_range
```

退出条件：

- `ruac` 能 parse 新语法。
- 旧 parser 测试仍绿。

### Step 4A.3：`rua-syntax` parser 同步闭包与 range

改动范围：

- `rua-syntax` rowan parser 同步 closure/range 节点。
- 补 AST wrappers 和 range accessor。
- 更新双 parser conformance。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p rua-syntax parser_conformance
cargo test -p rua-syntax range_golden
```

退出条件：

- 两套 parser 对闭包/range 的合法输入接受集一致。
- 关键 token/text range 可映射。

### Step 4A.4：闭包 typeck

改动范围：

- 支持闭包参数和返回类型推断。
- 支持只读捕获。
- 支持 iterator adapter 中的闭包上下文推断。
- 对不确定类型返回 `Unknown`，不要误报。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p ruac closure_typeck
cargo test -p rua-analysis closure_type_parity
```

退出条件：

- `|x| x + 1`、typed closure、block closure、read capture 可 typecheck。
- unsupported mutable/escaping case 有稳定 diagnostic golden。

### Step 4A.5：iterator type model 与 lazy `IterPlan`

改动范围：

- 引入 `IterPlan` 或等价内部表示。
- 支持 source：range、inclusive range、Vec、`.iter()`、`.into_iter()`。
- 支持 adapters：`map`、`filter`、`filter_map`、`enumerate`、`take`、`skip`。
- 支持 consumers：`for`、`collect::<Vec<_>>()`、`fold`、`count`、`any`、`all`、`find`。
- consumer 确定前不生成 Lua。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p ruac iterator_typeck
cargo test -p ruac iterator_plan
```

退出条件：

- iterator item type 可推断。
- adapter chain 可保留为 lazy plan。
- non-iterable/filter-non-bool/collect-mismatch 有 diagnostic golden。

### Step 4A.6：iterator fused Lua codegen

改动范围：

- range source 生成 numeric `for`。
- Vec source 生成 `for __i = 0, v.n - 1 do`。
- `map/filter/enumerate/take/skip/filter_map` fuse 到单 loop。
- `collect` 只在最终 consumer 处 materialize `rt.vec()`。
- `fold/count/any/all/find` 生成专用 accumulator/early-break loop。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p ruac iterator_codegen
cargo test -p ruac golden_compile_pass
```

退出条件：

- golden 证明简单 iterator chain 无中间 Vec。
- golden 证明不使用 coroutine。
- golden 证明不为每个 adapter 生成 per-item Lua closure。

### Step 4A.7：iterator escape fallback

改动范围：

- 对 stored/passed/returned iterator chain，选择其一：
  - 暂时报 unsupported diagnostic。
  - 或生成小型 runtime pull-iterator 协议。
- 不影响可静态融合的常见链。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p ruac iterator_escape
```

退出条件：

- iterator escape 行为明确，不静默生成低效或错误代码。

### Step 4A.8：IDE/LSP 支持闭包与 iterator

改动范围：

- completion/hover 能显示 closure param/item type。
- goto/reference/rename 能处理 closure params。
- semantic tokens 标记 closure param、adapter method、range。
- diagnostics 对 fast analysis 与 `ruac` parity。

验证命令：

```sh
cd /Users/bruce/GitProjects/rua
cargo test -p rua-analysis closure_iterator_ide
cargo test -p rua-lsp --features lsp
```

退出条件：

- IDE 对闭包/iterator 不退化为纯文本猜测。
- 不确定时降级 `Unknown`。

## 9. 每步完成记录模板

每完成一个 Step，在提交说明或施工日志里记录：

```text
Step:
Scope:
Changed files:
Verification:
  - command:
    result:
Known issues:
Next step:
```

## 10. 开工建议

第一天只做 Phase -1 的前半段：

```text
-1.1 迁移范围盘点
-1.2 创建新仓库骨架
-1.3 迁移 ruac
```

不要同时迁 syntax/LSP。先让 `ruac` 在新仓库独立跑通，这是整条线的锚点。
