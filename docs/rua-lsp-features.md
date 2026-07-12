# Rua LSP 功能清单

> 状态：S 级 + A 级 + B 级 + C 级 全部完成 ✅
> 最后更新：2026-07-12

本文档按 LSP 协议维度列出 Rua LSP 已实现的全部功能，作为 `rua-design.md` §8.1 P5e-C 的补充。

## 1. 补全 (textDocument/completion)

| 功能 | 说明 | 实现位置 |
|------|------|---------|
| 作用域补全 | locals + module items + keywords + builtins | `completion.rs:scope_completions` |
| 成员补全 | `.` 后字段/方法，含类型推断 | `completion.rs:member_completions` |
| 路径补全 | `::` 后模块成员 + enum variant | `completion.rs:path_completions` |
| sortText 排序 | 按 relevance 降序：locals(95) > members(90) > module(85) > keywords(50) > builtins(20-40) | `lsp.rs:completion_to_lsp` |
| textEdit 前缀替换 | 将 partial_range 转换为 LSP TextEdit | `lsp.rs:completion_to_lsp` |
| filterText 宏过滤 | 宏补全 lookup 不含 `!`，输入 `println` 匹配 `println!` | `completion.rs` / `lsp.rs` |
| 关键字上下文过滤 | 表达式位置（`=`, `return`, `(`, 运算符后）抑制 `fn`/`struct`/`enum` 等声明关键字 | `completion.rs:is_expression_context` |
| 类型位置过滤 | `:` 后只显示类型名（struct/enum/trait/builtins） | `completion.rs:is_type_position` |
| snippet 参数占位 | 函数补全生成 `${1:dx: i64}, ${2:dy: i64}$0` | `lsp.rs:completion_to_lsp` |
| 参数名 | 从 `CallableSignature::params()` 提取原始参数名 | `completion.rs:member_completions` |
| self 参数处理 | detail 显示 `&mut self`，snippet 不含 self | `completion.rs:member_completions` |
| 文档注释 | 提取 `///` 前导注释 → `CompletionItem.documentation` | `completion.rs:extract_doc_comment` |
| label_details | 结构化展示类型 + 来源描述（`fn`/`struct`/`method`/`local`） | `lsp.rs:completion_to_lsp` |
| deprecated 标签 | 接线 `CompletionItemTag::DEPRECATED` | `lsp.rs:completion_to_lsp` |
| isIncomplete | >100 项时返回 `CompletionList { is_incomplete: true }` | `lsp.rs:handle_completion` |
| resolve provider | 注册 `completionItem/resolve`，按需懒加载文档 | `lsp.rs:handle_resolve_completion` |
| match enum 补全 | `match c {` 内自动提示 enum variant | `completion.rs:match_scrutinee_enum` |
| postfix 补全 | `.if` `.match` `.not` `.ref` `.while` 模板展开 | `completion.rs:postfix_templates` |
| 关键字 snippet | `for`/`match`/`fn`/`struct` 等 11 个关键字展开为代码模板 | `completion.rs:keyword_snippet` |
| 服务端前缀过滤 | 按已输入文本做 subsequence 模糊匹配，纯数字前缀不触发 | `completion.rs:is_subsequence` |
| 类型匹配加权 | 推断光标处期望类型，匹配候选项 relevance +10 | `completion.rs:expected_type_at_cursor` |
| 触发字符 | `.` `:` | `lsp.rs:CompletionOptions` |

## 2. 导航

| 功能 | 协议 | 说明 |
|------|------|------|
| Go to Definition | `textDocument/definition` | 条目定义 + 局部变量定义 |
| Go to Implementation | `textDocument/implementation` | trait method → 所有 impl 块中的具体实现 |
| References | `textDocument/references` | 局部变量引用查找 |
| Document Symbol | `textDocument/documentSymbol` | 文档大纲树 |
| Workspace Symbol | `workspace/symbol` | 跨文件模糊搜索符号（上限 50） |
| Prepare Rename | `textDocument/prepareRename` | 检查光标位置是否可重命名 |

## 3. 悬停

| 功能 | 说明 |
|------|------|
| 条目 hover | markdown 代码块 + 签名 |
| 局部变量 hover | 显示推断类型 `let name: Ty` |
| 成员 hover | `receiver.field` / `receiver.method()` 类型 |
| 文档注释 | `///` 文本在 hover 中渲染为 markdown |
| 导航提示 | hover 底部显示 `F12 Go to Definition · Shift+F12 Find References` |

## 4. 诊断

| 功能 | 代码 | 说明 |
|------|------|------|
| Parse errors | E0001–E0005 | 解析错误（语法） |
| Name errors | E0100–E0104 | 未解析名称、重复定义等 |
| Type errors | E0200–E0210 | 类型不匹配、参数数量、trait bound 等 |
| Unused variable | W0300 | 未使用局部变量警告 |

## 5. 编辑

| 功能 | 协议 | 说明 |
|------|------|------|
| Formatting | `textDocument/formatting` | 全文格式化 |
| On-Type Formatting | `textDocument/onTypeFormatting` | Enter 续行 `/// ` + `{` 后缩进 |
| Rename | `textDocument/rename` | 局部变量重命名 |

## 6. 代码操作 (Code Actions)

| 功能 | 触发条件 | 说明 |
|------|---------|------|
| Fill match arms | 光标在 match 表达式上 | 生成缺失 enum variant 分支 |
| Generate impl members | 光标在 `impl Trait for Type {}` 上 | 生成所有 trait 方法 stub |
| Extract variable | 选中表达式 | 提取为 `let` 绑定 |
| Replace if-let with match | 光标在 `if let` 表达式上 | 转换为 `match` 表达式 |

## 7. 语义增强

| 功能 | 协议 | 说明 |
|------|------|------|
| Semantic Tokens | `textDocument/semanticTokens/full` | 语法高亮（16 token types + 5 modifiers 含 unused） |
| Inlay Hints | `textDocument/inlayHint` | `let` 绑定类型标注 |
| Document Highlight | `textDocument/documentHighlight` | 光标选中符号高亮所有出现 |
| Signature Help | `textDocument/signatureHelp` | 参数提示，触发字符 `(` `,` |
| Folding Range | `textDocument/foldingRange` | `{...}` 块 + `///` 注释块折叠 |
| Document Link | `textDocument/documentLink` | `/// ` 中 `[...]` 引用创建可点击链接 |

## 8. 工程

| 功能 | 说明 |
|------|------|
| 文件监视 | `workspace/didChangeWatchedFiles` 监听库文件 |
| 增量更新 | `didChange` 全量重建快照 |
| 快照隔离 | `Analysis` 快照不受后续变更影响 |
| 多文件 workspace | `WorkspaceFormat` 索引所有 `.rua`/`.ruai` |

## 9. LSP 协议版本

支持的 LSP 3.17 能力：
- `textDocumentSync` (Full)
- `completionProvider` (trigger: `.` `:`, resolve: true)
- `hoverProvider`
- `definitionProvider`
- `implementationProvider`
- `referencesProvider`
- `documentSymbolProvider`
- `workspaceSymbolProvider`
- `documentFormattingProvider`
- `documentOnTypeFormattingProvider` (trigger: `\n`)
- `documentHighlightProvider`
- `documentLinkProvider`
- `foldingRangeProvider`
- `renameProvider` (prepare: true)
- `signatureHelpProvider` (trigger: `(` `,`)
- `inlayHintProvider`
- `codeActionProvider`
- `semanticTokensProvider` (full)
