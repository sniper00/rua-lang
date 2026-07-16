//! Repository-level golden harness for the Rua compiler.

use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const UPDATE_ENV: &str = "RUA_UPDATE_GOLDENS";
const UPDATE_COMMAND: &str = "RUA_UPDATE_GOLDENS=1 cargo test -p ruac --test golden \
                              update_goldens -- --ignored --exact";
const MIN_COMPILE_PASS_CASES: usize = 30;
const MIN_COMPILE_FAIL_CASES: usize = 30;
const MIN_COVERAGE_ROWS: usize = 25;
const COVERAGE_HEADER: &str =
    "| Feature | Compile pass | Compile fail | Parser/range | IDE oracle | Notes |";
const REQUIRED_COVERAGE_MARKERS: &[&str] = &[
    "| Closures |",
    "| Iterator adapters and fusion |",
    "| External `.ruai` library roots |",
    "| Diagnostic codes and precise ranges |",
    "| Semantic tokens |",
    "| Inlay hints |",
    "## Known Gaps",
    "## Merge Gate",
];
const RUAI_COMPILE_PASS_CASES: &[&str] = &[
    "declaration_codegen_skip",
    "library_decl_basic",
    "library_decl_module_dir",
    "library_mount_single_file",
    "workspace_shadows_library",
];
const RUAI_COMPILE_FAIL_CASES: &[&str] = &["declaration_body_rejected", "declaration_type_error"];
const MODULE_COMPILE_PASS_CASES: &[&str] = &["nested_file_modules"];
const PHASE4A_ACTIVE_PASS_CASES: &[&str] = &[
    "closure_expr_inferred",
    "closure_block_typed",
    "closure_capture_read",
    "closure_capture_mut_fused",
    "iterator_vec_for",
    "iterator_map_filter_collect",
    "iterator_fold",
    "iterator_block_closure",
    "iterator_adapters_count",
    "iterator_any",
    "iterator_all",
    "iterator_find",
    "iterator_first_class",
];
const PHASE4A_ACTIVE_FAIL_CASES: &[&str] = &[
    "closure_param_cannot_infer",
    "closure_return_mismatch",
    "closure_mut_capture_invalid",
    "closure_escape_unsupported",
    "iterator_non_iterable_source",
    "iterator_map_arg_not_closure",
    "iterator_filter_not_bool",
    "iterator_collect_mismatch",
];
const REQUIRED_DIRS: &[&str] = &[
    "compile-pass",
    "compile-fail",
    "parser/accept",
    "parser/reject",
    "parser/ranges",
    "format",
    "modules",
    "ruai",
    "ide",
    "phase4a",
    "source-map",
];
static NEXT_LUA_SCRIPT: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
enum GoldenKind {
    Lua,
    Diagnostic,
}

impl GoldenKind {
    fn extension(self) -> &'static str {
        match self {
            Self::Lua => "lua.golden",
            Self::Diagnostic => "diag.golden",
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonicalize workspace root")
}

fn golden_root() -> PathBuf {
    workspace_root().join("tests/golden")
}

fn discover_rua(root: &Path) -> Result<Vec<PathBuf>, String> {
    fn visit(dir: &Path, found: &mut Vec<PathBuf>) -> Result<(), String> {
        let entries = fs::read_dir(dir)
            .map_err(|e| format!("cannot read golden directory {}: {e}", dir.display()))?;
        for entry in entries {
            let entry =
                entry.map_err(|e| format!("cannot read entry under {}: {e}", dir.display()))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|e| format!("cannot inspect {}: {e}", path.display()))?;
            if file_type.is_dir() {
                visit(&path, found)?;
            } else if file_type.is_file() && path.extension() == Some(OsStr::new("rua")) {
                found.push(path);
            }
        }
        Ok(())
    }

    let mut found = Vec::new();
    visit(root, &mut found)?;
    found.sort();
    Ok(found)
}

fn expected_path(source: &Path, kind: GoldenKind) -> PathBuf {
    source.with_extension(kind.extension())
}

fn compile_standalone(source: &Path) -> Result<String, ruac::CompileFailure> {
    let text = fs::read_to_string(source).unwrap_or_else(|error| {
        panic!(
            "cannot read standalone fixture {}: {error}",
            source.display()
        )
    });
    ruac::compile_str(&text).map_err(|mut failure| {
        if let Some(root_file) = failure.files.first_mut() {
            *root_file = source.display().to_string();
        }
        failure
    })
}

