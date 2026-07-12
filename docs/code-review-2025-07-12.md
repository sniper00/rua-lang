# 完整代码审查报告：Rua 全代码库

> **日期**: 2025-07-12
> **审查范围**: 全部核心源文件（10 个文件，~15,000 行）
> **参照标准**: rust-analyzer 架构与代码质量
> **审查维度**: 不清晰 (unclear) · 啰嗦 (verbose) · 问题 (bugs/problems)

---

## 一、本地变更

### `CLAUDE.md` — ✅ 好改动

引入 conventional commits (`feat:`/`fix:`/`docs:`)，删除无意义的版本号示例。与 rust-analyzer 风格一致。

### `lsp.rs:3157` — 🟡 正确的 bug 修复，但有残余 bug

新增 `self.file_to_uri.insert(id, uri)` 修复了 `ensure_file_id_for_path` 不维护 `file_to_uri` 反向映射的问题（与 `ensure_file_id` L125 行为对齐）。

**残余 bug**：L3151 的 fallback URI 使用了递增后的 `self.next_file_id`（已是 id+1）：

```rust
let id = FileId::new(self.next_file_id);
self.next_file_id += 1;                    // ← 已递增，指向 id+1
let uri = path_to_uri(path).unwrap_or_else(|| {
    format!("file:///unknown/{}", self.next_file_id)  // ← BUG: 用了递增后的值
        .parse()
        .unwrap_or_else(|_| "file:///unknown.rua".parse().unwrap())
});
```

应改为 `format!("file:///unknown/{}", id.0)`。

---

## 二、总体数据

| 文件 | 行数 | 发现问题 |
|------|------|---------|
| `crates/rua-lsp/src/lsp.rs` | 4,319 | 10 |
| `crates/rua-analysis/src/ide/completion.rs` | 1,613 | 14 |
| `crates/rua-analysis/src/ide/mod.rs` | 1,192 | 9 |
| `crates/rua-analysis/src/hir/infer.rs` | 2,029 | 17 |
| `crates/rua-analysis/src/diagnostic/mod.rs` | 747 | 8 |
| `crates/rua-analysis/src/hir/item_tree.rs` | 1,544 | 10 |
| `crates/rua-analysis/src/hir/def_map.rs` | 957 | 14 |
| `crates/rua-analysis/src/hir/scope.rs` | 830 | 9 |
| **总计** | **~15,000** | **91** |

---

## 三、严重问题（12 项）

### 问题 1 · `lsp.rs` — 30× handler boilerplate（~450 行冗余）

**严重度**: 🔴 严重 | **类别**: 啰嗦

每个 request handler 复制 15 行相同模式：

```rust
fn handle_xxx(&mut self, req: Request) {
    let id = req.id.clone();                           // 出现 30 次
    let (id, params) = match req.extract::<...>(...) {  // 出现 30 次
        Ok(v) => v,
        Err(e) => {
            let resp = Response::new_err(
                id,
                lsp_server::ErrorCode::InvalidParams as i32,  // 出现 32 次
                format!("invalid xxx params: {e:?}"),
            );
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        }
    };
    let uri = &params...uri;
    let pos = params...position;
    let result = self.project_position(uri, pos).and_then(|pp| {
        let analysis = self.host.analysis();
        analysis.some_method(pp)
    });
    let resp = Response::new_ok(id, result);
    let _ = self.connection.sender.send(Message::Response(resp));
}
```

**rust-analyzer 方案**: 用泛型分发或 macro：

```rust
fn handle_position_request<T, R>(
    &mut self, req: Request,
    f: impl FnOnce(&Analysis, ProjectPosition) -> Option<R>
) where R: serde::Serialize {
    // 统一处理 extract → project_position → call → respond
}
```

**影响**: 4,319 行的 `lsp.rs` 中约 450 行是纯粹 boilerplate。每新增 handler 需写 15 行模板，且不同 handler 间有细微不一致。

**建议**: 提取 `handle_position_request` 泛型方法或编写小 macro。

---

### 问题 2 · `lsp.rs` — 6× notification 解析重复（~60 行冗余）

**严重度**: 🔴 严重 | **类别**: 啰嗦

`handle_notification`（L2863-2937）的 6 个 arm 各自重复相同的 JSON 解析 + 错误处理：

```rust
DidOpenTextDocument::METHOD => {
    let params: lsp_types::DidOpenTextDocumentParams =
        match serde_json::from_value(not.params) {  // 出现 6 次
            Ok(p) => p,
            Err(e) => { eprintln!("rua-lsp: bad didOpen params: {e}"); return; }
        };
    self.open_document(params.text_document.uri, params.text_document.text);
}
```

**建议**: 用 macro 或泛型函数 `parse_notification<T>(params) -> Option<T>` 消除重复。

---

### 问题 3 · `completion.rs` — 14 个硬编码 relevance magic number

**严重度**: 🔴 严重 | **类别**: 不清晰

所有 relevance 分数是原始整数，分散在 `scope_completions()` 各处，无命名常量：

| Score | Line(s) | 什么获得此分数 | 备注 |
|-------|---------|--------------|------|
| 96 | 288 | `self` keyword inside method body | Boosted above all locals |
| 95 + extra | 318 | Local variables | Base 95, plus up to +5 for usage count (capped at 5) |
| 94 | 259 | Enum variants in if-let/while-let patterns | Slightly above match variants |
| 93 | 227, 245 | Match-arm variants and struct literal fields | Highest reusable score |
| 90 | 543 | Member completions (fields/methods after `.`) | |
| 90 | 426 | Builtin types in type position | |
| 88 | 424 | Numeric builtin types (`i64`, `f64`) in arithmetic context | Just below locals |
| 85 | 346 | Same-module definitions | |
| 85 | 630 | Postfix templates (`.if`, `.match`, etc.) | "below fields/methods, above keywords" |
| 85 | 850 | Enum variants in path completions (`Path::`) | |
| 80 | 826 | Path completion members | |
| 75 | 398 | Cross-module pub symbols (auto-import) | |
| 51 | 301 | Snippet patterns (`if let`, `while let`) | |
| 50 | 278 | Keywords (default, no snippet) | |
| 40 | 428 | Builtin types (default, non-type-pos, non-arithmetic) | |
| 35 | 445 | Builtin constructors (`Some`, `None`, `Ok`, `Err`) | |
| 20 | 464 | Built-in macros (`println!`, `vec!`, etc.) | |

