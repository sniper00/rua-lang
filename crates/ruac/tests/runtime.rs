//! End-to-end Rua -> Lua runtime tests.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let unique = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "ruac-runtime-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create runtime test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonicalize workspace root")
}

fn lua_program() -> String {
    std::env::var("RUA_LUA").unwrap_or_else(|_| "lua".to_string())
}

fn run_lua(script: &Path) -> Output {
    let runtime_pattern = workspace_root().join("crates/rua-resources/resources/std/?.lua");
    Command::new(lua_program())
        .arg(script)
        .env("LUA_PATH", format!("{};;", runtime_pattern.display()))
        .output()
        .expect("run Lua; set RUA_LUA to the Lua 5.5 executable when it is not on PATH")
}

fn assert_success(output: &Output, expected_stdout: &str, lua: &str) {
    assert!(
        output.status.success(),
        "generated Lua failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}\nLua:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        lua
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected_stdout);
}

fn compile_and_run(label: &str, source: &str) -> (String, Output) {
    compile_and_run_with_prelude(label, "", source)
}

fn compile_and_run_with_prelude(label: &str, prelude: &str, source: &str) -> (String, Output) {
    let root = workspace_root();
    let lua = ruac::compile_str_with_std(source, &root.join("crates/rua-resources/resources/std"))
        .unwrap_or_else(|error| panic!("compile {label}: {error}"));
    let temp = TempDir::new(label);
    let script = temp.path().join("main.lua");
    fs::write(&script, format!("{prelude}\n{lua}")).expect("write generated Lua");
    let output = run_lua(&script);
    (lua, output)
}

#[test]
fn runtime_harness_executes_lua() {
    let temp = TempDir::new("harness");
    let script = temp.path().join("smoke.lua");
    fs::write(&script, "print('harness-ok')\n").expect("write smoke Lua");
    let output = run_lua(&script);
    assert_success(&output, "harness-ok\n", "print('harness-ok')");
}

#[test]
fn generated_artifact_checks_runtime_abi() {
    let lua = ruac::compile_str("println!(\"abi\");").unwrap();
    assert!(
        lua.contains("assert(rua_std.ABI_VERSION == 1, \"incompatible rua_std ABI\")"),
        "{lua}"
    );
}

#[test]
fn first_class_iterators_execute_without_fusion() {
    let source = r#"
let mut pending = vec![1, 2, 3].iter().map(|value| value * 2).skip(1);
println!("{}", pending.next().unwrap());
println!("{}", pending.next().unwrap());

let mut characters = "你a".chars();
println!("{}", characters.next().unwrap());

let range = (1..4).map(|value| value + 1);
for value in range {
    println!("{}", value);
}
"#;
    let (lua, output) = compile_and_run("first-class-iterator", source);
    assert_success(&output, "4\n6\n你\n2\n3\n4\n", &lua);
    assert!(
        lua.contains(":map("),
        "stored adapter was unexpectedly fused: {lua}"
    );
    assert!(
        lua.contains(":next()"),
        "runtime iterator protocol was not used: {lua}"
    );
}

