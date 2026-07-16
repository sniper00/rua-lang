use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{collections::BTreeMap, fmt};

use rua_project::{
    FileId, LibraryMount, LogicalSourcePath, ProjectId, ProjectSpec, SourceProvider, SourceRootId,
    SourceRootKind, SourceRootSpec, SourceText,
};

struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "ruac-sysroot-{name}-{}-{nonce}",
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

#[test]
fn embedded_sysroot_is_cwd_independent_and_not_emitted() {
    let lua = ruac::compile_str("pub fn answer() -> i64 { 42 }").unwrap();
    assert!(lua.contains("function answer()"));
    assert!(!lua.contains("__rua_builtin"));
    assert!(!lua.contains("local Result ="));
}

#[test]
fn explicit_sysroot_failures_do_not_fall_back() {
    let root = TestDir::new("invalid");
    let missing = root.path().join("missing");
    let error = ruac::compile_str_with_std("fn main() {}", &missing).unwrap_err();
    assert!(error.contains("std.toml"), "{error}");

    let empty = root.path().join("empty");
    fs::create_dir(&empty).unwrap();
    let error = ruac::compile_str_with_std("fn main() {}", &empty).unwrap_err();
    assert!(error.contains("std.toml"), "{error}");

    let invalid = root.path().join("invalid");
    fs::create_dir(&invalid).unwrap();
    fs::write(invalid.join("std.toml"), "version =").unwrap();
    let error = ruac::compile_str_with_std("fn main() {}", &invalid).unwrap_err();
    assert!(error.contains("parsing std.toml"), "{error}");
}

#[test]
fn cli_loads_explicit_sysroot_outside_repository_cwd() {
    let root = TestDir::new("cli");
    let input = root.path().join("input.rua");
    let output = root.path().join("output.lua");
    fs::write(&input, "pub fn answer() -> i64 { 42 }").unwrap();

    let standard_library =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../rua-resources/resources/std");
    let result = Command::new(env!("CARGO_BIN_EXE_ruac"))
        .arg("build")
        .arg(&input)
        .arg("-o")
        .arg(&output)
        .arg("--std-path")
        .arg(&standard_library)
        .current_dir(root.path())
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        fs::read_to_string(output)
            .unwrap()
            .contains("function answer()")
    );
}

#[test]
fn cli_discovers_ruarc_toml_and_builds_with_bulk_lua_library() {
    let root = TestDir::new("external-library-config");
    let workspace = root.path().join("workspace");
    let declarations = root.path().join("declarations");
    let runtime = root.path().join("runtime");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(declarations.join("moon")).unwrap();
    fs::create_dir_all(runtime.join("moon")).unwrap();
    fs::write(
        workspace.join("main.rua"),
        r#"let actor: i64 = moon::query("bootstrap");
let status: String = moon::http::get("/health");
"#,
    )
    .unwrap();
    fs::write(
        declarations.join("moon.ruai"),
        "extern \"lua\" { pub fn query(name: String) -> i64; }\n",
    )
    .unwrap();
    fs::write(
        declarations.join("moon/http.ruai"),
        "extern \"lua\" { pub fn get(path: String) -> String; }\n",
    )
    .unwrap();
    fs::write(
        declarations.join("moon/unused.ruai"),
        "extern \"lua\" { pub fn never_loaded() -> i64; }\n",
    )
    .unwrap();
    fs::write(
        runtime.join("moon.lua"),
        "return { query = function(name) assert(name == \"bootstrap\"); return 7 end }\n",
    )
    .unwrap();
    fs::write(
        runtime.join("moon/http.lua"),
        "return { get = function(path) assert(path == \"/health\"); return \"ok\" end }\n",
    )
    .unwrap();
    fs::write(
        workspace.join(rua_project::PROJECT_CONFIG_FILE),
        "[[workspace.lua_library]]\ndeclaration_root = \"../declarations\"\nruntime_root = \"../runtime\"\n",
    )
    .unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_ruac"))
        .arg("build")
        .arg("main.rua")
        .current_dir(&workspace)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let lua = fs::read_to_string(workspace.join("main.lua")).unwrap();
    assert!(lua.contains("require(\"moon\")"), "{lua}");
    assert!(lua.contains("require(\"moon.http\")"), "{lua}");
    assert!(!lua.contains("require(\"moon.unused\")"), "{lua}");
    assert!(lua.contains(".query(\"bootstrap\")"), "{lua}");
    assert!(lua.contains(".get(\"/health\")"), "{lua}");
    assert!(lua.contains("../runtime/?.lua"), "{lua}");
    let run = Command::new(std::env::var("RUA_LUA").unwrap_or_else(|_| "lua".to_string()))
        .arg(workspace.join("main.lua"))
        .current_dir(&workspace)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "stdout: {}\nstderr: {}\nLua:\n{lua}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let modules_dir = workspace.join("dist");
    let result = Command::new(env!("CARGO_BIN_EXE_ruac"))
        .args(["build", "main.rua", "--emit", "modules", "--out-dir"])
        .arg(&modules_dir)
        .current_dir(&workspace)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let modular_lua = fs::read_to_string(modules_dir.join("main.lua")).unwrap();
    assert!(modular_lua.contains("package.path = "), "{modular_lua}");
    assert!(modular_lua.contains("../runtime/?.lua"), "{modular_lua}");
    assert!(modular_lua.contains("require(\"moon\")"), "{modular_lua}");
    assert!(
        modular_lua.contains("require(\"moon.http\")"),
        "{modular_lua}"
    );
    assert!(
        !modular_lua.contains("require(\"moon.unused\")"),
        "{modular_lua}"
    );
    assert!(
        modular_lua.contains(".query(\"bootstrap\")") && modular_lua.contains(".get(\"/health\")"),
        "{modular_lua}"
    );
    let run = Command::new(std::env::var("RUA_LUA").unwrap_or_else(|_| "lua".to_string()))
        .arg(modules_dir.join("main.lua"))
        .current_dir(&workspace)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "stdout: {}\nstderr: {}\nLua:\n{modular_lua}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}

#[derive(Default)]
struct MemorySources {
    files: BTreeMap<FileId, SourceText>,
    paths: BTreeMap<LogicalSourcePath, FileId>,
}

#[derive(Debug)]
struct MissingSource;

impl fmt::Display for MissingSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("missing in-memory source")
    }
}