**rust-analyzer 方案**: 用 `CompletionRelevance` 结构体表达正交子分数（type_match, name_match, provenance），组合而非硬编码。

**影响**: 新增补全类别时（1）必须手动分配数字，容易冲突；（2）无法表达「字段类型匹配期望类型时提升」这类语义；（3）不同类别间优先级关系不透明。

---

### 问题 4 · `completion.rs` — `scope_completions()` 310 行单函数（L191-500）

**严重度**: 🔴 严重 | **类别**: 不清晰

包含 8 个 completion 类别 + 后处理，全部内联在一个函数里：

- (0) Match-arms context: enum variants
- (0b) Struct literal context: field names
- (0c) if-let/while-let pattern context: variants
- (1) Keywords + snippets
- (2) Local variables
- (3) In-module definitions + cross-module pub symbols
- (4-6) Builtin types + constructors + macros
- Post-loop: type compatibility boost + replacement range setting

**rust-analyzer 方案**: 每个类别拆成独立 `complete_*` 函数，注册到数组中统一调用。

**影响**: 单一类别无法独立测试；控制流难以理解；新增类别时难以判断副作用。

---

### 问题 5 · `completion.rs` — Token-based context detection

**严重度**: 🔴 严重 | **类别**: 问题

`is_type_position()`（L1569-1585）只检测 `:` token 紧邻关系，**漏检**：
- `fn foo(x: |)` — 光标在参数列表中但不在 `:` 后
- `struct S { f: | }` — 同上
- `Vec<|>` — 光标在泛型参数位置

`is_expression_context()`（L1594-1611）依赖 19 个 token 硬编码列表，无法区分表达式位置和类型位置在 `,` 之后的场景。

**rust-analyzer 方案**: 从光标 AST 节点向上遍历，找到 enclosing `TypeClause` / `Expr` / `Pat` 节点。

**影响**: 中。补全质量不稳定，某些位置缺类型补全或多出不应出现的关键词。

---

### 问题 6 · `ide/mod.rs` + `completion.rs` — 4 对跨模块完全重复的函数

**严重度**: 🔴 严重 | **类别**: 问题/啰嗦

| 函数 | semantic/mod.rs | completion.rs |
|------|----------------|---------------|
| `previous_significant` | L312 (私有) | L1339 (`pub(crate)`) |
| `next_significant` | L320 (私有) | L1347 (私有) |
| `is_path_identifier` | L273 (私有) | L1335 (私有) |
| `module_at_position` | L328 (私有) | L1217 (`pub(crate)`) |

每对都是逐字相同的实现。

**建议**: 选 completion.rs 为 canonical 位置（版本已是 `pub(crate)`），删除 semantic/mod.rs 中的重复。

---

### 问题 7 · `infer.rs` — `infer_expr` 162 行单函数（L272-434）

**严重度**: 🔴 严重 | **类别**: 不清晰

这是中心推断分发函数。每个 match arm 有内联逻辑，经常 span 5-15 行。If arm 独占 40 行（L376-416），含 condition_diverges/then_ty/else_fact 元组、join 逻辑和延迟 mismatch 报告。Path arm 有特殊 `None` 逻辑（L286-297）。

Match/StructLiteral/MacroCall arms 已经委托，但 If/Assign/Range/Try/Index/Path 没有。

**建议**: 像 `infer_unary`、`infer_binary`、`infer_closure`、`infer_call` 一样提取 arm 为独立方法。

---

### 问题 8 · `infer.rs` — `infer_iterator_adapter` 131 行技术债务（L1168-1299）

**严重度**: 🔴 严重 | **类别**: 问题

文档注释承认这是临时方案（"handled inline until 4B.6's builtin metadata is complete enough"）。11 个 iterator 方法（count/any/all/find/fold/collect/filter/take/skip/map/filter_map/enumerate）各自内联 ad-hoc 闭包类型构造和参数/返回类型提取。

**影响**: 中。当 4B.6 完成时，整个函数需要废弃。

---

### 问题 9 · `infer.rs` — `infer_callable_call` 8 参数 + clippy suppression

**严重度**: 🔴 严重 | **类别**: 不清晰

L1396 显式压制 `#[allow(clippy::too_many_arguments)]`。`infer_method_call` 有同样问题。

**建议**: 引入 `CallContext` 结构体 bundling call/target/callable/args/expected/substitution/requirements/variadic。

---

### 问题 10 · `diagnostic/mod.rs` — 未使用函数 lint O(n²) + 字符串名校验（L488-526）

**严重度**: 🔴 严重 | **类别**: 问题

对每个非公开函数，遍历所有 body 的所有 NameRef，比对 `.name()` 字符串：

```rust
// L512 — 核对的是字符串名，不是实际决议
nr.name() == Some(name)
```

这意味着局部变量 `foo` 会错误抑制对未使用函数 `foo` 的警告。

**影响**: （1）O(defs × body_exprs) 复杂度；（2）字符串名匹配导致 false negative。

---

### 问题 11 · `diagnostic/mod.rs` — unreachable code lint 基于文本（L528-571）

**严重度**: 🔴 严重 | **类别**: 问题

通过正则匹配源码 `return;`/`break;`/`continue;`：
- 每行只检查第一个匹配（L567 的 `break` 退出关键字循环）
- 无法处理多行语句
- 字节偏移手动计算（L543-553）脆弱

**建议**: 基于 HIR 或 CFG 分析，而非文本匹配。

---

### 问题 12 · `def_map.rs` — `module_for_file` O(N×M)（L507-517）

**严重度**: 🔴 严重 | **类别**: 问题

```rust
pub fn module_for_file(&self, file_id: FileId) -> Option<ModuleId> {
    self.modules()
        .find(|module| {
            module.file_id() == Some(file_id)
                && !self.definitions().any(|definition| {
                    definition.target_module() == Some(module.id())
                        && definition.file_id() == file_id
                })
        })
        .map(ModuleData::id)
}
```

对每个 module 扫描所有 definitions。100 文件项目中每次调用 O(100²)。

**建议**: 建 `FileId -> ModuleId` 索引，O(1) 查找。

---

## 四、中等问题（48 项）

### lsp.rs（8 项）

13. **`handle_inlay_hint` 115 行 6 层嵌套**（L735-849）。类型提示 label 构造逻辑（L798-832）从 Ty::Named 匹配到 def 查找到 LSP Location 构造全部内联。建议提取 `make_type_hint_label()`。

14. **`register_watchers` 两个 for 循环相同模式**（L3042-3071）。library_roots 和 library_mounts 循环完全相同的 watcher 注册逻辑。提取 `try_add_watcher()`。

