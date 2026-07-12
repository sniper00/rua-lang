# Rua vs rust-analyzer: architecture gap analysis

> 基于当前代码（`57e7e23`）与 rust-analyzer 架构的对比。每条标注影响等级和借鉴建议。

---

## 1. Completion context detection：token heuristic vs AST walk

**rust-analyzer** 构建 `CompletionContext`，包含：
- 从光标所在 AST 节点向上推导的上下文（表达式/模式/类型位置/item 位置）
- 期望类型（从类型推断获取）
- 函数/impl/module 上下文
- 是否在 trait impl 中、是否在循环内等

**Rua** 使用原始 token 推断上下文：
```
is_type_position(token)  → 只检测 `let x: |` 模式，不检测 fn 参数、struct 字段、泛型
is_expression_context(token) → 硬编码 token 集合与上一个 significant token 比对
in_arithmetic           → 硬编码 `+` `-` `*` `/` 检测
```

**隐患**：
- `is_type_position` 漏检测 `fn foo(x: |)`、`struct S { f: | }`、`Vec<|>` 等场景
- `is_expression_context` 基于前一个 token，无法处理光标在行首、空块等边界
- 上下文间隐式冲突：多个 `if` 分支各自设置不同的 relevance，没有优先级层

**影响**：中。补全质量不稳定，某些位置会缺类型补全或多出不应出现的关键词。

**借鉴**：将 context 提取为独立结构体 `CompletionContext`，在入口处一次性构建，里面包含从 AST 推导的上下文标记和从 inference 推导的期望类型。

---

## 2. Completion relevance：硬编码整数 vs 分层优先级

**rust-analyzer** 使用结构化 `CompletionRelevance`：
```
struct CompletionRelevance {
    exact_type_match: bool,    // +1.0
    type_name_match: bool,     // +0.5
    is_local: bool,            // +0.2
    is_deprecated: bool,       // -1.0
    is_item_from_this_crate: bool,
    ...
}
```
然后通过 `Score` 比较规则排序。新增补全类别时只需添加一个标记位。

**Rua** 使用硬编码整数：

| 类别 | relevance |
|------|-----------|
| keyword | 50 |
| snippet (if-let等) | 51 |
| builtin type (expr) | 40 |
| builtin type (type pos) | 90 |
| cross-module | 75 |
| module definitions | 85 |
| postfix | 85 |
| member (field/method) | 90 |
| match variant | 93 |
| struct field (literal) | 93 |
| if-let variant | 94 |
| local | 95+usage |
| self keyword | 96 |

**隐患**：
- 新增补全类别时必须手动分配数字，容易冲突
- 无法表达 "字段类型匹配期望类型时提升" 这类语义
- 不同类别之间的优先级关系不透明（为什么 postfix=85、member=90？）

**影响**：低-中。当前规模可控，但随着补全类别增多会崩溃。

**借鉴**：用结构体替换整数，定义清晰的分层规则。

---

## 3. Hover & Goto-def 代码重复

`member_hover`（~95 行）和 `member_goto_definition`（~50 行）共享完全相同的 preamble：
1. 找到光标 token
2. 检查前一个 token 是否是 `.`
3. 提取 field/method 名称
4. 调用 `infer_dot_receiver` 解析 receiver 类型

**隐患**：
- 每个 bug fix 需要同步两处
- hover 有 fallback（扫描所有 body 查找同名 local），goto-def 没有
- 如果 preamble 行为不一致，用户会看到 hover 有提示但 Ctrl+Click 不跳转

**影响**：高。已导致实际 bug（hover 有效但 goto-def 不跳转）。

**借鉴**：提取公共函数 `resolve_member_access(position) -> Option<(Ty, String, MemberResolution)>`。

---

## 4. `find_def_at` 的定义查找策略

**rust-analyzer** 从 token 向 AST 上层遍历，在每一层尝试分类（NameClass），决定是 goto-def 到 item 还是 local。

**Rua** `Semantics::find_def_at`：
1. 扫描所有 `DefMap::definitions()`，找 `name_range` 包含 offset 的
2. 如果是 member access（前一个 token 是 `.`），直接返回 None
3. 回退到 local resolution

**隐患**：
- 步骤 1 是 O(n) 扫描。同名定义（如多个 struct 都有 `new` 方法）只匹配第一个
- 步骤 2 将 member access 完全排除，导致 `goto_definition` 必须单独调用 `member_goto_definition`
- `find_def_at` 和 member 解析之间的职责分裂使得调用方需要知道何时用哪个

**影响**：中。当前通过 `goto_definition` 中的三步 fallback 绕过了，但调用方容易遗漏。

**借鉴**：统一 `find_def_at` 为单入口，内部处理所有情况（包括 member access）。

---

## 5. 缓存失效粒度

**rust-analyzer** 使用 salsa 的细粒度增量重算。修改一个函数只重算该函数的 body/inference。

**Rua** `BaseDb::invalidate_file()`：
```rust
fn invalidate_file(&mut self, file_id: FileId) {
    self.parse_cache.remove(&file_id);
    self.item_tree_cache.remove(&file_id);
    self.def_map_cache.clear();           // ⚠️ 清除 ALL def_map
    self.member_index_cache.clear();       // ⚠️ 清除 ALL member_index
}
```

**隐患**：
- 修改任何文件都会清除所有 def_map 和 member_index
- 多文件项目（如 100 个文件），每次按键 O(100) 重算
- 当前测试只有 1-2 个文件，尚未暴露性能问题

