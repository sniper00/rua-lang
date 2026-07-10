//! Repository-level snapshots for IDE-facing analysis and workspace queries.

use std::fmt::Write as _;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use rua_syntax::analysis::Analysis;
use rua_syntax::workspace::{DiskLoader, Workspace, normalize_path};

const UPDATE_ENV: &str = "RUA_UPDATE_GOLDENS";
const UPDATE_COMMAND: &str = "RUA_UPDATE_GOLDENS=1 cargo test -p rua-syntax --test \
                              ide_goldens update_ide_snapshots -- --ignored --exact";
const CASES: &[&str] = &[
    "completion_local",
    "completion_member_struct",
    "completion_member_trait",
    "completion_module_path",
    "diagnostics_fast",
    "document_symbols",
    "goto_cross_file",
    "goto_local",
    "hover_function_signature",
    "hover_local_type",
    "references_cross_file",
    "references_local",
    "rename_cross_file",
    "rename_local",
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonicalize workspace root")
}

fn ide_root() -> PathBuf {
    workspace_root().join("tests/golden/ide")
}

fn fixture_label(path: &Path) -> String {
    path.strip_prefix(workspace_root().join("tests/golden"))
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("cannot read {}: {error}", path.display()))
}

fn single_source(case: &str) -> Result<(PathBuf, String), String> {
    let path = ide_root().join(format!("{case}.rua"));
    let source = read(&path)?;
    Ok((path, source))
}

fn cross_file_paths(case: &str) -> Result<(PathBuf, PathBuf, String), String> {
    let workspace = ide_root().join(case).join("workspace");
    let main = workspace.join("main.rua");
    let source = read(&main)?;
    Ok((workspace, main, source))
}

fn nth_word(source: &str, word: &str, target: usize) -> Result<usize, String> {
    let bytes = source.as_bytes();
    let is_ident = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let mut seen = 0;
    let mut cursor = 0;
    while let Some(relative) = source[cursor..].find(word) {
        let start = cursor + relative;
        let end = start + word.len();
        let left = start == 0 || !is_ident(bytes[start - 1]);
        let right = end == bytes.len() || !is_ident(bytes[end]);
        if left && right {
            if seen == target {
                return Ok(start);
            }
            seen += 1;
        }
        cursor = start + 1;
    }
    Err(format!("word {word:?} occurrence {target} not found"))
}

fn member_offset(source: &str, receiver: &str) -> Result<usize, String> {
    let query = format!("{receiver}.");
    source
        .rfind(&query)
        .map(|start| start + query.len())
        .ok_or_else(|| format!("member query {query:?} not found"))
}

fn path_offset(source: &str, receiver: &str) -> Result<usize, String> {
    let query = format!("{receiver}::");
    source
        .rfind(&query)
        .map(|start| start + query.len())
        .ok_or_else(|| format!("path query {query:?} not found"))
}

fn source_slice(source: &str, range: (usize, usize)) -> Result<&str, String> {
    source
        .get(range.0..range.1)
        .ok_or_else(|| format!("range {range:?} is outside source"))
}

