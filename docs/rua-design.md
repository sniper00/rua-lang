# Rua 语言与运行时设计

Rua 是一门采用 Rust 风格语法、编译到可读 Lua 5.5 源码的静态类型脚本语言。它提供结构化数据、模式匹配、trait、泛型、闭包和 iterator，但不实现所有权、借用检查和生命周期。

本文描述当前语言和对外实现契约。工具链内部结构见[工具链架构](rua-architecture.md)。

## 1. 执行模型

Rua 文件本身就是可执行 chunk，没有特殊入口函数。声明与语句可以在文件中按源码顺序混排：

```rua
fn answer() -> i64 { 42 }

let value = answer();
print("answer = {}", value);
```

名为 `main` 的函数只是普通函数，不会自动执行。compiler 先为声明分配 identity，再按源码顺序执行初始化，因此前向引用和互递归不会改变可观察副作用的顺序。root chunk 完成后返回 public export table。

## 2. 类型与数据

| Rua 类型 | Lua 表示 |
|---|---|
| `i64` / `f64` | integer / number |
| `bool` | boolean |
| `String` | string |
| `()` | `nil` / no value |
| tuple | positional table |
| `Option<T>` | `T` 或 `nil` |
| `Result<T, E>` | tagged runtime value |
| `Vec<T>` | 1-based table，加显式长度 `n` |
| `HashMap<K, V>` | runtime map |
| struct | table + class metatable |
| enum | tagged table |
| closure / function | Lua function |

类型在编译期检查，在 Lua 后端擦除。Rua 支持 mutable binding、struct field、unit/tuple/struct enum variant、trait/impl、泛型约束、模式匹配、闭包捕获和 iterator adapter；这些能力不隐含 Rust 的 move、borrow 或 lifetime 语义。

### 2.1 Result ABI

`Result<T, E>` 在 Rua 内部始终只产生一个 Lua value：

```text
Ok(v)  = { true,  v }
Err(e) = { false, e }
```

索引 1 是 boolean `is_ok` tag，索引 2 是 payload。赋值、参数、返回值、字段、
容器、闭包、嵌套 Result、`?` 和 pattern 都使用同一表示。tag 与 payload 分离，
所以 `Ok(nil)` 和 `Err(nil)` 可区分。两个 1-based slot 使用 Lua table 的 array part，
不为每个 Result 分配命名 hash 字段。该表示属于 `rua_std` ABI v2；用户定义的同名
`Result` 不会触发 builtin lowering。

`Option<T>` 则使用 `T | nil`，`Some(v)` 擦除为 `v`，`None` 为 `nil`。它与 Result 是两个独立 ABI。

`Option` 与 `Result` 的 `map`、`unwrap`、`expect`、`unwrap_or` 和状态查询由标准
declaration 定义。`expect(message)` 返回成功值；`Option::None` 或 `Result::Err`
路径通过 `rua_std` 报告消息并终止执行。

### 2.3 内嵌 Lua 代码

需要直接使用宿主 Lua API 或暂未被 Rua 封装的 Lua 语法时，可以在 statement 位置写
`lua! { ... }` 原始代码块：

```rua
lua! {
    local payload = { ready = true, values = { 1, 2, 3 } }
    print(payload.ready, #payload.values)
}
```

`lua!` 代码块在词法阶段作为一个整体保留，支持 Lua 的嵌套 table、字符串、长字符串
和注释。codegen 将块内容原样写入当前 Lua chunk，并把生成范围锚定到该 Rua block，
因此运行时错误仍能定位到 Rua 源文件的 block 行。

内嵌 Lua 不参与 Rua 类型检查、名称解析或变量插值；Rua local 不能直接作为 Lua
local 使用。需要稳定参数和返回值时，应优先使用 `extern "lua"` 声明 Lua ABI，再由
Rua 表达式调用。原始块允许出现在顶层、函数和控制流 block 中，也可以在末尾使用一个
分号；它本身没有 Rua 表达式值。

### 2.4 脚本表达式

Rua 吸收动态脚本语言中高频、低歧义的表达方式，但每个操作仍在编译期确定类型与
lowering，不引入运行时名称猜测。

`[value, ...]` 构造 `Vec<T>`，元素从左到右求值一次；非空 literal 合并元素类型，
空 `[]` 使用 expected `Vec<T>`。Vec 使用 1-based storage 与显式长度 `n`，因此
literal、`get`、`set` 和 `[]` 下标都从 1 开始。
格式化、输出、panic、断言和控制流辅助项是标准 `.ruai` 中的普通函数：

```rua
let values = [1, 2, 3];
let message = format("count={}", values:len());
print("{}", message);
assert(values:len() == 3);
```

这些调用通过 resolved declaration identity 连接 `rua_std`，签名、文档、completion、
hover 和 definition 使用同一标准库记录。

