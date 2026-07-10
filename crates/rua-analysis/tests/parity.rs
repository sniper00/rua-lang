//! Module/name-resolution parity against the compiler oracle corpus.

use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use rua_analysis::{
    Analysis, AnalysisHost, Change, DefKind, FileId, FileKind, FilePosition, SourceRootId,
    SourceRootKind,
};
use rua_syntax::{AstNode, Named, ast::ClosureExpr};
use ruac::ast::{Item as CompilerItem, Program};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonical workspace root")
}

fn golden_root() -> PathBuf {
    workspace_root().join("tests/golden")
}

fn analysis_for_source(source: &str) -> (Analysis, FileId) {
    let root_id = SourceRootId::new(0);
    let file_id = FileId::new(0);
    let mut change = Change::new();
    change.set_source_root(root_id, SourceRootKind::Workspace);
    change.set_file_with_path(file_id, root_id, FileKind::Source, "main.rua", source);
    let mut host = AnalysisHost::new();
    host.apply_change(change);
    (host.analysis(), file_id)
}

fn analysis_for_workspace(directory: &Path, main: &Path) -> (Analysis, FileId) {
    let mut paths = Vec::new();
    discover_sources(directory, &mut paths);
    paths.sort();
    let root_id = SourceRootId::new(0);
    let mut main_id = None;
    let mut change = Change::new();
    change.set_source_root(root_id, SourceRootKind::Workspace);
    for (index, path) in paths.into_iter().enumerate() {
        let file_id = FileId::new(index as u32);
        if path == main {
            main_id = Some(file_id);
        }
        let relative = path
            .strip_prefix(directory)
            .expect("source belongs to fixture workspace");
        let kind = if path.extension() == Some(OsStr::new("ruai")) {
            FileKind::Declaration
        } else {
            FileKind::Source
        };
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        change.set_file_with_path(file_id, root_id, kind, relative, text);
    }
    let mut host = AnalysisHost::new();
    host.apply_change(change);
    (
        host.analysis(),
        main_id.unwrap_or_else(|| panic!("main file {} was not discovered", main.display())),
    )
}

fn discover_sources(directory: &Path, paths: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read directory {}: {error}", directory.display()));
    for entry in entries {
        let path = entry.expect("read directory entry").path();
        if path.is_dir() {
            discover_sources(&path, paths);
        } else if matches!(
            path.extension().and_then(OsStr::to_str),
            Some("rua" | "ruai")
        ) {
            paths.push(path);
        }
    }
}

fn compiler_definitions(program: &Program) -> BTreeSet<(String, &'static str)> {
    let mut definitions = BTreeSet::new();
    collect_compiler_items(&program.items, &mut Vec::new(), &mut definitions);
    definitions
}

