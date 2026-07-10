//! Repository-level IDE snapshots for `.ruai` declaration modules.

use std::fmt::Write as _;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use rua_syntax::workspace::{DiskLoader, Workspace, normalize_path};

const UPDATE_ENV: &str = "RUA_UPDATE_GOLDENS";
const UPDATE_COMMAND: &str = "RUA_UPDATE_GOLDENS=1 cargo test -p rua-syntax --test \
                              ruai_goldens update_ruai_ide_snapshots -- --ignored --exact";
const IDE_CASES: &[&str] = &[
    "completion_members",
    "goto_hover_signature",
    "references_include_declaration",
    "rename_readonly_rejected",
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonicalize workspace root")
}

fn ruai_root() -> PathBuf {
    workspace_root().join("tests/golden/ruai")
}

fn fixture_label(path: &Path) -> String {
    path.strip_prefix(workspace_root().join("tests/golden"))
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn case_paths(case: &str) -> (PathBuf, PathBuf) {
    let workspace = ruai_root().join(case).join("workspace");
    let main = workspace.join("main.rua");
    (workspace, main)
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("cannot read {}: {error}", path.display()))
}

fn query_offset(source: &str, needle: &str, member: &str) -> Result<usize, String> {
    let start = source
        .find(needle)
        .ok_or_else(|| format!("query {needle:?} not found"))?;
    let within = needle
        .find(member)
        .ok_or_else(|| format!("member {member:?} not found in query {needle:?}"))?;
    Ok(start + within)
}

fn relative_path(path: &Path, workspace: &Path) -> String {
    let path = normalize_path(path);
    let workspace = normalize_path(workspace);
    path.strip_prefix(&workspace)
        .unwrap_or(&path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn slice_at(path: &Path, range: (usize, usize)) -> Result<String, String> {
    let source = read(path)?;
    source
        .get(range.0..range.1)
        .map(str::to_string)
        .ok_or_else(|| format!("range {range:?} is outside {}", path.display()))
}

fn goto_hover_snapshot(case: &str) -> Result<String, String> {
    let (workspace, main) = case_paths(case);
    let source = read(&main)?;
    let offset = query_offset(&source, "moon::log", "log")?;
    let mut index = Workspace::new(DiskLoader);
    index.index_root(&workspace);
    let (target, range, kind, detail) = index
        .goto_definition(&main, offset)
        .ok_or_else(|| "goto definition returned no result".to_string())?;
    let hover = index
        .hover(&main, offset)
        .ok_or_else(|| "hover returned no result".to_string())?;

    Ok(format!(
        "query: moon::log\ntarget: {} {}..{} {:?}\ntext: {:?}\ndetail: {:?}\nhover: {:?}\n",
        relative_path(&target, &workspace),
        range.0,
        range.1,
        kind,
        slice_at(&target, range)?,
        detail,
        hover
    ))
}

fn completion_snapshot(case: &str) -> Result<String, String> {
    let (workspace, main) = case_paths(case);
    let source = read(&main)?;
    let offset = source
        .rfind("client.")
        .map(|start| start + "client.".len())
        .ok_or_else(|| "completion query `client.` not found".to_string())?;
    let mut index = Workspace::new(DiskLoader);
    index.index_root(&workspace);
    let mut members = index
        .member_completions(&main, offset)
        .ok_or_else(|| "member completion returned no context".to_string())?;
    members.sort_by(|left, right| left.name.cmp(&right.name));

    let mut output = String::from("query: client.\n");
    for member in members {
        writeln!(
            output,
            "member: {} {:?} {:?}",
            member.name, member.kind, member.detail
        )
        .expect("write to String");
    }
    Ok(output)
}

fn references_snapshot(case: &str) -> Result<String, String> {
    let (workspace, main) = case_paths(case);
    let source = read(&main)?;
    let offset = query_offset(&source, "moon::log", "log")?;
    let mut index = Workspace::new(DiskLoader);
    index.index_root(&workspace);
    let references = index.references(&main, offset, true);
    if references.is_empty() {
        return Err("references returned no results".to_string());
    }

    let mut output = String::from("query: moon::log\n");
    for (path, range) in references {
        writeln!(
            output,
            "reference: {} {}..{} {:?}",
            relative_path(&path, &workspace),
            range.0,
            range.1,
            slice_at(&path, range)?
        )
        .expect("write to String");
    }
    Ok(output)
}

fn rename_snapshot(case: &str) -> Result<String, String> {
    let (workspace, main) = case_paths(case);
    let source = read(&main)?;
    let offset = query_offset(&source, "moon::log", "log")?;
    let mut index = Workspace::new(DiskLoader);
    index.index_root(&workspace);
    match index.rename_edits(&main, offset, "debug") {
        Ok(edits) => Err(format!(
            "rename unexpectedly produced edits for {} file(s)",
            edits.len()
        )),
        Err(error) => Ok(format!("query: moon::log\nrename: rejected {error:?}\n")),
    }
}

fn snapshot(case: &str) -> Result<String, String> {
    match case {
        "completion_members" => completion_snapshot(case),
        "goto_hover_signature" => goto_hover_snapshot(case),
        "references_include_declaration" => references_snapshot(case),
        "rename_readonly_rejected" => rename_snapshot(case),
        _ => Err(format!("unknown .ruai IDE case {case}")),
    }
}

fn expected_path(case: &str) -> PathBuf {
    ruai_root().join(case).join("result.ide.golden")
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
        ".ruai IDE golden mismatch for {case} at line {line}\nexpected: {expected_line:?}\n  \
         actual: {actual_line:?}\nto update, run: {UPDATE_COMMAND}"
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
                "missing .ruai IDE golden for {case}: {}\nto update, run: {UPDATE_COMMAND}",
                fixture_label(&expected_path)
            ));
        }
        Err(error) => {
            return Err(format!(
                "cannot read .ruai IDE golden {}: {error}",
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
    for case in IDE_CASES {
        let actual = snapshot(case)?;
        assert_or_update(case, &actual, update)?;
    }
    Ok(())
}

fn run(result: Result<(), String>) {
    result.unwrap_or_else(|error| panic!("{error}"));
}

#[test]
fn ruai_ide_golden() {
    run(run_goldens(false));
}

#[test]
#[ignore = "updates repository .ruai IDE snapshots; run the documented explicit command"]
fn update_ruai_ide_snapshots() {
    assert_eq!(
        std::env::var(UPDATE_ENV).as_deref(),
        Ok("1"),
        "refusing to update without {UPDATE_ENV}=1"
    );
    run(run_goldens(true));
}
