# Rua Source Maps 施工方案 —— 运行期 Lua 错误 → `.rua:line`（P5e-A）

> 承接 `rua-design.md` §7/§8.1「P5e-A source maps」。目标：Rua→Lua 转译后，运行期 Lua 抛错给出的 `chunk.lua:N` 能回映到原始 `file.rua:M`。每阶段结束 `cargo test` / `cargo clippy --all-targets` 全绿方可进入下一阶段。

> **状态**：⬜ 规划中。A0 ⬜ / A1 ⬜ / A2 ⬜ / A3 ⬜ / A4 ⬜（A5 运行期自动改写为可选增量）。

## 1. 目标与非目标

### 目标
- `ruac build --sourcemap file.rua` 额外产出 **`file.lua.map`**（旁车文件），记录 `lua 行 → (源文件, 源行)`。
- `ruac trace` 离线子命令：读入一段 Lua traceback + `.lua.map`，把其中 `name.lua:N` 改写/标注为 `path.rua:M`。
- **生成的 `.lua` 逐字节不变**（映射走旁车），golden codegen 安全网原样有效。
- 多文件（`mod name;` 合并成单一 `.lua`）：map 的 `sources` 镜像编译期 `files` 注册表，条目携带源文件下标。

### 非目标（本轮不做）
- **运行期自动改写**（在 `lua_actor.rs` 里拦截 traceback 就地回映）——列为 A5 可选增量（需运行时按 chunk 名加载 map，集成面更大）。
- **列级**映射（只做行级）；一条 Rua 语句展开成多行 Lua 时，取该语句**首个发射行**为锚（粗粒度 MVP）。
- 内联 `-- @rua path:line` 注释方案（会污染"可读 Lua"且动 golden）——放弃，改用旁车。
- Lua VM 原生 sourcemap 消费（不存在，故永远是"第二步"离线/钩子改写）。

## 2. 现状与约束（依据代码勘察）

| 事实 | 位置 | 影响 |
|---|---|---|
| codegen 累积到 `String`，每行走 `line()`，空行走 `blank()` | `codegen.rs` `Codegen::{line,blank}` | 行计数需在这两处埋点 |
| `generate()` 在 `cg.out` 前置 1 行 banner（+ `uses_rt` 时再 1 行 `require`） | `codegen.rs:123` | map 的 lua 行号需加 **preamble 偏移**（编译末尾已知 `uses_rt` 后统一加） |
| `Expr.span: SourceRange` 带 `line`(1-based, 每文件独立) + `file` | `ast.rs` / `token.rs:7` | **无需 LineIndex**，直接取 `span.line`/`span.file` 作 Rua 行 |
| `Stmt`/`Item` **无 span**；`FnDecl`/`Field`/`TraitMethod` 有 `name_span` | `ast.rs` | 语句锚点由其内含 expr 的 `span` 派生；fn 头用 `name_span` |
| `resolve::set_file_expr` 只改 `span.file`，不改 `span.line` | `resolve.rs` | 子文件 expr 的 `(file, line)` 已正确（file 为子 id、line 为子文件内行） |
| 多文件合并成单一输出 `.lua` | `compile_path` | map 用单文件、多 `sources` |
| 运行期 `luaL_loadfile`（chunk=`@file.lua`），traceback 走 `luaL_traceback` | `lua_actor.rs:60`、`laux.rs:273` | A3 离线改写匹配 `\t<name>:<N>:`；A5 才动运行期 |
| moon-ruac 无 serde 依赖 | `Cargo.toml` | map 用**紧凑文本格式**，不引 JSON 依赖 |

## 3. 数据结构与 `.lua.map` 格式

