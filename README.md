# Rua

**Rua** is a Rust-inspired language that compiles to readable, idiomatic Lua 5.4+.
Write in familiar Rust syntax — structs, enums, traits, generics, pattern matching,
iterators, closures — and get clean Lua that reads as if hand-written.

```bash
$ ruac build app.rua              # app.rua → app.lua
```

## Why Rua?

| | Rust | Lua | Rua |
|---|------|-----|-----|
| Type safety | ✅ borrow checker | ❌ dynamic | ✅ static, erased at runtime |
| Syntax | ✅ expressive | ❌ verbose | ✅ Rust-like, compiles to Lua |
| Ecosystem | ✅ cargo | ✅ luasocket/lpeg | ✅ Lua interop + Rua LSP |
| Learning curve | steep | gentle | **gentle → productive** |

- **No borrow checker**. Rua has Rust's syntax and type system *without* lifetimes, ownership, or borrow checking. Types are checked at compile time and erased in the output.
- **Idiomatic Lua output**. `Result<T, E>` compiles to Lua's native `nil, err` multi-return. `Option<T>` is bare `T | nil`. Iterators fuse into efficient `for` loops.
- **IDE support included**. LSP server with hover, goto-def, completions, references, rename, inlay hints, diagnostics, and semantic tokens.
- **Zero allocation**. `Some(x)` and `Ok(x)` are bare values, not wrapped in tables. Only structs and user enums use metatables.

## Quick Start

```bash
cargo build --release -p ruac -p rua-lsp --features lsp
```

### Hello World

```rua
fn main() {
    println!("Hello, {}!", "world");
}
```

⇩

```lua
local rt = require("rua_rt")

local function main()
    rt.println("Hello, {}!", "world")
end

main()
```

### Structs + Methods

```rua
struct Point { x: i64, y: i64 }

impl Point {
    fn new(x: i64, y: i64) -> Point { Point { x, y } }
    fn distance(&self) -> i64 { self.x * self.x + self.y * self.y }
}

fn main() {
    let p = Point::new(3, 4);
    println!("{}", p.distance());
}
```

⇩

```lua
local rt = require("rua_rt")
---@class Point
---@field x integer
---@field y integer
local Point = {}
Point.__index = Point

local function main()
    local p = Point.new(3, 4)
    rt.println("{}", p:distance())
end

---@return Point
function Point.new(x, y)
    return setmetatable({ x = x, y = y }, Point)
end

---@return integer
function Point:distance()
    return self.x * self.x + self.y * self.y
end

main()
```

### Enums + Pattern Matching

```rua
enum Message {
    Quit,
    Move { x: i64, y: i64 },
    Write(String),
}

fn handle(msg: Message) -> String {
    match msg {
        Message::Quit => "bye",
        Message::Move { x, y } => format!("move to ({}, {})", x, y),
        Message::Write(text) => text,
    }
}
```

⇩

```lua
local rt = require("rua_rt")
---@class Message
local Message = {}
Message.__index = Message

---@return string
local function handle(msg)
    local __t1 = msg
    if __t1.tag == "Quit" then
        return "bye"
    elseif __t1.tag == "Move" then
        local x = __t1.x
        local y = __t1.y
        return rt.format("move to ({}, {})", x, y)
    elseif __t1.tag == "Write" then
        local text = __t1[1]
        return text
    else
        error("non-exhaustive match")
    end
end
```

### Error Handling — Lua‑idiomatic

`Result<T, E>` compiles to Lua's native `nil, err` multi‑return:

```rua
fn load_config(path: String) -> Result<String, String> {
    if path == "" {
        Err("empty path")
    } else {
        Ok("config")
    }
}

fn use_config() -> Result<String, String> {
    let config = load_config("app.rua")?;
    Ok(config)
}
```

⇩

```lua
---@return string|nil, string|nil
local function load_config(path)
    if path == "" then
        return nil, "empty path"
    else
        return "config"
    end
end

---@return string|nil, string|nil
local function use_config()
    local config, __t1 = load_config("app.rua")
    if __t1 ~= nil then return nil, __t1 end
    if config == nil then return nil end
    return config
end
```

`Option<T>` compiles to bare `T | nil` — zero allocation:

```rua
fn maybe_double(x: Option<i64>) -> Option<i64> {
    let v = x?;
    Some(v * 2)
}
```

⇩

```lua
---@return integer|nil
local function maybe_double(x)
    local v = x
    if v == nil then return nil end
    return v * 2
end
```

### Modules

```rua
mod math {
    pub fn add(a: i64, b: i64) -> i64 { a + b }
    fn helper() -> i64 { 0 }
}
use math::add;

fn main() {
    let sum = add(3, 4);
}
```

⇩

```lua
---@class math
local math = {}

---@return integer
function math.add(a, b)
    return a + b
end

---@return integer
function math.helper()
    return 0
end

local function main()
    local sum = math.add(3, 4)
end

main()
```

### Generics + Traits

```rua
trait Greet {
    fn hello(&self) -> String;
}

struct Person { name: String }

impl Greet for Person {
    fn hello(&self) -> String { format!("Hi, {}!", self.name) }
}

fn greet<T: Greet>(value: &T) -> String {
    value.hello()
}
```

⇩

```lua
local rt = require("rua_rt")
---@class Person
---@field name string
local Person = {}
Person.__index = Person

---@generic T
---@return string
local function greet(value)
    return value:hello()
end

---@return string
function Person:hello()
    return rt.format("Hi, {}!", self.name)
end
```

