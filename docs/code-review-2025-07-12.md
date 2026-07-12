# Rua 全盘代码审查报告

> 日期: 2025-07-12 | 审查范围: 全部 4 个 crate (~56k 行) | 方法: 4 个并行 agent + 人工审查

---

## 发现汇总

| 严重度 | 数量 | 说明 |
|--------|------|------|
| 🔴 Bug | 18 | 影响正确性，会导致错误行为 |
| 🟠 Crash | 3 | 会导致 LSP 进程崩溃 |
| 🟡 Correctness | 9 | 边界情况下结果不正确 |
| 🟢 Perf | 8 | 性能问题，在大型项目中会显现 |
| 🔵 Maintenance | 12 | 代码重复，维护风险 |
| ⚪ Feature Gap | 9 | 功能实现但质量不足 |

---

## 🔴 Completion Engine 专项（最高优先级 — 4 个 CRITICAL bug）

> 以下发现来自 completion.rs 的独立深入审查。**基于 HIR 的补全引擎完全没有测试**。所有现有测试覆盖的是 `rua-syntax/src/analysis.rs` 中不同的基于 symbol 的旧实现。

### CB1. `path_segments_before` 完全损坏 — `::` 路径补全静默返回空
**文件**: `completion.rs:860-890`
光标在 `math::|` 后，`previous_significant(token)` 返回 `::`。函数调用 `is_path_identifier(::)`，返回 false（`::` 不是 Ident/KwSelf）。函数永远返回空列表 → `resolve_path_prefix_module` 收到空 segments → `path_completions` 返回空。**所有 `::` 路径补全完全失效**。

### CB2. `infer_dot_receiver` 用原始 `token_at_offset` 而非 wrapper
**文件**: `completion.rs:722-726`
直接调用 `root.token_at_offset()` 而不是本地包装函数（1280 行）。wrapper 对于 `Between(left, right)` 当 `left.kind() == Dot` 时偏好 `right`，但原始调用偏好 `left`。光标在 `.x` 边界时，infer 取 `.` 而 completions 入口取 `x`，导致 AST 向上遍历从错误 token 开始。

### CB3. `infer_dot_receiver` 接收者子节点种类遗漏 7 种
**文件**: `completion.rs:741-750`
只匹配 `PathExpr, Ident, CallExpr, FieldExpr, IndexExpr, ParenExpr, LiteralExpr`。缺失：`MethodCallExpr`（`a.foo().bar`）、`Block`（`{}.foo`）、`ArrayExpr`（`[1,2].foo`）、`UnaryExpr`（`*ptr.foo`）、`BinExpr`（`(a+b).foo`）、`StructLitExpr`、`ClosureExpr`。

### CB4. `pattern_scrutinee_enum` 只处理 if-let，不处理 while-let
**文件**: `completion.rs:1347-1380`
文档说处理 "if-let or while-let"，但只遍历 `body.exprs()`，其中 `If` 是 Expr 但 `While` 是 Statement。`while let` 的变体补全完全失效。
**修**: 同时遍历 `body.exprs()` 和 block 中的 `Statement::While`。

---

## 🔴 Bug（影响正确性）

### B1. `signature_help` 对方法调用失效
**文件**: `ide/mod.rs:204-208`
`type_of_expr()` 对 Call/MethodCall 返回的是**结果类型**（如 `i64`），不是 callable type。代码假设返回 `Ty::Function`/`Ty::Closure`，结果对所有调用都返回 None。而且从未查询 `member_index` 获取方法签名。
**修**: 对 MethodCall 通过 `member_index.resolve_method()` 获取 `CallableTy`。

### B2. `references()` 跨文件 rename 返回假 URI
**文件**: `lsp.rs:3721-3725`
`source_change_to_workspace_edit` 生成 `file:///unknown/3` 格式的 URI，客户端无法定位真实文件，导致多文件 rename 静默失败。
**修**: 通过 `Server.file_ids` 反向查找真实 URI。

### B3. "Inline function" 从错误文件读 body
**文件**: `lsp.rs:1594`
`source[body_start..body_end]` 使用当前文件的 `source`，但 `body_start`/`body_end` 是被调用函数文件的偏移量。跨文件 inline 会读到垃圾数据或 panic。
**修**: 先获取被调用函数的文件源码。

### B4. Code action 被错误地门控在 selection 内
**文件**: `lsp.rs:1314-1491`
"Sort struct fields"、"Remove trailing comma"、"Extract variable" 等 code action 不需要选区也能工作，但被放在 `if sel_end > sel_start` 块内，无选区时不可达。
**修**: 将不需选区的 action 移到 selection 检查之外。