15. **`handle_did_save` 硬编码列宽**（L2996）：`end: Position::new(sl as u32, 80.min(source.len() as u32))`。ruac 错误不解析行列，粗暴高亮到第 80 列。应解析 ruac 的行列信息。

16. **`close_document` 不必要 clone**（L2960）：`.map(|(u, f)| (u.clone(), *f))` → `.map(|(_, f)| *f)`。

17. **`handle_completion` 双重查找**（L645+654）：`file_id_for_uri` 先检查 `.is_none()`，后在 `project_position` 内部再查一次。合并为单次查找。

18. **Handler 模式不统一**：
    - `handle_completion`：`file_id_for_uri(uri).is_none()` 检查
    - `handle_hover`：`project_position` + `Option::None`
    - `handle_inlay_hint`：let-else + 返回 null
    - `handle_document_highlight`：`project_position` + let-else

19. **`ensure_file_id` vs `ensure_file_id_for_path` 双重入口**：两个函数做几乎相同的事但用不同 key 类型。同一文件通过两个路径注册会产生重复 FileId 条目。

20. **`handle_watched_file_change` 递增 `next_root_id`**（L3111-3113）：每次文件变更事件创建新 SourceRootId，频繁变更导致 ID 无限增长。

---

### completion.rs（9 项）

21. **`replacement_range` 设置逻辑重复两次**（L490-496, L858-864）。提取 helper。

22. **`BodyData` / `BodyFullData` 几乎相同**（L1165-1198）。两个仅差一个 `Arc<BodyScopes>` 字段的类型和两个几乎相同的查找函数。合并。

23. **`type_compatibility_score` 未使用参数 `_expected: &Ty`**（L945）。只通过字符串匹配 detail text。

24. **`_token` 未使用参数**（L509）。accept 了未使用的 token。

25. **Method parameter name resolution 40 行深层嵌套**（L552-589）。复杂的 `Option` 链：`method_res` → `callable` → `res.target()` → `def_map.definition()` → `sig.params()` → fallback。

26. **`pattern_scrutinee_enum` while-let 用硬编码窗口**（L1462）：`saturating_sub(100)` 作为左边距。应检查光标是否在 while-let 的 pattern 节点内。

27. **`is_subsequence` 只做 ASCII lowercase**（L88）。非 ASCII 标识符不匹配。

28. **`postfix_templates` 每次分配 5 个 `String`**。可改为 `Cow<'static, str>` 或 lazy compute。

29. **`find_containing_body_data` 解构模式在 5 处重复**（L972, 1363, 1414, 1484, 774）。

---

### ide/mod.rs（6 项）

30. **`member_goto_definition` 同时解析 field 和 method 而不短路**（L442-444）：`field.or(method)` → 应改为 `field.or_else(|| method)`。

31. **`_bid` 变量名误导**（L508）：前缀暗示 unused，但紧接着使用。重命名为 `bid`。

32. **双重否定守卫**（L478）：`.is_none_or(|t| t.kind() != Dot)` → 改为显式 match。

33. **`resolve_call_target` 签名跨 12 行**（L172-182）：因完全限定路径。导入类型或使用别名。

34. **`references()` 中重复构造 `Semantics`**（L583 + L606-607）：只差一个 `.clone()`，第一个可复用。

35. **Hover 局部绑定 4 层嵌套**（L334-355）：混合 `if let` / `let... && let...` 语法。提取 `local_binding_info()` helper。

---

### infer.rs（10 项）

36. **`Condition::Let` 处理在 `visit_expr` 和 `visit_statement` 中重复**（scope.rs L477-486, 559-568）。提取 `visit_let_condition()`。

37. **"diverges" 模式重复 15+ 次**。模板：`let diverges = ...; if diverges { Ty::Never } else { actual }`。提取 `fn diverge_or(diverges: bool, ty: Ty) -> Ty`。

38. **`infer_builtin_call` Ok/Err arms 几乎相同**（L1560-1586）。各 13 行，仅差 Result 槽位。提取 `infer_result_constructor(is_ok, argument, expected)`。

39. **`invalidate_generics` / `refine_generic_bindings` 相同递归结构**（L1956-2028）。相同 Ty enum case 分解，仅动作不同。提取 `walk_ty` 或 visitor pattern。

40. **`infer_closure` 手动 stack save/restore**（L917-921）。若 `infer_expr` panic，栈会损坏。用 guard struct 实现 `Drop`。

41. **`proclaimed_return` 命名且注释与代码矛盾**（L932-936）。注释说 "Prefer the inferred return type when the expected type is Unknown"，但代码是 prefer expected unless unknown。

42. **`resolve_variant_payload` / `resolve_variant_def` 共享 6 行 prefix**（L657-663, 707-713）。提取 `resolve_variant_from_scrutinee()`。

43. **`infer_binary` 5 个否定条件连写**（L796-809）。提取 `valid_ordering_operands()`。

44. **`closure_origin` / `closure_target` 重复 paren unwrap**（L1764-1775, 1860-1875）。提取 `unwrap_parens()`。

45. **`InferenceContext` 15 字段**（L159-174）。分组为 sub-struct（如 `InferenceState` + `InferenceOutput`）。

46. **`seed_parameters` 3 层 `.or_else()` 链过于密集**（L253-266）。每个 level 做不同事：annotation → signature → self-receiver。提取 helper。

47. **`infer_pattern` 是无文档的单行 wrapper**（L586-588）。应加 `#[doc]` 解释为何与 `infer_pattern_with_narrow` 并存。

---

### diagnostic/mod.rs（6 项）

48. **未使用的 enum 变体**：
    - `DiagnosticSource::Name`（L172）— 定义但从未赋值
    - `DiagnosticSource::Structural`（L174）— 定义但从未赋值
    - `DiagnosticSeverity::Information`（L141）— 定义但从未使用
    - `DiagnosticSeverity::Hint`（L142）— 定义但从未使用

49. **`parse_error_code` substring 匹配顺序依赖**（L710-722）。`contains("unterminated")` → `contains("comment")` → ... 解析器改措辞导致静默回归到 `ParseUnexpectedToken`。

50. **`Diagnostic::new()` 所有调用重复 `DiagnosticOrigin::FastAnalysis` 6 次**。提取 `fast_diag(file_id, range, message) -> Diagnostic`。

