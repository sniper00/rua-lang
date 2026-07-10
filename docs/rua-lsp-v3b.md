# Rua LSP v3-b 施工方案 —— 成员访问解析（`x.field` / `x.method()`）

> 承接 `rua-design.md` §8.1「C3/C4 v3 状态 → v3-b」。本文是可执行的施工计划；每个阶段结束应 `cargo test`/`cargo clippy --all-targets` 全绿方可进入下一阶段。

> **状态（已完成 ✅）**：B0 ✅ / B1 ✅ / B2 ✅ / B3 ✅ / B4 ✅ / **B5 ✅（跨文件成员，v3-b-2）** —— 单文件与跨文件的 `p.x`/`p.get()`/`self.x` go-to-def 与 hover 均已端到端可用。

## 1. 目标与非目标

### 目标
- 光标落在 **成员访问** 上时，LSP 能：
  - **go-to-definition**：`p.x`（字段）跳到 `struct` 里字段 `x` 的定义；`p.dist()`（方法）跳到 `impl`/`trait` 里 `dist` 的定义。
  - **hover**：展示成员的类型/签名（字段 `x: f64`、方法 `fn dist(&self) -> f64`）。
- 解锁 v3-c（成员补全 `x.` → 字段/方法列表）的类型底座（同一座桥）。

### 非目标（本轮不做）
- 成员 **references / rename**（跨 impl 的字段/方法改名，风险高，另立 v3-d）。
- 链式/表达式接收者的深推断（`f().g().h`）——先只保证 **接收者为局部量/参数/`self`/字段** 且其类型为**当前文件内定义**的 `struct`/`enum` 时可解析；其余返回 `None`（零假阳性，与 v2 一致的保守原则）。
- 跨文件成员类型（接收者类型定义在另一文件）——列为 v3-b-2 增量。

## 2. 核心难点与总体思路

成员解析需要**类型信息**：先知道 `x` 是什么类型，才能查它的字段/方法表。类型推断的唯一权威是 `moon-ruac` 的 `typeck`（拥有式 AST，rowan-free）。而 LSP 建在 `moon-rua-syntax` 的 CST 上。二者靠**字节 span 平价**（同一套 `RuaTokenize`/偏移，P7-4 已验证逐字节一致）连接。

**思路：沿用 `TypeInfo` 既有范式** —— `typeck` 已经用 `HashSet<(start,len)>` 把「codegen 需要的语义事实」按字节 span 外发（`int_divs`/`str_methods`/…）。v3-b 只是**新增一类按 span 索引的语义事实**：成员访问 → 解析目标。

```
moon-ruac (typeck, owns types)         moon-rua-syntax (CST, owns LSP)
─────────────────────────────         ──────────────────────────────
推断接收者类型 → 查字段/方法表          Analysis 调 moon_ruac 取 MemberIndex
  为每个成员访问表达式记录：             按字节 span 缓存
    (member_span) → MemberTarget        nameres 在成员访问点查 MemberIndex
                                          → 命中则返回 Resolution
```

依赖方向不变（`moon-rua-syntax → moon-ruac`），rowan 仍隔离在 syntax crate，`moon-ruac` 不引入 rowan。

## 3. 数据结构（桥的形状）

在 `moon-ruac` 侧新增（`typeck.rs` 或新 `semantic.rs`），保持**纯数据**（无 rowan、无 CST）：

```rust
/// 一次成员访问（字段或方法）的解析结果，按字节 span 索引。
pub struct MemberTarget {
    /// 成员标识符本身的 span（`p.x` 里的 `x`；`p.dist()` 里的 `dist`）。
    pub member_start: usize,
    pub member_len: usize,
    /// 目标定义所在文件 id（编译期文件注册表下标，0=根）。
    pub target_file: u32,
    /// 目标定义标识符的 span（字段名/方法名的定义处）。
    pub target_start: usize,
    pub target_len: usize,
    /// 悬停详情（如 `x: f64` 或 `fn dist(&self) -> f64`）。
    pub detail: String,
    pub kind: MemberKind, // Field | Method
}

/// typeck 产出的成员解析表。
pub struct MemberIndex {
    hits: Vec<MemberTarget>, // 或 HashMap<(usize,usize), MemberTarget>
}
```

`moon-ruac` 顶层暴露一个入口（与 `check_diags` 平行）：