impl std::error::Error for MissingSource {}

impl SourceProvider for MemorySources {
    type Error = MissingSource;

    fn load(&self, file: FileId) -> Result<SourceText, Self::Error> {
        self.files.get(&file).cloned().ok_or(MissingSource)
    }

    fn file_for_path(&self, path: &LogicalSourcePath) -> Result<Option<FileId>, Self::Error> {
        Ok(self.paths.get(path).copied())
    }
}

#[test]
fn compiler_project_api_loads_modules_without_filesystem_or_dense_ids() {
    let root = FileId::new(41);
    let api = FileId::new(97);
    let root_path = LogicalSourcePath::new("src/main.rua").unwrap();
    let api_path = LogicalSourcePath::new("src/api.rua").unwrap();
    let mut provider = MemorySources::default();
    provider.files.insert(
        root,
        SourceText {
            text: "let value = api::answer();\n".to_string(),
        },
    );
    provider.files.insert(
        api,
        SourceText {
            text: "pub fn answer() -> i64 { 42 }\n".to_string(),
        },
    );
    provider.paths.insert(root_path.clone(), root);
    provider.paths.insert(api_path, api);
    let project = ProjectSpec {
        id: ProjectId::new(7),
        root_file: root,
        roots: vec![SourceRootSpec {
            id: SourceRootId::new(3),
            kind: SourceRootKind::Workspace,
            logical_base: LogicalSourcePath::new("src").unwrap(),
        }],
        libraries: Vec::new(),
        files: provider.paths.clone(),
    };

    let artifact =
        ruac::compile_project_artifact(&project, &provider).expect("compile in-memory project");
    let lua = artifact.source;
    assert!(lua.contains("\napi.answer()\n"), "{lua}");
    assert!(!lua.contains("local value"), "{lua}");
    assert!(
        artifact
            .source_map
            .iter()
            .any(|mapping| mapping.source.file == root.index()),
        "root chunk statements should retain FileId 41 anchors"
    );
    assert!(
        artifact
            .source_map
            .iter()
            .any(|mapping| mapping.source.file == api.index()),
        "module functions should retain FileId 97 anchors"
    );

    let modules = ruac::compile_project_modules_artifact(&project, &provider)
        .expect("compile in-memory project as Lua modules");
    assert_eq!(modules.root_output_path, "main.lua");
    assert_eq!(modules.modules.len(), 2);
    assert_eq!(modules.modules[0].output_path, "main.lua");
    assert_eq!(modules.modules[1].output_path, "api.lua");
    assert!(
        modules.modules[0]
            .source_map
            .iter()
            .all(|mapping| mapping.source.file == root.index())
    );
    assert!(
        modules.modules[1]
            .source_map
            .iter()
            .all(|mapping| mapping.source.file == api.index())
    );
}