51. **unused/lint 两个 lint 分别遍历 `body.bindings()`**（L392, 427）。可合并为单次遍历，因为两者共享相同的 body/source_map/resolution 获取逻辑。

52. **`dedup_by` 只比较 range/code/source**（L296-298）。同一位置、相同 code 但不同 message 的诊断会被合并，可能丢失更具体的错误信息。

53. **`_file_id` 未使用参数**（L687）。函数签名可省略。

---

### item_tree.rs（8 项）

54. **`impl_header` 66 行多功能**（L1173-1238）。5 个不同职责：找 impl 关键词、跳过泛型尖括号、确定 header 结束、扫描 `for` 关键词、构建 trait_ref/target_type。拆分为子函数。

55. **Angle-depth 追踪模式重复 5+ 次**。出现在 `collect_angle_clause`、`impl_header`（两处）、`where_predicates`、`split_constraint`、`split_top_level`。提取 `scan_balanced()`。

56. **模块 fingerprint 计算两次**（L574 + L665）。第一次用 `module_kind: None, imports: &[]` 的占位值，构造后再 refresh。中间 fingerprint 计算是浪费的。

57. **`.into_iter().collect()` 在 `lower_non_module_item` 的 6 个 arm 中重复**（L678-696）。每个 arm: `Self::lower_*(...).into_iter().collect()`。提取小 helper。

58. **`where_predicates` token 扫描脆弱**（L1047-1078）。`skip_while` + `skip(1)` pattern 假定 `where` token 被找到。

59. **`SignatureSyntax::from_tokens` 每个 token 触发 `format!` 分配**（L105）。`token_key` 为每个 token 分配临时 String。应直接 hash 结构化数据。

60. **`word_like` 函数 24 个 match arm**（L1252-1285）。新关键词可能被静默视为 non-word-like。

61. **`TypeRef::from_type` / `unit_if_missing` 几乎相同**（L168-176）。仅 fallback 不同（`missing` vs `from_display("()")`）。

---

### def_map.rs（12 项）

62. **`add_definition` 66 行 5+ 职责**（L751-816）。计算 DefKind、构建路径、计数出现次数、intern 标识、构造 Definition struct（15 字段）、插入 3 个数据结构。拆分。

63. **`lower_items` 56 行 3 条控制流路径**（L633-688）。Non-module items / inline modules / file modules / none module_kind 四条路径。`item.module_kind()` 被 match 两次（L648-651, L669-685）。

64. **`resolve_path`/`_unique`/`_lexical` 4 个方法近似重复**（L547-601）。`_unique` variants 仅差 `resolve_name` vs `resolve_name_unique`。合并。

65. **`resolve_member` O(n) 线性扫描**（L503）。对有大 trait impl 的类型，每次 O(n)。加 per-owner `BTreeMap` 索引。

66. **重复 import 收集模式**（L623-630, 672-678）。提取 `collect_imports()`。

67. **`base_path.clone()` 双份 String 分配**（L768-770）。`base_path` 格式化为 HashMap key 同时另外分配 `structural_path`。

68. **`build_inner` inline 构造复杂 `DefMap` struct**（L404-450）。47 行构造含 inline resolution_directory match。提取为独立方法。

69. **`add_module` panic 而非返回 `Option`**（L743）。`.expect("parent module belongs to this DefMap")` 对内部不一致不够优雅。

70. **`IdentityInterner` 无 u32 耗尽时的友好报错**（L102-103, 143）。`expect("definition identity space exhausted")`。

71. **`occurrences` HashMap 用 `String` key**（L611-612）。`HashMap<(FileId, String), u32>` 每个 (file, name) 对都分配。可用 `CompactString` 或 intern。

72. **`lower_file` 重置 scope_path 为 `""` 无文档**（L617 → L682）：不对称于 inline modules（继承路径）。应有注释或不同方法名。

---

### scope.rs（7 项）

73. **`poison_bindings` 参数名和 or-pattern 语义无文档**（L334, 600）。`poison_bindings` 表示 "mark bindings as ambiguous" 但命名非标准。Match arm 的 or-pattern 规则（`poison = arm.patterns().len() > 1 && !bindings.is_empty()`）无注释。

74. **`set_expr_scope`/`set_pat_scope`/`set_name_ref_scope` 结构完全相同**（L673-693）。都是 `if let Some(slot) = self.scopes.X_scopes.get_mut(id.index()) { *slot = Some(scope); }`。extract macro。

75. **`LocalResolver::resolve` 61 行混合 capture + use recording**（L715-776）。for 循环做 4 件事杂糅：（a）获取 name text，（b）scope lookup，（c）计算 crossed closures + dedup/harden captures，（d）记录 local use。提取 `record_capture` helper。

76. **`crossed_closures` 命名非标准**（L806-828）。→ `closures_between` 或 `enclosing_closures` 更清晰。

77. **`visit_expr` 有死 match arm `Expr::Block(_) => {}`**（L514）。Block 在顶部已 short-circuit（L397-406）。

78. **`add_bindings` 收集 Vec 然后遍历两次**（L362-391）。可一次完成。

79. **`infer_struct_literal` 两 pass 通过 `resolved_fields` 沟通**（L963-1028）。中间 Vec 作为 pass 间桥梁脆弱，需注释解释。

---

## 五、低优先级问题（28 项）

### lsp.rs
80. `project_position` 每次调用都重新 parse source + 建 LineIndex — 考虑 cache
81. `handle_did_save` 的 `ruac check` 错误信息解析无结构化 — 仅 strip `error:` prefix
82. `handle_watched_file_change` 的 `open_uris` 收集每次都 clone URI
83. `send_diagnostics` vs `publish_diagnostics` — 两个方法有重叠

### completion.rs
84. `in_arithmetic` 只检查紧邻前一个 significant token — `x + y + |` 不工作
85. `scope_completions()` 中 `seen: HashSet` 在两个位置分别创建（L201, L378）
86. Subsequence 过滤 case-insensitive 但对 Unicode 无支持
87. `completions()` 入口处对 dot/::/scope 三路分发 — 应用 enum 而非 bool flags
88. `type_compatibility_score` 通过字符串匹配 type rendering — 脆弱

### ide/mod.rs
89. `item_hover_text` 和 `resolve_callee_param_names` 都从 Callable 签名提取参数 — 共享
90. 局部绑定解析在 `hover`（L334-355）和 `prepare_rename`（L688-701）中重复
91. `call_hierarchy_incoming` 用 `body.expr(*callee).unwrap_or(&Missing)` — 脆弱
92. `type_hierarchy_subtypes`/`supertypes` 几乎相同的控制流