fn relative_path(path: &Path, workspace: &Path) -> String {
    let path = normalize_path(path);
    let workspace = normalize_path(workspace);
    path.strip_prefix(&workspace)
        .unwrap_or(&path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn completion_local_snapshot(case: &str) -> Result<String, String> {
    let (_, source) = single_source(case)?;
    let offset = nth_word(&source, "local_value", 1)?;
    let analysis = Analysis::new(&source);
    let mut locals = analysis.scope_locals(offset);
    locals.sort_by(|left, right| left.name.cmp(&right.name));

    let mut output = String::from("query: local_value\n");
    for local in locals {
        writeln!(output, "completion: {} {:?}", local.name, local.detail).expect("write to String");
    }
    Ok(output)
}

fn completion_member_snapshot(case: &str, receiver: &str) -> Result<String, String> {
    let (_, source) = single_source(case)?;
    let offset = member_offset(&source, receiver)?;
    let analysis = Analysis::new(&source);
    let mut members = analysis
        .member_completions(offset)
        .ok_or_else(|| format!("no member completion context for {receiver}."))?;
    members.sort_by(|left, right| left.name.cmp(&right.name));

    let mut output = format!("query: {receiver}.\n");
    for member in members {
        writeln!(
            output,
            "completion: {} {:?} {:?}",
            member.name, member.kind, member.detail
        )
        .expect("write to String");
    }
    Ok(output)
}

fn completion_path_snapshot(case: &str) -> Result<String, String> {
    let (_, source) = single_source(case)?;
    let offset = path_offset(&source, "math")?;
    let analysis = Analysis::new(&source);
    let mut symbols = analysis
        .path_completions(offset)
        .ok_or_else(|| "no path completion context for math::".to_string())?;
    symbols.sort_by(|left, right| left.name.cmp(&right.name));

    let mut output = String::from("query: math::\n");
    for symbol in symbols {
        writeln!(
            output,
            "completion: {} {:?} {:?}",
            symbol.name, symbol.kind, symbol.detail
        )
        .expect("write to String");
    }
    Ok(output)
}

fn resolution_snapshot(case: &str, word: &str, occurrence: usize) -> Result<String, String> {
    let (_, source) = single_source(case)?;
    let offset = nth_word(&source, word, occurrence)?;
    let analysis = Analysis::new(&source);
    let resolution = analysis
        .definition_at(offset)
        .ok_or_else(|| format!("definition lookup returned no result for {word}"))?;
    Ok(format!(
        "query: {word}\ntarget: {}..{} {:?}\ntext: {:?}\ndetail: {:?}\n",
        resolution.target_range.0,
        resolution.target_range.1,
        resolution.kind,
        source_slice(&source, resolution.target_range)?,
        resolution.detail
    ))
}

fn goto_cross_file_snapshot(case: &str) -> Result<String, String> {
    let (workspace, main, source) = cross_file_paths(case)?;
    let offset = nth_word(&source, "area", 0)?;
    let mut index = Workspace::new(DiskLoader);
    index.index_root(&workspace);
    let (target, range, kind, detail) = index
        .goto_definition(&main, offset)
        .ok_or_else(|| "cross-file goto returned no result".to_string())?;
    let target_source = read(&target)?;
    Ok(format!(
        "query: geometry::area\ntarget: {} {}..{} {:?}\ntext: {:?}\ndetail: {:?}\n",
        relative_path(&target, &workspace),
        range.0,
        range.1,
        kind,
        source_slice(&target_source, range)?,
        detail
    ))
}

fn references_local_snapshot(case: &str) -> Result<String, String> {
    let (_, source) = single_source(case)?;
    let offset = nth_word(&source, "value", 1)?;
    let analysis = Analysis::new(&source);
    let references = analysis.references_at(offset);
    if references.is_empty() {
        return Err("local references returned no results".to_string());
    }

    let mut output = String::from("query: value\n");
    for range in references {
        writeln!(
            output,
            "reference: {}..{} {:?}",
            range.0,
            range.1,
            source_slice(&source, range)?
        )
        .expect("write to String");
    }
    Ok(output)
}

fn references_cross_file_snapshot(case: &str) -> Result<String, String> {
    let (workspace, main, source) = cross_file_paths(case)?;
    let offset = nth_word(&source, "area", 0)?;
    let mut index = Workspace::new(DiskLoader);
    index.index_root(&workspace);
    let references = index.references(&main, offset, true);
    if references.is_empty() {
        return Err("cross-file references returned no results".to_string());
    }

    let mut output = String::from("query: geometry::area\n");
    for (path, range) in references {
        let target_source = read(&path)?;
        writeln!(
            output,
            "reference: {} {}..{} {:?}",
            relative_path(&path, &workspace),
            range.0,
            range.1,
            source_slice(&target_source, range)?
        )
        .expect("write to String");
    }
    Ok(output)
}

fn rename_local_snapshot(case: &str) -> Result<String, String> {
    let (_, source) = single_source(case)?;
    let offset = nth_word(&source, "value", 1)?;
    let analysis = Analysis::new(&source);
    let edits = analysis
        .rename_edits(offset, "total")
        .map_err(|error| format!("local rename failed: {error:?}"))?;

    let mut output = String::from("query: value -> total\n");
    for (start, end, replacement) in edits {
        writeln!(
            output,
            "edit: {start}..{end} {:?} -> {:?}",
            source_slice(&source, (start, end))?,
            replacement
        )
        .expect("write to String");
    }
    Ok(output)
}

fn rename_cross_file_snapshot(case: &str) -> Result<String, String> {
    let (workspace, main, source) = cross_file_paths(case)?;
    let offset = nth_word(&source, "area", 0)?;
    let mut index = Workspace::new(DiskLoader);
    index.index_root(&workspace);
    let edits = index
        .rename_edits(&main, offset, "surface")
        .map_err(|error| format!("cross-file rename failed: {error:?}"))?;
    let mut edits = edits.into_iter().collect::<Vec<_>>();
    edits.sort_by(|left, right| left.0.cmp(&right.0));

    let mut output = String::from("query: geometry::area -> surface\n");
    for (path, mut file_edits) in edits {
        let target_source = read(&path)?;
        file_edits.sort_by_key(|(start, _, _)| *start);
        for (start, end, replacement) in file_edits {
            writeln!(
                output,
                "edit: {} {start}..{end} {:?} -> {:?}",
                relative_path(&path, &workspace),
                source_slice(&target_source, (start, end))?,
                replacement
            )
            .expect("write to String");
        }
    }
    Ok(output)
}

fn diagnostics_snapshot(case: &str) -> Result<String, String> {
    let (_, source) = single_source(case)?;
    let (diagnostics, _) = ruac::check_diags(&source);
    if diagnostics.is_empty() {
        return Err("diagnostic query returned no results".to_string());
    }

    let mut output = String::new();
    for diagnostic in diagnostics {
        let range = (diagnostic.start, diagnostic.start + diagnostic.len);
        let text = if diagnostic.len == 0 {
            "".to_string()
        } else {
            source_slice(&source, range)?.to_string()
        };
        writeln!(
            output,
            "diagnostic: line={} range={}..{} text={:?} message={:?}",
            diagnostic.line, range.0, range.1, text, diagnostic.msg
        )
        .expect("write to String");
    }
    Ok(output)
}

fn document_symbols_snapshot(case: &str) -> Result<String, String> {
    let (_, source) = single_source(case)?;
    let analysis = Analysis::new(&source);
    let mut output = String::new();
    for symbol in analysis.symbols() {
        let container = if symbol.container.is_empty() {
            "<root>".to_string()
        } else {
            symbol.container.join("::")
        };
        writeln!(
            output,
            "symbol: {} {:?} container={} name={}..{} full={}..{} detail={:?} doc={:?}",
            symbol.name,
            symbol.kind,
            container,
            symbol.name_range.0,
            symbol.name_range.1,
            symbol.full_range.0,
            symbol.full_range.1,
            symbol.detail,
            symbol.doc
        )
        .expect("write to String");
    }
    Ok(output)
}

fn snapshot(case: &str) -> Result<String, String> {
    match case {
        "completion_local" => completion_local_snapshot(case),
        "completion_member_struct" => completion_member_snapshot(case, "point"),
        "completion_member_trait" => completion_member_snapshot(case, "job"),
        "completion_module_path" => completion_path_snapshot(case),
        "diagnostics_fast" => diagnostics_snapshot(case),
        "document_symbols" => document_symbols_snapshot(case),
        "goto_cross_file" => goto_cross_file_snapshot(case),
        "goto_local" => resolution_snapshot(case, "value", 1),
        "hover_function_signature" => resolution_snapshot(case, "add", 1),
        "hover_local_type" => resolution_snapshot(case, "total", 1),
        "references_cross_file" => references_cross_file_snapshot(case),
        "references_local" => references_local_snapshot(case),
        "rename_cross_file" => rename_cross_file_snapshot(case),
        "rename_local" => rename_local_snapshot(case),
        _ => Err(format!("unknown IDE snapshot case {case}")),
    }
}

fn expected_path(case: &str) -> PathBuf {
    ide_root().join(format!("{case}.snap"))
}

fn mismatch_message(case: &str, expected: &str, actual: &str) -> String {
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
        "IDE snapshot mismatch for {case} at line {line}\nexpected: {expected_line:?}\n  actual: \
         {actual_line:?}\nto update, run: {UPDATE_COMMAND}"
    )
}

fn assert_or_update(case: &str, actual: &str, update: bool) -> Result<(), String> {
    let expected_path = expected_path(case);
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
                "missing IDE snapshot for {case}: {}\nto update, run: {UPDATE_COMMAND}",
                fixture_label(&expected_path)
            ));
        }
        Err(error) => {
            return Err(format!(
                "cannot read IDE snapshot {}: {error}",
                expected_path.display()
            ));
        }
    };
    if expected != actual {
        return Err(mismatch_message(case, &expected, actual));
    }
    Ok(())
}

fn run_goldens(update: bool) -> Result<(), String> {
    for case in CASES {
        let actual = snapshot(case)?;
        assert_or_update(case, &actual, update)?;
    }
    Ok(())
}

fn run(result: Result<(), String>) {
    result.unwrap_or_else(|error| panic!("{error}"));
}

#[test]
fn ide_snapshot_golden() {
    run(run_goldens(false));
}

#[test]
#[ignore = "updates repository IDE snapshots; run the documented explicit command"]
fn update_ide_snapshots() {
    assert_eq!(
        std::env::var(UPDATE_ENV).as_deref(),
        Ok("1"),
        "refusing to update without {UPDATE_ENV}=1"
    );
    run(run_goldens(true));
}
