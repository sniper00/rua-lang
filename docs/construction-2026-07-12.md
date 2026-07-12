# 施工文档：Rua 代码质量改进

> **日期**: 2026-07-12
> **基于**: `docs/code-review-2026-07-12.md` 复验结果
> **目标**: 分阶段修复代码审查中发现的问题，消除冗余，提升可维护性

---

## 目录

- [一、总体策略](#一总体策略)
- [二、第1阶段：快速胜利（1天，6项）](#二第1阶段快速胜利1天6项)
- [三、第2阶段：LSP 层重构（2天，4项）](#三第2阶段lsp-层重构2天4项)
- [四、第3阶段：Completion 体系重构（2天，4项）](#四第3阶段completion-体系重构2天4项)
- [五、第4阶段：Infer 引擎整理（2天，5项）](#五第4阶段infer-引擎整理2天5项)
- [六、第5阶段：DefMap + Diagnostic 优化（2天，4项）](#六第5阶段defmap--diagnostic-优化2天4项)
- [七、第6阶段：架构级改进（按需）](#七第6阶段架构级改进按需)
- [八、测试策略](#八测试策略)
- [九、风险与回滚](#九风险与回滚)

---

## 一、总体策略

### 原则

1. **每项改动独立提交** — 一个 commit 只做一件事，方便 review 和 bisect
2. **测试先行** — 涉及行为变更的先写测试（红→绿），纯重构的可后补
3. **不影响外部行为** — 重构不改变 LSP 响应、补全结果、类型推断结果
4. **低风险优先** — 先做机械性消除冗余，再做结构性重组

### 预期总收益

| 维度 | 改进量 |
|------|--------|
| 删除冗余代码 | ~630 行 |
| 消除 magic number | 14 个 → 0 |
| 消除重复函数/模式 | 20+ 处 |
| 减少 handler 模板 | 30 处 → 2-8 行宏调用 |
| 提升补全可测试性 | 8 个独立函数可分别测试 |

---

## 二、第1阶段：快速胜利（1天，6项）

> 目标：用最小改动修复最明显的 bug 和冗余。每项 ≤30 分钟。

### 任务 1.1 · 修复 `ensure_file_id_for_path` fallback URI bug

- **文件**: `crates/rua-lsp/src/lsp.rs`
- **行号**: L3148-3158
- **问题**: `format!("file:///unknown/{}", self.next_file_id)` 使用了递增后的值（已是 id+1）
- **修复**:

```rust
// 修复前 (L3148-3151):
let id = FileId::new(self.next_file_id);
self.next_file_id += 1;
let uri = path_to_uri(path).unwrap_or_else(|| {
    format!("file:///unknown/{}", self.next_file_id)  // BUG: id+1

// 修复后:
let id = FileId::new(self.next_file_id);
self.next_file_id += 1;
let uri = path_to_uri(path).unwrap_or_else(|| {
    format!("file:///unknown/{}", id.0)  // 使用正确的 id
```

- **验证**: 构造一个 path 无法转换为 URI 的场景，检查 fallback URI 中的数字是否与 FileId 一致
- **风险**: 零（纯 bug fix）

---

### 任务 1.2 · 删除 `close_document` 不必要 clone

- **文件**: `crates/rua-lsp/src/lsp.rs`
- **行号**: L2960
- **问题**: `.map(|(u, f)| (u.clone(), *f))` 中 `u` 的 clone 未被使用
- **修复**:

```rust
// 修复前:
if let Some((_, file_id)) = self.file_ids.get(&key).map(|(u, f)| (u.clone(), *f)) {

// 修复后:
if let Some((_, file_id)) = self.file_ids.get(&key).map(|(_, f)| *f) {
```

- **验证**: 编译通过即可（语义等价）
- **风险**: 零

---

### 任务 1.3 · 重命名 `_bid` → `bid`

- **文件**: `crates/rua-analysis/src/ide/mod.rs`
- **行号**: L508
- **问题**: `_bid` 前缀暗示 unused，但紧接着在 L510 使用
- **修复**:

```rust
// 修复前:
for (_bid, binding) in body.bindings() {
    if binding.name() == Some(&receiver_name) {
        return inference.type_of_binding(_bid).cloned();

// 修复后:
for (bid, binding) in body.bindings() {
    if binding.name() == Some(&receiver_name) {
        return inference.type_of_binding(bid).cloned();
```

- **验证**: 编译通过即可
- **风险**: 零

---

### 任务 1.4 · 删除未使用的 enum 变体

- **文件**: `crates/rua-analysis/src/diagnostic/mod.rs`
- **行号**: L145-146 (`DiagnosticSeverity::Information`, `DiagnosticSeverity::Hint`), L176, L178 (`DiagnosticSource::Name`, `DiagnosticSource::Structural`)
- **问题**: 4 个变体定义但从未赋值/匹配
- **修复**: 删除这 4 个变体，检查所有 `match` 是否仍然 exhaustive

```rust
// 修复前:
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,  // 删除
    Hint,         // 删除
}

pub enum DiagnosticSource {
    Parse,
    Name,         // 删除
    Type,
    Structural,   // 删除
}
```

- **验证**: `cargo build -p rua-analysis` 确认无编译错误
- **风险**: 低（需确认无下游 crate 使用这些变体）

---

### 任务 1.5 · 移除未使用参数 `_token` 和 `_file_id`

- **文件 1**: `crates/rua-analysis/src/ide/completion.rs` L509 — `_token: Option<&SyntaxToken>`
- **文件 2**: `crates/rua-analysis/src/diagnostic/mod.rs` L687 — `_file_id`
- **修复**: 删除未使用参数，更新所有调用点

```rust
// completion.rs: 检查 member_completions 签名和所有调用点
fn member_completions(
    db: &Rc<BaseDb>,
    position: FilePosition,
    // _token: Option<&SyntaxToken>,  // 删除
) -> Vec<CompletionItem> {
```

- **验证**: 编译 + 现有测试通过
- **风险**: 低

---

### 任务 1.6 · 修复 `member_goto_definition` 惰性求值

- **文件**: `crates/rua-analysis/src/ide/mod.rs`
- **行号**: L442-444
- **问题**: `field.or(method)` 两个方法都已急切求值
- **修复**:

```rust
// 修复前:
let field = member_index.resolve_field(&receiver_ty, &field_name);
let method = member_index.resolve_method(&receiver_ty, &field_name);
let resolution = field.or(method)?;

// 修复后:
let field = member_index.resolve_field(&receiver_ty, &field_name);
let resolution = field.or_else(|| {
    member_index.resolve_method(&receiver_ty, &field_name)
})?;
```

- **验证**: 现有 goto-def 测试通过
- **风险**: 低（纯性能优化，行为不变）

---

## 三、第2阶段：LSP 层重构（2天，4项）

> 目标：消除 lsp.rs 中 ~500 行重复模板，统一 handler 模式。

### 任务 2.1 · 引入 `handle_position_request!` 和 `handle_doc_request!` 宏

- **文件**: `crates/rua-lsp/src/lsp.rs`
- **影响**: 28 个 handler
- **方案**: 参照审查文档 R1 方案

**实施步骤**:

1. 在 `lsp.rs` 顶部（use 语句之后）添加两个 macro：

```rust
/// 位置请求分发：hover, goto-def, references, prepare-rename, etc.
/// 统一 extract → project_position → call → respond 流程。
macro_rules! handle_position_request {
    ($self:ident, $req:ident, $Params:ty, |$pp:ident, $analysis:ident| $body:expr) => {{
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
            let $analysis = $self.host.analysis();
            $body(pp, $analysis)
        });
        let resp = Response::new_ok(id, result);
        let _ = $self.connection.sender.send(Message::Response(resp));
    }};
}

/// 文档请求分发：completion, inlay-hint, semantic-tokens, folding, etc.
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

2. 逐个迁移 handler。以 `handle_hover` 为例：

```rust
// 迁移前（~30 行）:
fn handle_hover(&mut self, req: Request) {
    let id = req.id.clone();
    let (id, params) = match req.extract::<lsp_types::HoverParams>(HoverRequest::METHOD) {
        Ok(v) => v,
        Err(e) => {
            let resp = Response::new_err(
                id,
                lsp_server::ErrorCode::InvalidParams as i32,
                format!("invalid hover params: {e:?}"),
            );
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        }
    };
    let uri = &params.text_document_position_params.text_document.uri;
    let pos = params.text_document_position_params.position;
    let result = self.project_position(uri, pos).and_then(|pp| {
        let analysis = self.host.analysis();
        analysis.hover(pp).map(|hover| to_lsp_hover(&hover))
    });
    let resp = Response::new_ok(id, result);
    let _ = self.connection.sender.send(Message::Response(resp));
}

// 迁移后（6 行）:
fn handle_hover(&mut self, req: Request) {
    handle_position_request!(self, req, lsp_types::HoverParams, |pp, analysis| {
        analysis.hover(pp).map(|hover| to_lsp_hover(&hover))
    });
}
```

3. 每迁移一个 handler 后运行测试确认无回归

**覆盖范围**: 以下 handler 可使用 `handle_position_request!`:
- `handle_hover`
- `handle_definition`
- `handle_goto_implementation`
- `handle_references`
- `handle_prepare_rename`
- `handle_rename`
- `handle_document_highlight`
- `handle_signature_help`
- `handle_code_lens`
- `handle_document_link`
- `handle_call_hierarchy_prepare/incoming/outgoing`
- `handle_type_hierarchy_prepare/subtypes/supertypes`

以下可使用 `handle_doc_request!`:
- `handle_completion`
- `handle_inlay_hint`
- `handle_semantic_tokens_full/range`
- `handle_folding_range`
- `handle_selection_range`
- `handle_document_symbol`
- `handle_code_action`
- `handle_formatting/range_formatting/on_type_formatting`

- **验证**: `cargo test -p rua-lsp` 全部通过
- **风险**: 中（macro hygiene 需要仔细验证，建议逐 handler 迁移逐测试）
- **收益**: 消除 ~400 行重复

---

### 任务 2.2 · 引入 `extract_notification!` 宏

- **文件**: `crates/rua-lsp/src/lsp.rs`
- **行号**: L2863-2937
- **方案**: 参照审查文档 R12

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

// 使用:
fn handle_notification(&mut self, not: Notification) {
    match not.method.as_str() {
        DidOpenTextDocument::METHOD => {
            extract_notification!(not, lsp_types::DidOpenTextDocumentParams, "didOpen", |p| {
                self.open_document(p.text_document.uri, p.text_document.text);
            });
        }
        DidChangeTextDocument::METHOD => {
            extract_notification!(not, lsp_types::DidChangeTextDocumentParams, "didChange", |p| {
                if let Some(change) = p.content_changes.last() {
                    self.change_document(p.text_document.uri, change.text.clone());
                }
            });
        }
        // ... 其余 4 个 arm 类似
    }
}
```

- **验证**: 手动测试 didOpen/didChange/didClose/didSave 流程
- **风险**: 低
- **收益**: 消除 ~60 行重复

---

### 任务 2.3 · 提取 `register_watchers` 公共循环

- **文件**: `crates/rua-lsp/src/lsp.rs`
- **行号**: L3042-3071
- **问题**: `library_roots` 和 `library_mounts` 循环完全相同的 watcher 注册逻辑

```rust
fn try_add_watcher(
    path: &Path,
    watched_paths: &mut Vec<PathBuf>,
    watchers: &mut Vec<FileSystemWatcher>,
) {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        let glob = canonical.to_string_lossy().to_string();
        if !watched_paths.iter().any(|p| p.to_string_lossy() == glob) {
            watchers.push(FileSystemWatcher {
                glob_pattern: lsp_types::GlobPattern::String(glob.clone()),
                kind: Some(WatchKind::all()),
            });
            watched_paths.push(PathBuf::from(&glob));
        }
    }
}

// 简化为:
for root in &self.library_roots {
    let pattern = if root.is_dir() { root.join("**/*.ruai") } else { root.clone() };
    try_add_watcher(&pattern, &mut self.watched_paths, &mut watchers);
}
for mount_path in self.library_mounts.values() {
    try_add_watcher(mount_path, &mut self.watched_paths, &mut watchers);
}
```

- **验证**: 编译通过
- **风险**: 零
- **收益**: 消除 ~15 行重复

---

### 任务 2.4 · 统一 `ensure_file_id` / `ensure_file_id_for_path` 双重入口

- **文件**: `crates/rua-lsp/src/lsp.rs`
- **行号**: L117 (`ensure_file_id`) + L3144 (`ensure_file_id_for_path`)
- **问题**: 两个函数做几乎相同的事，可能导致同一文件产生重复 FileId
- **方案**: 将 `ensure_file_id_for_path` 改为内部调用 `ensure_file_id`：

```rust
fn ensure_file_id_for_path(&mut self, path: &Path) -> FileId {
    // 如果已有映射，直接返回
    if let Some((_, id)) = self.file_ids.get(path) {
        return *id;
    }
    // 否则通过 URI 路径创建
    let uri = path_to_uri(path).unwrap_or_else(|| {
        let id = self.next_file_id;
        format!("file:///unknown/{id}")
            .parse()
            .unwrap_or_else(|_| "file:///unknown.rua".parse().unwrap())
    });
    self.ensure_file_id(&uri)
}
```

- **验证**: 文件打开/关闭/变更流程测试
- **风险**: 中（涉及文件身份管理，需仔细测试）
- **收益**: 消除重复 FileId 条目的可能

---

## 四、第3阶段：Completion 体系重构（2天，4项）

> 目标：建立可扩展的补全体系，消除 magic number 和 token-based 检测。

### 任务 3.1 · 引入 `CompletionRelevance` 结构体

- **文件**: `crates/rua-analysis/src/ide/completion.rs`
- **方案**: 参照审查文档 R2 方案

**实施步骤**:

1. 在 `completion.rs` 或新文件 `crates/rua-analysis/src/ide/completion_relevance.rs` 中添加：

```rust
/// 补全相关度评分，参照 rust-analyzer 的 CompletionRelevance。
/// 各子分数通过 score() 组合，而非硬编码整数。
#[derive(Debug, Clone, Copy, Default)]
pub struct CompletionRelevance {
    pub base: u8,
    pub exact_type_match: bool,
    pub type_name_match: bool,
    pub is_local: bool,
    pub is_from_this_crate: bool,
    pub is_deprecated: bool,
}

impl CompletionRelevance {
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

    pub fn with_exact_type_match(mut self, matches: bool) -> Self {
        self.exact_type_match = matches;
        self
    }
    pub fn with_deprecated(mut self, deprecated: bool) -> Self {
        self.is_deprecated = deprecated;
        self
    }

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

2. 修改 `CompletionItem.relevance` 字段类型：`u16` → `CompletionRelevance`
3. 在 `scope_completions()` 中搜索 `.with_relevance(N)` 替换为 `.with_relevance(CompletionRelevance::xxx())`
4. 排序时使用 `.score()` 进行比较

- **验证**: 补全测试全部通过，补全排序不变
- **风险**: 中（影响补全排序）
- **收益**: 消除 14 个 magic number，新增补全类别时有明确指导

---

### 任务 3.2 · 引入 `CompletionContext` 结构体

- **文件**: `crates/rua-analysis/src/ide/completion.rs`
- **方案**: 参照审查文档 R4

```rust
pub(crate) struct CompletionContext<'a> {
    pub db: &'a BaseDb,
    pub position: FilePosition,
    pub offset: u32,
    pub def_map: &'a DefMap,
    pub token: SyntaxToken,

    // 从 AST 推导的上下文标记
    pub in_type_position: bool,
    pub in_expression_position: bool,
    pub in_pattern_position: bool,
    pub in_method_body: bool,
    pub in_impl_block: bool,
    pub in_loop: bool,

    // 从 inference 推导
    pub expected_type: Option<Ty>,
}

impl<'a> CompletionContext<'a> {
    pub fn new(db: &'a BaseDb, position: FilePosition, offset: u32) -> Option<Self> {
        let parse = db.parse(position.file_id);
        let root = parse.syntax_node();
        let token = token_at_offset(&root, offset)?;

        let mut ctx = Self {
            db, position, offset,
            def_map: db.def_map(position.file_id),
            token,
            in_type_position: false,
            in_expression_position: false,
            in_pattern_position: false,
            in_method_body: false,
            in_impl_block: false,
            in_loop: false,
            expected_type: None,
        };

        // 从 token 向上遍历 AST，确定上下文
        let mut node = ctx.token.parent();
        while let Some(current) = node {
            match current.kind() {
                SyntaxKind::TypeClause
                | SyntaxKind::ParamList
                | SyntaxKind::FieldDecl => ctx.in_type_position = true,
                SyntaxKind::FnBody
                | SyntaxKind::BlockExpr => ctx.in_expression_position = true,
                SyntaxKind::LetPat
                | SyntaxKind::MatchArmPat => ctx.in_pattern_position = true,
                SyntaxKind::ImplBlock => ctx.in_impl_block = true,
                SyntaxKind::WhileExpr
                | SyntaxKind::LoopExpr
                | SyntaxKind::ForExpr => ctx.in_loop = true,
                _ => {}
            }
            node = current.parent();
        }

        Some(ctx)
    }
}
```

- **迁移**: `scope_completions()` / `member_completions()` / `path_completions()` 签名从多参数改为接收 `&CompletionContext`
- **删除**: `is_type_position()` 和 `is_expression_context()` token-based 函数
- **验证**: 补全测试全部通过
- **风险**: 中高（涉及补全上下文判断逻辑变更）
- **收益**: 补全质量提升，消除 token 检测盲区

---

### 任务 3.3 · 拆分 `scope_completions()` 为独立 `complete_*` 函数

- **文件**: `crates/rua-analysis/src/ide/completion.rs`
- **方案**: 参照审查文档 R5

```rust
type CompleteFn = fn(&CompletionContext, &mut Vec<CompletionItem>, &mut HashSet<String>);

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

pub(crate) fn scope_completions(ctx: &CompletionContext) -> Vec<CompletionItem> {
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

**拆分出的独立函数**:
- `complete_keywords()`
- `complete_snippets()`
- `complete_locals()`
- `complete_module_items()`
- `complete_cross_module_items()`
- `complete_builtin_types()`
- `complete_builtin_constructors()`
- `complete_builtin_macros()`
- `complete_match_variants()` (已存在，迁移)
- `complete_struct_fields()` (已存在，迁移)
- `complete_iflet_variants()` (已存在，迁移)

- **验证**: 每个 `complete_*` 函数可独立测试
- **风险**: 中（大量代码移动，需确保逻辑不变）
- **收益**: 每种补全可独立测试和修改

---

### 任务 3.4 · 合并 `BodyData`/`BodyFullData` + 去重 `find_containing_body_data` 模式

- **文件**: `crates/rua-analysis/src/ide/completion.rs`
- **行号**: L1165-1198
- **方案**:

```rust
// 统一为一个类型，缺少的可选字段用 None
pub(crate) struct BodyContext {
    pub body: Arc<Body>,
    pub source_map: Arc<BodySourceMap>,
    pub scopes: Option<Arc<BodyScopes>>,
    pub inference: Option<Arc<InferenceResult>>,
}

pub(crate) fn find_containing_body(
    db: &BaseDb,
    def_map: &DefMap,
    position: FilePosition,
    offset: u32,
) -> Option<BodyContext> {
    let owner = innermost_body_owner(def_map, position, offset)?;
    Some(BodyContext {
        body: db.body(owner.id())?,
        source_map: db.body_source_map(owner.id())?,
        scopes: db.body_scopes(owner.id()),
        inference: db.infer(owner.id()),
    })
}
```

- **验证**: 补全 + hover + goto-def 测试通过
- **风险**: 低
- **收益**: 消除两个几乎相同的类型和两个查找函数

---

## 五、第4阶段：Infer 引擎整理（2天，5项）

> 目标：拆分大函数，消除重复模式，提升可读性。

### 任务 4.1 · 引入 `diverge_or` helper

- **文件**: `crates/rua-analysis/src/hir/infer.rs`
- **问题**: "diverges" 模式重复 15+ 次
- **方案**: 参照审查文档 R7

```rust
/// 如果表达式分支发散，整个表达式类型为 Never；否则返回实际类型。
#[inline]
fn diverge_or(diverges: bool, ty: Ty) -> Ty {
    if diverges { Ty::Never } else { ty }
}

// 迁移示例:
// 修复前:
let diverges = condition_diverges && then_diverges && else_diverges;
if diverges { Ty::Never } else { actual_ty }

// 修复后:
diverge_or(diverges, actual_ty)
```

- **验证**: 类型推断测试全部通过
- **风险**: 零（纯重构）
- **收益**: 消除 ~30 行重复，语义更清晰

---

### 任务 4.2 · 拆分 `infer_expr` 的 If/Assign/Range/Try/Index arms

- **文件**: `crates/rua-analysis/src/hir/infer.rs`
- **行号**: L272-434
- **方案**: 参照审查文档 R6。将以下 arms 提取为独立方法：

```rust
fn infer_expr(&mut self, expr_id: ExprId, expected: Option<&Ty>) -> Ty {
    let Some(expr) = self.body.expr(expr_id).cloned() else {
        return Ty::Unknown;
    };
    match expr {
        // 已委托
        Expr::Block(body)            => self.infer_block(*body, expected),
        Expr::Match(scrutinee, arms) => self.infer_match(arms, ...),
        Expr::Call(call)             => self.infer_call(expr_id, call, expected),
        Expr::MethodCall(call)       => self.infer_method_call(expr_id, call, expected),
        Expr::Closure(closure)       => self.infer_closure(expr_id, closure, expected),
        Expr::StructLiteral(lit)     => self.infer_struct_literal(expr_id, lit, expected),
        Expr::Unary(op, inner)       => self.infer_unary(expr_id, *op, *inner),
        Expr::Binary(lhs, op, rhs)   => self.infer_binary(expr_id, *lhs, *op, *rhs),

        // 新提取
        Expr::If(cond, then_b, else_b) => self.infer_if_expr(expr_id, *cond, *then_b, else_b.as_ref(), expected),
        Expr::Assign(target, value)    => self.infer_assign_expr(expr_id, *target, *value),
        Expr::Range(start, end)        => self.infer_range_expr(expr_id, *start, *end, expected),
        Expr::Try(inner)               => self.infer_try_expr(expr_id, *inner, expected),
        Expr::Index(base, index)       => self.infer_index_expr(expr_id, *base, *index, expected),

        // 已在独立方法中
        Expr::Path(path)               => self.infer_path(&path, expected),
        Expr::Literal(lit)             => ...,
        Expr::Missing                  => Ty::Unknown,
        // ... rest
    }
}

fn infer_if_expr(
    &mut self, expr_id: ExprId, cond: ExprId,
    then_b: ExprId, else_b: Option<&ExprId>, expected: Option<&Ty>,
) -> Ty {
    // 原 infer_expr 中 If arm 的 40 行逻辑移入
}
```

- **验证**: 类型推断测试全部通过
- **风险**: 低（纯代码移动）
- **收益**: `infer_expr` 从 162 行降至 ~60 行

---

### 任务 4.3 · 引入 `CallContext` 结构体消除 8 参数

- **文件**: `crates/rua-analysis/src/hir/infer.rs`
- **行号**: L1397 (`infer_callable_call`), L1062 (`infer_method_call`)
- **方案**: 参照审查文档问题 9

```rust
struct CallContext<'a> {
    call: ExprId,
    target: Ty,
    callable: &'a CallableTy,
    args: &'a [ExprId],
    expected: Option<&'a Ty>,
    substitution: Substitution,
    requirements: Vec<TraitRequirement>,
    variadic: bool,
}

impl InferenceContext<'_> {
    fn infer_callable_call(&mut self, ctx: &CallContext) -> Ty {
        // ...
    }
}
```

- **验证**: 编译通过（需要同时修改 `infer_method_call`）
- **风险**: 低
- **收益**: 消除 clippy suppression，改善 API 可读性

---

### 任务 4.4 · `infer_closure` panic 安全：用 guard struct 替代手动 stack

- **文件**: `crates/rua-analysis/src/hir/infer.rs`
- **行号**: L917-921
- **问题**: 手动 `std::mem::replace` + `pop` 在 panic 时栈损坏

```rust
struct ReturnTyGuard<'a> {
    ctx: &'a mut InferenceContext<'a>,
    outer_return: Ty,
}

impl Drop for ReturnTyGuard<'_> {
    fn drop(&mut self) {
        self.ctx.return_ty = std::mem::replace(&mut self.outer_return, Ty::Unknown);
        // closure_returns 的 pop 也需要 guard
    }
}

// 使用:
let _guard = ReturnTyGuard::new(self, closure_return);
let actual_return = self.infer_expr(body, expected_return.as_ref());
// guard 在 drop 时自动恢复 return_ty
```

- **验证**: 现有闭包类型推断测试 + 可考虑添加 panic 场景测试
- **风险**: 中（涉及 Drop 语义）
- **收益**: panic safety

---

### 任务 4.5 · 提取 `infer_builtin_call` Ok/Err 公共逻辑

- **文件**: `crates/rua-analysis/src/hir/infer.rs`
- **行号**: L1560-1586
- **问题**: Ok 和 Err arms 各 13 行，仅差 Result 槽位

```rust
fn infer_result_constructor(
    &mut self,
    is_ok: bool,
    argument: ExprId,
    expected: Option<&Ty>,
) -> Ty {
    let expected_parts = match expected {
        Some(Ty::Result(ok, error)) => Some(((**ok).clone(), (**error).clone())),
        _ => None,
    };
    let expected_item = expected_parts.as_ref().map(|p| if is_ok { &p.0 } else { &p.1 });
    let actual = self.infer_expr(argument, expected_item);
    self.report_argument_mismatch(argument, expected_item, &actual);
    let item = prefer_expected_if_unknown(actual, expected_item);
    if is_ok {
        (vec![item.clone()], Ty::Result(Box::new(item), Box::new(Ty::Unknown)))
    } else {
        (vec![item.clone()], Ty::Result(Box::new(Ty::Unknown), Box::new(item)))
    }
}
```

- **验证**: Result 类型推断测试
- **风险**: 低
- **收益**: 消除 ~26 行重复

---

## 六、第5阶段：DefMap + Diagnostic 优化（2天，4项）

> 目标：修复算法复杂度问题，提升诊断质量。

### 任务 5.1 · `module_for_file` 建索引 → O(1)

- **文件**: `crates/rua-analysis/src/hir/def_map.rs`
- **行号**: L507-517
- **方案**: 在 `DefMap` 构建时建 `FileId → ModuleId` 索引

```rust
// DefMap 增加字段:
file_to_module: HashMap<FileId, ModuleId>,

// 构建时填充（在 build_inner 末尾）:
let mut file_to_module = HashMap::new();
for module in modules.values() {
    if let Some(file_id) = module.file_id() {
        file_to_module.insert(file_id, module.id());
    }
}

// 查找:
pub fn module_for_file(&self, file_id: FileId) -> Option<ModuleId> {
    self.file_to_module.get(&file_id).copied()
}
```

- **验证**: 现有 def_map 测试 + 性能基准
- **风险**: 低（逻辑等价）
- **收益**: O(N×M) → O(1)

---

### 任务 5.2 · 合并 `resolve_path` 4 方法为 1 个

- **文件**: `crates/rua-analysis/src/hir/def_map.rs`
- **行号**: L547-601
- **方案**: 参照审查文档 R8

```rust
#[derive(Clone, Copy)]
pub enum ResolveStrategy {
    First,
    Unique,
    Lexical,
    LexicalUnique,
}

impl DefMap {
    pub fn resolve_path(
        &self,
        module_id: ModuleId,
        segments: &[&str],
        strategy: ResolveStrategy,
    ) -> Option<&Definition> {
        let resolve_fn = match strategy {
            ResolveStrategy::First | ResolveStrategy::Lexical => Self::resolve_name,
            ResolveStrategy::Unique | ResolveStrategy::LexicalUnique => Self::resolve_name_unique,
        };
        // 公共逻辑...
    }
}
```

- **验证**: hover + goto-def + completion 测试
- **风险**: 中（影响多条代码路径）
- **收益**: 4 方法 → 1 方法，公共逻辑集中

---

### 任务 5.3 · 未使用函数 lint：改进字符串匹配为决议查找

- **文件**: `crates/rua-analysis/src/diagnostic/mod.rs`
- **行号**: L649-688
- **问题**: `nr.name() == Some(name)` 字符串匹配，false negative

```rust
// 修复后: 对每个可能的调用点，检查 name ref 是否决议到目标函数
let is_referenced = def_map.definitions().any(|d| {
    if !matches!(d.kind(), DefKind::Function | DefKind::Method) {
        return false;
    }
    let Some(body) = db.body(d.id()) else { return false };
    let Some(resolution) = db.body_resolution(d.id()) else { return false };
    body.name_refs().any(|(nrid, _nr)| {
        matches!(
            resolution.resolve(nrid),
            Some(crate::hir::LocalResolveResult::ResolvedByDef(def_id))
                if def_id == definition.id()
        )
    })
});
```

> ⚠️ 注意：需要确认 `LocalResolveResult` 是否支持跨文件 def 决议。如不支持，此任务需推迟。

- **验证**: 未使用函数 lint 测试用例（包括 false negative 场景）
- **风险**: 中
- **收益**: 消除 false negative

---

### 任务 5.4 · 提取 `fast_diag()` helper + 合并 unused/lint 遍历

- **文件**: `crates/rua-analysis/src/diagnostic/mod.rs`
- **问题**: `Diagnostic::new(..., DiagnosticOrigin::FastAnalysis)` 重复 6 次；两个 lint 分别遍历 body

```rust
fn fast_diag(file_id: FileId, range: TextRange, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(file_id, range, message, DiagnosticOrigin::FastAnalysis)
}

// 合并单次遍历:
fn lint_body_bindings(
    db: &dyn BaseDb,
    def_map: &DefMap,
    definition: &Definition,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let file_id = definition.file_id();
    // 一次获取 body/source_map/resolution，然后跑 unused + redundant-mut 两个 lint
    let Some(body) = db.body(definition.id()) else { return };
    let Some(source_map) = db.body_source_map(definition.id()) else { return };
    let Some(resolution) = db.body_resolution(definition.id()) else { return };

    lint_unused_variables(file_id, &body, &source_map, &resolution, diagnostics);
    lint_redundant_mut(file_id, &body, &source_map, &resolution, diagnostics);
}
```

- **验证**: lint 测试通过
- **风险**: 低
- **收益**: 消除重复的 body/source_map/resolution 获取

---

## 七、第6阶段：架构级改进（按需）

> 以下项目需要更多设计和讨论，不适合立即开工。

### 7.1 · LSP Server 模块拆分

- **目标**: `lsp.rs` 4,413 行 → `handlers/` 目录
- **方案**: 参照审查文档 R9
- **工时**: 6-8h
- **前置条件**: 第2阶段 macro 重构完成

### 7.2 · 缓存细粒度失效

- **目标**: 修改单文件不刷新全局 def_map
- **方案**: 参照审查文档 R10
- **工时**: 8-12h
- **前置条件**: 项目有 20+ 文件时有实际收益

### 7.3 · 测试 Fixture 系统

- **目标**: `$0` 标记光标位置，替代手动计算 column
- **方案**: 参照审查文档 R11
- **工时**: 4-6h
- **前置条件**: 需跨 crate 修改测试基础设施

### 7.4 · Unreachable code lint 基于 HIR

- **目标**: 替代当前的文本正则匹配
- **方案**: 使用 HIR body 的控制流信息
- **工时**: 6-8h
- **前置条件**: 需要基本的 CFG 构建

---

## 八、测试策略

### 每阶段测试要求

| 阶段 | 测试类型 | 命令 |
|------|---------|------|
| 1-5 | 单元测试 | `cargo test -p rua-analysis -p rua-lsp` |
| 2 | LSP 集成测试 | `cargo test -p rua-lsp --test incremental_stress` |
| 3 | 补全专项测试 | `cargo test -p rua-analysis -- completion` |
| 4 | 类型推断测试 | `cargo test -p rua-analysis -- hir` |
| 5 | 诊断测试 | `cargo test -p rua-analysis -- diagnostic` |
| 全部 | 全量回归 | `cargo test --all` |
| 全部 | Clippy | `cargo clippy -p rua-analysis -p rua-lsp --all-targets -- -D warnings` |

### 新增测试要求

1. **任务 1.1** (fallback URI bug): 新增测试验证 fallback URI 中的数字与 FileId 一致
2. **任务 3.1** (CompletionRelevance): 新增测试验证各构造器的 score 值
3. **任务 3.2** (CompletionContext): 新增测试验证各种 AST 位置的上下文标记
4. **任务 3.3** (拆分 scope_completions): 每个 `complete_*` 函数至少一个测试
5. **任务 5.3** (未使用函数 lint): 新增测试覆盖 false negative 场景（局部变量与函数同名）

---

## 九、风险与回滚

### 风险矩阵

| 任务 | 风险等级 | 回滚难度 | 备注 |
|------|---------|---------|------|
| 1.1-1.6 | 极低 | 极低 | 每项可独立 revert |
| 2.1 | 中 | 中 | macro 重构影响 28 个 handler |
| 2.4 | 中 | 中 | 涉及文件身份管理 |
| 3.2 | 中高 | 中 | 改变补全上下文判断 |
| 3.1 | 中 | 低 | 影响补全排序 |
| 4.4 | 中 | 低 | Drop 实现需仔细 review |
| 5.3 | 中 | 低 | 可能需先扩展决议系统 |

### 回滚策略

- **每个 commit 独立可 revert** — commit message 包含任务编号
- **阶段间有验证门** — 阶段 N 全部测试通过后才进入阶段 N+1
- **补全排序变更** — 在阶段 3 前后分别抓取补全结果快照做对比

---

## 附录：Commit 序列建议

```
第1阶段:
  fix: use correct id in ensure_file_id_for_path fallback URI (1.1)
  chore: remove unnecessary clone in close_document (1.2)
  chore: rename _bid to bid in resolve_dot_access (1.3)
  chore: remove unused DiagnosticSeverity/DiagnosticSource variants (1.4)
  chore: remove unused _token and _file_id parameters (1.5)
  perf: use or_else for lazy method resolution in goto-def (1.6)

第2阶段:
  refactor: add handle_position_request and handle_doc_request macros (2.1)
  refactor: migrate hover/definition/references to position_request macro (2.1a)
  refactor: migrate remaining handlers to macros (2.1b)
  refactor: add extract_notification macro (2.2)
  refactor: extract try_add_watcher helper (2.3)
  refactor: unify ensure_file_id and ensure_file_id_for_path (2.4)

第3阶段:
  refactor: introduce CompletionRelevance struct (3.1)
  refactor: migrate scope_completions to CompletionRelevance (3.1a)
  refactor: introduce CompletionContext struct (3.2)
  refactor: split scope_completions into complete_* functions (3.3)
  refactor: merge BodyData and BodyFullData (3.4)

第4阶段:
  refactor: introduce diverge_or helper in infer (4.1)
  refactor: extract infer_if_expr/infer_assign_expr from infer_expr (4.2)
  refactor: introduce CallContext struct for callable calls (4.3)
  refactor: use guard struct for closure return_ty stack (4.4)
  refactor: extract infer_result_constructor helper (4.5)

第5阶段:
  perf: add FileId→ModuleId index for O(1) module_for_file (5.1)
  refactor: merge resolve_path 4 methods into 1 with strategy enum (5.2)
  fix: use resolution-based check in unused function lint (5.3)
  refactor: extract fast_diag helper and merge lint passes (5.4)
```
