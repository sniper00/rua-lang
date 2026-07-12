# Rua

**Rua** is a Rust-inspired language that compiles to readable, idiomatic Lua 5.4+.
Write in familiar Rust syntax — structs, enums, traits, generics, pattern matching,
iterators, closures — and get clean Lua that reads as if hand-written.

```bash
$ cat demo.rua | ruac build -   # compile to stdout
$ ruac build app.rua             # app.rua → app.lua
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
- **Zero runtime overhead**. No persistent runtime objects. A small `rua_rt` library provides `String` methods and iterator plumbing.

## Quick Start

### Install

```bash
cargo build --release -p ruac -p rua-lsp --features lsp
```

### Hello World

```rua
fn main() {
    println!("Hello, {}!", "world");
}
```

### Structs + Methods

```rua
struct Point {
    x: i64,
    y: i64,
}

impl Point {
    fn new(x: i64, y: i64) -> Point { Point { x, y } }
    fn distance(&self) -> i64 { self.x * self.x + self.y * self.y }
}

fn main() {
    let p = Point::new(3, 4);
    println!("{}", p.distance());  // 25
}
```

Compiles to:

```lua
---@class Point
---@field x integer
---@field y integer
local Point = {}
Point.__index = Point

function Point.new(x, y)
    return { x = x, y = y }
end

function Point:distance()
    return self.x * self.x + self.y * self.y
end

local function main()
    local p = Point.new(3, 4)
    print(p:distance())  -- 25
end
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

### Error Handling — Lua-idiomatic

Rua compiles `Result` to Lua's natural `nil, err` convention:

```rua
fn load_config(path: String) -> Result<String, String> {
    if path == "" {
        Result::Err("empty path")     // → return nil, "empty path"
    } else {
        Result::Ok("config")          // → return "config"
    }
}

fn use_config() -> Result<String, String> {
    let config = load_config("app.rua")?;  // → local v, e = f(); if e then return nil, e end
    Result::Ok(config)
}
```

`Option<T>` compiles to bare `T | nil`:

```rua
fn maybe_double(x: Option<i64>) -> Option<i64> {
    let v = x?;          // unwrap Some, propagate None
    Option::Some(v * 2)  // → return v * 2  (bare value, no allocation)
}
```

### Iterators

```rua
let values = vec![1, 2, 3, 4, 5];
let doubled: Vec<i64> = values.iter()
    .map(|x| x * 2)
    .filter(|x| x > 5)
    .collect();                    // → { 6, 8, 10 }

let total = values.iter().fold(0, |acc, x| acc + x);  // → 15
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

### Modules + Visibility

```rua
mod math {
    pub fn add(a: i64, b: i64) -> i64 { a + b }
    fn helper() -> i64 { 0 }         // private, not exported
}
use math::add;

fn main() {
    let sum = add(3, 4);             // → math.add(3, 4) in Lua
}
```

### ? Operator — Error Propagation

```rua
fn chain(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    let va = a?;     // None propagates nil
    let vb = b?;     // Some unwraps value
    Some(va + vb)    // bare i64, no allocation
}

fn result_chain() -> Result<i64, String> {
    let config = load_config("app.rua")?;  // Err propagates nil, msg
    Ok(config.len())
}
```

Generates clean Lua:

```lua
local function chain(a, b)
    local __t1, __t2 = a
    if __t2 ~= nil then return nil, __t2 end
    if __t1 == nil then return nil end
    local va = __t1
    local __t3, __t4 = b
    if __t4 ~= nil then return nil, __t4 end
    if __t3 == nil then return nil end
    local vb = __t3
    return va + vb
end
```

## Language Reference

### Types

| Rua | Lua Runtime |
|-----|-------------|
| `i64` / `i32` / `u64` / … | Lua `integer` |
| `f64` / `f32` | Lua `number` |
| `bool` | Lua `boolean` |
| `String` / `str` | Lua `string` |
| `Vec<T>` | Lua `{ T, T, … }` (array table) |
| `HashMap<K, V>` | Lua `rt.map()` table |
| `Option<T>` | `T` (Some) or `nil` (None) |
| `Result<T, E>` | `T` (Ok) or `nil, E` (Err) |
| `struct` / `enum` | table + metatable |
| `&T` / `&mut T` | same as `T` (types erased) |

### Control Flow

```rua
if n > 0 { "pos" } else if n < 0 { "neg" } else { "zero" }  // expression
while count < 10 { count = count + 1; }
for i in 0..10 { sum = sum + i; }                             // exclusive
for j in 1..=5 { sum = sum + j; }                             // inclusive
loop { if done { break; } }
match val { Some(v) => v, None => 0 }
if let Some(p) = maybe { return p.x; }
while let Some(v) = opt { opt = Some(v + 1); }
```

### Closures

```rua
let inc = |x: i64| x + 1;
let add = |a: i64, b: i64| -> i64 { a + b };
let base = 10;
let offset = |x| x + base;           // captures base by value (fused)
```

### Extern Functions

```rua
extern "lua" {
    pub fn log(message: String);
    pub fn format(template: String, ...) -> String;
}
```

## Tooling

### Compiler (`ruac`)

```bash
ruac build src/main.rua          # compile to src/main.lua
ruac check src/main.rua          # type-check only (no output)
ruac build --builtins-dir ./std  # custom builtins path
```

### Language Server (`rua-lsp`)

VS Code / Neovim support with:

| Feature | Status |
|---------|--------|
| Hover (type info + docs) | ✅ |
| Goto Definition | ✅ |
| Find References | ✅ |
| Rename | ✅ |
| Completions (keywords, locals, paths, members) | ✅ |
| Inlay Hints (type annotations) | ✅ |
| Diagnostics (parse + type + lint) | ✅ |
| Semantic Tokens | ✅ |
| Code Actions | ✅ |
| Folding Ranges | ✅ |
| Document Symbols | ✅ |
| Call Hierarchy | ✅ |
| Type Hierarchy | ✅ |
| Signature Help | ✅ |
| Formatting | ✅ |

### VS Code Setup

```json
{
    "languages": [{
        "id": "rua",
        "extensions": [".rua"],
        "aliases": ["Rua"]
    }],
    "rua-lsp": {
        "command": "target/release/rua-lsp"
    }
}
```

## Project Structure

```
rua/
├── crates/
│   ├── ruac/          # compiler: parse → typeck → codegen
│   ├── rua-syntax/    # lossless CST, parser, lexer, formatter
│   ├── rua-analysis/  # incremental semantic analysis, IDE queries
│   └── rua-lsp/       # LSP server (stdio JSON-RPC)
├── lualib/
│   └── rua_rt.lua     # runtime library (String methods, iterators)
├── tests/
│   ├── demo.rua       # comprehensive syntax demo (600+ lines)
│   └── golden/        # compile-pass (44) + compile-fail (45) snapshots
└── docs/              # design docs, architecture, construction plan
```

## License

MIT
