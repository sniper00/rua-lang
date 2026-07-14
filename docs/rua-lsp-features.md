# Rua LSP 功能

`rua-lsp` 是 protocol adapter；语法、名称解析、类型推断和引用索引由长期 `rua-analysis::AnalysisHost` 提供。功能按当前 server capability 与端到端测试维护，不按历史阶段编号。

## 1. 语言理解

| 能力 | 当前行为 |
|---|---|
| Hover | 显示 item、local、field、method、enum variant 和 builtin macro 的签名、类型与 API 文档 |
| Definition | 导航到 local、module item、member、associated item、`.ruai` declaration 和 enum variant |
| Implementation | 从 trait / trait method 定位具体 impl |
| References | 基于 resolved identity 跨文件查找，不按同名文本匹配 |
| Rename | 先验证全部目标，再原子生成 workspace edit；readonly library 或冲突时整体拒绝 |
| Call hierarchy | prepare、incoming 和 outgoing calls |
| Symbols | document symbol tree 与 workspace symbol search |
| Highlight | 区分当前符号的 read/write occurrence |

enum variant 在 declaration、constructor、qualified path、alias 和 match pattern 中使用同一 identity。function、method、trait item、extern、`.ruai` 和 `vec!` / `print!` / `println!` / `format!` / `panic!` 的文档来自统一 semantic record。

## 2. Completion 与签名

- lexical local、module item、keyword、builtin、field、method、associated item 和 enum variant completion。
- `.` member、`::` path、match/pattern、postfix 和关键字 snippet 上下文。
- expected type 与作用域相关性排序、前缀 text edit、function 参数占位和 `self` 过滤。
- completion item resolve 按需提供文档。
- signature help 支持 `(`、`,` 触发与重触发。

unknown member receiver 会返回空的 member context，避免在 `.` 后混入全局关键字噪音。

## 3. Diagnostics 与语义显示

| 能力 | 当前行为 |
|---|---|
| Diagnostics | parser、name resolution、type checking、trait/module、`.ruai` 和 control-flow diagnostics |
| Lints | unused variable、redundant `mut`、unreachable code、unused function、infinite loop |
| Semantic tokens | full 与 range token，包含 declaration、readonly、static、async 和 unused modifier |
| Inlay hints | inferred binding type、parameter name、closure parameter/return 和 iterator chain type |
| Code lens | definition reference count 与 type/trait implementation 信息 |

diagnostic 使用稳定 code 和精确 source range；human message 不作为协议分类依据。打开文档发布带 version 的结果，过期 snapshot 不会覆盖新诊断。

## 4. 编辑与重构

| 协议能力 | 说明 |
|---|---|
| Full formatting | 格式化完整文档 |
| Range formatting | 只格式化选定范围 |
| On-type formatting | Enter 后延续 doc comment 或处理缩进 |
| Selection range | 按 syntax tree 扩大选择范围 |
| Folding range | block、item 和 doc comment folding |
| Document link | 解析文档注释中的可导航引用 |
| Code actions | fill match arms、generate trait impl members、extract variable、if-let 转 match |

formatter 写文件时使用临时文件与原子替换；LSP formatting 返回 text edit，不直接修改 editor buffer。

## 5. Workspace 行为

- 支持 multi-root workspace、ad-hoc opened file、readonly `.ruai` library 和 logical library mount。
- `didOpen` / `didChange` / `didClose` 保持 disk 与 overlay 分离，并拒绝倒退的 document version。
- library configuration 与 watcher 更新按 project 隔离；删除 project/root 会回收 mapping 与 cache。
- scan 识别 `.gitignore`、`.ignore`、`.ruaignore`，默认跳过 `.git`、`target` 和 `node_modules`。
- references、workspace symbol 和扫描任务可取消；stale generation 返回 `ContentModified`。
- 所有 LSP position 使用 UTF-16，语义引擎内部保持 UTF-8 byte range。

VS Code 配置和启动方式见[扩展说明](../editors/vscode/README.md)。

## 6. Advertised protocol capabilities

server initialize 当前声明：

- full text synchronization
- completion + completion resolve
- hover, definition, implementation, references, prepare rename, rename
- document/workspace symbol, document highlight, document link
- call hierarchy, code lens
- diagnostics publication
- semantic tokens full/range, inlay hints, signature help
- formatting, range formatting, on-type formatting
- code actions, folding range, selection range

协议生命周期、取消、multi-root 和真实编辑器启动由 `crates/rua-lsp/tests/` 与 `editors/vscode/src/test/` 覆盖。
