# Rua LSP v3-c 施工方案 —— 成员补全（`x.` → 字段/方法列表）

> 承接 `rua-design.md` §8.1「C3/C4 v3 状态 → v3-c」与 [`rua-lsp-v3b.md`](rua-lsp-v3b.md)。v3-b 已打通「成员 **访问**（go-to-def / hover）」的类型桥；v3-c 复用同一座桥做「成员 **补全**」。每阶段结束应 `cargo test` / `cargo clippy --all-targets` 全绿方可进入下一阶段。

> **状态（已完成 ✅）**：C0 ✅ / C1 ✅ / C2 ✅ / C3 ✅ / C4 ✅ —— 单文件 + 跨文件成员补全（`x.` / `x.par`）端到端可用。查询返回 `Option<Vec>`：`None` = 非成员位置（回落全局补全），`Some` = 成员位置（空表示接收者类型未知，抑制 `.` 后的关键字/全局噪音）。测试：moon-ruac 182 + moon-rua-syntax 246 + moon-rua-lsp 35 全绿；golden codegen 无漂移；clippy 零新增。

## 1. 目标与非目标

### 目标
- 光标在 **成员访问位置**（`recv.` 或 `recv.par|`）触发补全时，LSP 返回接收者类型的：
  - **字段**（`struct` 的字段，kind = Field，detail = `name: Ty`）。
  - **方法**（impl 固有 / trait impl / trait 默认，kind = Method，detail = 签名）。
- 单文件 **与** 跨文件（接收者类型定义在别文件）均支持——直接复用 v3-b/B5 的 `member_index_src` 多文件机制。
- 与既有「全局符号 + 关键字」补全**互斥切换**：成员位置只出成员，非成员位置维持 v1 行为。

### 非目标（本轮不做）
- 方法补全自动补 `()` / 参数占位（snippet）——先只插入方法名。
- `Enum::`（路径/变体补全，属项名补全，另做）、`Some(`/`Ok(` 等内建构造补全。
- 补全项文档（doc comment）——detail 已够用，doc 悬停留待后续。
- 宏参数内、字符串/注释内的成员补全。
- 深链式 **在中间类型不可 typeck 时** 的补全（`f().g().` 若 `f().g()` 无法定型则空——零假阳性）。

## 2. 核心难点

补全在**语法不完整**时触发：`recv.`（点后为空）或 `recv.par`（半个成员名）。两处困难：

1. **`moon-ruac` 解析器非容错**（返回 `Result<Program, String>`）：`recv.` 单独是 parse error，拿不到任何类型。
2. **`member_index` 按已解析的成员 use-site 索引**：`recv.` 处没有成员标识符，天然无命中。

而 `moon-rua-syntax` 的 **CST 是容错的**（错误恢复后仍产出树，能定位 `.` 与接收者 token），但 CST 无类型。

### 思路：**「补全修复 + 权威 typeck」**
在 CST 定位到「成员补全上下文」后，**合成一份修复源**：把 `recv.`（或 `recv.par`）改写成 `recv.<哨兵标识符>`，使 `moon-ruac` 能 parse + typeck。typeck 在**接收者**表达式处记录其类型名（记录发生在「查成员是否存在」**之前**，故哨兵成员不存在也不影响接收者定型）。再用类型名查「类型成员目录」列出字段 + 方法。

- **权威**：类型来自真正的 `moon-ruac` typeck（call 返回类型、链式中间类型都能定型），与项目「类型只属 moon-ruac、零假阳性」一致。
- **偏移稳定**：哨兵插在 `.` **之后**（即光标处），接收者 span 在 `.` **之前**，修复前后**字节不变**——CST 给的接收者 span 与修复后 typeck 记录的接收者 span 一致，可直接匹配。
- **成本**：补全为用户触发（客户端有去抖），单文件 typeck 微秒~低毫秒级；与 `didChange` 已跑的 `check_diags` 同量级，可接受。类型成员目录可按文档版本缓存（不随 `x.` 改动而变）。

```
CST（容错，定位 recv 与 .）        moon-ruac（权威类型）
──────────────────────────        ─────────────────────
1. 判定成员补全上下文              2. member_completion_src(修复源, path):
   拿接收者 span + 半成员前缀          - ReceiverIndex: recv_span → 类型名
3. 合成修复源 recv.<哨兵>            - TypeMembers: 类型名 → [字段|方法]
4. 用 recv_span 查 ReceiverIndex → 类型名 → 查 TypeMembers → 补全项
```

## 3. 数据结构（桥的形状）

