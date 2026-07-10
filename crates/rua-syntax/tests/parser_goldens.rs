//! Repository-level parser conformance and byte-range golden harness.

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use rua_syntax::{Parse, SyntaxElement, SyntaxNode, ast::SourceFile, parse_source_file};

const UPDATE_ENV: &str = "RUA_UPDATE_GOLDENS";
const UPDATE_COMMAND: &str = "RUA_UPDATE_GOLDENS=1 cargo test -p rua-syntax --test \
                              parser_goldens update_parser_range_snapshots -- --ignored --exact";
const MIN_ACCEPT_CASES: usize = 15;
const MIN_REJECT_CASES: usize = 6;
const MIN_RANGE_CASES: usize = 15;

struct ParserCases {
    accept: Vec<PathBuf>,
    reject: Vec<PathBuf>,
    ranges: Vec<PathBuf>,
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonicalize workspace root")
}

fn parser_root() -> PathBuf {
    workspace_root().join("tests/golden/parser")
}

fn discover_rua(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = fs::read_dir(dir).map_err(|error| {
        format!(
            "cannot read parser golden directory {}: {error}",
            dir.display()
        )
    })?;
    let mut sources = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("cannot read entry under {}: {error}", dir.display()))?;
        let path = entry.path();
        if entry
            .file_type()
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?
            .is_file()
            && path.extension() == Some(OsStr::new("rua"))
        {
            sources.push(path);
        }
    }
    sources.sort();
    Ok(sources)
}

fn fixture_label(path: &Path) -> String {
    path.strip_prefix(workspace_root().join("tests/golden"))
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn read_source(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("cannot read {}: {error}", path.display()))
}

fn parser_cases() -> Result<ParserCases, String> {
    let root = parser_root();
    let accept = discover_rua(&root.join("accept"))?;
    let reject = discover_rua(&root.join("reject"))?;
    let ranges = discover_rua(&root.join("ranges"))?;
    if accept.len() < MIN_ACCEPT_CASES {
        return Err(format!(
            "parser accept corpus has {} cases; expected at least {MIN_ACCEPT_CASES}",
            accept.len()
        ));
    }
    if reject.len() < MIN_REJECT_CASES {
        return Err(format!(
            "parser reject corpus has {} cases; expected at least {MIN_REJECT_CASES}",
            reject.len()
        ));
    }
    if ranges.len() < MIN_RANGE_CASES {
        return Err(format!(
            "parser range corpus has {} cases; expected at least {MIN_RANGE_CASES}",
            ranges.len()
        ));
    }
    Ok(ParserCases {
        accept,
        reject,
        ranges,
    })
}

fn assert_lossless(path: &Path, source: &str, parsed: &Parse<SourceFile>) -> Result<(), String> {
    let actual = parsed.syntax_node().text().to_string();
    if actual != source {
        return Err(format!(
            "CST parser was not lossless for {}",
            fixture_label(path)
        ));
    }
    Ok(())
}

fn run_parser_conformance() -> Result<(), String> {
    let ParserCases { accept, reject, .. } = parser_cases()?;

    for path in accept {
        let source = read_source(&path)?;
        let parsed = parse_source_file(&source);
        assert_lossless(&path, &source, &parsed)?;
        if !parsed.errors.is_empty() {
            return Err(format!(
                "CST parser rejected {}: {:?}",
                fixture_label(&path),
                parsed.errors
            ));
        }
        ruac::parser::parse(&source).map_err(|error| {
            format!("compiler parser rejected {}: {error}", fixture_label(&path))
        })?;
    }

    for path in reject {
        let source = read_source(&path)?;
        let parsed = parse_source_file(&source);
        // Recovery details intentionally need not match the compiler parser:
        // the IDE parser must retain every byte and report at least one error.
        assert_lossless(&path, &source, &parsed)?;
        if parsed.errors.is_empty() {
            return Err(format!(
                "CST parser accepted reject case {}",
                fixture_label(&path)
            ));
        }
        if ruac::parser::parse(&source).is_ok() {
            return Err(format!(
                "compiler parser accepted reject case {}",
                fixture_label(&path)
            ));
        }
    }

    Ok(())
}

fn render_element(element: SyntaxElement, depth: usize, output: &mut String) {
    let indent = "  ".repeat(depth);
    match element {
        SyntaxElement::Node(node) => render_node(&node, depth, output),
        SyntaxElement::Token(token) if !token.kind().is_trivia() => {
            let range = token.text_range();
            writeln!(
                output,
                "{indent}{:?} {}..{} {:?}",
                token.kind(),
                u32::from(range.start()),
                u32::from(range.end()),
                token.text()
            )
            .expect("write to String");
        }
        SyntaxElement::Token(_) => {}
    }
}

fn render_node(node: &SyntaxNode, depth: usize, output: &mut String) {
    let indent = "  ".repeat(depth);
    let range = node.text_range();
    writeln!(
        output,
        "{indent}{:?} {}..{}",
        node.kind(),
        u32::from(range.start()),
        u32::from(range.end())
    )
    .expect("write to String");
    for child in node.children_with_tokens() {
        render_element(child, depth + 1, output);
    }
}

fn range_snapshot(path: &Path) -> Result<String, String> {
    let source = read_source(path)?;
    let parsed = parse_source_file(&source);
    assert_lossless(path, &source, &parsed)?;
    if !parsed.errors.is_empty() {
        return Err(format!(
            "range case {} has CST parse errors: {:?}",
            fixture_label(path),
            parsed.errors
        ));
    }
    ruac::parser::parse(&source).map_err(|error| {
        format!(
            "compiler parser rejected range case {}: {error}",
            fixture_label(path)
        )
    })?;

    let mut output = format!("source_len: {}\n", source.len());
    render_node(parsed.syntax_node(), 0, &mut output);
    Ok(output)
}

fn expected_range_path(source: &Path) -> PathBuf {
    source.with_extension("range.golden")
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
        "range golden mismatch for {} at line {line}\nexpected: {expected_line:?}\n  actual: \
         {actual_line:?}\nto update, run: {UPDATE_COMMAND}",
        fixture_label(source)
    )
}

fn assert_or_update_range(source: &Path, actual: &str, update: bool) -> Result<(), String> {
    let expected_path = expected_range_path(source);
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
                "missing range golden for {}: {}\nto update, run: {UPDATE_COMMAND}",
                fixture_label(source),
                fixture_label(&expected_path)
            ));
        }
        Err(error) => {
            return Err(format!(
                "cannot read range golden {}: {error}",
                expected_path.display()
            ));
        }
    };
    if expected != actual {
        return Err(mismatch_message(source, &expected, actual));
    }
    Ok(())
}

fn run_range_goldens(update: bool) -> Result<(), String> {
    let ParserCases { ranges, .. } = parser_cases()?;
    for source in ranges {
        let actual = range_snapshot(&source)?;
        assert_or_update_range(&source, &actual, update)?;
    }
    Ok(())
}

fn run(result: Result<(), String>) {
    result.unwrap_or_else(|error| panic!("{error}"));
}

#[test]
fn parser_conformance() {
    run(run_parser_conformance());
}

#[test]
fn range_golden() {
    run(run_range_goldens(false));
}

#[test]
#[ignore = "updates repository range snapshots; run the documented explicit command"]
fn update_parser_range_snapshots() {
    assert_eq!(
        std::env::var(UPDATE_ENV).as_deref(),
        Ok("1"),
        "refusing to update without {UPDATE_ENV}=1"
    );
    run(run_parser_conformance());
    run(run_range_goldens(true));
}