`crates/moon-ruac/src/sourcemap.rs`（新）——纯数据：
```rust
/// One line-level mapping: generated Lua line -> original (source file, source line).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapEntry {
    pub lua_line: u32,   // 1-based, into the FINAL emitted .lua (preamble included)
    pub src_file: u32,   // index into `sources`
    pub src_line: u32,   // 1-based, into that source file
}

#[derive(Debug, Clone, Default)]
pub struct SourceMap {
    pub sources: Vec<String>,   // mirrors the compile-time `files` registry
    pub entries: Vec<MapEntry>, // sorted ascending by lua_line, deduped
}

impl SourceMap {
    /// Nearest mapping at or before `lua_line` (tracebacks may point mid-statement).
    pub fn lookup(&self, lua_line: u32) -> Option<MapEntry>;
    /// Serialize to the compact text format below.
    pub fn to_text(&self) -> String;
    /// Parse the text format (for `ruac trace`).
    pub fn from_text(s: &str) -> Result<SourceMap, String>;
}
```

**`.lua.map` 文本格式（v1，无依赖）**：
```
rua-sourcemap 1
S tests/fixtures/examples/rua_moon/main.rua
S tests/fixtures/examples/rua_moon/util.rua
M 4 0 3
M 5 0 4
M 9 1 2
```
- 首行版本；`S <path>` 按 `src_file` 顺序声明源；`M <lua_line> <src_file> <src_line>` 升序。空/未知源路径写 `-`。

## 4. 施工阶段

### A0 —— codegen 语句锚点来源（`moon-ruac`，无 AST 破坏性改动）
- 新增自由函数 `stmt_anchor(s: &Stmt) -> Option<(u32 file, u32 line)>`：从语句内含 expr 取 `span`（`Let.init`/`Expr`/`Return(Some)`/`While.cond`/`For.iter`/`WhileLet.expr` 等）；无 expr 的 `Break`/`Continue`/`Loop`/`Return(None)` 返回 `None`（由最近前一条锚点覆盖）。
- fn/方法头锚点用 `FnDecl.name_span`（`gen_item`/`gen_method` 发射 `local function …`/方法行时）。
- **测试**：`stmt_anchor` 对各类语句返回预期 `(file,line)`。

### A1 —— codegen 埋点采集映射（`moon-ruac`）
- `Codegen` 增 `lua_line: u32`（从 1 起）、`map: Vec<MapEntry>`、`pending: Option<(u32,u32)>`。
- `line()`：发射前 `self.lua_line += 1`；若 `pending` 有值则 `map.push(MapEntry{ lua_line, src_file, src_line })` 并清空（把映射钉在语句**首个物理行**）。`blank()` 同样自增 `lua_line`（不记录）。
- `gen_stmt` 入口 `self.pending = stmt_anchor(stmt)`；`gen_item`(Fn)/`gen_method` 入口 `self.pending = name_span → (file,line)`。
- `generate` 保持**返回 `String` 不变**；新增 `pub fn generate_with_map(prog, info) -> (String, SourceMap)`：跑完后按 preamble 行数（`1 + uses_rt as u32`）给每个 `entry.lua_line` 加偏移，填 `sources`（见 A2 传入）。`generate` 改为 `generate_with_map(...).0`。
- **测试**：给定小 `.rua`，断言若干 `(lua_line → src_line)` 映射正确（含 preamble 偏移、含一条多行展开语句锚到首行）。golden `.lua` **不变**（同一 `out` 缓冲）。

### A2 —— 编译入口 + CLI（`moon-ruac`）
- `lib.rs`：`pub fn compile_path_with_map(path) -> Result<(String, SourceMap), String>`（复用 `parse_and_resolve` 的 `files` 作 `sources`）；`compile_path` 委托取 `.0`。
- `main.rs`：`ruac build --sourcemap <in.rua> [-o out.lua]` → 写 `out.lua` + `out.lua.map`（`SourceMap::to_text`）。无 `--sourcemap` 时行为不变。
- **测试**：CLI 参数解析单测；`compile_path_with_map` 产出 `sources` = 文件注册表、entries 非空。

