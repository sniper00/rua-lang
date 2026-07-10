//! Repository-level golden harness for the Rua compiler.

use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

const UPDATE_ENV: &str = "RUA_UPDATE_GOLDENS";
const UPDATE_COMMAND: &str = "RUA_UPDATE_GOLDENS=1 cargo test -p ruac --test golden \
                              update_goldens -- --ignored --exact";
const MIN_COMPILE_PASS_CASES: usize = 30;
const REQUIRED_DIRS: &[&str] = &[
    "compile-pass",
    "compile-fail",
    "parser/accept",
    "parser/reject",
    "parser/ranges",
    "modules",
    "ruai",
    "ide",
];

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

fn fixture_label(path: &Path) -> String {
    path.strip_prefix(golden_root())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn stable_diagnostic(error: &str) -> String {
    let normalized = error.replace('\\', "/");
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
        let actual = ruac::compile_path(&source).map_err(|error| {
            format!(
                "compile-pass case {} failed:\n{error}",
                fixture_label(&source)
            )
        })?;
        assert_or_update(&source, &actual, GoldenKind::Lua, update)?;
    }
    Ok(())
}

fn run_compile_fail(update: bool) -> Result<(), String> {
    let root = golden_root().join("compile-fail");
    for source in discover_rua(&root)? {
        let error = ruac::compile_path(&source).err();
        let Some(error) = error else {
            return Err(format!(
                "compile-fail case {} compiled successfully",
                fixture_label(&source)
            ));
        };
        let actual = stable_diagnostic(&error);
        assert_or_update(&source, &actual, GoldenKind::Diagnostic, update)?;
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
fn golden_compile_fail() {
    run(run_compile_fail(false));
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
    run(run_compile_fail(true));
}