### Iterators

```rua
fn main() {
    let doubled: Vec<i64> = vec![1, 2, 3, 4, 5].iter()
        .map(|x| x * 2)
        .filter(|x| x > 5)
        .collect();

    let total = vec![1, 2, 3].iter().fold(0, |acc, x| acc + x);
}
```

⇩

```lua
local rt = require("rua_rt")

local function main()
    local doubled
    local __t1 = rt.vec({ [0] = 1, [1] = 2, [2] = 3, [3] = 4, [4] = 5, n = 5 })
    local __t2 = rt.vec({ n = 0 })
    for __t4 = 0, __t1.n - 1 do
        local __t3 = __t1[__t4]
        local __t5 = true
        if __t5 then
            local __t6
            do
                local x = __t3
                __t6 = x * 2          -- map body inlined directly
            end
            __t3 = __t6
        end
        if __t5 then
            local __t7
            do
                local x = __t3
                __t7 = x > 5           -- filter body inlined directly
            end
            if not __t7 then __t5 = false end
        end
        if __t5 then
            __t2[__t2.n] = __t3
            __t2.n = __t2.n + 1
        end
    end
    doubled = __t2

    local total
    local __t8 = rt.vec({ [0] = 1, [1] = 2, [2] = 3, n = 3 })
    local __t9 = 0
    for __t11 = 0, __t8.n - 1 do
        local __t10 = __t8[__t11]
        local __t12 = true
        if __t12 then
            local __t13
            do
                local acc = __t9
                local x = __t10
                __t13 = acc + x       -- fold body inlined directly
            end
            __t9 = __t13
        end
    end
    total = __t9
end

main()
```

> Iterator chains (`map`, `filter`, `fold`, `any`, `all`, `find`, `count`, `collect`,
> `enumerate`, `take`, `skip`) fuse into a single `for` loop — no intermediate
> allocations, closures inlined directly into the loop body.

### ? Operator — Error Propagation

```rua
fn chain(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    let va = a?;
    let vb = b?;
    Some(va + vb)
}
```

⇩

```lua
---@return integer|nil
local function chain(a, b)
    local va = a
    if va == nil then return nil end
    local vb = b
    if vb == nil then return nil end
    return va + vb
end
```

### Closures

```rua
let inc = |x: i64| x + 1;
let add = |a: i64, b: i64| -> i64 { a + b };
let base = 10;
let offset = |x| x + base;            // fused: captures by value
```

### Extern Functions

```rua
extern "lua" {
    pub fn log(message: String);
    pub fn format(template: String, ...) -> String;
}
```

⇩

```lua
local log = log or function(...) end
local format = format or function(...) end
```

## Language Reference

### Types

| Rua | Lua Runtime |
|-----|-------------|
| `i64`, `i32`, `u64`, … | Lua `integer` |
| `f64`, `f32` | Lua `number` |
| `bool` | Lua `boolean` |
| `String`, `str` | Lua `string` |
| `Vec<T>` | Lua array table `{ T, … }` |
| `HashMap<K, V>` | `rt.map()` table |
| `Option<T>` | `T` (Some) 或 `nil` (None) |
| `Result<T, E>` | `T` (Ok) 或 `nil, E` (Err) |
| `struct` / `enum` | table + metatable |
| `&T` / `&mut T` | 等同于 `T` (类型已擦除) |

### Control Flow

```rua
if n > 0 { "pos" } else if n < 0 { "neg" } else { "zero" }  // 表达式
while count < 10 { count = count + 1; }
for i in 0..10 { sum = sum + i; }                              // 左闭右开
for j in 1..=5 { sum = sum + j; }                              // 闭区间
loop { if done { break; } }
match val { Some(v) => v, None => 0 }
if let Some(p) = maybe { return p.x; }
while let Some(v) = opt { opt = Some(v + 1); }
```

## Tooling

### Compiler (`ruac`)

```bash
ruac build src/main.rua              # 编译 → src/main.lua
ruac check src/main.rua              # 仅类型检查
ruac build --builtins-dir ./std      # 自定义内置库路径
```

### Language Server (`rua-lsp`)

| Feature | |
|---------|-|
| Hover (类型信息 + 文档) | ✅ |
| Goto Definition | ✅ |
| Find References | ✅ |
| Rename | ✅ |
| Completions (关键词 / 局部变量 / 路径 / 成员) | ✅ |
| Inlay Hints (仅 let 绑定) | ✅ |
| Diagnostics (parse + type + lint) | ✅ |
| Semantic Tokens | ✅ |
| Code Actions | ✅ |
| Folding Ranges | ✅ |
| Document Symbols | ✅ |
| Call Hierarchy | ✅ |
| Type Hierarchy | ✅ |
| Signature Help | ✅ |
| Formatting | ✅ |

## Project Structure

```
rua/
├── crates/
│   ├── ruac/          # 编译器: parse → typeck → codegen
│   ├── rua-syntax/    # 无损 CST, parser, lexer, formatter
│   ├── rua-analysis/  # 增量语义分析, IDE 查询引擎
│   └── rua-lsp/       # LSP 服务器 (stdio JSON-RPC)
├── lualib/
│   └── rua_rt.lua     # 运行时库 (String 方法, 迭代器)
├── tests/
│   ├── demo.rua       # 综合语法演示 (620+ 行)
│   └── golden/        # compile-pass (44) + compile-fail (45) 快照
└── docs/              # 设计文档
```

## License

MIT