二元操作符对已知具体类型采用以下规则：

| 操作符 | 合法操作数 |
|---|---|
| `+` | 两个数值、两个 `String`，或左类型实现 `Add` |
| `-` / `*` / `/` / `%` | 两个数值，或左类型实现对应的 `Sub` / `Mul` / `Div` / `Rem` |
| `==` / `!=` | 两侧类型兼容；`i64` 与 `f64` 视为兼容数值 |
| `<` / `<=` / `>` / `>=` | 两个数值或两个 `String` |
| `&&` / `\|\|` | 两个 `bool` |
| `??` | `Option<T>` 与 `T` |
| `in` | 元素类型与容器的 element/key 类型兼容 |

用户算术实现的方法必须恰好接受对应的右操作数类型，表达式结果取方法返回类型。
已知具体类型不满足规则时产生 `E0206`；泛型算术要求声明相应 operator trait bound，
缺失类型信息则不会提前制造次生诊断。

复合赋值支持 `+=`、`-=`、`*=`、`/=` 和 `%=`：

```rua
let mut attempts = 1;
attempts += 2;
values[next_index()] *= 4;
```

其类型规则与对应二元运算一致，结果必须能写回左值。左值的 base、field 与 index
表达式只求值一次，因此最后一行不会重复调用 `next_index()`。

`loop` 是表达式，`break value` 决定表达式结果：

```rua
let port = loop {
    if ready() { break 8080; }
};
```

同一 `loop` 的所有 `break` 值必须兼容；裸 `break;` 的值是 `()`。`while` 与
`for` 仍是 statement，只允许裸 `break;`，不能返回值。无可达 `break` 的 `loop`
类型为 never，不会产生伪造的运行时值。

`Option<T>` 提供空值合并与可选链：

```rua
let city = account?.profile?.city ?? "unknown";
let label = account?.display_name(expensive_suffix()) ?? "anonymous";
```

- `option ?? fallback` 要求左侧为 `Option<T>`、右侧为 `T`。左侧只求值一次，右侧仅
  在 `None` 时求值；`Some(false)` 不会错误地选择 fallback。
- `option?.field` 与 `option?.method(args)` 在 `Some` 时访问成员，在 `None` 时返回
  `None`。method 参数也只在 receiver 存在时求值。
- 链中成员已经返回 `Option<U>` 时结果保持 `Option<U>`，不会形成额外嵌套。成员
  identity 仍来自被解包的 `T`，所以 completion、hover 与 goto definition 指向
  正常的 field/method declaration。

成员测试使用 `in`，map literal 使用 `#{...}`：

```rua
let scores: HashMap<String, i64> = #{
    "alice": 10,
    "bob": 20,
};

let known = "alice" in scores;
let small = 3 in 0..10;
```

`value in container` 对 `Vec<T>` 测试元素、对 `HashMap<K, V>` 测试 key、对 `String`
执行纯文本子串测试，对 `Iter<T>`（包括 range）逐项测试。iterator membership 是
consumer：它会消费到首个匹配项或末尾；其他三种不会消费容器。结果始终为 `bool`，
元素与容器类型不兼容时编译失败。

非空 map literal 从所有 entry 推断统一的 `K` 与 `V`，不接受混合 key/value 类型；
空 literal 应通过类型标注提供类型，例如
`let empty: HashMap<String, i64> = #{};`。codegen 将 map literal 直接生成 Lua
table，再由 `map.from_table` 包装成运行时 map，不再逐项调用 `insert`。

优先级上 `?.` 是 postfix；range 比 `in` 结合更紧，因此 `x in 0..10` 等价于
`x in (0..10)`；`??` 低于布尔运算并短路求值。assignment 仍是最低层 statement
expression，不允许隐式链式赋值。

### 2.5 Attribute、cfg 与 annotation

Rua 的外部 attribute 使用 Rust 风格语法，附着到声明、field、variant、method 和
extern function。`cfg` 在名称收集、类型检查和 codegen 之前求值，
未激活声明不进入当前 project 的 ItemTree、DefMap 或生成结果，但 tolerant syntax tree
仍保留源码，供格式化、语义着色和 inactive hover 使用。

```rua
#[cfg(feature = "http")]
fn start_http() {}

#[cfg_attr(feature = "http", Route(method = "GET", path = "/listen"))]
pub fn listen() {}
```

feature 与自定义 cfg 来自 `.ruarc.toml` 的 `[build]` 配置，也可以由 `ruac` 命令行覆盖。
同一套 project cfg 输入供 compiler 与 LSP 使用，不在 codegen 中重新解释 attribute 文本。

annotation 是带 schema 的声明，不是可执行宏：

```rua
#[retention(runtime)]
#[targets(function, struct)]
pub annotation Route(path: String, method: String);

#[Route(path = "/health", method = "GET")]
pub fn health() -> String { "ok" }
```