**影响**：中。当前项目规模小不致命，但架构上需要修复才能扩展。

**借鉴**：引入文件级依赖追踪（file → which def_map entries it contributes to），只失效受影响的部分。或等 salsa 成熟后迁移。

---

## 6. Completion 上下文推断：缺了关键场景

**已实现**：
- scope completion（keywords, locals, module items, builtins, cross-module pub items）
- member completion（dot 后 field/method）
- path completion（`::` 后）
- match scrutinee enum variants
- struct literal fields
- if-let pattern enum variants
- postfix templates（`.if`, `.match`）

**缺失**：
- `self.` 在方法内 → 当前 `self` keyword relevance=96，但 `self.` dot completion 不保证工作
- trait method completion → `instance_candidates` 应该包括 trait methods，但需验证
- 方法调用的参数补全（当前补全只是名字，输入参数时无帮助）
- completion 不感知 impl block 的 self type（如 `impl Point { fn | }` 应补全 trait 方法签名）

**借鉴**：rust-analyzer 的 `DotAccess` 和 `QualifierCtx` 模式。

---

## 7. Inlay hints 覆盖不全

**已实现**：`let` binding 的类型标注（`: i64`），支持点击跳转。

**缺失**：
- 闭包参数类型标注（`|x, y|` → `|x: i64, y: i64|`）
- 函数返回类型（`fn foo() -> i64` 中 `i64` 可能是推断出来的，应标注）
- 方法调用参数的 name hint（`foo(42)` → `foo(x: 42)`）

**影响**：低-中。当前最常用的场景已覆盖。

---

## 8. Signature help 质量

**当前实现**：
1. 找包含光标的 Call/MethodCall 表达式
2. 从 inference 获取 callable type
3. 用逗号分隔的参数数量推断 active parameter

**缺失**：
- 无重载函数的选择
- 无参数名称显示（只显示类型）
- active parameter 统计使用 `expr_range.end() <= offset`，但逗号后的空格会导致跳过一个参数

**影响**：低。基本功能可用但体验粗糙。

---

## 9. References 跨文件查找

**当前实现**：
- 局部引用：通过 `BodyResolution` 精确查找
- 跨文件引用：扫描所有 body，对每个 NonLocal name ref 调用 `find_def_at` 验证

**隐患**：
- O(n*m) 复杂度：n 个定义 × m 个 body
- 每次查询都重新扫描，没有索引
- `find_def_at` 的验证依赖 name_range 包含 offset，不精确

**借鉴**：构建 `ReferenceIndex`（已在 Phase 2.1 规划中）。

---

## 10. 测试基础设施

**当前**：手动计算 column 偏移量。
```rust
let pp = srv.pp(&uri, 4, 44).unwrap(); // 第 4 行第 44 列
```

**隐患**：
- 源码修改后 column 计算全部偏移
- 多次出现 off-by-one bug
- 难以阅读：不知道 offset 指向哪个符号

**借鉴**：rust-analyzer 的 fixture 标记系统：
```rust
fn foo() { let x$0 = 42; }  // $0 标记光标位置
```
这需要实现 fixture 解析器（~100 行），但长期收益巨大。

---

## 11. 缺少的 LSP 功能（质量层面）

以下功能有实现但存在质量问题：

| 功能 | 状态 | 问题 |
|------|------|------|
| Completions | ✅ | relevance 硬编码，部分上下文缺失 |
| Hover | ✅ | member 和 item 分离，fallback 不一致 |
| Goto-def | ✅ | 同上 |
| Inlay hints | ✅ | 只有 let binding |
| Signature help | ✅ | 无参数名，active param 粗糙 |
| References | ✅ | 无索引，O(n*m) 扫描 |
| Rename | ✅ | 只支持 local |
| Code actions | ✅ | 基础实现（flip comma, fill fields, gen match arms, gen impl methods） |
| Semantic tokens | ✅ | 覆盖不完全 |
| Document symbols | ✅ | 基础实现 |
| Folding range | ✅ | 基础实现 |
| Call hierarchy | ✅ | 基础实现 |
| Type hierarchy | ✅ | 基础实现 |
| Selection range | ✅ | 基础实现 |

---

## 优先级排序

按影响 × 修复成本排序：

1. **🔴 统一 hover/goto-def preamble**（高影响，低成本）
   - 提取 `resolve_member_access()` 公共函数
   - 消除 hover fallback 与 goto-def 的不一致

2. **🔴 Completion 上下文结构体**（中影响，中成本）
   - 定义 `CompletionContext` 结构
   - 将 token 检测替换为 AST 推导
   - 加入期望类型信息

3. **🔴 Completion relevance 分层**（中影响，低成本）
   - 用结构体替换整数
   - 加入 type-match 权重

4. **🟡 缓存失效粒度**（中影响，高成本）
   - 引入依赖追踪，精确失效

5. **🟡 References 索引**（中影响，中成本）
   - 构建 `ReferenceIndex`（已在计划中）

6. **🟡 测试 fixture 系统**（中影响，中成本）
   - 实现 `$0` 标记解析器
   - 迁移现有测试

7. **🔵 Inlay hints 扩展**（低影响，低成本）
   - 闭包参数类型
   - 链式方法调用中间类型

8. **🔵 Signature help 改进**（低影响，低成本）
   - 加入参数名称
   - 修复 active parameter 统计
