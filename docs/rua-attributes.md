# Rua Attribute 与条件编译

Rua 使用 `#[...]` 为声明和成员附加结构化 metadata。`cfg` 与 `cfg_attr` 由 project
配置求值，其他 attribute 作为用户 annotation 解析。compiler、formatter、analysis
和 LSP 使用同一 meta-item 结构与 `CfgOptions`。

## 1. 语法与附着位置

```rua
#[cfg(feature = "http")]
pub fn serve() {}

#[cfg(any(runtime = "moon", embedded))]
pub struct Server {
    #[cfg(feature = "metrics")]
    pub requests: i64,
}
```

meta item 支持 word、name-value 和嵌套 list：

```text
enabled
runtime = "moon"
all(feature = "http", not(embedded))
```

attribute 可以附着到顶层 item、struct field、enum variant、struct variant field、
impl method、trait method 和 extern function。statement、expression、local binding、
parameter 没有 attribute 语义。

## 2. cfg 条件

`cfg` 接受一个条件：

```rua
#[cfg(true)]
#[cfg(feature = "http")]
#[cfg(embedded)]
#[cfg(runtime = "moon")]
#[cfg(all(feature = "http", not(embedded)))]
#[cfg(any(runtime = "moon", runtime = "lua"))]
```

条件形式：

| 形式 | 含义 |
|---|---|
| `true` / `false` | 常量条件 |
| `flag` | project flag |
| `key = "value"` | project key/value |
| `all(...)` | 全部匹配 |
| `any(...)` | 任一匹配 |
| `not(...)` | 条件取反 |

同一目标上的多个 `cfg` 使用逻辑与。inactive item/member 不进入当前 project 的名称
解析、类型检查、annotation index 或 Lua 输出。Rowan syntax tree 保留完整源码，供
formatter、inactive semantic token 和条件 hover 使用。

## 3. cfg_attr

`cfg_attr` 的第一个参数是条件，其余参数是条件成立时附加的 attribute：

```rua
#[cfg_attr(
    feature = "http",
    Route(method = "GET", path = "/health"),
)]
pub fn health() -> String { "ok" }
```

展开顺序固定为：递归展开 `cfg_attr`、求值全部 `cfg`、构造 active view、解析用户
annotation。inactive 分支中的 annotation 不参与 identity 或参数校验。

## 4. Project 配置

`.ruarc.toml` 使用 `[build]` 与 `[build.cfg]`：

```toml
[build]
features = ["http", "metrics"]

[build.cfg]
runtime = "moon"
embedded = true
capability = ["timer", "network"]
disabled = false
```

- `features` 产生 `feature = "name"` 条件。
- `true` 产生 flag，`false` 不产生 flag。
- string 产生一个 key/value。
- string array 为同一 key 提供多个可匹配 value。

CLI 可以为单次 compiler 调用补充配置：

```bash
ruac build src/main.rua --features http,metrics \
  --cfg runtime=moon --cfg embedded
```

compiler 与 LSP 从同一 `.ruarc.toml` 构造 project-scoped `CfgOptions`。配置 revision
参与 analysis cache key，不同 workspace folder 的 cfg 视图彼此隔离。

## 5. IDE 与诊断

LSP 为 `cfg`、`cfg_attr`、predicate 和当前 target 合法的用户 annotation 提供
completion。inactive 源码使用 semantic token modifier 标记，hover 显示条件；active
代码中的 unresolved reference、type error 和 annotation error 按正常规则诊断。

稳定错误覆盖 malformed meta item、非法 predicate、`cfg` 参数数量、`all`/`any`/`not`
形状、`cfg_attr` 缺少结果 attribute 以及递归展开上限。

用户 annotation 的 schema、retention、metadata 和运行时查询见
[Rua Annotation](rua-annotations.md)。