### B5. `goto_implementation` 子串匹配
**文件**: `ide/mod.rs:494`
`trait_ref.contains(trait_name)` 使用 `contains` 而非 `==`。trait `Foo` 会错误匹配 `impl FooBar for T`。
**修**: 改为精确匹配。

### B6. `type_hierarchy_supertypes` 同样的子串匹配 bug
**文件**: `ide/mod.rs:849`
同上。

### B7. `member_goto_definition` 缺少 fallback
**文件**: `ide/mod.rs:439-441`
`infer_dot_receiver` 返回 None 时 `?` 直接返回 None。但 `member_hover` 有 AST 扫描 fallback（迭代所有 body 查找同名 local）。goto-def 应使用相同策略。
**修**: 提取公共 `resolve_member_access()` 函数。

### B8. Unused variable lint 误报 `self`
**文件**: `diagnostic/mod.rs:381-393`
方法的 `self` 参数从不直接使用（直接使用的是它的字段），lint 将其标记为 "unused variable `self`"。
**修**: 跳过名为 `self` 的 binding。

### B9. Unreachable code lint 在字符串/标识符内误报
**文件**: `diagnostic/mod.rs:517-518`
纯文本匹配 `"return"`、`"break"`、`"continue"`，不检查语法上下文。`let s = "return"` 会触发，`let return_value = 5` 也会触发。
**修**: 使用 AST 检测而非纯文本匹配。

### B10. `handle_did_save` 将源码文本当作文件路径
**文件**: `lsp.rs:2932`
```rust
.arg(&source)  // source 是文件内容，不是路径
```
命令变成 `ruac check "fn main() {}"`，永远找不到文件。
**修**: 传递文件的实际磁盘路径。

### B11. CodeLens 报告总定义数而非每定义的引用数
**文件**: `lsp.rs:401-423`
```rust
format!("{} reference(s)", refs.saturating_sub(1))
```
计算的是文件中所有函数/方法的数量，不是指向该定义的引用数。每个函数显示相同计数。
**修**: 使用 `ReferenceIndex` 或按名称查找引用。

### B12. "Extract variable" 硬编码 `"var_name"`
**文件**: `lsp.rs:1321`
注释说 "Generate a variable name from the text"，实际总是 `let var_name = expr;`。
**修**: 从表达式文本生成有意义的变量名。

### B13. `suppress_cascade` 用 100 字节硬编码启发式
**文件**: `diagnostic/mod.rs:321-325`
行可以有远超 100 字节的内容。同行的 parse error 和 type error 超过 100 字节距离不会被 suppress，不同行但 100 字节内的会被误 suppress。
**修**: 使用行号比较。

### B14. `rfind("let ")` 匹配标识符/字符串内部
**文件**: `lsp.rs:1064`
"Add mut" quickfix 向后搜索 `"let "`，会匹配 `outlet_name` 或字符串内的 `"let "`。
**修**: 使用 AST token 查找。

### B15. 推断：算术运算对不兼容类型静默返回 Unknown
**文件**: `infer.rs:739-772`
`STRING + F64`、`MyStruct * I64` 等不兼容类型的算术运算无诊断信息地返回 Unknown。只在 BOOL/UNIT 上触发 `InvalidBinary`。
**修**: 当双方都是具体类型但不兼容时发射 `InvalidBinary`。

---

## 🟠 Crash（进程崩溃）

### C1. 7 处 URI `.unwrap()` panic
**文件**: `lsp.rs:1855, 1919, 1990, 2050, 2113, 2178, 3099`
格式化为 `file:///unknown/N` 后直接 `parse().unwrap()`。如果 N 产生非法 URI 字符就崩溃。
**修**: 使用 `Url::parse()` 返回 Result 并优雅处理。

### C2. SSR: 空 pattern 导致死循环
**文件**: `lsp.rs:2219, 2237`
`source[..].find("")` 返回 `Some(0)`，导致 while 循环永久运行直到 100 个编辑上限。
**修**: 对空 pattern 提前返回错误。

### C3. "Wrap in block" 替换整行
**文件**: `lsp.rs:1368-1377`
选区替换的 range 从 `Position::new(line, 0)` 开始（列 0），而不是选区起始位置，导致销毁选区前面的内容。
**修**: 使用选区真实的起始位置。

---

## 🟡 Correctness（边界情况不正确）

### CO1. `references()` 缺类型标注/字段/常量中的引用
**文件**: `ide/mod.rs:556-558`
只扫描 Function/Method body，遗漏：类型标注、struct 字段类型、enum variant payload、const 初始化表达式。
**修**: 扩展到所有包含表达式的位置。

