# Lua 堆栈转换

Rua 编译器生成 Lua，而 Lua 运行时错误通常只携带生成文件和 Lua 行号。`ruac::stacktrace`
提供一个独立的 host-side 转换层：它解析 Lua traceback，再使用编译产物的
`LuaSourceMapping` 将生成行映射回 Rua 的 `SourceRange`。

## API

```rust
use ruac::stacktrace::convert_lua_traceback;

let traceback = std::fs::read_to_string("lua-error.txt")?;
let artifact = ruac::compile_path_artifact(std::path::Path::new("src/main.rua"))?;
let files = vec!["src/main.rua".to_string()];

let converted = convert_lua_traceback(
    &traceback,
    &artifact.source,
    &artifact.source_map,
    &files,
);

println!("{}", converted.message);
for frame in converted.frames {
    match (frame.rua_file, frame.rua_range) {
        (Some(file), Some(range)) => {
            println!("{}:{}: {}", file, range.line, frame.lua.raw);
        }
        _ => println!("{}", frame.lua.raw),
    }
}
```

`parse_lua_traceback` 负责解析 Lua 默认格式，包括：

- `lua: main.lua:12: message` 错误首行；
- `stack traceback:` 后的 `file.lua:line: in ...` frame；
- Windows drive path 中的冒号；
- `[C]: in ?` 等没有 Rua source map 的 runtime frame。

`convert_lua_traceback` 对每个有 Lua 行号的 frame 查找 generated source map。映射成功时
返回 Rua 文件、`FileId` 和原始 `SourceRange`；生成头、标准库、外部 Lua library 和 C frame
保持为 unmapped frame，不会被伪造为 Rua 位置。

编译 artifact 的 `source_files` 与 `SourceRange.file` 使用同一索引：`source_files[file]` 是
Rua 源文件路径。宿主应直接使用 artifact 提供的表，不要按 module 名称或 traceback 文本
重新推断源文件身份。

## 高频位置映射

日志、采样 profiler 和调试器会反复查询同一个 generated chunk。此类调用不应为每个位置
重新遍历 `LuaSourceMapping`。宿主应在加载 chunk 时构建一次 `LuaSourceMapIndex`：

```rust
use ruac::stacktrace::LuaSourceMapIndex;

let index = LuaSourceMapIndex::new(&artifact.source, &artifact.source_map);

// Hot path: O(1), no allocation.
if let Some(range) = index.map_line(generated_lua_line) {
    let rua_file = &artifact.source_files[range.file as usize];
    println!("{}:{}", rua_file, range.line);
}
```

`LuaSourceMapIndex::new` 保持普通转换相同的重叠规则：优先选择 generated span 最小的
mapping，span 相同时选择 `generated_start` 更小的 mapping。索引只属于 host 的运行时状态，
不写入 artifact，也不改变 artifact 版本或序列化格式。

`convert_lua_traceback` 会为一次 traceback 构建一个索引并供其中所有 frame 共享。
`convert_lua_frame` 是一次性便利接口；高频调用应改用缓存索引配合
`convert_lua_frame_with_index`，或直接调用 `map_line`。

## Bundle 与 modules

bundle 输出只有一个 generated source 和 source map，直接调用一次转换即可。modules 输出
为每个生成 Lua 文件保留独立的 `GeneratedLuaModule::source_map`；host 应先根据 traceback
中的 chunk path 选择对应 module，再调用转换函数。跨模块聚合 dispatch、Lua 进程启动和
DAP 协议不属于当前 API，后续调试器可以在这一层之上复用解析与映射结果。

## 映射边界

当前 source map 以 compiler statement anchor 为粒度，Rua range 保留字节偏移和源文件行号；
没有映射的生成行是正常情况。调用方应展示原始 Lua frame 作为 fallback，而不是丢弃整个
traceback。后续 source map v2 可以在不改变本 API 基本语义的情况下增加 generated/source
列号、反向断点索引和序列化 sidecar。
