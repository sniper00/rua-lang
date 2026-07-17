use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let unique = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "ruac-modules-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn write_module_project(root: &Path) {
    fs::write(
        root.join("main.rua"),
        r#"print("root before");
let answer = left::answer(4);
print("result={}", answer);
"#,
    )
    .unwrap();
    fs::write(
        root.join("left.rua"),
        r#"print("left init");

pub fn answer(value: i64) -> i64 {
    right::adjust(value)
}
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("left")).unwrap();
    fs::write(root.join("left/nested.rua"), "print(\"nested init\");\n").unwrap();
    fs::write(
        root.join("right.rua"),
        r#"print("right init");

pub fn adjust(value: i64) -> i64 {
    value + 1
}
"#,
    )
    .unwrap();
}

#[test]
fn cli_modules_mode_uses_plain_require_and_configured_lua_path() {
    let project = TestDir::new("runtime");
    write_module_project(project.path());
    let output_dir = project.path().join("dist");
    let standard_library = workspace_root().join("crates/rua-resources/resources/std");
    let compile = Command::new(env!("CARGO_BIN_EXE_ruac"))
        .args(["build", "main.rua", "--emit", "modules", "--out-dir"])
        .arg(&output_dir)
        .arg("--lua-path")
        .arg(&output_dir)
        .arg("--lua-path")
        .arg(&standard_library)
        .current_dir(project.path())
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr)
    );

    let generated = ["main.lua", "left.lua", "left/nested.lua", "right.lua"];
    for relative in generated {
        assert!(
            output_dir.join(relative).is_file(),
            "missing generated module {relative}"
        );
    }
    let root_lua = fs::read_to_string(output_dir.join("main.lua")).unwrap();
    let left_lua = fs::read_to_string(output_dir.join("left.lua")).unwrap();
    assert!(
        root_lua.contains("local Left = require(\"left\")"),
        "{root_lua}"
    );
    assert!(
        root_lua.contains("local Right = require(\"right\")"),
        "{root_lua}"
    );
    assert!(
        left_lua.contains("local LeftNested = require(\"left.nested\")"),
        "{left_lua}"
    );
    assert!(!root_lua.contains("__rua_mod_"), "{root_lua}");
    assert!(!left_lua.contains("__rua_mod_"), "{left_lua}");
    assert!(root_lua.contains("package.path = "), "{root_lua}");
    for internal in [
        "---@class __rua_module",
        "__rua_link",
        "__rua_allocate",
        "__rua_define",
        "__rua_initialize",
    ] {
        assert!(!root_lua.contains(internal), "{root_lua}");
        assert!(!left_lua.contains(internal), "{left_lua}");
    }
    assert!(!root_lua.contains("package.loaded"), "{root_lua}");

    let run = Command::new(std::env::var("RUA_LUA").unwrap_or_else(|_| "lua".to_string()))
        .arg(output_dir.join("main.lua"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "stdout:\n{}\nstderr:\n{}\nroot Lua:\n{root_lua}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "nested init\nright init\nleft init\nroot before\nresult=5\n"
    );
}

#[test]
fn modules_artifact_has_stable_paths_and_per_file_source_maps() {
    let project = TestDir::new("artifact");
    write_module_project(project.path());

    let artifact = ruac::compile_path_modules_artifact(&project.path().join("main.rua")).unwrap();
    let paths = artifact
        .modules
        .iter()
        .map(|module| module.output_path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        ["main.lua", "left.lua", "left/nested.lua", "right.lua"]
    );
    assert_eq!(artifact.root_output_path, "main.lua");
    assert_eq!(
        artifact
            .modules
            .iter()
            .map(|module| module.module_name.as_str())
            .collect::<Vec<_>>(),
        ["main", "left", "left.nested", "right"]
    );

    for module in &artifact.modules {
        assert!(
            !module.source_map.is_empty(),
            "{} should retain source mappings",
            module.output_path
        );
        assert!(module.source_map.iter().all(|mapping| {
            mapping.generated_start < mapping.generated_end
                && mapping.generated_end <= module.source.len()
        }));
    }
}