### CO2. `references()` 跨 crate 可能遗漏引用
**文件**: `ide/mod.rs:586-589`
使用 `self.db.def_map(ref_file)` 以单文件为根，跨 crate 时可能没有目标的定义。
**修**: 使用 project def_map。

### CO3. `member_hover` fallback 跨 body 取第一个匹配
**文件**: `ide/mod.rs:323-335`
AST fallback 迭代所有 body 找同名 local，取第一个匹配。如果两个函数中都有 `p`，第一个（随机顺序）胜出。应该使用包含光标的 body。
**修**: 先确定光标所在 body，再查找。

### CO4. `LocalCapture` 缺少 Read/Write 区分
**文件**: `scope.rs:214-219`
闭包捕获不区分只读捕获 vs 可变捕获。对于需要 mutability 分析的下游消费者是缺口。
**修**: 添加 `capture_kind: Read | Write | ByValue` 字段。

### CO5. MethodCall receiver 总是标记为 Read
**文件**: `scope.rs:440`
`&mut self` 方法调用不会将 receiver 标记为 Write。对依赖 LocalUseKind 的分析是缺口。
**修**: 等 inference 阶段识别 `&mut self` 后回标记。

### CO6. Comparison 运算符不检查操作数类型
**文件**: `infer.rs:779-784`
`MyStruct < OtherStruct` 会被推断为 `Bool` 而无任何诊断。应检查操作数兼容性。
**修**: 至少要求双方是 numeric 或同类型。

### CO7. `enumerate` adapter 忽略多余参数
**文件**: `infer.rs:1257-1259`
`iter.enumerate(extra_arg)` 应报错但被静默接受。
**修**: 检查 `args.is_empty()`。

### CO8. LSP import additional_text_edits 总是插入 (0,0)
**文件**: `lsp.rs:3646-3654`
导入语句的 text edit 总是定位到文件第一行第一列，会毁坏已有内容。
**修**: 扫描文件找到正确的 import 插入位置。

---

## 🟢 Perf

### P1. `references()` O(N*M*resolve)
**文件**: `ide/mod.rs:556-601`
对每个定义扫描所有 body 的 name_ref，O(N*M)，N = 定义数，M = name_ref 数。
**修**: 构建 ReferenceIndex。

### P2. Redundant mut lint O(B*E*D)
**文件**: `diagnostic/mod.rs:427-450`
对每个 mutable binding 遍历所有 body expression。B 个 binding × E 个 expression。
**修**: 单次遍历收集所有 field-write-through 的 binding。

### P3. Unused function lint O(D²*N)
**文件**: `diagnostic/mod.rs:490-497`
对每个定义扫描所有其他定义的 body name_ref。
**修**: 构建 name→DefId 索引。

### P4. `find_def_at` O(n) 线性扫描
**文件**: `semantic/mod.rs:58`
每次调用都遍历所有定义的 name_range。
**修**: 按 file_id 建立空间索引。

### P5. `body_data_at` 又一次线性扫描
**文件**: `semantic/mod.rs:212-218`
遍历所有定义找包含 offset 的 innermost function/method。
**修**: 同上，空间索引。

### P6. Completion handler 每次复制完整源码
**文件**: `lsp.rs:627`
`analysis.parse(...).text().to_string()` 克隆整个文件，每次补全请求一次。
**修**: 缓存 LineIndex 或使用 Arc<str> 引用。

### P7. `resolve_variant_field` O(n) 扫描
**文件**: `member.rs:428-435`
遍历所有 enum 的所有 variant 找匹配 DefId。
**修**: 加 `BTreeMap<DefId, &VariantTemplate>` 索引。

### P8. Binding 去重 O(n²)
**文件**: `scope.rs:358-369`
`valid[..index].iter().any(|prev| ...)` 嵌套循环。
**修**: 使用 `HashSet<&str>`。

---

## 🔵 Maintenance（代码重复和维护风险）

### M1. hover/goto-def preamble 重复 ~30 行
**文件**: `ide/mod.rs:292-308, 414-434`
提取 `resolve_member_access()`。

### M2. Request extraction 模式重复 30 次
**文件**: `lsp.rs:~202-2774`
每 handler 都有 10 行相同的 extract+error 模板。用宏消除。

### M3. Hierarchy item 转换重复 6 次
**文件**: `lsp.rs:1839-2190`
提取公共转换函数。

### M4. "File not found → return empty" 模式重复 11 次
**文件**: `lsp.rs:303-2779`
提取 `file_id_or_empty!()` 宏。