### infer.rs
93. `InferenceContext::new` 15 个字段初始化
94. `infer_match` 的 `arm_facts` / `else_fact` 命名 — → `arm_body_types`
95. `infer_closure` 的 `closure_returns` push/pop 逻辑
96. `expect_bool` 和 `report_mismatch` 应该合并为条件报告

### diagnostic/mod.rs
97. `n == "self"` 比较也会跳过用户声明的 `let self = ...`（L398）
98. Unreachable lint 只检测单行语句
99. `fast_diagnostics` 中 `inference_diag` 重复 pattern matching 类型错误

### item_tree.rs
100. `where_predicates` 中 `angle_depth` 用 `i32` — 应使用 `u32`（深度不会为负）
101. `ParameterData` 的 `type_ref` 是 Option 但 always set in practice
102. `ItemTreeItem::new` 签名 fingerprint 逻辑与 `refresh_signature_fingerprint` 重叠

### def_map.rs
103. `DefMapBuilder::lower_file` 构造 `IdentityContext::Lowering { file_id, module_id }` — 该 pattern 重复 3 次
104. `Definition` struct 15 字段无 builder pattern
105. `ModuleData` 的 `item_tree` 字段在 build 后即清空但仍存储在 struct

### scope.rs
106. `ScopeData::from_kind` 总是创建空 vectors — 无 lazy init
107. `LocalCandidate` 是私有 struct 但直接暴露 pub fields

---

## 六、消除冗余的预期收益

| 改进 | 可减行数 | 类别 |
|------|---------|------|
| LSP handler macro | ~400 | 啰嗦 |
| Notification 解析 macro | ~60 | 啰嗦 |
| 跨模块重复函数统一（4 对） | ~50 | 啰嗦 |
| `invalidate_generics`/`refine_generic_bindings` visitor | ~40 | 啰嗦 |
| "diverges" 模式 15+ 次 → helper | ~30 | 不清晰 |
| `resolve_path` 4 方法合并 | ~30 | 啰嗦 |
| `register_watchers` 循环提取 | ~15 | 啰嗦 |
| `from_type`/`unit_if_missing` 合并 | ~5 | 啰嗦 |
| **总计** | **~630** | |

---

## 七、与 rust-analyzer 的关键差距

| 维度 | rust-analyzer | Rua | 差距 |
|------|--------------|-----|------|
| Completion context | `CompletionContext` 结构体，从 AST 推导 | Token-based 硬编码 | 大 |
| Completion relevance | `CompletionRelevance` 结构体，正交子分数 | 14 个 magic integer | 大 |
| LSP dispatch | Macro/泛型分发，无 boilerplate | 30× 复制 15 行重复 | 大 |
| Completion 函数组织 | 独立 `complete_*` 注册到数组 | 310 行单函数 | 中 |
| Hover/Goto-def | 统一 `find_def_at` 处理所有场景 | 分离 member/item 步骤 | 中 |
| 缓存失效 | salsa 细粒度增量重算 | 全量清除 def_map + member_index | 中 |
| 未使用代码 lint | CFG/IR-level 分析 | 文本级正则匹配 | 大 |
| def_map lookup | 索引化 O(1) | O(n) 或 O(n²) 扫描 | 中 |
| 测试 | fixture `$0` 标记系统 | 手动计算列偏移 | 中 |

---

## 八、重构方案（允许重构级别修改）

> 以下方案分三个梯队：**最佳回报**（低投入高收益）、**中等回报**（中投入中收益）、**架构级**（高投入长期收益）。每个方案含可落地的代码草案。

---

### 第一梯队：最佳回报重构（低投入，高收益）

#### 重构 R1 · LSP Handler Macro — 消除 ~400 行重复

当前每个 handler 复制 15 行相同的 extract/error/send 模板。用两个 macro 覆盖所有场景：

```rust
// === 位置请求（hover, goto-def, references, prepare-rename 等） ===
macro_rules! handle_position_request {
    ($self:ident, $req:ident, $Params:ty, |$pp:ident| $body:expr) => {{
        let id = $req.id.clone();
        let (id, params) = match $req.extract::<$Params>(
            <$Params as lsp_types::request::Request>::METHOD
        ) {
            Ok(v) => v,
            Err(e) => {
                let resp = Response::new_err(
                    id,
                    lsp_server::ErrorCode::InvalidParams as i32,
                    format!("invalid params: {e:?}"),
                );
                let _ = $self.connection.sender.send(Message::Response(resp));
                return;
            }
        };
        let $pp = $self.project_position(
            &params.text_document_position_params.text_document.uri,
            params.text_document_position_params.position,
        );
        let result = $pp.and_then(|pp| {
            let analysis = $self.host.analysis();
            $body(analysis, pp)
        });
        let resp = Response::new_ok(id, result);
        let _ = $self.connection.sender.send(Message::Response(resp));
    }};
}

// === 文档请求（completion, inlay-hint, semantic-tokens, folding 等） ===
macro_rules! handle_doc_request {
    ($self:ident, $req:ident, $Params:ty, $empty:expr, |$file_id:ident, $analysis:ident| $body:expr) => {{
        let id = $req.id.clone();
        let (id, params) = match $req.extract::<$Params>(
            <$Params as lsp_types::request::Request>::METHOD
        ) {
            Ok(v) => v,
            Err(e) => {
                let resp = Response::new_err(
                    id,
                    lsp_server::ErrorCode::InvalidParams as i32,
                    format!("invalid params: {e:?}"),
                );
                let _ = $self.connection.sender.send(Message::Response(resp));
                return;
            }
        };
        let Some($file_id) = $self.file_id_for_uri(params.uri()) else {
            let resp = Response::new_ok(id, $empty);
            let _ = $self.connection.sender.send(Message::Response(resp));
            return;
        };
        let $analysis = $self.host.analysis();
        let result = $body;
        let resp = Response::new_ok(id, result);
        let _ = $self.connection.sender.send(Message::Response(resp));
    }};
}
```

**迁移示例** — `handle_hover` 从 30 行变为：

```rust
fn handle_hover(&mut self, req: Request) {
    handle_position_request!(self, req, lsp_types::HoverParams, |pp| {
        analysis.hover(pp).map(|hover| to_lsp_hover(&hover))
    });
}
```

`handle_completion` 从 55 行变为：

