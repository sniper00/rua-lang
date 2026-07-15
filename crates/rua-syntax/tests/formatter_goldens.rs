//! Repository-level formatter golden harness.

use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use rua_syntax::format::{check_format, format_str};
use rua_syntax::{SyntaxKind, lex, parse_source_file};

const UPDATE_ENV: &str = "RUA_UPDATE_GOLDENS";
const UPDATE_COMMAND: &str = "RUA_UPDATE_GOLDENS=1 cargo test -p rua-syntax --test \
                              formatter_goldens update_formatter_goldens -- --ignored --exact";
const MIN_FORMAT_CASES: usize = 10;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonicalize workspace root")
}

fn format_root() -> PathBuf {
    workspace_root().join("tests/golden/format")
}

fn format_cases() -> Result<Vec<PathBuf>, String> {
    let root = format_root();
    let mut cases = fs::read_dir(&root)
        .map_err(|error| format!("cannot read {}: {error}", root.display()))?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|error| format!("cannot read entry under {}: {error}", root.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    cases.retain(|path| path.extension() == Some(OsStr::new("rua")));
    cases.sort();
    if cases.len() < MIN_FORMAT_CASES {
        return Err(format!(
            "formatter corpus has {} cases; expected at least {MIN_FORMAT_CASES}",
            cases.len()
        ));
    }
    Ok(cases)
}

fn fixture_label(path: &Path) -> String {
    path.strip_prefix(workspace_root().join("tests/golden"))
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn expected_path(source: &Path) -> PathBuf {
    PathBuf::from(format!("{}.golden", source.display()))
}

fn significant_tokens(source: &str) -> Vec<(SyntaxKind, String)> {
    lex(source)
        .into_iter()
        .filter(|token| !token.kind.is_trivia() && token.kind != SyntaxKind::Comma)
        .map(|token| {
            let end = token.start + token.len;
            (token.kind, source[token.start..end].to_owned())
        })
        .collect()
}

fn validate_format(source_path: &Path, source: &str, formatted: &str) -> Result<(), String> {
    let label = fixture_label(source_path);
    let parsed = parse_source_file(source);
    if !parsed.errors().is_empty() {
        return Err(format!(
            "formatter input {label} has parse errors: {:?}",
            parsed.errors()
        ));
    }
    if parsed.syntax_node().text() != source {
        return Err(format!("formatter input {label} is not lossless"));
    }
    let token_bytes: usize = lex(source).iter().map(|token| token.len).sum();
    if token_bytes != source.len() {
        return Err(format!("lexer did not cover every byte of {label}"));
    }
    ruac::parser::parse(source)
        .map_err(|error| format!("compiler parser rejected {label}: {error}"))?;

    let reparsed = parse_source_file(formatted);
    if !reparsed.errors().is_empty() {
        return Err(format!(
            "formatted {label} has parse errors: {:?}",
            reparsed.errors()
        ));
    }
    if format_str(formatted) != formatted {
        return Err(format!("formatter is not idempotent for {label}"));
    }
    if !check_format(formatted) {
        return Err(format!("formatted {label} does not pass check_format"));
    }
    if significant_tokens(source) != significant_tokens(formatted) {
        return Err(format!("formatter changed significant tokens in {label}"));
    }
    Ok(())
}

fn assert_or_update(source: &Path, actual: &str, update: bool) -> Result<(), String> {
    let expected_path = expected_path(source);
    if update {
        fs::write(&expected_path, actual)
            .map_err(|error| format!("cannot write {}: {error}", expected_path.display()))?;
        println!("updated {}", fixture_label(&expected_path));
        return Ok(());
    }

    let expected = match fs::read_to_string(&expected_path) {
        Ok(expected) => expected,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Err(format!(
                "missing formatter golden for {}: {}\nto update, run: {UPDATE_COMMAND}",
                fixture_label(source),
                fixture_label(&expected_path)
            ));
        }
        Err(error) => {
            return Err(format!(
                "cannot read formatter golden {}: {error}",
                expected_path.display()
            ));
        }
    };
    if expected != actual {
        return Err(format!(
            "formatter golden mismatch for {}\nto update, run: {UPDATE_COMMAND}",
            fixture_label(source)
        ));
    }
    Ok(())
}

fn run_formatter_goldens(update: bool) -> Result<(), String> {
    for path in format_cases()? {
        let source = fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let formatted = format_str(&source);
        validate_format(&path, &source, &formatted)?;
        assert_or_update(&path, &formatted, update)?;
    }
    Ok(())
}

#[test]
fn formatter_golden() {
    run_formatter_goldens(false).unwrap_or_else(|error| panic!("{error}"));
}

#[test]
fn check_rejects_unformatted_source() {
    let source = "fn foo()  {\n    let x = 1;\n}\n";
    assert!(parse_source_file(source).errors().is_empty());
    assert!(!check_format(source));
}

#[test]
#[ignore = "updates repository formatter snapshots; run the documented explicit command"]
fn update_formatter_goldens() {
    assert_eq!(
        std::env::var(UPDATE_ENV).as_deref(),
        Ok("1"),
        "refusing to update without {UPDATE_ENV}=1"
    );
    run_formatter_goldens(true).unwrap_or_else(|error| panic!("{error}"));
}