fn collect_compiler_items(
    items: &[CompilerItem],
    module_path: &mut Vec<String>,
    definitions: &mut BTreeSet<(String, &'static str)>,
) {
    for item in items {
        match item {
            CompilerItem::Fn(item) => {
                insert_definition(definitions, module_path, &item.name, "function");
            }
            CompilerItem::Struct(item) => {
                insert_definition(definitions, module_path, &item.name, "struct");
            }
            CompilerItem::Enum(item) => {
                insert_definition(definitions, module_path, &item.name, "enum");
            }
            CompilerItem::Trait(item) => {
                insert_definition(definitions, module_path, &item.name, "trait");
            }
            CompilerItem::Extern(block) => {
                for function in &block.fns {
                    insert_definition(definitions, module_path, &function.name, "function");
                }
            }
            CompilerItem::Mod(item) => {
                insert_definition(definitions, module_path, &item.name, "module");
                module_path.push(item.name.clone());
                collect_compiler_items(&item.items, module_path, definitions);
                module_path.pop();
            }
            CompilerItem::Impl(_) | CompilerItem::Use(_) => {}
        }
    }
}

fn insert_definition(
    definitions: &mut BTreeSet<(String, &'static str)>,
    module_path: &[String],
    name: &str,
    kind: &'static str,
) {
    let path = if module_path.is_empty() {
        name.to_string()
    } else {
        format!("{}::{name}", module_path.join("::"))
    };
    definitions.insert((path, kind));
}

fn analysis_definitions(
    analysis: &Analysis,
    root_file: FileId,
) -> BTreeSet<(String, &'static str)> {
    analysis
        .workspace_symbols(root_file, "")
        .into_iter()
        .map(|symbol| {
            let path = symbol
                .container_name()
                .map(|container| format!("{container}::{}", symbol.name()))
                .unwrap_or_else(|| symbol.name().to_string());
            (path, analysis_kind(symbol.kind()))
        })
        .collect()
}

fn analysis_kind(kind: DefKind) -> &'static str {
    match kind {
        DefKind::Function => "function",
        DefKind::Struct => "struct",
        DefKind::Enum => "enum",
        DefKind::Trait => "trait",
        DefKind::Module => "module",
        DefKind::TypeAlias => "type_alias",
    }
}

fn offset_of(source: &str, needle: &str, occurrence: usize) -> u32 {
    source
        .match_indices(needle)
        .nth(occurrence)
        .unwrap_or_else(|| panic!("missing occurrence {occurrence} of {needle:?}"))
        .0 as u32
        + 1
}

#[test]
fn parity_inline_module_and_name_resolution() {
    let cases = [
        ("module_inline_basic.rua", "add", 1, "add"),
        ("module_inline_nested.rua", "value", 2, "value"),
        ("module_use_alias.rua", "answer", 1, "value"),
        ("module_use_grouped.rua", "one", 2, "one"),
        ("module_use_grouped.rua", "second", 1, "two"),
        ("visibility_pub_access.rua", "visible", 1, "visible"),
        ("visibility_private_same_module.rua", "hidden", 1, "hidden"),
    ];

    for (fixture, needle, occurrence, expected_definition) in cases {
        let path = golden_root().join("compile-pass").join(fixture);
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        ruac::compile_str(&source)
            .unwrap_or_else(|error| panic!("compiler rejected {fixture}: {error}"));
        let compiler = ruac::parser::parse(&source)
            .unwrap_or_else(|error| panic!("compiler parse failed for {fixture}: {error}"));
        let (analysis, file_id) = analysis_for_source(&source);

        assert_eq!(
            analysis_definitions(&analysis, file_id),
            compiler_definitions(&compiler),
            "definition parity failed for {fixture}"
        );
        let definition = analysis
            .semantics(file_id)
            .find_def_at(FilePosition::new(
                file_id,
                offset_of(&source, needle, occurrence),
            ))
            .unwrap_or_else(|| panic!("analysis did not resolve {needle:?} in {fixture}"));
        assert_eq!(
            definition.name(),
            expected_definition,
            "name-resolution parity failed for {fixture}"
        );
    }
}

#[test]
fn parity_file_modules_and_declaration_files() {
    let cases = [
        "library_decl_basic",
        "library_decl_module_dir",
        "library_mount_single_file",
        "workspace_shadows_library",
    ];

    for fixture in cases {
        let directory = golden_root().join("ruai").join(fixture).join("workspace");
        let main = directory.join("main.rua");
        ruac::compile_path(&main)
            .unwrap_or_else(|error| panic!("compiler rejected {fixture}: {error}"));
        let (compiler, _) = ruac::parse_and_resolve(&main)
            .unwrap_or_else(|error| panic!("compiler resolution failed for {fixture}: {error}"));
        let (analysis, main_id) = analysis_for_workspace(&directory, &main);

        assert_eq!(
            analysis_definitions(&analysis, main_id),
            compiler_definitions(&compiler),
            "file-module parity failed for {fixture}"
        );
    }
}

#[test]
fn closure_type_parity_matches_rowan_parameter_ranges() {
    let source = concat!(
        "fn main() -> i64 {\n",
        "  let factor = 3;\n",
        "  let scale = |value| value * factor;\n",
        "  let scaled = scale(14);\n",
        "  let add = |left: i64, right| -> i64 { left + right };\n",
        "  let mapped_count = vec![1, 2].iter().map(|item| item + 1).count();\n",
        "  add(scaled, 8)\n",
        "}\n",
    );
    let parsed = rua_syntax::parse(source);
    assert!(parsed.errors().is_empty(), "rowan parser rejected closure");

    let rowan_params: Vec<_> = parsed
        .syntax_node()
        .descendants()
        .filter_map(ClosureExpr::cast)
        .flat_map(|closure| closure.params().collect::<Vec<_>>())
        .map(|param| {
            let name = param.name().expect("closure parameter name");
            let range = name.text_range();
            (
                name.text().to_string(),
                usize::from(range.start()),
                usize::from(range.len()),
            )
        })
        .collect();

    let compiler = ruac::binding_types(source);
    let compiler_params: Vec<_> = compiler
        .hits()
        .iter()
        .filter(|binding| binding.display.starts_with("closure parameter "))
        .map(|binding| {
            (
                source[binding.name_start..binding.name_start + binding.name_len].to_string(),
                binding.name_start,
                binding.name_len,
                binding.display.clone(),
            )
        })
        .collect();

    assert_eq!(rowan_params.len(), compiler_params.len());
    for ((name, start, len), (compiler_name, compiler_start, compiler_len, display)) in
        rowan_params.iter().zip(&compiler_params)
    {
        assert_eq!(
            (name, start, len),
            (compiler_name, compiler_start, compiler_len)
        );
        assert_eq!(display, &format!("closure parameter {name}: i64"));
    }

    let (diagnostics, _) = ruac::check_diags(source);
    assert!(
        diagnostics.is_empty(),
        "compiler type checker rejected rowan-accepted closure: {diagnostics:?}"
    );
}

#[test]
fn closure_type_parity_preserves_mutable_capture_range() {
    let source = concat!(
        "fn main() {\n",
        "  let mut total = 0;\n",
        "  let update = |value: i64| { total = total + value; };\n",
        "  update(1);\n",
        "}\n",
    );
    let parsed = rua_syntax::parse(source);
    assert!(parsed.errors().is_empty());
    let (diagnostics, _) = ruac::check_diags(source);
    let diagnostic = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.msg.starts_with("mutable capture of `total`"))
        .expect("mutable capture diagnostic");
    assert_eq!(
        &source[diagnostic.start..diagnostic.start + diagnostic.len],
        "total"
    );
    assert!(
        parsed
            .syntax_node()
            .descendants_with_tokens()
            .any(|element| {
                element.into_token().is_some_and(|token| {
                    token.text() == "total"
                        && usize::from(token.text_range().start()) == diagnostic.start
                        && usize::from(token.text_range().len()) == diagnostic.len
                })
            })
    );
}
