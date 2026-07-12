# 代码审查复验报告：Rua 全代码库

> **原审查日期**: 2026-07-12
> **复验日期**: 2026-07-12
> **审查范围**: 全部 8 个核心源文件（~13,500 行）
> **复验方法**: 逐项验证原始 91 项 findings 的当前状态

---

## 复验结论摘要

| 状态 | 数量 | 说明 |
|------|------|------|
| ✅ 已修复 | 1 | 跨模块重复函数已去重 |
| ⚠️ 部分修复 | 1 | 未使用变量 lint 已改用决议，未使用函数 lint 仍用字符串匹配 |
| 🔴 仍然存在 | 89 | 所有其他 findings 均未修复 |

---

## 一、已修复

### ✅ 问题 6 · 4 对跨模块完全重复的函数 — **已修复**

`previous_significant`、`next_significant`、`is_path_identifier`、`module_at_position` 四对函数已从 `ide/mod.rs` 中移除，仅保留在 `completion.rs` 中。`ide/mod.rs` 中不再有任何重复。

---

## 二、部分修复

### ⚠️ 问题 10 · 未使用变量/函数 lint — **部分修复**

**已修复部分**：未使用变量 lint（`diagnostic/mod.rs:499-535`）现在使用 `resolution.resolve(name_ref_id)` 进行正确的决议查找，而非字符串匹配。

**未修复部分**：未使用函数 lint（`diagnostic/mod.rs:649-688`）仍使用字符串名匹配 `nr.name() == Some(name)`，存在：
- O(n²) 复杂度（每个 def 扫描所有 def 的 body）
- 局部变量 `foo` 会错误抑制对未使用函数 `foo` 的警告（false negative）

---

## 三、所有未修复的严重问题（11 项）

### 🔴 问题 1 · `lsp.rs:3151` — `ensure_file_id_for_path` fallback URI 残余 bug

```rust
let id = FileId::new(self.next_file_id);
self.next_file_id += 1;                    // ← 已递增，指向 id+1
let uri = path_to_uri(path).unwrap_or_else(|| {
    format!("file:///unknown/{}", self.next_file_id)  // ← BUG: 用了递增后的值
```
**状态**: 未修复。行号从 L3151 变为 L3151（无变化）。

### 🔴 问题 1（续）· 30× handler boilerplate — **未修复**

`lsp.rs` 仍为 4,413 行（原 4,319），30 个 handler 仍各自复制 extract/error/send 模板。无 macro 或泛型分发。

### 🔴 问题 2 · 6× notification 解析重复 — **未修复**

`handle_notification`（L2863-2937）6 个 arm 仍各自复制 `serde_json::from_value` + error handling。

### 🔴 问题 3 · 14 个硬编码 relevance magic number — **未修复**

所有 relevance 分数仍为原始整数 `.with_relevance(93)` `.with_relevance(50)` 等。无 `CompletionRelevance` 结构体。

### 🔴 问题 4 · `scope_completions()` 310 行单函数 — **未修复**

`scope_completions()` 仍为单一函数（L191-L505，约 315 行），包含 8 个补全类别全程内联。

### 🔴 问题 5 · Token-based context detection — **未修复**

`is_type_position()`（L1569-1585）和 `is_expression_context()`（L1594+）仍依赖 token 检测，未使用 AST 遍历。

### 🔴 问题 7 · `infer_expr` 162 行单函数 — **未修复**

`infer_expr`（L272-L434）仍将 If/Assign/Range/Try/Index/Path arms 内联在主函数中。未提取为独立方法。

### 🔴 问题 8 · `infer_iterator_adapter` 131 行技术债务 — **未修复**

`infer_iterator_adapter`（L1168-1299）仍存在，含 11 个 iterator 方法的 ad-hoc 实现。

### 🔴 问题 9 · `infer_callable_call` 8 参数 + clippy suppression — **未修复**

L1396 仍有 `#[allow(clippy::too_many_arguments)]`。未引入 `CallContext` 结构体。

### 🔴 问题 11 · Unreachable code lint 基于文本 — **未修复**

`diagnostic/mod.rs:690-733` 仍通过正则匹配源码 `return;`/`break;`/`continue;` 进行检测。基于文本，非 HIR/CFG。