```rust
// lib.rs
pub fn member_index(src: &str) -> MemberIndex;          // 单串（LSP 单文件）
// 多文件增量再加 member_index_path(path) -> (MemberIndex, files)
```

> 复用现有 `TypeInfo` 的定义处 span：字段定义、方法定义此前用于 codegen 的登记；v3-b 需要它们的**标识符 span**。`typeck` 建字段/方法表时把定义 span 一并存入（下方 B1）。

## 4. 施工阶段

### B0 —— typeck 记录定义处 span（`moon-ruac`）
- 字段表：`struct` 收集时，为每个字段登记 `(field_name → 定义标识符 span)`。
- 方法表（`FnSig`）：impl 固有 / trait impl / trait 默认方法建签名时，登记方法名定义 span 与签名文本（供 detail）。
- 产物：内部表，暂不外发。**测试**：单元测试断言字段/方法定义 span 命中预期字节。

### B1 —— typeck 产出 `MemberIndex`（`moon-ruac`）
- 在推断**字段访问** `recv.field` 处（typeck 已推断 `recv` 类型并查字段）：命中具体 `Named` 结构体且字段存在时，push 一条 `MemberTarget{kind:Field}`（member span = 字段标识符 span，target = B0 的字段定义 span，detail = `field: Ty`）。
- 在**方法调用** `recv.method()` / `self.method()` 处（typeck 已解析方法签名）：命中具体类型的方法时 push `MemberTarget{kind:Method}`（detail = 方法签名）。
- **保守**：`Vec`/`HashMap`/`String`/extern/`Unknown`/泛型接收者一律不 push（零假阳性）。
- `lib.rs::member_index(src)` 跑 `parse → resolve(单文件视角) → typeck` 并返回。
- **测试**：`member_index` 对结构体字段访问、方法调用、`self.field`、`self.method()` 命中；对 Vec/未知/泛型接收者不命中。

### B2 —— `Analysis` 缓存 `MemberIndex`（`moon-rua-syntax`）
- `Analysis::new(src)` 额外调用 `moon_ruac::member_index(src)`，把结果与 CST 一起缓存（新增字段 `members: MemberIndex`）。
- 暴露查询：`Analysis::member_at(offset) -> Option<&MemberTarget>`（按 offset 落在 `member_start..member_start+member_len` 命中）。
- **平价校验**：CST 里成员标识符 token 的 `text_range()` 应与 `MemberTarget.member_*` 逐字节一致（单文件）。加一个断言测试（用语料里的结构体样例）。

### B3 —— `nameres` 接入成员解析（`moon-rua-syntax`）
- 现状：`resolve_at`/`definition_at` 遇成员访问（`is_member_access`）直接返回 `None`。
- 改为：成员访问点先查 `Analysis::member_at(offset)`：命中则构造 `Resolution{ kind:Item, target_range:(target_start,target_start+target_len), detail }` 返回；未命中仍 `None`。
- 注意作用域：成员解析结果**不参与局部/项名解析**，只在成员访问分支消费，避免污染 v2 逻辑。
- **测试**：hover/def 在 `p.x`、`p.dist()`、`self.x` 命中；在链式/未知接收者返回 `None`。

### B4 —— LSP 打通 + 文档
- `handle_hover`/`handle_definition` 已走 `workspace.hover/goto_definition` → `Analysis::definition_at`；B3 命中后自动生效，无需改 handler（单文件路径）。
- 确认单文件 def/hover 端到端：构造 CST offset → `member_at` → `Resolution` → `Location`/`Hover`。
- 更新 `rua-design.md` v3-b 状态为 ✅，登记「单文件成员解析已完成 / 跨文件成员为 v3-b-2」。