#[test]
fn modules_mode_does_not_emit_directory_namespace_files() {
    let project = TestDir::new("directory-namespace");
    fs::create_dir_all(project.path().join("presentation")).unwrap();
    fs::write(
        project.path().join("main.rua"),
        "print(\"{}\", presentation::console::message());\n",
    )
    .unwrap();
    fs::write(
        project.path().join("presentation/console.rua"),
        "pub fn message() -> String { \"hello\" }\n",
    )
    .unwrap();

    let artifact = ruac::compile_path_modules_artifact(&project.path().join("main.rua")).unwrap();
    assert_eq!(
        artifact
            .modules
            .iter()
            .map(|module| module.output_path.as_str())
            .collect::<Vec<_>>(),
        ["main.lua", "presentation/console.lua"]
    );
    let root = &artifact.modules[0].source;
    assert!(
        root.starts_with(
            "-- Generated by ruac (Rua -> Lua 5.5 modules). Do not edit by hand.\n\n\
             local rua_std = require(\"rua_std\")\n\
             assert(rua_std.ABI_VERSION == 2, \"incompatible rua_std ABI\")\n\n\
             local PresentationConsole = require(\"presentation.console\")\n\n\
             local fmt = rua_std.fmt\n\n"
        ),
        "{root}"
    );
    assert!(root.contains("require(\"presentation.console\")"), "{root}");
    assert!(!root.contains("require(\"presentation\")"), "{root}");
    assert!(root.contains("---@class Main\nlocal Main = {}"), "{root}");
    assert!(!root.contains("__rua_module"), "{root}");
    let console = artifact
        .modules
        .iter()
        .find(|module| module.output_path == "presentation/console.lua")
        .unwrap();
    assert!(
        console
            .source
            .contains("---@class presentation.Console\nlocal Console = {}"),
        "{}",
        console.source
    );
    assert!(console.source.ends_with("return Console\n"));

    let output_dir = project.path().join("dist");
    let standard_library = workspace_root().join("crates/rua-resources/resources/std");
    let compile = Command::new(env!("CARGO_BIN_EXE_ruac"))
        .args(["build", "main.rua", "--emit", "modules", "--out-dir"])
        .arg(&output_dir)
        .arg("--lua-path")
        .arg(&output_dir)
        .arg("--lua-path")
        .arg(&standard_library)
        .current_dir(project.path())
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr)
    );
    assert!(!output_dir.join("presentation.lua").exists());

    let run = Command::new(std::env::var("RUA_LUA").unwrap_or_else(|_| "lua".to_string()))
        .arg(output_dir.join("main.lua"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "hello\n");
}

#[test]
fn modules_flatten_one_public_type_but_wrap_multiple_public_types() {
    let project = TestDir::new("public-type-layout");
    fs::create_dir_all(project.path().join("domain")).unwrap();
    fs::write(
        project.path().join("main.rua"),
        r#"let DomainProduct = "local";
print("{}", DomainProduct);
let product = domain::product::Product::new("book");
print("{}", product.name);
let first = domain::pair::First::new();
print("{}", first.value);
"#,
    )
    .unwrap();
    fs::write(
        project.path().join("domain/product.rua"),
        r#"pub struct Product {
    pub name: String,
}

impl Product {
    pub fn new(name: String) -> Product {
        Product { name: name }
    }
}
"#,
    )
    .unwrap();
    fs::write(
        project.path().join("domain/pair.rua"),
        r#"pub struct First {
    pub value: i64,
}

pub struct Second {
    pub value: i64,
}