### 🔴 问题 12 · `module_for_file` O(N×M) — **未修复**

`def_map.rs:507-517` 仍对每个 module 扫描所有 definitions。未建 `FileId → ModuleId` 索引。

---

## 四、所有未修复的中等问题（48 项）

### lsp.rs（8 项）- 均未修复

| # | 描述 | 当前状态 |
|---|------|---------|
| 13 | `handle_inlay_hint` 115 行 6 层嵌套 | L735-849，未提取 `make_type_hint_label()` |
| 14 | `register_watchers` 两个 for 循环相同模式 | L3042-3071，未提取 |
| 15 | `handle_did_save` 硬编码列宽 `80.min(...)` | L2996，仍为硬编码 |
| 16 | `close_document` 不必要 clone `.map(\|(u, f)\| (u.clone(), *f))` | L2960，未修复 |
| 17 | `handle_completion` 双重查找 `file_id_for_uri` | 未修复 |
| 18 | Handler 模式不统一（5 种不同风格） | 未修复 |
| 19 | `ensure_file_id` vs `ensure_file_id_for_path` 双重入口 | 两个函数仍存在，未统一 |
| 20 | `handle_watched_file_change` 递增 `next_root_id` | L3111，仍无限增长 |

### completion.rs（9 项）- 均未修复

| # | 描述 | 当前状态 |
|---|------|---------|
| 21 | `replacement_range` 设置逻辑重复两次 | 未修复 |
| 22 | `BodyData`/`BodyFullData` 几乎相同（差一个 Arc 字段） | L1165-1198，未合并 |
| 23 | `type_compatibility_score` 未使用参数 `_expected: &Ty` | L945，未修复 |
| 24 | `_token` 未使用参数 | L509，未移除 |
| 25 | Method parameter name resolution 40 行深层嵌套 | L552-589，未修复 |
| 26 | `pattern_scrutinee_enum` while-let 硬编码窗口 `saturating_sub(100)` | L1462，未修复 |
| 27 | `is_subsequence` 只做 ASCII lowercase | L88，未修复 |
| 28 | `postfix_templates` 每次分配 5 个 String | 未改为 Cow |
| 29 | `find_containing_body_data` 解构模式在 5 处重复 | 未修复 |

### ide/mod.rs（6 项）- 均未修复

| # | 描述 | 当前状态 |
|---|------|---------|
| 30 | `member_goto_definition` `field.or(method)` 不短路 | L444，未改为 `or_else` |
| 31 | `_bid` 变量名误导（前缀暗示 unused 但实际使用） | L508，未重命名 |
| 32 | 双重否定守卫 `.is_none_or(\|t\| t.kind() != Dot)` | L478，未改为显式 match |
| 33 | `resolve_call_target` 签名跨 12 行 | L171-182，未修复 |
| 34 | `references()` 中重复构造 `Semantics` | L583+606，未修复 |
| 35 | Hover 局部绑定 4 层嵌套 | L334-355，未提取 helper |

### infer.rs（10 项）- 均未修复

| # | 描述 | 当前状态 |
|---|------|---------|
| 36 | `Condition::Let` 处理在 `visit_expr` 和 `visit_statement` 中重复 | L477-486 + L559-568，重复仍存在 |
| 37 | "diverges" 模式重复 15+ 次 | 无 `diverge_or` helper |
| 38 | `infer_builtin_call` Ok/Err arms 几乎相同 | L1560-1586，未提取 |
| 39 | `invalidate_generics`/`refine_generic_bindings` 相同递归结构 | 未提取 visitor |
| 40 | `infer_closure` 手动 stack save/restore（panic 不安全） | L917-921，未用 guard struct |
| 41 | `proclaimed_return` 注释与代码矛盾 | L932-936，注释说 prefer inferred 但代码 prefer expected |
| 42 | `resolve_variant_payload`/`resolve_variant_def` 共享 6 行 prefix | 未提取 |
| 43 | `infer_binary` 5 个否定条件连写 | 未提取 `valid_ordering_operands()` |
| 44 | `closure_origin`/`closure_target` 重复 paren unwrap | 未提取 `unwrap_parens()` |
| 45 | `InferenceContext` 14 字段 | L159-174，未分组为 sub-struct |