### B5 —— 跨文件成员类型（v3-b-2）✅
- **前置修复（B5-0）**：`resolve::set_file_items` 此前**只给函数/方法体内的表达式 span 打文件 id**，不触碰 B0 新增的 `Field.name_span`/`FnDecl.name_span`/`TraitMethod.name_span`，且 `Item::Struct` 根本未被匹配（落入 `_ => {}`），导致子文件（`mod name;`）里的定义 span 保留 `file = 0`。已扩展：`Item::Struct` 递归字段 `name_span`，`Item::Fn`（自由函数）/`Impl` 方法/`Trait` 方法均 stamp `name_span`。
- **B5-1**：`MemberTarget` 增 `member_file: u32`（use-site 文件 id，来自 B1 已打好的 `method_span.file`/`name_span.file`）；`MemberIndex::at` 改为 `at(file, offset)`，单文件调用方传 `0`。
- **B5-2**：`lib.rs::member_index_src(root_src, root_path)`（LSP 入口，根文件用内存缓冲、`mod` 子文件从磁盘）+ `member_index_path(path)` 委托之。解析容错：任何 parse/resolve/type 错误只返回可得的部分索引，绝不 panic/硬失败。
- **B5-3**：`Workspace::member_at(file, offset)`（单文件命中优先，未命中回落多文件 `member_index_src`）+ `Workspace::goto_definition` 新增第 3 步 `cross_file_member`，把 `target_file` 经文件注册表译回路径。`prepare_rename` 守卫改用结构化 `Analysis::is_member_access`（无需类型即可判定，跨文件成员同样拦截，避免误开放成员改名）。
- **B5-4**：`Workspace` 级真实磁盘文件（temp dir + `DiskLoader`）端到端测试：跨文件字段/方法 go-to-def + hover + 未知接收者 `None`。
- **已知限制**：`member_index_src` 的 `mod` 子文件从**磁盘**读取，子文件的未保存缓冲改动不反映（活动文件即根文件，其未保存缓冲已正确使用）。跨文件成员 **references/rename** 仍不支持（v3-d）。**hover 保真**：`method_detail` 仍把 `self`/`&self`/`&mut self` 统一渲染为 `&self`（`has_self: bool` 丢失可变性），独立 polish 修正。

## 5. 边界与保守策略（沿用 v2「零假阳性」）
- 只有 typeck 能**确定**接收者为具体 `Named` 且成员存在时才解析；否则 `None`。
- 接收者为 `Vec`/`HashMap`/`String`/extern/泛型/`Unknown`：不解析（这些本就走 `rt.*` 或宿主，无 Rua 定义处）。
- 同名跨模块类型被 typeck 降级为 `Unknown`（P4c-6）时：不解析，天然不误报。

## 6. 测试矩阵
| 场景 | 期望 |
|---|---|
| `p.x`（x 为 struct 字段） | def → 字段定义；hover → `x: Ty` |
| `p.dist()`（固有方法） | def → 方法定义；hover → 签名 |
| `self.x` / `self.m()` | 命中 |
| trait 默认方法 / trait impl 方法 | 命中方法定义 |
| `v.push(..)`（Vec） | `None` |
| `s.len()`（String） | `None`（走 rt.str，无 Rua 定义） |
| 泛型接收者 `t.m()` | `None`（除非约束可解析——保持与 typeck 一致） |
| 链式 `a.b.c` | 至少末段命中（若中间类型可推断），否则 `None` |
| 跨模块同名类型字段 | `None`（降级） |

## 7. 风险
- **双 parser span 漂移**：B2 平价断言 + conformance 网守护；若漂移，成员访问会静默 `None`（不误报，仅少功能）。
- **typeck 未持久化中间类型**：需在推断点顺手记录，注意不改变现有诊断/ codegen 行为（golden codegen 安全网守护）。
- **多文件**：单文件视角的 `member_index` 对跨文件类型返回 `Unknown` → `None`，功能缺失但不误报；B5 再补。

## 8. 有序任务清单
1. ✅ B0 typeck 登记字段/方法**定义 span**（+ 单测）。
2. ✅ B1 typeck 产出 `MemberIndex` + `lib.rs::member_index`（+ 单测）。
3. ✅ B2 `Analysis` 缓存 + `member_at` + 平价断言。
4. ✅ B3 `Analysis::resolve_at`/`definition_at` 成员分支接入 + `prepare_rename` 拒绝成员（+ hover/def 单测）。
5. ✅ B4 `Workspace` 级端到端验证（goto/hover on `p.x`/`p.get()`）+ 文档更新。
6. ✅ B5 跨文件成员（v3-b-2）：`set_file_items` 打 `name_span` 文件 id → `MemberTarget.member_file` → `member_index_src` → `Workspace::member_at`/`cross_file_member` → 真实磁盘端到端测试。

> B0–B5 已交付「单文件 + 跨文件成员 go-to-def/hover」。v3-c（成员补全）随后仅需在补全 handler 里，对 `x.` 触发时用类型表列出字段与方法，复用同一座桥（`x.` 场景需处理不完整 parse 的健壮性，见 rua-design.md）。