compiler 在 resolve 后建立 `AnnotationIndex`，验证 target、参数、重复策略和
retention。source retention 只存在于 syntax semantic view，build retention 进入
compiler artifact，runtime retention 同时进入 artifact 与 Lua registry。bundle 输出
直接注册记录，modules 输出生成单一聚合
`rua_annotations.lua` 并按需定位目标。没有 runtime annotation 的程序不引入 registry
或启动开销。运行时按 annotation
的规范名查询，因为 Rua/Lua 当前没有可用于泛型查询的类型反射。

完整语法与配置见 [Attribute 与条件编译](rua-attributes.md)；annotation schema、
artifact 和 runtime API 见 [Annotation](rua-annotations.md)。

## 3. 函数、闭包与 iterator

函数和闭包都可以作为一等值传递。闭包支持表达式体、block body、参数/返回类型和 lexical capture：

```rua
let offset = 2;
let add = |x: i64| -> i64 { x + offset };
```

iterator 是惰性、可组合的运行时协议。range、`Vec` 和 adapter 可以进入 `for`，并支持 `map`、`filter`、`fold`、`find`、`any`、`all`、`count` 和 `collect`。compiler 可以融合已知 adapter 链，但优化前后必须保持相同的可观察语义。

标准 iterator 同时是一等 runtime value：它可以赋值、返回、传参和分阶段组合。`String::chars` 按 UTF-8 Unicode scalar 迭代，不按字节拆分。

## 4. 模块与接口文件

Rua 使用 path-as-module：源码不包含 `mod` 声明，文件相对 source root 的路径就是
模块路径。`src/main.rua` 作为入口时，`src/domain/order.rua` 映射为
`domain::order`；缺少实体文件的中间目录仍形成虚拟 namespace。
`name.rua` 与 `name/mod.rua` 表示同一模块，`.ruai` 也遵循相同规则；同一优先级
同时存在多个映射会产生歧义错误。logical path 必须是合法 Rua identifier 序列，
不得逃离 source root。源码级 `mod name;` 和 inline `mod name { ... }` 均为语法错误。

filesystem compiler、IO-free `ProjectSpec` 与 native analysis 都调用
`rua-project::module_path_from_relative_file`，因此 build、diagnostics、completion、
hover 和 goto definition 看到同一模块图。workspace source 优先于共置 declaration
和配置库；低优先级库不会覆盖 workspace 模块。

`.ruai` 是 declaration-only 接口文件。函数和 method 可用空 `{}` 表示声明，trait signature 也可使用 `;`；以下内容产生 `E0108`：

- 非空函数、impl method 或 trait method body。
- 文件顶层 executable statement。

编译器递归发现入口目录中的 `.rua`/`.ruai`；LSP 对同一 project source root 建立
相同 DefMap。模块本身按路径可达，模块内 item 是否能跨模块访问仍由 `pub` 控制。

### 4.1 外部 Lua 库

普通 Lua 库按根目录配置，不逐模块登记：

```toml
[[workspace.lua_library]]
root = "../moon_rs/lualib"
```

`root` 表示 `.ruai` 与 `.lua` 共置；也可使用 `declaration_root` 和
`runtime_root` 分离两棵同构目录。声明文件的相对路径同时确定 Rua module path
和 Lua package name，例如 `moon/http/client.ruai` 映射为
`moon::http::client` 与 `require("moon.http.client")`。文件型 `.ruai` 是独立
require boundary。

配置解析把 declaration root 合并到 compiler/LSP 的只读 library 输入，把
runtime root 合并到 codegen `package.path`。compiler 与 LSP 都递归扫描同一
declaration root，并使用相同相对路径规则。`workspace.library` 提供
declaration-only 输入，`library_mounts` 提供显式逻辑名称映射。

### 4.2 标准库配置

默认标准库随 `rua-resources` 内嵌。自定义标准库目录必须包含 `std.toml`，其中显式列出 declaration 文件、runtime source、language item、Lua 包、导出子表、局部别名和可选 ABI。`.ruai` 是类型签名、文档、completion、hover 和 goto definition 的唯一来源；runtime binding 只决定已解析标准定义如何连接到 Lua 导出。

`Option` 的 nullable 表示和 `Result` 的 tagged 表示需要 compiler 参与，因此由 `[lang_items]` 指定。`Vec`、`HashMap`、`Iter` 与 `String` 没有专用构造器表或成员表，它们通过 declaration 和单个 `rua_std` Lua 包的普通导出实现；Result 的 tagged 构造也由 `result` 导出提供。bundle codegen 对该包只执行一次 `require` 和 ABI 检查；modules codegen 在实际使用它的输出单元中声明 import，由 Lua `require` cache 共享同一包实例。两种模式都只为实际使用的 `vec`、`map` 等 export 生成 local。用户类型不需要修改 `std.toml`：直接声明 `struct`、`enum`、`trait` 和 `impl` 即可；名称恰好相同也不会获得 language item 语义。