### diagnostic/mod.rs（6 项）- 均未修复

| # | 描述 | 当前状态 |
|---|------|---------|
| 48 | 未使用的 enum 变体：`DiagnosticSource::Name`、`DiagnosticSource::Structural`、`DiagnosticSeverity::Information`、`DiagnosticSeverity::Hint` | 4 个变体仍定义但从未使用 |
| 49 | `parse_error_code` substring 匹配顺序依赖 | L710-722，未修复 |
| 50 | `Diagnostic::new()` 所有调用重复 `DiagnosticOrigin::FastAnalysis` 6 次 | 未提取 `fast_diag()` |
| 51 | unused/lint 两个 lint 分别遍历 `body.bindings()` | L392+427，未合并为单次遍历 |
| 52 | `dedup_by` 只比较 range/code/source | L407-411，可能丢失更具体的错误信息 |
| 53 | `_file_id` 未使用参数 | L687，未移除 |

### item_tree.rs（8 项）- 均未修复

| # | 描述 | 当前状态 |
|---|------|---------|
| 54 | `impl_header` 66 行多功能 | L1173-1238，未拆分 |
| 55 | Angle-depth 追踪模式重复 5+ 次 | 未提取 `scan_balanced()` |
| 56 | 模块 fingerprint 计算两次 | L574+665，未优化 |
| 57 | `.into_iter().collect()` 在 6 个 arm 中重复 | L678-696，未提取 |
| 58 | `where_predicates` token 扫描脆弱 | L1047-1078，未修复 |
| 59 | `SignatureSyntax::from_tokens` 每个 token 触发 format! 分配 | L105，未修复 |
| 60 | `word_like` 函数 24 个 match arm | L1252-1285，未修复 |
| 61 | `TypeRef::from_type`/`unit_if_missing` 几乎相同 | L168-176，未合并 |

### def_map.rs（12 项）- 均未修复

| # | 描述 | 当前状态 |
|---|------|---------|
| 62 | `add_definition` 66 行 5+ 职责 | L751-816，未拆分 |
| 63 | `lower_items` 56 行 3 条控制流路径 | L633-688，未修复 |
| 64 | `resolve_path`/`_unique`/`_lexical`/`_lexical_unique` 4 个方法近似重复 | L547-601，未合并 |
| 65 | `resolve_member` O(n) 线性扫描 | L503，未加索引 |
| 66 | 重复 import 收集模式 | L623-630+672-678，未提取 |
| 67 | `base_path.clone()` 双份 String 分配 | L768-770，未修复 |
| 68 | `build_inner` inline 构造复杂 DefMap struct 47 行 | L404-450，未提取 |
| 69 | `add_module` panic 而非返回 Option | L743，未修复 |
| 70 | `IdentityInterner` 无 u32 耗尽时的友好报错 | L102-103+143，未修复 |
| 71 | `occurrences` HashMap 用 String key 导致分配 | L611-612，未用 CompactString |
| 72 | `lower_file` 重置 scope_path 无文档 | L617→682，未添加注释 |

### scope.rs（7 项）- 均未修复

| # | 描述 | 当前状态 |
|---|------|---------|
| 73 | `poison_bindings` 参数名和 or-pattern 语义无文档 | L334+600，未添加注释 |
| 74 | `set_expr_scope`/`set_pat_scope`/`set_name_ref_scope` 结构完全相同 | L673-693，未提取 macro |
| 75 | `LocalResolver::resolve` 61 行混合 4 件事 | L715-776，未提取 `record_capture` |
| 76 | `crossed_closures` 命名非标准 | L806-828，未重命名 |
| 77 | `visit_expr` 有死 match arm `Expr::Block(_) => {}` | L514，Block 在 L397 已 short-circuit |
| 78 | `add_bindings` 收集 Vec 然后遍历两次 | L362-391，未一次完成 |
| 79 | `infer_struct_literal` 两 pass 通过 `resolved_fields` 沟通 | L963-1028，未添加注释 |

---

## 五、所有未修复的低优先级问题（28 项）