### M5. WorkspaceEdit 构造重复 12+ 次
**文件**: `lsp.rs:1038-1788`
提取 `single_file_edit(uri, edits)` 辅助函数。

### M6. `item_hover_text` 和 `definition_signature` 重复
**文件**: `ide/mod.rs:926-968` 和 `completion.rs:1246-1273`
统一为单个签名格式化函数。

### M7. 77 处 `let _ = self.connection.sender.send(...)` 
**文件**: `lsp.rs` 全域
单次检查或 early-return 模式可避免丢弃发送错误。

### M8. `usize → u32` 截断 50+ 处
**文件**: `lsp.rs` 全域
>4GB 文件会截断偏移量。虽然源码通常不大但技术上不严谨。

### M9. `watched_paths` 无界增长
**文件**: `lsp.rs:3004, 3016`
每次配置重载追加而不去重或清除旧项，内存泄漏。

### M10. 两个 DiagnosticCode 变体永远不被分配
**文件**: `diagnostic/mod.rs:27,29`（ParseUnterminatedComment, ParseMissingDelimiter）
死代码。

### M11. `TypeMismatchContext::Argument { index }` 丢弃 index
**文件**: `diagnostic/mod.rs:677`
类型不匹配消息只写 " in argument"，扔掉了参数位置信息。

### M12. `goto_definition` 和 `member_goto_definition` 各自获取 def_map
**文件**: `ide/mod.rs:388-389, 437-438`
同一请求内获取两次 `self.db.def_map(...)`。

---

## ⚪ Feature Gap

### FG1. 0 个集成测试覆盖 rename, references, semantic_tokens, document_symbols, workspace_symbols, folding, code_action, call_hierarchy, type_hierarchy
**文件**: `incremental_stress.rs`
38 个测试中：completions 45 引用，hover 16，goto_def 6，signature_help 9。7+ 个功能完全没有集成测试。
**修**: 为每个 handler 添加至少 1 个集成测试。

### FG2. 只有 W0300 和 E0206 有 quickfix
**文件**: `lsp.rs:1018-1090`
其他诊断代码全部落到 `_ => {}`。parse error 没有 quickfix，type error 多数没有。
**修**: 为常见诊断添加 quickfix（missing field、type mismatch suggest cast 等）。

### FG3. CodeLens command 字段为空字符串
**文件**: `lsp.rs:441`
VS Code 将其渲染为不可点击的 code lens。
**修**: 设为 None（纯显示）或提供有效命令。

### FG4. Completion `is_incomplete` 阈值硬编码 100
**文件**: `lsp.rs:640`
没有 `all_commit_characters` 支持，客户端不知道哪些字符触发 commit vs re-query。
**修**: 注册 commit_characters 或设为 is_incomplete: false。

### FG5. 补全不感知 impl block 的 self type
在 `impl Point { fn | }` 中应补全 trait 方法签名，当前只补全关键字。
**修**: 检测 impl block 上下文，提供 trait 方法的 snippet。

### FG6. Inlay hints 只覆盖 let binding
没有闭包参数类型标注（`|x, y|` → `|x: i64, y: i64|`）、方法链中间类型、函数返回类型。
**修**: 扩展 inlay hint 到闭包参数和 return type。

### FG7. "Extract function" 硬编码函数名 `"extracted"`
**文件**: `lsp.rs:1498`
**修**: 从上下文推断有意义的函数名。

### FG8. 补全 relevance 无法表达 "类型匹配" 权重
当补全的字段类型与期望类型匹配时，应自动提升排名。当前硬编码整数无法实现。

---

## 优先修复顺序

### 第一优先（本周修）— 影响用户可见行为
1. **B1**: signature_help 对方法调用失效
2. **B7**: goto-def 比 hover 能力弱（提取公共函数）
3. **B4**: Code action 无选区时不可用
4. **B8**: self 参数误报 unused variable

### 第二优先（下周修）— 影响正确性但边界情况
5. **B2**: 多文件 rename 返回假 URI
6. **B5/B6**: trait name substring match
7. **B9**: unreachable code lint 误报
8. **B10**: handle_did_save 传错参数

### 第三优先（技术债）
9. **M1**: 提取 hover/goto-def 公共函数（先修 B7）
10. **P1-P5**: 构建索引改善性能
11. **FG1**: 为未测试的 handler 添加集成测试

### 第四优先（代码清洁）
12. **M2-M5**: 消除 LSP handler 重复模板（宏 + 辅助函数）
13. **M8**: usize → u32 截断统一处理
14. **M9**: watched_paths 内存泄漏