fn fixture_label(path: &Path) -> String {
    path.strip_prefix(golden_root())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn stable_diagnostic(error: &impl std::fmt::Display) -> String {
    let normalized = error.to_string().replace('\\', "/");
    let root = golden_root().to_string_lossy().replace('\\', "/");
    normalized.replace(&root, "<golden>")
}

fn missing_message(source: &Path, expected: &Path) -> String {
    format!(
        "missing golden for {}: {}\nto update, run: {UPDATE_COMMAND}",
        fixture_label(source),
        fixture_label(expected),
    )
}

fn mismatch_message(source: &Path, expected: &str, actual: &str) -> String {
    let offset = expected
        .as_bytes()
        .iter()
        .zip(actual.as_bytes())
        .position(|(left, right)| left != right)
        .unwrap_or_else(|| expected.len().min(actual.len()));
    let line = expected.as_bytes()[..offset.min(expected.len())]
        .iter()
        .filter(|&&byte| byte == b'\n')
        .count()
        + 1;
    let expected_line = expected.split('\n').nth(line - 1).unwrap_or("<eof>");
    let actual_line = actual.split('\n').nth(line - 1).unwrap_or("<eof>");

    format!(
        "golden mismatch for {} at line {line}\nexpected: {expected_line:?}\n  actual: \
         {actual_line:?}\nto update, run: {UPDATE_COMMAND}",
        fixture_label(source),
    )
}

fn assert_or_update(
    source: &Path,
    actual: &str,
    kind: GoldenKind,
    update: bool,
) -> Result<(), String> {
    let expected_path = expected_path(source, kind);
    if update {
        fs::write(&expected_path, actual)
            .map_err(|e| format!("cannot write {}: {e}", expected_path.display()))?;
        println!("updated {}", fixture_label(&expected_path));
        return Ok(());
    }

    let expected = match fs::read_to_string(&expected_path) {
        Ok(expected) => expected,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Err(missing_message(source, &expected_path));
        }
        Err(error) => {
            return Err(format!(
                "cannot read golden {}: {error}",
                expected_path.display()
            ));
        }
    };
    if expected != actual {
        return Err(mismatch_message(source, &expected, actual));
    }
    Ok(())
}

fn assert_named_golden(expected_path: &Path, actual: &str, update: bool) -> Result<(), String> {
    if update {
        fs::write(expected_path, actual)
            .map_err(|error| format!("cannot write {}: {error}", expected_path.display()))?;
        println!("updated {}", fixture_label(expected_path));
        return Ok(());
    }
    let expected = fs::read_to_string(expected_path)
        .map_err(|error| format!("cannot read {}: {error}", expected_path.display()))?;
    if expected != actual {
        return Err(mismatch_message(expected_path, &expected, actual));
    }
    Ok(())
}