`moon-ruac` 侧新增，保持**纯数据**（无 rowan、无 CST），与 `MemberIndex` 并列：

```rust
/// 一个可补全成员（字段或方法）。
pub struct CompletionMember {
    pub name: String,
    pub kind: MemberKind,   // 复用 v3-b 的 Field | Method
    pub detail: String,     // 字段 `x: f64`；方法 `fn dist(&self) -> f64`
}

/// 类型名 → 其全部字段与方法（含 trait impl / 默认方法）。
pub struct TypeMembers {
    map: HashMap<String, Vec<CompletionMember>>,
}
impl TypeMembers {
    pub fn get(&self, type_name: &str) -> &[CompletionMember];
}

/// 一次成员访问的**接收者**定型记录，按字节 span 索引。
pub struct ReceiverType {
    pub recv_file: u32,
    pub recv_start: usize,
    pub recv_len: usize,
    pub type_name: String,  // 具体 Named 类型；Vec/String/未知/泛型不记录
}
pub struct ReceiverIndex { hits: Vec<ReceiverType> }
impl ReceiverIndex {
    pub fn at(&self, file: u32, offset: usize) -> Option<&ReceiverType>;
}
```

顶层入口（与 `member_index_src` 平行）：

```rust
// lib.rs
pub fn member_completion(src: &str) -> (TypeMembers, ReceiverIndex);                 // 单串（单元测试）
pub fn member_completion_src(root_src: &str, root_path: &Path)
    -> (TypeMembers, ReceiverIndex, Vec<String>);                                    // LSP（多文件）
```

> 复用 v3-b/B0 已收集的表：`TypeMembers` 由 `structs`（字段 + 定义 detail）+ `method_defs` + `trait_method_defs` 直接汇编；`ReceiverIndex` 由 `infer` 在 `Field`/`MethodCall` 定型接收者时顺手记录。

## 4. 施工阶段

### C0 —— typeck 汇编「类型成员目录」（`moon-ruac`）
- 由 B0 已有的 `structs`（`Vec<(name, Ty, span)>`）、`method_defs`、`trait_method_defs` 汇编出 `TypeMembers`：每个类型名 → 字段（`CompletionMember{Field}`）+ 方法（`CompletionMember{Method}`）。方法去重（trait 默认被 impl 覆盖时取 impl）。
- 枚举：只列**方法**（`e.method()`），不列变体（变体属 `E::` 路径补全）。
- 产物：内部表 + 顶层 `member_completion`（暂只返回 `TypeMembers`，`ReceiverIndex` 见 C1）。
- **测试**：对含字段/固有方法/trait 默认方法的类型，`TypeMembers::get` 返回预期集合与 detail；Vec/String 不在目录里。

### C1 —— typeck 记录「接收者类型」（`moon-ruac`）
- `Tc` 增 `receivers: Vec<ReceiverType>`。`infer` 的 `ExprKind::Field` 与 `ExprKind::MethodCall` 分支：**在查成员存在性之前**，若接收者定型为具体 `Named(T)`，push `ReceiverType{recv_span(=recv.span), type_name:T}`。`self` 走 impl 目标类型。保守：`Vec`/`HashMap`/`String`/extern/泛型/`Unknown` 不记录。
- 顶层 `member_completion` / `member_completion_src` 返回 `(TypeMembers, ReceiverIndex[, files])`；`ReceiverIndex::at(file, offset)` 命中「offset 落在接收者 span」。
- **测试**：`p.x`、`p.get()`、`self.x`、链式 `a.b.c`（中间可定型时）在接收者处记录类型名；未知/Vec 接收者不记录。

### C2 —— CST 修复 + 补全查询（`moon-rua-syntax`）
- **上下文判定**：`token_at_offset(offset)` 找到光标左侧最近的 `.` token（跳过其后可能的半成员 Ident/错误节点），其左兄弟表达式节点即**接收者**；拿接收者 `text_range()`（字节 span）与半成员前缀（若有）。非此形态返回空。
- **修复源合成**：
  - 若 `.` 后**无** Ident（`recv.` 或 `recv.` 后接非标识符）：在 `.` 后插入哨兵 `__rua_complete`。
  - 若 `.` 后**已有**合法 Ident（`recv.par`）：无需插入（`recv.par` 本就可解析），直接用原源。
  - 哨兵/半成员均在接收者之后，**接收者 span 不位移**。
