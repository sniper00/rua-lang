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
        r#"println!("root before");
let answer = left::answer(4);
println!("result={}", answer);
"#,
    )
    .unwrap();
    fs::write(
        root.join("left.rua"),
        r#"println!("left init");

pub fn answer(value: i64) -> i64 {
    right::adjust(value)
}
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("left")).unwrap();
    fs::write(root.join("left/nested.rua"), "println!(\"nested init\");\n").unwrap();
    fs::write(
        root.join("right.rua"),
        r#"println!("right init");

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
        root_lua.contains("local __rua_dep_left = require(\"left\")"),
        "{root_lua}"
    );
    assert!(
        root_lua.contains("local __rua_dep_right = require(\"right\")"),
        "{root_lua}"
    );
    assert!(
        left_lua.contains("local __rua_dep_left_nested = require(\"left.nested\")"),
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
fn modules_mode_rejects_cyclic_lua_requires() {
    let project = TestDir::new("cycle");
    fs::write(
        project.path().join("main.rua"),
        "fn root_value() -> i64 { 1 }\nprintln!(\"{}\", child::value());\n",
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
    fs::write(project.path().join("main.rua"), "println!(\"ok\");\n").unwrap();

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