fn execute_lua(source: &Path, lua: &str) -> Result<(), String> {
    let unique = NEXT_LUA_SCRIPT.fetch_add(1, Ordering::Relaxed);
    let script =
        std::env::temp_dir().join(format!("ruac-golden-{}-{unique}.lua", std::process::id()));
    fs::write(&script, lua)
        .map_err(|error| format!("cannot write {}: {error}", script.display()))?;
    let runtime = workspace_root().join("crates/rua-resources/resources/std/?.lua");
    let output = Command::new(std::env::var("RUA_LUA").unwrap_or_else(|_| "lua".to_string()))
        .arg("-e")
        .arg("function host_format(...) return '' end")
        .arg(&script)
        .env("LUA_PATH", format!("{};;", runtime.display()))
        .output()
        .map_err(|error| {
            format!(
                "cannot execute Lua for {}: {error}; set RUA_LUA to a Lua 5.5 executable",
                fixture_label(source)
            )
        })?;
    let _ = fs::remove_file(&script);
    if !output.status.success() {
        return Err(format!(
            "generated Lua for {} failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
            fixture_label(source),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    Ok(())
}

fn structured_failure_entry(source: &Path, failure: &ruac::CompileFailure) -> String {
    let mut output = format!("{}\n", fixture_label(source));
    for diagnostic in &failure.diagnostics {
        let file = diagnostic
            .file
            .and_then(|file| failure.files.get(file.index() as usize))
            .filter(|path| !path.is_empty())
            .map(Path::new)
            .map(fixture_label)
            .unwrap_or_else(|| "-".to_string());
        let range = diagnostic
            .range
            .map(|range| format!("{}..{}", range.start(), range.end()))
            .unwrap_or_else(|| "-".to_string());
        let mut arguments = diagnostic.arguments.iter().collect::<Vec<_>>();
        arguments.sort_by_key(|argument| (&argument.name, &argument.value));
        let root = golden_root().to_string_lossy().replace('\\', "/");
        let arguments = arguments
            .into_iter()
            .map(|argument| {
                let value = argument.value.replace('\\', "/");
                format!("{}={:?}", argument.name, value.replace(&root, "<golden>"))
            })
            .collect::<Vec<_>>()
            .join(", ");
        output.push_str(&format!(
            "  {} file={} range={} arguments=[{}]\n",
            diagnostic.code.as_str(),
            file,
            range,
            arguments,
        ));
    }
    output
}

fn source_map_snapshot() -> Result<String, String> {
    let root = golden_root().join("source-map");
    let main = root.join("main.rua");
    let module = root.join("api.rua");
    let sources = [
        fs::read_to_string(&main).map_err(|error| format!("read {}: {error}", main.display()))?,
        fs::read_to_string(&module)
            .map_err(|error| format!("read {}: {error}", module.display()))?,
    ];
    let artifact = ruac::compile_path_artifact(&main)
        .map_err(|error| format!("source-map fixture failed to compile:\n{error}"))?;
    let mut snapshot = String::new();
    for (index, mapping) in artifact.source_map.iter().enumerate() {
        if mapping.generated_start > mapping.generated_end
            || mapping.generated_end > artifact.source.len()
            || !artifact.source.is_char_boundary(mapping.generated_start)
            || !artifact.source.is_char_boundary(mapping.generated_end)
        {
            return Err(format!("invalid generated source mapping {mapping:?}"));
        }
        let source = sources
            .get(mapping.source.file as usize)
            .ok_or_else(|| format!("mapping references unknown file: {mapping:?}"))?;
        let source_end = mapping.source.end();
        if source_end > source.len()
            || !source.is_char_boundary(mapping.source.start)
            || !source.is_char_boundary(source_end)
        {
            return Err(format!("invalid Rua source mapping {mapping:?}"));
        }
        snapshot.push_str(&format!(
            "{index}: generated={}..{} lua={:?} <- file={} source={}..{} rua={:?}\n",
            mapping.generated_start,
            mapping.generated_end,
            &artifact.source[mapping.generated_start..mapping.generated_end],
            mapping.source.file,
            mapping.source.start,
            source_end,
            &source[mapping.source.start..source_end],
        ));
    }
    Ok(snapshot)
}

fn run_compile_pass(update: bool) -> Result<(), String> {
    let root = golden_root().join("compile-pass");
    let sources = discover_rua(&root)?;
    if sources.len() < MIN_COMPILE_PASS_CASES {
        return Err(format!(
            "compile-pass corpus has {} cases; expected at least {MIN_COMPILE_PASS_CASES}",
            sources.len()
        ));
    }
    for source in sources {
        let actual = compile_standalone(&source).map_err(|error| {
            format!(
                "compile-pass case {} failed:\n{error}",
                fixture_label(&source)
            )
        })?;
        assert_or_update(&source, &actual, GoldenKind::Lua, update)?;
        execute_lua(&source, &actual)?;
    }
    Ok(())
}

fn run_compile_fail(update: bool) -> Result<(), String> {
    let root = golden_root().join("compile-fail");
    let sources = discover_rua(&root)?;
    if sources.len() < MIN_COMPILE_FAIL_CASES {
        return Err(format!(
            "compile-fail corpus has {} cases; expected at least {MIN_COMPILE_FAIL_CASES}",
            sources.len()
        ));
    }
    let mut structured = String::new();
    for source in sources {
        let error = compile_standalone(&source).err();
        let Some(error) = error else {
            return Err(format!(
                "compile-fail case {} compiled successfully",
                fixture_label(&source)
            ));
        };
        let actual = stable_diagnostic(&error);
        assert_or_update(&source, &actual, GoldenKind::Diagnostic, update)?;
        structured.push_str(&structured_failure_entry(&source, &error));
    }
    assert_named_golden(
        &root.join("structured-diagnostics.golden"),
        &structured,
        update,
    )?;
    Ok(())
}

fn run_ruai(update: bool) -> Result<(), String> {
    let root = golden_root().join("ruai");
    for case in RUAI_COMPILE_PASS_CASES {
        let source = root.join(case).join("workspace/main.rua");
        let actual = ruac::compile_path(&source).map_err(|error| {
            format!(
                ".ruai compile-pass case {} failed:\n{error}",
                fixture_label(&source)
            )
        })?;
        assert_or_update(&source, &actual, GoldenKind::Lua, update)?;
    }
    let mut structured = String::new();
    for case in RUAI_COMPILE_FAIL_CASES {
        let source = root.join(case).join("workspace/main.rua");
        let error = ruac::compile_path(&source).err();
        let Some(error) = error else {
            return Err(format!(
                ".ruai compile-fail case {} compiled successfully",
                fixture_label(&source)
            ));
        };
        let actual = stable_diagnostic(&error);
        assert_or_update(&source, &actual, GoldenKind::Diagnostic, update)?;
        structured.push_str(&structured_failure_entry(&source, &error));
    }
    assert_named_golden(
        &root.join("structured-diagnostics.golden"),
        &structured,
        update,
    )?;
    Ok(())
}

fn run_modules(update: bool) -> Result<(), String> {
    let root = golden_root().join("modules");
    for case in MODULE_COMPILE_PASS_CASES {
        let source = root.join(case).join("main.rua");
        let actual = ruac::compile_path(&source).map_err(|error| {
            format!(
                "module compile-pass case {} failed:\n{error}",
                fixture_label(&source)
            )
        })?;
        assert_or_update(&source, &actual, GoldenKind::Lua, update)?;
        execute_lua(&source, &actual)?;
    }
    Ok(())
}

fn run_phase4a_compile_fail(update: bool) -> Result<(), String> {
    let root = golden_root().join("phase4a/compile-fail");
    let mut structured = String::new();
    for case in PHASE4A_ACTIVE_FAIL_CASES {
        let source = root.join(format!("{case}.rua"));
        let error = compile_standalone(&source).err();
        let Some(error) = error else {
            return Err(format!(
                "Phase 4A compile-fail case {} compiled successfully",
                fixture_label(&source)
            ));
        };
        let actual = stable_diagnostic(&error);
        assert_or_update(&source, &actual, GoldenKind::Diagnostic, update)?;
        structured.push_str(&structured_failure_entry(&source, &error));
    }
    assert_named_golden(
        &root.join("structured-diagnostics.golden"),
        &structured,
        update,
    )?;
    Ok(())
}

fn run_phase4a_compile_pass(update: bool) -> Result<(), String> {
    let root = golden_root().join("phase4a/compile-pass");
    for case in PHASE4A_ACTIVE_PASS_CASES {
        let source = root.join(format!("{case}.rua"));
        let actual = compile_standalone(&source).map_err(|error| {
            format!(
                "Phase 4A compile-pass case {} failed:\n{error}",
                fixture_label(&source)
            )
        })?;
        if case.starts_with("iterator_") && *case != "iterator_first_class"
            || *case == "closure_capture_mut_fused"
        {
            if actual.matches("for ").count() != 1 {
                return Err(format!(
                    "Phase 4A fused output {} must contain exactly one loop",
                    fixture_label(&source)
                ));
            }
            for forbidden in [
                "coroutine",
                ":iter(",
                ":into_iter(",
                ":map(",
                ":filter(",
                ":fold(",
                ":collect(",
                "function(",
            ] {
                if actual.contains(forbidden) {
                    return Err(format!(
                        "Phase 4A fused output {} contains forbidden shape {forbidden:?}",
                        fixture_label(&source)
                    ));
                }
            }
            if *case == "iterator_map_filter_collect"
                && actual.matches("vec.from_table({ n = 0 })").count() != 1
            {
                return Err(format!(
                    "Phase 4A collect output {} must allocate exactly one result Vec",
                    fixture_label(&source)
                ));
            }
        }
        assert_or_update(&source, &actual, GoldenKind::Lua, update)?;
        execute_lua(&source, &actual)?;
    }
    Ok(())
}

fn run(result: Result<(), String>) {
    result.unwrap_or_else(|error| panic!("{error}"));
}

#[test]
fn golden_layout_is_present() {
    let root = golden_root();
    for relative in REQUIRED_DIRS {
        let path = root.join(relative);
        assert!(path.is_dir(), "missing golden directory {}", path.display());
    }
}

#[test]
fn phase4a_goldens_are_registered() {
    let root = golden_root().join("phase4a");
    for case in PHASE4A_ACTIVE_PASS_CASES {
        let path = root.join("compile-pass").join(format!("{case}.rua"));
        assert!(
            path.is_file(),
            "missing active Phase 4A case {}",
            path.display()
        );
    }
    for case in PHASE4A_ACTIVE_FAIL_CASES {
        let path = root.join("compile-fail").join(format!("{case}.rua"));
        assert!(
            path.is_file(),
            "missing active Phase 4A case {}",
            path.display()
        );
    }
}

#[test]
fn golden_coverage_matrix_is_present() {
    let path = golden_root().join("COVERAGE.md");
    let coverage = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    assert!(
        coverage.contains(COVERAGE_HEADER),
        "coverage matrix header changed"
    );
    for status in ["Yes", "Partial", "No", "N/A"] {
        assert!(
            coverage.contains(status),
            "coverage matrix is missing `{status}` status"
        );
    }
    for marker in REQUIRED_COVERAGE_MARKERS {
        assert!(
            coverage.contains(marker),
            "coverage matrix is missing `{marker}`"
        );
    }
    let rows = coverage
        .lines()
        .filter(|line| {
            line.starts_with("| ") && !line.starts_with("| Feature") && !line.starts_with("| ---")
        })
        .count();
    assert!(
        rows >= MIN_COVERAGE_ROWS,
        "coverage matrix has {rows} feature rows; expected at least {MIN_COVERAGE_ROWS}"
    );
}

#[test]
fn harness_reports_missing_golden_with_update_command() {
    let source = golden_root().join("compile-pass/example.rua");
    let expected = expected_path(&source, GoldenKind::Lua);
    let message = missing_message(&source, &expected);
    assert!(message.contains("compile-pass/example.rua"));
    assert!(message.contains(UPDATE_COMMAND));
}

#[test]
fn harness_reports_mismatch_location_with_update_command() {
    let source = golden_root().join("compile-pass/example.rua");
    let message = mismatch_message(&source, "one\ntwo\n", "one\nchanged\n");
    assert!(message.contains("at line 2"));
    assert!(message.contains("expected: \"two\""));
    assert!(message.contains("actual: \"changed\""));
    assert!(message.contains(UPDATE_COMMAND));
}

#[test]
fn golden_compile_pass() {
    run(run_compile_pass(false));
}

#[test]
fn phase4a_golden_compile_pass() {
    run(run_phase4a_compile_pass(false));
}

#[test]
fn golden_compile_fail() {
    run(run_compile_fail(false));
}

#[test]
fn phase4a_golden_compile_fail() {
    run(run_phase4a_compile_fail(false));
}

#[test]
fn generated_lua_source_map_golden() {
    let expected = golden_root().join("source-map/main.map.golden");
    run(source_map_snapshot().and_then(|snapshot| assert_named_golden(&expected, &snapshot, false)));
}

#[test]
fn golden_ruai() {
    run(run_ruai(false));
}

#[test]
fn golden_modules() {
    run(run_modules(false));
}

#[test]
#[ignore = "updates repository module golden files; run with RUA_UPDATE_GOLDENS=1"]
fn update_module_goldens() {
    assert_eq!(
        std::env::var(UPDATE_ENV).as_deref(),
        Ok("1"),
        "refusing to update without {UPDATE_ENV}=1"
    );
    run(run_modules(true));
}

#[test]
#[ignore = "updates repository golden files; run the documented explicit command"]
fn update_goldens() {
    assert_eq!(
        std::env::var(UPDATE_ENV).as_deref(),
        Ok("1"),
        "refusing to update without {UPDATE_ENV}=1"
    );
    run(run_compile_pass(true));
    run(run_phase4a_compile_pass(true));
    run(run_compile_fail(true));
    run(run_phase4a_compile_fail(true));
    run(run_modules(true));
    run(run_ruai(true));
    let expected = golden_root().join("source-map/main.map.golden");
    run(source_map_snapshot().and_then(|snapshot| assert_named_golden(&expected, &snapshot, true)));
}
