# Rua 迁仓后测试基线

> 记录日期：2026-07-10
> 基线提交：`0d5a000cab72d9d214fd39427fd1079087f2edb9`
> 工具链：`rustc 1.94.0 (4a4ef493e 2026-03-02)`、`cargo 1.94.0 (85eff7c80 2026-01-15)`

本文记录 Rua 工具链从 `moon_rs` 迁入独立仓库后的可重复测试状态。它只固定
Step 0.1 的命令与结果；golden 目录、输出快照和更新 harness 留给 Step 0.2。

## 必需测试

| 命令 | 状态 | 结果 |
| --- | --- | --- |
| `cargo test -p ruac` | PASS | `ruac` library 194 passed；binary 0 tests |
| `cargo test -p rua-syntax` | PASS | 278 passed，1 ignored：269 unit、2 conformance、6 format、1 golden passed |
| `cargo test -p rua-lsp --features lsp` | PASS | `rua-lsp` 52 passed；`rua-fmt` 0 tests |
| `cargo test --workspace` | PASS | 472 passed，1 ignored |

`cargo test --workspace` 使用各 crate 的默认 feature，因此会构建 `rua-fmt`，但不会
启用 `rua-lsp` binary 及其 52 个测试。LSP 基线必须继续单独运行
`cargo test -p rua-lsp --features lsp`。

## 结构与静态检查

| 命令 | 状态 | 结果 |
| --- | --- | --- |
| `cargo metadata --format-version 1 --no-deps` | PASS | workspace 包含 `ruac`、`rua-syntax`、`rua-lsp` |
| `cargo tree -p ruac` | PASS | `ruac` 没有第三方或 workspace dependency |
| `cargo clippy --workspace --all-targets` | PASS | 退出码 0，存在迁仓前已有 warning |

Clippy warning 基线：

- `ruac` library：23 条，涉及 `collapsible_if`、`collapsible_match`、
  `clone_on_copy` 和 `useless_format`。
- `rua-syntax`：library 有 1 条 `collapsible_if`；test target 另有 1 条
  `items_after_test_module`。

Step 0.1 不修改实现来清理 warning，避免把行为变化混入基线提交。

## 已知失败

Rua 仓库在此基线上没有已知测试失败，也没有需要关联 issue/TODO 的失败项。