#[test]
fn compiler_project_api_mounts_directory_root_declaration() {
    let root = FileId::new(21);
    let declaration = FileId::new(22);
    let root_path = LogicalSourcePath::new("src/main.rua").unwrap();
    let declaration_path = LogicalSourcePath::new("deps/host/mod.ruai").unwrap();
    let mut provider = MemorySources::default();
    provider.files.insert(
        root,
        SourceText {
            text: "let value = host::answer();\n".to_string(),
        },
    );
    provider.files.insert(
        declaration,
        SourceText {
            text: "pub fn answer() -> i64 {}\n".to_string(),
        },
    );
    provider.paths.insert(root_path.clone(), root);
    provider.paths.insert(declaration_path, declaration);
    let project = ProjectSpec {
        id: ProjectId::new(8),
        root_file: root,
        roots: vec![
            SourceRootSpec {
                id: SourceRootId::new(5),
                kind: SourceRootKind::Workspace,
                logical_base: LogicalSourcePath::new("src").unwrap(),
            },
            SourceRootSpec {
                id: SourceRootId::new(6),
                kind: SourceRootKind::Library,
                logical_base: LogicalSourcePath::new("deps/host").unwrap(),
            },
        ],
        libraries: vec![LibraryMount {
            name: "host".to_string(),
            root: SourceRootId::new(6),
            logical_base: LogicalSourcePath::new("deps/host").unwrap(),
        }],
        files: provider.paths.clone(),
    };

    let lua = ruac::compile_project(&project, &provider).expect("compile mounted declaration");
    assert!(lua.contains("require(\"host\")"), "{lua}");
    assert!(lua.contains("host.answer()"), "{lua}");
}

#[test]
fn compiler_project_api_returns_structured_module_and_type_diagnostics() {
    let root = FileId::new(11);
    let flat = FileId::new(12);
    let nested = FileId::new(13);
    let root_path = LogicalSourcePath::new("src/main.rua").unwrap();
    let flat_path = LogicalSourcePath::new("src/foo.rua").unwrap();
    let nested_path = LogicalSourcePath::new("src/foo/mod.rua").unwrap();
    let mut provider = MemorySources::default();
    provider.files.insert(
        root,
        SourceText {
            text: String::new(),
        },
    );
    provider.files.insert(
        flat,
        SourceText {
            text: String::new(),
        },
    );
    provider.files.insert(
        nested,
        SourceText {
            text: String::new(),
        },
    );
    provider.paths.insert(root_path.clone(), root);
    provider.paths.insert(flat_path.clone(), flat);
    provider.paths.insert(nested_path.clone(), nested);
    let mut project = ProjectSpec {
        id: ProjectId::new(9),
        root_file: root,
        roots: vec![SourceRootSpec {
            id: SourceRootId::new(4),
            kind: SourceRootKind::Workspace,
            logical_base: LogicalSourcePath::new("src").unwrap(),
        }],
        libraries: Vec::new(),
        files: provider.paths.clone(),
    };

    let failure = ruac::compile_project_with_diagnostics(&project, &provider).unwrap_err();
    assert_eq!(
        failure.diagnostics[0].code,
        rua_core::DiagnosticCode::NameAmbiguousImport
    );
    assert_eq!(failure.files[root.index() as usize], root_path.as_str());

    project.files.remove(&flat_path);
    project.files.remove(&nested_path);
    provider.files.insert(
        root,
        SourceText {
            text: "let value: bool = 1;\n".to_string(),
        },
    );
    let failure = ruac::compile_project_with_diagnostics(&project, &provider).unwrap_err();
    assert_eq!(
        failure.diagnostics[0].code,
        rua_core::DiagnosticCode::TypeMismatch
    );
    assert_eq!(failure.diagnostics[0].file_index(), Some(root.index()));
    assert!(!failure.diagnostics[0].is_empty());
}