## 5. Extern 与宿主边界

普通 Lua 函数使用单值 ABI：

```rua
extern "lua" {
    fn clock() -> f64;
}
```

Lua 常见的 `(value, nil)` / `(nil, error)` 约定必须显式声明为 `lua-result`：

```rua
extern "lua-result" {
    fn read_value(key: String) -> Result<String, String>;
}
```

adapter 在边界把 multi-return 转成 Rua tagged Result，并把 Result 参数反向展开。该 ABI 要求 function 非 variadic，且返回类型解析到 builtin `Result<T, E>` identity。普通 `extern "lua"` 不会根据类型名称猜测转换方式。

`std.toml` 中 runtime module 的可选 `abi` 是 compiler 与该 Lua 模块的硬契约。生成物只 require 实际使用的模块，并逐模块检查 `ABI_VERSION`；自定义库未声明 `abi` 时只生成 require。

## 6. Compiler API

推荐的宿主入口是 IO-free project API：

```rust
let artifact = ruac::compile_project_with_diagnostics(
    &project_spec,
    &source_provider,
)?;
```

`ProjectSpec` 提供 root、source root、library mount、stable `FileId` 和 logical path；`SourceProvider` 提供源码。成功 artifact 包含 Lua source 和 generated-to-Rua source map，失败 `CompileFailure` 包含稳定 diagnostic code、文件、byte range 和命名参数。

`compile_str`、`compile_path`、`compile_project` 与 artifact convenience API 使用同一结构化失败类型。只有 CLI adapter 把诊断渲染为终端文字；`compile_path_with_std` 和 `--std-path` 是显式标准库文件系统入口，未指定时使用内嵌标准库。

### 6.1 Lua 输出模式

`bundle` 是默认模式：所有 resolved module 共享一个 Lua chunk。`modules`
模式通过 `ruac build <root> --emit modules --out-dir <dir>` 启用，root 输出
保留输入文件名，其他 module 按逻辑路径输出，例如 `domain::order` 对应
`domain/order.lua`。每个输出文件在顶部用 `require("domain.order")` 加载其
identity-resolved 依赖，随后定义并初始化当前 module。仅有一个公开
`struct`/`enum` 的源模块直接返回该类型 table；多个公开类型或没有公开类型时返回
独立 module table。`.ruai` declaration 不采用此扁平规则，其 Lua ABI 完全由配置决定。

依赖 local 由完整逻辑路径生成 PascalCase 名称，例如 `application.checkout` 对应
`ApplicationCheckout`；路径折叠重名时追加稳定 identity 后缀。文件头按 `package.path`、
`rua_std` ABI、module require 和标准库 alias 分组，声明区、函数与最终 `return` 使用
稳定空行。EmmyLua class 使用限定 identity，例如 `presentation.Console` 与
`domain.Product`。

modules 模式不是按源文件做文本切分：依赖名来自 resolved `ModuleId`，codegen
不重新猜测源路径。它采用 Lua 原生加载语义，因此初始化按 `require` 深度优先发生，
不保证与 bundle 的跨文件顶层副作用顺序相同。普通 `require` 无法可靠表示的 module
依赖环会在编译期拒绝；modules 工程的依赖图必须无环，循环依赖可以使用 bundle。

`runtime.lua_path = ["dir"]` 或重复的 `--lua-path <dir>` 会把目录转换为
`dir/?.lua` 并前置到 root 输出的 `package.path`。未配置时由 host 使用 Lua 自身的
`package.path`/`LUA_PATH`。bundle 与 modules 使用相同规则；该选项不参与 Rua
源码与 `.ruai` 的编译期查找。

`compile_path_modules_artifact[_with_options]` 和
`compile_project_modules_artifact` 返回每个输出的 module name、相对输出路径、Lua
源码与独立 source map。`.ruai` declaration 不产生输出文件；它仍按配置映射到普通
Lua `require`。

runtime-retained annotation 聚合到一个 `rua_annotations.lua`。registry 保存 canonical
module/path locator，加载 registry 不执行应用 module；`Annotations::load` 才解析并加载
target。没有 runtime annotation 时 artifact 不包含该文件。

## 7. 稳定契约

以下行为属于兼容边界：

- runtime ABI version、Result/Option/container 表示和 FFI adapter。
- public export key、module 初始化顺序和顶层副作用顺序。
- diagnostic code 与 source range 的语义。
- source map 对 generated span、source file 和 byte range 的关联。

human diagnostic wording、Lua 临时变量名和 printer 的非语义排版不属于稳定 machine
contract。ABI 或运行时表示必须由真实 Lua execution test 覆盖；grammar 必须同时覆盖
strict parser、IDE parser 和 range conformance。