```rust
fn handle_completion(&mut self, req: Request) {
    handle_doc_request!(
        self, req, lsp_types::CompletionParams,
        CompletionResponse::Array(Vec::new()),
        |file_id, analysis| {
            let source = analysis.parse(file_id).syntax_node().text().to_string();
            let line_index = LineIndex::new(&source);
            let pp = self.project_position(uri, pos)?;
            let native_items = analysis.completions(pp);
            let items: Vec<_> = native_items
                .into_iter()
                .map(|item| completion_to_lsp(&item, &line_index, &source, file_id))
                .collect();
            if items.len() > 100 {
                CompletionResponse::List(CompletionList { is_incomplete: true, items })
            } else {
                CompletionResponse::Array(items)
            }
        }
    );
}
```

**覆盖范围**: 28 个 handler 可简化为 2-8 行宏调用每个。

---

#### 重构 R2 · CompletionRelevance 结构体 — 替换 14 个 magic number

```rust
/// 补全相关度评分，参照 rust-analyzer 的 CompletionRelevance。
/// 各子分数通过 score() 组合，而非硬编码整数。
#[derive(Debug, Clone, Copy, Default)]
pub struct CompletionRelevance {
    /// 基础类别分数（0-100）
    pub base: u8,
    /// 类型精确匹配时的加分
    pub exact_type_match: bool,
    /// 类型名匹配时的加分（较弱的信号）
    pub type_name_match: bool,
    /// 来自当前模块/作用域
    pub is_local: bool,
    /// 来自当前 crate
    pub is_from_this_crate: bool,
    /// 是 deprecated 的符号
    pub is_deprecated: bool,
}

impl CompletionRelevance {
    // ── 类别构造器（集中管理所有基准分）──

    pub const fn keyword()          -> Self { Self { base: 50, ..Self::default() } }
    pub const fn snippet()          -> Self { Self { base: 51, ..Self::default() } }
    pub const fn builtin_type()     -> Self { Self { base: 40, ..Self::default() } }
    pub const fn builtin_type_pos() -> Self { Self { base: 90, ..Self::default() } }
    pub const fn local(usage: u8)   -> Self { Self { base: 95 + usage.min(5), is_local: true, ..Self::default() } }
    pub const fn self_keyword()     -> Self { Self { base: 96, is_local: true, ..Self::default() } }
    pub const fn member()           -> Self { Self { base: 90, ..Self::default() } }
    pub const fn same_module()      -> Self { Self { base: 85, is_from_this_crate: true, ..Self::default() } }
    pub const fn cross_module()     -> Self { Self { base: 75, ..Self::default() } }
    pub const fn postfix()          -> Self { Self { base: 85, ..Self::default() } }
    pub const fn match_variant()    -> Self { Self { base: 93, ..Self::default() } }
    pub const fn iflet_variant()    -> Self { Self { base: 94, ..Self::default() } }
    pub const fn path_member()      -> Self { Self { base: 80, ..Self::default() } }
    pub const fn path_variant()     -> Self { Self { base: 85, ..Self::default() } }
    pub const fn builtin_const()    -> Self { Self { base: 35, ..Self::default() } }
    pub const fn builtin_macro()    -> Self { Self { base: 20, ..Self::default() } }
    pub const fn arithmetic_num()   -> Self { Self { base: 88, ..Self::default() } }

    /// 链式修饰：如果期望类型匹配，提升相关度
    pub fn with_exact_type_match(mut self, matches: bool) -> Self {
        self.exact_type_match = matches;
        self
    }

    pub fn with_type_name_match(mut self, matches: bool) -> Self {
        self.type_name_match = matches;
        self
    }

    pub fn with_deprecated(mut self, deprecated: bool) -> Self {
        self.is_deprecated = deprecated;
        self
    }

    /// 解析为可比较的分数值
    pub fn score(&self) -> u16 {
        let mut s = self.base as u16;
        if self.exact_type_match   { s += 10; }
        if self.type_name_match    { s += 5;  }
        if self.is_local           { s += 2;  }
        if self.is_from_this_crate { s += 3;  }
        if self.is_deprecated      { s = s.saturating_sub(20); }
        s
    }
}
```

**迁移**: 在 `scope_completions()` 中搜索 `relevance: 50` 替换为 `relevance: CompletionRelevance::keyword()`，全部 14 个分数集中到构造器。`CompletionItem.relevance` 字段类型从 `u16` 改为 `CompletionRelevance`。

---

#### 重构 R3 · 去重 4 对跨模块函数 — 选 canonical 位置

```rust
// completion.rs 中保留（已经是 pub(crate)）
pub(crate) fn previous_significant(token: &SyntaxToken) -> Option<SyntaxToken> { ... }
pub(crate) fn is_path_identifier(kind: SyntaxKind) -> bool { ... }
pub(crate) fn module_at_position(db: &dyn BaseDb, def_map: &DefMap, file_id: FileId, offset: usize) -> Option<ModuleId> { ... }

// semantic/mod.rs 中删除重复实现，改为 re-export
use crate::ide::completion::{previous_significant, is_path_identifier, module_at_position};
```

---

#### 重构 R4 · CompletionContext 结构体 — 替代 token-based 检测