所有 28 项低优先级问题（#80-#107）经抽查均未修复，包括：
- `project_position` 每次调用都重新 parse source + 建 LineIndex
- `handle_did_save` ruac check 错误信息解析无结构化
- `handle_watched_file_change` 的 `open_uris` 收集每次都 clone URI
- `send_diagnostics` vs `publish_diagnostics` 方法重叠
- `in_arithmetic` 只检查紧邻前一个 significant token
- `scope_completions()` 中 `seen: HashSet` 在两个位置分别创建
- Subsequence 过滤对 Unicode 无支持
- `completions()` 入口处三路分发应用 enum 而非 bool flags
- `type_compatibility_score` 通过字符串匹配 type rendering
- `item_hover_text` 和 `resolve_callee_param_names` 都从 Callable 签名提取参数
- 局部绑定解析在 `hover` 和 `prepare_rename` 中重复
- `call_hierarchy_incoming` 用 `body.expr(*callee).unwrap_or(&Missing)`
- `type_hierarchy_subtypes`/`supertypes` 几乎相同的控制流
- `InferenceContext::new` 14 个字段初始化
- `infer_match` 的 `arm_facts`/`else_fact` 命名
- `infer_closure` 的 `closure_returns` push/pop 逻辑
- `expect_bool` 和 `report_mismatch` 应合并为条件报告
- `n == "self"` 比较跳过用户声明的 `let self = ...`（L510）
- Unreachable lint 只检测单行语句
- `fast_diagnostics` 中 `inference_diag` 重复 pattern matching 类型错误
- `where_predicates` 中 `angle_depth` 用 `i32`
- `ParameterData` 的 `type_ref` 是 Option 但 always set
- `ItemTreeItem::new` 签名 fingerprint 逻辑与 `refresh_signature_fingerprint` 重叠
- `DefMapBuilder::lower_file` 构造 IdentityContext 重复 3 次
- `Definition` struct 15 字段无 builder pattern
- `ModuleData` 的 `item_tree` 字段在 build 后即清空但仍存储
- `ScopeData::from_kind` 总是创建空 vectors
- `LocalCandidate` 是私有 struct 但直接暴露 pub fields

---

## 六、重构方案状态

| 重构 | 描述 | 状态 |
|------|------|------|
| R1 | LSP Handler Macro | ❌ 未实施 |
| R2 | CompletionRelevance 结构体 | ❌ 未实施 |
| R3 | 跨模块函数去重 | ✅ 已完成（唯一完成的重构） |
| R4 | CompletionContext 结构体 | ❌ 未实施 |
| R5 | `scope_completions` 拆分 | ❌ 未实施 |
| R6 | `infer_expr` 拆分 | ❌ 未实施 |
| R7 | "diverges" helper | ❌ 未实施 |
| R8 | `resolve_path` 4 方法合并 | ❌ 未实施 |
| R9 | LSP Server 模块拆分 | ❌ 未实施 |
| R10 | 缓存失效粒度 | ❌ 未实施 |
| R11 | Fixture 系统 | ❌ 未实施 |
| R12 | Notification 解析 macro | ❌ 未实施 |

---

## 七、新增发现

### N1 · 新增 W0304 infinite loop lint — 基于源文本

`diagnostic/mod.rs:275-333` 新增了 W0304 lint 用于检测可疑无限循环。该 lint 同样基于源文本启发式匹配（正则、token 序列），存在与 W0302 unreachable code lint（问题 11）相同的问题：基于文本而非 HIR/CFG。

### N2 · diagnostic/mod.rs 增至 913 行（原 747 行）

增加了 166 行，主要为 W0304 lint 逻辑（~60 行）和其他诊断改进。

---

## 八、建议的优先修复顺序

鉴于原始 91 项 findings 中仅 1 项完全修复、1 项部分修复，建议优先处理以下高收益低风险项目：

1. **问题 1 残余 bug**（L3151 的 `self.next_file_id`）— 改动 1 行，修复实际 bug
2. **R1 Handler Macro** — 消除 ~400 行重复，大幅降低新增 handler 出错率
3. **R2 CompletionRelevance** — 消除 14 个 magic number，建立可扩展体系
4. **R12 Notification Macro** — 消除 60 行重复
5. **问题 16 close_document clone** — 改动 1 行
6. **问题 31 _bid 命名** — 改动 1 行
7. **问题 48 未使用 enum 变体** — 删除 4 个变体