#[test]
fn missing_lua_extern_fails_at_load_instead_of_becoming_a_noop() {
    let source = "extern \"lua\" { fn absent(value: i64) -> i64; }\nabsent(1);";
    let lua = ruac::compile_str(source).unwrap();
    assert!(
        lua.contains("assert(_G[\"absent\"], \"missing Lua extern `absent`\")"),
        "{lua}"
    );
    let temp = TempDir::new("missing-extern");
    let script = temp.path().join("main.lua");
    fs::write(&script, &lua).expect("write generated Lua");
    let output = run_lua(&script);
    assert!(
        !output.status.success(),
        "missing extern unexpectedly succeeded"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("missing Lua extern `absent`"),
        "stderr:\n{}\nLua:\n{lua}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn provided_lua_extern_binds_the_host_global() {
    let (lua, output) = compile_and_run(
        "provided-extern",
        "extern \"lua\" { fn print(value: String); }\nprint(\"extern-ok\");",
    );
    assert_success(&output, "extern-ok\n", &lua);
}

#[test]
fn lua_result_extern_adapts_host_multi_returns_and_rua_tagged_arguments() {
    let (lua, output) = compile_and_run_with_prelude(
        "result-extern",
        r#"
        function host_fetch(ok)
            if ok then return 42, nil end
            return nil, "boom"
        end

        function host_echo(value, error)
            if error ~= nil then return nil, "echo:" .. error end
            return value + 1, nil
        end

        function host_nested(value)
            return value * 2, nil
        end
        "#,
        r#"
        extern "lua-result" {
            fn host_fetch(ok: bool) -> Result<i64, String>;
            fn host_echo(input: Result<i64, String>) -> Result<i64, String>;
        }

        mod nested {
            extern "lua-result" {
                fn host_nested(value: i64) -> Result<i64, String>;
            }
            pub fn run(value: i64) -> Result<i64, String> { host_nested(value) }
        }

        match host_fetch(true) {
            Ok(value) => println!("{}", value),
            Err(error) => println!("{}", error),
        }
        match host_fetch(false) {
            Ok(value) => println!("{}", value),
            Err(error) => println!("{}", error),
        }
        match host_echo(Ok(9)) {
            Ok(value) => println!("{}", value),
            Err(error) => println!("{}", error),
        }
        match host_echo(Err("bad")) {
            Ok(value) => println!("{}", value),
            Err(error) => println!("{}", error),
        }
        match nested::run(6) {
            Ok(value) => println!("{}", value),
            Err(error) => println!("{}", error),
        }
        "#,
    );
    assert_success(&output, "42\nboom\n10\necho:bad\n12\n", &lua);
    assert!(lua.contains("local function host_fetch(ok)"), "{lua}");
    assert!(lua.contains("local function host_echo(input)"), "{lua}");
    assert!(lua.contains("function nested.host_nested(value)"), "{lua}");
}

#[test]
fn top_level_chunk_runs_without_main() {
    let (lua, output) = compile_and_run("top-level", r#"println!("top-level");"#);
    assert_success(&output, "top-level\n", &lua);
}

#[test]
fn function_named_main_is_not_an_entry_point() {
    let (lua, output) = compile_and_run(
        "ordinary-main",
        r#"
        fn main() { println!("wrong"); }
        println!("chunk");
        "#,
    );
    assert_success(&output, "chunk\n", &lua);
}

#[test]
fn root_and_module_initializers_preserve_source_order() {
    let (lua, output) = compile_and_run(
        "initializer-order",
        r#"
        println!("root-before");
        mod api {
            println!("module");
            pub fn answer() -> i64 { 42 }
        }
        println!("root-after {}", api::answer());
        "#,
    );
    assert_success(&output, "root-before\nmodule\nroot-after 42\n", &lua);
}

#[test]
fn module_sibling_calls_use_resolved_target() {
    let (lua, output) = compile_and_run(
        "module-sibling",
        r#"
        mod api {
            fn add(a: i64, b: i64) -> i64 { a + b }
            pub fn double(value: i64) -> i64 { add(value, value) }
        }

        println!("{}", api::double(4));
        "#,
    );
    assert_success(&output, "8\n", &lua);
}

#[test]
fn nested_module_uses_one_parent_owned_table() {
    let (lua, output) = compile_and_run(
        "nested-module",
        r#"
        mod outer {
            pub mod inner {
                pub fn answer() -> i64 { 42 }
            }
        }

        println!("{}", outer::inner::answer());
        "#,
    );
    assert_success(&output, "42\n", &lua);
}

#[test]
fn module_local_type_uses_one_backend_place() {
    let (lua, output) = compile_and_run(
        "module-type",
        r#"
        mod geo {
            pub struct Point { x: i64, y: i64 }
            impl Point {
                pub fn new(x: i64, y: i64) -> Point { Point { x: x, y: y } }
            }
        }

        let point = geo::Point::new(2, 3);
        println!("{}", point.x + point.y);
        "#,
    );
    assert_success(&output, "5\n", &lua);
}

#[test]
fn variant_aliases_use_identity_in_construction_and_patterns() {
    let (lua, output) = compile_and_run(
        "variant-alias",
        r#"
        mod api {
            pub enum Event {
                Ready,
                Code(i64),
                Move { x: i64 },
            }
        }
        use api::Event::Ready as R;
        use api::Event::Code as C;
        use api::Event::Move as M;

        let ready = R;
        let code = C(7);
        let movement = M { x: 5 };
        match ready { R => println!("ready") }
        match code { C(value) => println!("{}", value) }
        match movement { M { x } => println!("{}", x) }
        "#,
    );
    assert_success(&output, "ready\n7\n5\n", &lua);
}

#[test]
fn mutual_recursion_uses_one_lexical_binding_per_function() {
    let (lua, output) = compile_and_run(
        "mutual-recursion",
        r#"
        fn is_even(value: i64) -> bool {
            if value == 0 { true } else { is_odd(value - 1) }
        }
        fn is_odd(value: i64) -> bool {
            if value == 0 { false } else { is_even(value - 1) }
        }

        println!("{}", is_even(10));
        "#,
    );
    assert_success(&output, "true\n", &lua);
    assert!(
        lua.lines().any(|line| line == "local is_even, is_odd"),
        "{lua}"
    );
    assert!(lua.contains("\nfunction is_even(value)"), "{lua}");
    assert!(lua.contains("\nfunction is_odd(value)"), "{lua}");
    assert!(!lua.contains("local function is_even"), "{lua}");
    assert!(!lua.contains("local function is_odd"), "{lua}");
}

#[test]
fn acyclic_forward_reference_is_reordered_without_declaration() {
    let (lua, output) = compile_and_run(
        "acyclic-forward-reference",
        r#"
        fn caller() -> i64 { callee() }
        fn independent() -> i64 { 7 }
        fn callee() -> i64 { 42 }

        println!("{} {}", caller(), independent());
        "#,
    );
    assert_success(&output, "42 7\n", &lua);

    let callee = lua
        .find("---@return integer\nlocal function callee()")
        .unwrap_or_else(|| panic!("callee annotation is detached from local function:\n{lua}"));
    let caller = lua
        .find("---@return integer\nlocal function caller()")
        .unwrap_or_else(|| panic!("caller annotation is detached from local function:\n{lua}"));
    assert!(
        callee < caller,
        "callee must be in scope before caller:\n{lua}"
    );
    assert!(!lua.lines().any(|line| line == "local caller, callee"));
    assert!(!lua.lines().any(|line| line == "local caller"));
}

#[test]
fn direct_recursion_uses_local_function_without_forward_declaration() {
    let (lua, output) = compile_and_run(
        "direct-recursion",
        r#"
        fn factorial(value: i64) -> i64 {
            if value <= 1 { 1 } else { value * factorial(value - 1) }
        }
        println!("{}", factorial(5));
        "#,
    );
    assert_success(&output, "120\n", &lua);
    assert!(
        lua.contains("---@return integer\nlocal function factorial(value)"),
        "{lua}"
    );
    assert!(!lua.lines().any(|line| line == "local factorial"));
}

#[test]
fn module_function_captures_root_local_function() {
    let (lua, output) = compile_and_run(
        "module-to-root-function",
        r#"
        mod api {
            pub fn answer() -> i64 { root_answer() }
        }
        fn root_answer() -> i64 { 42 }

        println!("{}", api::answer());
        "#,
    );
    assert_success(&output, "42\n", &lua);
    let root = lua.find("local function root_answer()").unwrap();
    let module = lua.find("function api.answer()").unwrap();
    assert!(
        root < module,
        "root local must precede module function:\n{lua}"
    );
}

#[test]
fn user_method_dispatch_is_not_shadowed_by_same_named_field() {
    let (lua, output) = compile_and_run(
        "field-method-collision",
        r#"
        trait Named { fn name(&self) -> String; }
        struct Person { name: String }
        impl Named for Person {
            fn name(&self) -> String { self.name }
        }

        fn concrete(person: &Person) -> String { person.name() }
        fn generic<T: Named>(value: T) -> String { value.name() }
        fn dynamic(value: &dyn Named) -> String { value.name() }

        let person = Person { name: "Rua" };
        println!("{} {} {}", concrete(person), generic(person), dynamic(person));
        "#,
    );
    assert_success(&output, "Rua Rua Rua\n", &lua);
    assert!(lua.contains("Person.name(person)"), "{lua}");
    assert!(lua.contains("getmetatable(value).name(value)"), "{lua}");
    assert!(!lua.contains("person:name()"), "{lua}");
    assert!(!lua.contains("value:name()"), "{lua}");
}

#[test]
fn option_map_inlines_without_allocating_an_unused_closure() {
    let (lua, output) = compile_and_run(
        "option-map-inline",
        r#"
        let mapped = Option::Some(4).map(|value| value + 1);
        match mapped {
            Option::Some(value) => println!("{}", value),
            Option::None => println!("none"),
        }
        "#,
    );
    assert_success(&output, "5\n", &lua);
    assert!(!lua.contains("local function __t"), "{lua}");
}

#[test]
fn returning_match_uses_direct_if_elseif_control_flow() {
    let (lua, output) = compile_and_run(
        "returning-match",
        r#"
        enum Color { Red, Green, Blue }
        fn name(color: Color) -> String {
            match color {
                Color::Red => "red",
                Color::Green => "green",
                Color::Blue => "blue",
            }
        }
        println!("{}", name(Color::Green));
        "#,
    );
    assert_success(&output, "green\n", &lua);
    assert!(lua.contains("local __t1 = color.tag"), "{lua}");
    assert!(lua.contains("elseif __t1 == \"Green\" then"), "{lua}");
    assert!(!lua.contains("== \"Blue\""), "{lua}");
    assert!(!lua.contains("not __t"), "{lua}");
}

#[test]
fn fused_iterator_uses_nested_guards_without_active_flag() {
    let (lua, output) = compile_and_run(
        "iterator-guards",
        r#"
        let total = vec![1, 2, 3, 4]
            .iter()
            .map(|value| value * 2)
            .filter(|value| value > 4)
            .fold(0, |sum, value| sum + value);
        println!("{}", total);
        "#,
    );
    assert_success(&output, "14\n", &lua);
    assert!(
        !lua.lines().any(|line| {
            let line = line.trim();
            line.starts_with("local __t")
                && (line.ends_with(" = true") || line.ends_with(" = false"))
        }),
        "{lua}"
    );
}

#[test]
fn exact_size_collect_uses_lua_table_capacity_without_changing_vec_layout() {
    let (lua, output) = compile_and_run(
        "iterator-table-capacity",
        r#"
        let mapped = vec![1, 2, 3]
            .iter()
            .map(|value| value * 10)
            .collect::<Vec<i64>>();
        println!("{} {} {}", mapped.len(), mapped[0], mapped[2]);
        "#,
    );
    assert_success(&output, "3 10 30\n", &lua);
    assert!(
        lua.contains("local __rua_table_create = table.create"),
        "{lua}"
    );
    assert!(lua.contains("vec.from_table(__rua_table_create("), "{lua}");
}

#[test]
fn unused_iterator_with_capture_side_effect_is_not_eliminated() {
    let (lua, output) = compile_and_run(
        "iterator-effect-dce",
        r#"
        let mut calls = 0;
        let unused = vec![1, 2, 3]
            .iter()
            .map(|value| {
                calls = calls + 1;
                value * 2
            })
            .collect::<Vec<i64>>();
        println!("{}", calls);
        "#,
    );
    assert_success(&output, "3\n", &lua);
    assert!(lua.contains("calls = calls + 1"), "{lua}");
}

#[test]
fn result_remains_tagged_after_storage() {
    let (lua, output) = compile_and_run(
        "result-storage",
        r#"
        fn make_result(ok: bool) -> Result<i64, String> {
            if ok { Ok(7) } else { Err("failed") }
        }

        let result = make_result(false);
        match result {
            Ok(value) => println!("ok {}", value),
            Err(message) => println!("err {}", message),
        }
        "#,
    );
    assert_success(&output, "err failed\n", &lua);
}

#[test]
fn match_guard_failure_continues_to_later_arm() {
    let (lua, output) = compile_and_run(
        "match-guard-fallback",
        r#"
        fn classify(value: i64) -> String {
            match value {
                candidate if candidate > 0 => "positive",
                0 => "zero",
                _ => "negative",
            }
        }

        println!("{}", classify(2));
        println!("{}", classify(0));
        println!("{}", classify(-2));
        "#,
    );
    assert_success(&output, "positive\nzero\nnegative\n", &lua);
}

#[test]
fn same_named_traits_in_modules_keep_default_method_identity() {
    let (lua, output) = compile_and_run(
        "module-trait-identity",
        r#"
        mod left {
            pub trait Named { fn name(&self) -> String { "left" } }
            pub struct Item {}
            impl Named for Item {}
            pub fn make() -> Item { Item {} }
        }
        mod right {
            pub trait Named { fn name(&self) -> String { "right" } }
            pub struct Item {}
            impl Named for Item {}
            pub fn make() -> Item { Item {} }
        }

        println!("{}", left::make().name());
        println!("{}", right::make().name());
        "#,
    );
    assert_success(&output, "left\nright\n", &lua);
}

#[test]
fn lua_keywords_are_mangled_without_changing_rua_semantics() {
    let (lua, output) = compile_and_run(
        "lua-keywords",
        r#"
struct Holder { end: i64 }

impl Holder {
    fn repeat(&self) -> i64 { self.end }
}

fn end(repeat: i64) -> i64 {
    let until = repeat + 1;
    until
}

let value = Holder { end: end(4) };
println!("{}", value.repeat());
"#,
    );
    assert_success(&output, "5\n", &lua);
}

#[test]
fn module_and_local_with_same_name_get_distinct_backend_places() {
    let (lua, output) = compile_and_run(
        "namespace-collision",
        r#"
mod api {
    pub fn value() -> i64 { 40 }
}

let api = 2;
println!("{}", api + api::value());
"#,
    );
    assert_success(&output, "42\n", &lua);
}

#[test]
fn result_remains_tagged_in_vec_and_with_nil_payload() {
    let (lua, output) = compile_and_run(
        "result-containers",
        r#"
        let values = vec![Err("inside")];
        match values[0] {
            Ok(_) => println!("wrong"),
            Err(message) => println!("{}", message),
        }

        let nullable = Ok(None);
        match nullable {
            Ok(value) => if let None = value { println!("nil-ok"); },
            Err(_) => println!("wrong"),
        }
        "#,
    );
    assert_success(&output, "inside\nnil-ok\n", &lua);
}

#[test]
fn require_returns_public_exports_after_initialization() {
    let root = workspace_root();
    let source = r#"
        pub fn answer() -> i64 { 42 }
    "#;
    let lua = ruac::compile_str_with_std(source, &root.join("crates/rua-resources/resources/std"))
        .expect("compile export module");
    let temp = TempDir::new("exports");
    let module = temp.path().join("module.lua");
    let runner = temp.path().join("runner.lua");
    fs::write(&module, &lua).expect("write generated module");
    fs::write(
        &runner,
        format!(
            "local exports = dofile({:?})\nprint(exports.answer())\n",
            module.display().to_string()
        ),
    )
    .expect("write export runner");
    let output = run_lua(&runner);
    assert_success(&output, "42\n", &lua);
}

#[test]
fn require_exports_public_types_and_modules() {
    let source = r#"
        pub struct Point { x: i64 }
        pub mod api { pub fn answer() -> i64 { 42 } }
    "#;
    let lua = ruac::compile_str(source).expect("compile public exports");
    let temp = TempDir::new("item-exports");
    let module = temp.path().join("module.lua");
    let runner = temp.path().join("runner.lua");
    fs::write(&module, &lua).unwrap();
    fs::write(
        &runner,
        format!(
            "local exports = dofile({:?})\nprint(exports.api.answer())\nprint(type(exports.Point))\n",
            module.display().to_string()
        ),
    )
    .unwrap();
    let output = run_lua(&runner);
    assert_success(&output, "42\ntable\n", &lua);
}