```rust
/// 一次性在入口处构建的补全上下文，后续传给各 complete_* 函数。
/// 参照 rust-analyzer 的 CompletionContext。
pub(crate) struct CompletionContext<'a> {
    pub db: &'a dyn BaseDb,
    pub position: ProjectPosition,
    pub file_id: FileId,
    pub offset: usize,
    pub def_map: &'a DefMap,
    pub token: SyntaxToken,

    // ── 从 AST 推导的上下文标记（替代 token 检测）──
    pub in_type_position: bool,
    pub in_expression_position: bool,
    pub in_pattern_position: bool,
    pub in_method_body: bool,
    pub in_impl_block: bool,
    pub in_loop: bool,

    // ── 从 inference 推导 ──
    pub expected_type: Option<Ty>,
    pub self_receiver_ty: Option<Ty>,
}

impl<'a> CompletionContext<'a> {
    /// 从光标位置一次性构建上下文。
    /// 从光标 AST 节点向上遍历确定 enclosing 上下文，而非 token 检测。
    pub fn new(db: &'a dyn BaseDb, position: ProjectPosition) -> Option<Self> {
        let parse = db.parse(position.position.file_id);
        let root = parse.syntax_node();
        let offset = position.position.offset as u32;
        let token = token_at_offset(&root, offset)?;

        let mut in_type_position = false;
        let mut in_expression_position = false;
        let mut in_pattern_position = false;
        let mut in_method_body = false;
        let mut in_impl_block = false;
        let mut in_loop = false;

        // 从 token 向上遍历 AST，确定上下文
        let mut node = token.parent();
        while let Some(current) = node {
            match current.kind() {
                SyntaxKind::TypeClause   => in_type_position = true,
                SyntaxKind::ParamList    => in_type_position = true,  // fn foo(x: |)
                SyntaxKind::FieldDecl    => in_type_position = true,  // struct S { f: | }
                SyntaxKind::FnBody | SyntaxKind::BlockExpr => in_expression_position = true,
                SyntaxKind::LetPat       => in_pattern_position = true,
                SyntaxKind::MatchArmPat  => in_pattern_position = true,
                SyntaxKind::FnDecl       => { /* 检查是否是方法 */ }
                SyntaxKind::ImplBlock    => in_impl_block = true,
                SyntaxKind::WhileExpr |
                SyntaxKind::LoopExpr  |
                SyntaxKind::ForExpr      => in_loop = true,
                _ => {}
            }
            node = current.parent();
        }

        // 获取期望类型（如果光标在表达式位置）
        let expected_type = if in_expression_position {
            expected_type_at_cursor(db, position)
        } else {
            None
        };

        Some(CompletionContext {
            db, position,
            file_id: position.position.file_id,
            offset: offset as usize,
            def_map: db.def_map(position.position.file_id),
            token,
            in_type_position,
            in_expression_position,
            in_pattern_position,
            in_method_body,
            in_impl_block,
            in_loop,
            expected_type,
            self_receiver_ty: None, // 从 innermost_body_owner 推导
        })
    }
}
```

**迁移**: `scope_completions()` / `member_completions()` / `path_completions()` 的签名从 5+ 个参数改为接收 `&CompletionContext`。`is_type_position()` 和 `is_expression_context()` 可删除。

---

### 第二梯队：中等回报重构（中投入，中收益）

#### 重构 R5 · 拆分 `scope_completions()` — 310 行 → 独立 `complete_*` 函数

```rust
/// 补全函数的统一签名
type CompleteFn = fn(&CompletionContext, &mut Vec<CompletionItem>, &mut HashSet<String>);

/// 注册所有补全类别（参照 rust-analyzer 的 completions 数组）
const COMPLETIONS: &[CompleteFn] = &[
    complete_keywords,
    complete_snippets,
    complete_locals,
    complete_module_items,
    complete_cross_module_items,
    complete_builtin_types,
    complete_builtin_constructors,
    complete_builtin_macros,
];

fn scope_completions(ctx: &CompletionContext) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();

    // 上下文相关的专项补全
    if let Some(enum_ty) = match_scrutinee_enum(ctx) {
        complete_match_variants(ctx, &enum_ty, &mut items, &mut seen);
    }
    if let Some(struct_ty) = struct_literal_type(ctx) {
        complete_struct_fields(ctx, &struct_ty, &mut items, &mut seen);
    }
    if let Some(enum_ty) = pattern_scrutinee_enum(ctx) {
        complete_iflet_variants(ctx, &enum_ty, &mut items, &mut seen);
    }

    // 通用补全（批量调用）
    for complete_fn in COMPLETIONS {
        complete_fn(ctx, &mut items, &mut seen);
    }

    // 后处理
    apply_type_compatibility_boost(&mut items, ctx);
    apply_replacement_ranges(&mut items, ctx);
    CompletionItem::normalize(items)
}
```

**收益**: 每种补全类型可独立测试，新增补全类别只需写一个函数 + 注册到数组。

---

#### 重构 R6 · `infer_expr` 拆分 — 162 行 → arm 提取

```rust
impl InferenceContext {
    fn infer_expr(&mut self, expr_id: ExprId, expected: Option<&Ty>) -> Ty {
        match &self.body.expr(expr_id) {
            // 已委托的 arms
            Expr::Block(body)            => self.infer_block(*body, expected),
            Expr::Match(scrutinee, arms) => self.infer_match(expr_id, *scrutinee, arms, expected),
            Expr::Call(call)             => self.infer_call(expr_id, call, expected),
            Expr::MethodCall(call)       => self.infer_method_call(expr_id, call, expected),
            Expr::Closure(closure)       => self.infer_closure(expr_id, closure, expected),
            Expr::StructLiteral(lit)     => self.infer_struct_literal(expr_id, lit, expected),
            Expr::MacroCall(mac)         => self.infer_macro(expr_id, mac, expected),
            Expr::Unary(op, inner)       => self.infer_unary(expr_id, *op, *inner),
            Expr::Binary(lhs, op, rhs)   => self.infer_binary(expr_id, *lhs, *op, *rhs),

            // 新提取的 methods
            Expr::If(cond, then_b, else_b) => self.infer_if_expr(expr_id, *cond, *then_b, else_b.as_ref(), expected),
            Expr::Assign(target, value)    => self.infer_assign_expr(expr_id, *target, *value),
            Expr::Range(start, end)        => self.infer_range_expr(expr_id, *start, *end, expected),
            Expr::Try(inner)               => self.infer_try_expr(expr_id, *inner, expected),
            Expr::Index(base, index)       => self.infer_index_expr(expr_id, *base, *index, expected),
            Expr::Path(path)               => self.infer_path(expr_id, path, expected),
            Expr::Literal(lit)             => self.infer_literal(lit),
            // ... rest
        }
    }

    fn infer_if_expr(
        &mut self, expr_id: ExprId, cond: ExprId,
        then_b: ExprId, else_b: Option<&ExprId>, expected: Option<&Ty>,
    ) -> Ty {
        // 40 行从原 infer_expr 的 If arm 移入
        // ...
    }
}
```

---

#### 重构 R7 · "diverges" 模式统一 — 15+ 处 → 1 个 helper

```rust
/// 如果表达式分支发散（返回 Never），则整个表达式类型为 Never。
/// 否则返回实际类型。
fn diverge_or(diverges: bool, ty: Ty) -> Ty {
    if diverges { Ty::Never } else { ty }
}

// 迁移前：
let diverges = /* check */;
if diverges { Ty::Never } else { actual_ty }

// 迁移后：
diverge_or(diverges, actual_ty)
```

---

#### 重构 R8 · `resolve_path` 4 方法合并 — 4 → 1