impl First {
    pub fn new() -> First {
        First { value: 1 }
    }
}
"#,
    )
    .unwrap();

    let artifact = ruac::compile_path_modules_artifact(&project.path().join("main.rua")).unwrap();
    let root = artifact
        .modules
        .iter()
        .find(|module| module.output_path == "main.lua")
        .unwrap();
    let product = artifact
        .modules
        .iter()
        .find(|module| module.output_path == "domain/product.lua")
        .unwrap();
    let pair = artifact
        .modules
        .iter()
        .find(|module| module.output_path == "domain/pair.lua")
        .unwrap();

    assert!(
        product
            .source
            .contains("---@class domain.Product\n---@field name string\nlocal Product = tbcreate"),
        "{}",
        product.source
    );
    assert!(
        !product.source.contains("Product.Product"),
        "{}",
        product.source
    );
    assert!(
        product.source.contains("function Product.new("),
        "{}",
        product.source
    );
    assert!(
        product
            .source
            .contains("Product.__index = Product\n\n---@return domain.Product"),
        "{}",
        product.source
    );
    assert!(
        product
            .source
            .contains("---@return domain.Product\nfunction Product.new("),
        "{}",
        product.source
    );
    assert!(
        product.source.ends_with("end\n\nreturn Product\n"),
        "{}",
        product.source
    );
    assert!(
        root.source.contains("DomainProduct.new(\"book\")"),
        "{}",
        root.source
    );
    assert!(
        root.source
            .contains("local DomainProduct = require(\"domain.product\")"),
        "{}",
        root.source
    );
    assert!(
        root.source
            .contains("local DomainProduct__local = \"local\""),
        "{}",
        root.source
    );

    assert!(pair.source.contains("local Pair = {}"), "{}", pair.source);
    assert!(pair.source.contains("Pair.First"), "{}", pair.source);
    assert!(pair.source.contains("Pair.Second"), "{}", pair.source);
    assert!(
        pair.source
            .contains("Pair.First.__index = Pair.First\n\n---@class Second"),
        "{}",
        pair.source
    );
    assert!(
        pair.source.ends_with("end\n\nreturn Pair\n"),
        "{}",
        pair.source
    );
}

#[test]
fn modules_mode_rejects_cyclic_lua_requires() {
    let project = TestDir::new("cycle");
    fs::write(
        project.path().join("main.rua"),
        "fn root_value() -> i64 { 1 }\nprint(\"{}\", child::value());\n",
    )
    .unwrap();
    fs::write(
        project.path().join("child.rua"),
        "pub fn value() -> i64 { root_value() }\n",
    )
    .unwrap();

    let error = ruac::compile_path_modules_artifact(&project.path().join("main.rua"))
        .expect_err("plain Lua require output must reject dependency cycles");
    assert!(
        error.contains("cyclic Lua require dependency: main -> child -> main"),
        "{error}"
    );
}

#[test]
fn modules_mode_requires_an_output_directory_and_rejects_bundle_output() {
    let project = TestDir::new("arguments");
    fs::write(project.path().join("main.rua"), "print(\"ok\");\n").unwrap();

    let missing_dir = Command::new(env!("CARGO_BIN_EXE_ruac"))
        .args(["build", "main.rua", "--emit", "modules"])
        .current_dir(project.path())
        .output()
        .unwrap();
    assert!(!missing_dir.status.success());
    assert!(String::from_utf8_lossy(&missing_dir.stderr).contains("requires `--out-dir <dir>`"));

    let mixed_outputs = Command::new(env!("CARGO_BIN_EXE_ruac"))
        .args([
            "build",
            "main.rua",
            "--emit",
            "modules",
            "--out-dir",
            "dist",
            "-o",
            "main.lua",
        ])
        .current_dir(project.path())
        .output()
        .unwrap();
    assert!(!mixed_outputs.status.success());
    assert!(String::from_utf8_lossy(&mixed_outputs.stderr).contains("cannot be used"));

    let bundle_lua_path = Command::new(env!("CARGO_BIN_EXE_ruac"))
        .args(["build", "main.rua", "--lua-path", "dist"])
        .current_dir(project.path())
        .output()
        .unwrap();
    assert!(bundle_lua_path.status.success());
    let bundle = fs::read_to_string(project.path().join("main.lua")).unwrap();
    assert!(
        bundle.contains(&format!("{}/dist/?.lua", project.path().display())),
        "{bundle}"
    );
}

#[test]
fn cli_rejects_legacy_mod_syntax() {
    let project = TestDir::new("legacy-mod");
    fs::write(project.path().join("main.rua"), "mod child;\n").unwrap();
    fs::write(
        project.path().join("child.rua"),
        "pub fn value() -> i64 { 1 }\n",
    )
    .unwrap();

    let check = Command::new(env!("CARGO_BIN_EXE_ruac"))
        .args(["check", "main.rua"])
        .current_dir(project.path())
        .output()
        .unwrap();
    assert!(!check.status.success());
    assert!(
        String::from_utf8_lossy(&check.stderr).contains("expected expression, found `mod`"),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
}