### A3 —— `ruac trace`（`moon-ruac`）
- `ruac trace <file.lua.map>`：从 **stdin** 读 traceback，逐行用正则/手写扫描匹配 `(<name>):(<N>)`（尤其 `\t…:N:` 帧与 `name.lua:N:` 头），对 `name` 以该 map 的 `.lua` 文件名结尾者，用 `SourceMap::lookup(N)` 求 `(src, M)`，改写/追加为 `path.rua:M`（保留原文，追加 ` (=> path.rua:M)` 更安全）。
- 非匹配行原样输出；无映射命中原样输出。
- **测试**：喂样例 traceback + map，断言含 `=> …rua:…` 标注；无关行不变。

### A4 —— 文档
- `rua-design.md` §8.1 P5e-A → ✅（MVP：旁车 map + 离线 trace；A5 运行期自动改写待办）；本文状态勾选。
- `rua_moon/main.rua` 头注释补 `ruac build --sourcemap` 与 `ruac trace` 用法示例。

### （可选增量）A5 —— 运行期自动回映
- 在 `lua_actor.rs` `lua_pcall` 失败分支（init 316–330、dispatch 354–415）拿到 traceback 字符串后，若该服务源为 ruac 产物且存在同名 `.lua.map`，加载并改写 `chunk.lua:N`→`rua:M` 再 `response_error`/日志。
- 需运行期按 chunk 名定位 map（约定 `<source>.map` 邻放）；缓存已加载 map。风险/集成面更大，独立评估。

## 5. 边界与保守策略
- **粒度**：行级 + 语句首行锚；`lookup` 取"≤N 的最近条目"，未映射行落到最近前一条（粗但可用）。
- **preamble 偏移**：`uses_rt` 编译后才定，偏移在 `generate_with_map` 末尾统一加，避免早绑。
- **多文件**：`sources` 镜像 `files`；子文件 `(file,line)` 已由 parser/resolve 正确置位。
- **零污染**：`.lua` 不变 → golden 安全网守护；map 缺失时运行流程与今日完全一致。

## 6. 测试矩阵
| 场景 | 期望 |
|---|---|
| 单文件基础：`let`/`return`/调用 | 各语句 lua 行 → 正确 rua 行（含 preamble 偏移） |
| `uses_rt`（有 `rt.*`）| 偏移 +2；无则 +1 |
| 多行展开语句（`match`/`if let`）| 锚到首个发射行 |
| fn 头 | `local function f` 行 → fn 名 rua 行 |
| 多文件（`mod`）| entry.src_file 指向子源；`sources[idx]` = 子路径 |
| `ruac trace` | traceback 帧被标注 `=> path.rua:M`；无关行不变 |
| golden | 所有 `.lua` 逐字节不变 |

## 7. 风险
- **语句锚缺失**（无 expr 语句）：落到最近前一条映射；可接受。若需精确，后续做 **I2（Stmt/Item span）** 升级锚源。
- **多对多行**：一条 Rua 行 → 多 Lua 行只钉首行；`lookup(≤N)` 兜底。
- **A5 运行期**：chunk 名与 map 定位约定、性能（缓存）、以及跨 actor 加载——单独评估，不阻塞 A0–A4。
- **trace 误匹配**：只改写以该 map 的 `.lua` basename 结尾的 `name:N`，避免误伤无关文件名。

## 8. 有序任务清单
1. ⬜ A0 `stmt_anchor`（+ 单测）。
2. ⬜ A1 codegen 埋点 + `generate_with_map`（golden 不变，+ 映射单测）。
3. ⬜ A2 `compile_path_with_map` + `ruac build --sourcemap`（+ 单测）。
4. ⬜ A3 `ruac trace`（+ 单测）。
5. ⬜ A4 文档。
6. ⬜（可选）A5 运行期自动回映（`lua_actor.rs`）。

> A0–A4 交付「旁车 map + 离线 `ruac trace`」——`.lua` 零变化、golden 零漂移。A5 再把回映搬到运行期。