```rust
#[derive(Clone, Copy)]
pub enum ResolveStrategy {
    /// 返回第一个匹配（用于 hover）
    First,
    /// 要求唯一匹配（用于 goto-def）
    Unique,
    /// 词法作用域查找（用于 completion scope）
    Lexical,
    /// 词法作用域 + 唯一匹配
    LexicalUnique,
}

impl DefMap {
    pub fn resolve_path(
        &self,
        module_id: ModuleId,
        segments: &[String],
        strategy: ResolveStrategy,
    ) -> Option<PathResolution> {
        // 合并原 resolve_path / resolve_path_unique /
        //     resolve_path_lexical / resolve_path_lexical_unique 的公共逻辑
        // strategy.unique → 调用 resolve_name vs resolve_name_unique
        // strategy.lexical → 决定遍历逻辑
        // ...
    }
}
```

---

### 第三梯队：架构级重构（高投入，长期收益）

#### 重构 R9 · LSP Server 模块拆分

当前 `lsp.rs` 4,319 行。参照 rust-analyzer 的 `handlers/` 目录：

```
crates/rua-lsp/src/
├── lsp.rs              # ~200 行: Server struct, main_loop, file identity
├── handlers/
│   ├── mod.rs          # handle_request dispatch
│   ├── hover.rs        # handle_hover, handle_definition, handle_goto_implementation
│   ├── completion.rs   # handle_completion, handle_resolve_completion
│   ├── signature.rs    # handle_signature_help
│   ├── inlay_hint.rs   # handle_inlay_hint
│   ├── code_action.rs  # handle_code_action, handle_execute_command
│   ├── symbol.rs       # handle_document_symbol, handle_workspace_symbol
│   ├── reference.rs    # handle_references, handle_rename, handle_prepare_rename
│   ├── highlight.rs    # handle_document_highlight
│   ├── semantic.rs     # handle_semantic_tokens(_range)
│   ├── folding.rs      # handle_folding_range, handle_selection_range
│   ├── link.rs         # handle_document_link
│   ├── hierarchy.rs    # handle_call_hierarchy_*, handle_type_hierarchy_*
│   ├── formatting.rs   # handle_formatting, handle_range_formatting, handle_on_type_formatting
│   └── lens.rs         # handle_code_lens
├── notification.rs     # handle_notification + 6 notification 类型
├── convert.rs          # LSP ↔ analysis 类型转换
└── config.rs           # library config, watchers, file discovery
```

**收益**: 每个 handler 独立测试，新人可快速定位代码，模块职责清晰。

---

#### 重构 R10 · 缓存失效粒度

当前修改任何一个文件清除所有 `def_map` + `member_index`。概念方案：

```rust
/// 为每个 def_map entry 记录它依赖的文件
struct DefMapDeps {
    /// def_id → 定义所在的 file_id
    definition_files: HashMap<DefId, FileId>,
    /// file_id → 哪些 DefId 的定义写在这个文件中
    file_to_defs: HashMap<FileId, Vec<DefId>>,
}

impl BaseDb {
    fn invalidate_file(&mut self, file_id: FileId) {
        self.parse_cache.remove(&file_id);
        self.item_tree_cache.remove(&file_id);

        // 精确失效：只移除这个文件产生的 def，其他文件的 def 保留
        if let Some(defs) = self.def_map_deps.file_to_defs.get(&file_id) {
            for def_id in defs {
                self.def_map_cache.remove(def_id);
            }
        }
        // member_index: 只失效引用这些 def 的 member 缓存
    }
}
```

**适用时机**: 当项目有 20+ 文件时再来做。

---

#### 重构 R11 · 测试 Fixture 系统

```rust
/// 参照 rust-analyzer 的 fixture 标记。
/// `$0` 标记光标位置，取代手动计算 column 偏移量。
#[test]
fn hover_shows_type() {
    check_hover(
        r#"
        fn main() {
            let x$0 = 42;
        }
        "#,
        "let x: i64",
    );
}

#[test]
fn completion_after_dot() {
    check_completion(
        r#"
        struct Point { x: i64, y: i64 }
        fn main() {
            let p = Point { x: 1, y: 2 };
            p.$0
        }
        "#,
        &["x: i64", "y: i64"],
    );
}

// fixture 解析器（~100 行）
fn parse_fixture(source: &str) -> (String, Vec<FixturePosition>) {
    // 解析 $0（光标）, $1, $2...（选区）等标记
    // 返回纯净源码 + 位置映射
}
```

---

#### 重构 R12 · Notification 解析 macro

```rust
macro_rules! extract_notification {
    ($not:expr, $T:ty, $label:literal, |$params:ident| $body:expr) => {{
        match serde_json::from_value::<$T>($not.params) {
            Ok($params) => $body,
            Err(e) => {
                eprintln!("rua-lsp: bad {} params: {e}", $label);
                return;
            }
        }
    }};
}

// 使用：
DidOpenTextDocument::METHOD => {
    extract_notification!(not, lsp_types::DidOpenTextDocumentParams, "didOpen", |p| {
        self.open_document(p.text_document.uri, p.text_document.text);
    });
}
```

**消除**: 6 个 notification arm 各 7 行重复 → 各 3 行。

---

## 九、重构执行路线图

| 阶段 | 重构项 | 预计工时 | 风险 | 收益 |
|------|--------|---------|------|------|
| **第1周** | R1 Handler macro | 3h | 低 | 消除 400 行重复 |
| **第1周** | R3 跨模块函数去重 | 0.5h | 低 | 消除 4 对重复 |
| **第1周** | R2 Relevance 结构体 | 2h | 低 | 可扩展性 |
| **第1周** | R7 "diverges" helper | 0.5h | 低 | 可读性 |
| **第1周** | R12 Notification macro | 0.5h | 低 | 消除 60 行重复 |
| **第2周** | R4 CompletionContext 结构体 | 4h | 中 | 补全质量 |
| **第2周** | R5 `scope_completions` 拆分 | 3h | 中 | 可测试性 |
| **第3周** | R6 `infer_expr` 拆分 | 2h | 低 | 可维护性 |
| **第3周** | R8 `resolve_path` 合并 | 1h | 低 | DRY |
| **第4周+** | R9 LSP 模块拆分 | 6h | 中 | 长期架构 |
| **以后** | R10 缓存细粒度 | 8h | 高 | 扩展性 |
| **以后** | R11 Fixture 系统 | 4h | 中 | 测试体验 |

**第1周五项可在一个工作日内完成**，净减少 ~500 行重复代码，同时建立可扩展的 completion relevance 体系。建议先做 R1（最直观的收益）和 R2（消除 magic number），再做 R4（改动较多但影响最大）。