- **查询**：`Analysis::member_completions(offset) -> Vec<CompletionMember>`（单文件）与 `Workspace::member_completions(file, offset)`（跨文件）：
  1. C2 上下文判定 → 接收者 span（无则空）。
  2. 合成修复源 → `moon_ruac::member_completion[_src]`。
  3. `ReceiverIndex::at(0, recv_span.start)` → 类型名（无则空）。
  4. `TypeMembers::get(类型名)` → 成员列表（客户端按半成员前缀自行过滤；服务端可不过滤或做前缀过滤）。
- **缓存**：`TypeMembers`（不随 `x.` 变）可随 `Analysis` 版本缓存；`ReceiverIndex` 依赖修复源，每次查询重算（成本可控）。
- **测试**：struct 接收者出「字段 + 方法」；`self.` 出成员；Vec/未知接收者空；`recv.par` 前缀场景；跨文件接收者（temp dir + `DiskLoader`）。

### C3 —— LSP 打通（`moon-rua-lsp`）
- `handle_completion`：先判成员补全上下文（`Workspace::member_completions` 非空）：
  - **是**：只返回成员项（`CompletionItem` kind = `FIELD`/`METHOD`，`detail` = 成员 detail，`insert_text` = 成员名）。
  - **否**：维持 v1「全局符号 + 关键字」补全。
- `trigger_characters` 已含 `.`（无需改 capability）。
- **测试**：LSP 层 `textDocument/completion` 在 `p.` 返回字段+方法、在顶层返回符号+关键字、在 Vec 接收者返回符号（非成员）。

### C4 —— 文档
- `rua-design.md` v3-c 段落状态 → ✅，登记单/跨文件成员补全、限制（无 `()` snippet、无 `Enum::`、宏内不补）。
- 本文状态横幅与任务清单勾选。

## 5. 边界与保守策略（沿用「零假阳性」）
- 只有 typeck 能把接收者**确定**为具体 `Named` 时才补全；否则空。
- Vec/HashMap/String/extern/泛型/`Unknown` 接收者：空（无 Rua 成员目录）。
- 跨模块同名类型被降级为 `Unknown`（P4c-6）时：空。
- 修复失败（CST 无法定位接收者、或修复源仍 parse error）：空——宁缺毋误。

## 6. 测试矩阵
| 场景 | 期望 |
|---|---|
| `p.`（p: struct） | 字段 + 方法全列 |
| `p.g`（前缀） | 含 `get` 等匹配项（客户端过滤） |
| `self.`（impl 内） | 本类型字段 + 方法 |
| `e.`（enum 有方法） | 只方法 |
| 跨文件 `p.`（类型在别文件） | 命中（复用 member_completion_src） |
| 链式 `a.b.`（b 可定型） | b 类型的成员 |
| `v.`（Vec） | 空（回落全局补全或不出成员） |
| 未知/泛型接收者 | 空 |
| 宏参数内 `x.` | 空 |

## 7. 风险
- **修复脆弱**：哨兵插入需保证 `recv.<哨兵>` 可 parse；若接收者本身跨越复杂结构，CST 定位或修复可能失败 → 静默空（不误报）。conformance/单测守护。
- **偏移一致**：哨兵/半成员必须在接收者之后（已论证 span 稳定）；加断言测试防回归。
- **性能**：每次成员补全跑一次（多文件）typeck；`TypeMembers` 缓存 + 客户端去抖缓解；大文件如成瓶颈再引入「last-good 目录 + 增量」优化。
- **双 parser span 漂移**：接收者 span 平价由 v3-b 的 conformance 网守护。

## 8. 有序任务清单
1. ✅ C0 typeck 汇编 `TypeMembers` + `member_completion`（+ 单测）。
2. ✅ C1 typeck 记录 `ReceiverIndex`（`at_end` 末尾锚点）+ 顶层 `member_completion`/`member_completion_src`（+ 单测）。
3. ✅ C2 `completion.rs`（`completion_context`/`repair`/哨兵）+ `Analysis`/`Workspace::member_completions`（单/跨文件，返回 `Option<Vec>`；+ 单测）。
4. ✅ C3 LSP `handle_completion` 成员/全局互斥（`Some` = 仅成员即使为空、`None` = 全局）+ `member_to_item`（Field→FIELD、Method→METHOD）（+ handler 镜像测试）。
5. ✅ C4 文档（`rua-design.md` v3-c → ✅ + 本文状态）。

> C0–C4 已交付「单/跨文件成员补全」。**Option 语义**：`member_completions` 返回 `None`（非成员位置→全局补全）或 `Some(list)`（成员位置；`list` 空 = 接收者类型未知，`.` 后不再弹关键字）。
> 后续可选增强：方法 `()` snippet、`Enum::` 变体补全、补全项 doc、last-good 目录性能优化。
