use std::{
    collections::BTreeSet,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use rua_analysis::{
    AnalysisHost, Change, DocumentSymbol, FileId, FileKind, SourceRootId, SourceRootKind,
};
use rua_syntax::workspace::{DiskLoader, Workspace, normalize_path};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum QueryKind {
    Completion,
    Hover,
    GotoDefinition,
    References,
    Rename,
    DocumentSymbols,
    Diagnostics,
    SemanticTokens,
}

impl QueryKind {
    pub(crate) const ALL: [Self; 8] = [
        Self::Completion,
        Self::Hover,
        Self::GotoDefinition,
        Self::References,
        Self::Rename,
        Self::DocumentSymbols,
        Self::Diagnostics,
        Self::SemanticTokens,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Completion => "completion",
            Self::Hover => "hover",
            Self::GotoDefinition => "goto-definition",
            Self::References => "references",
            Self::Rename => "rename",
            Self::DocumentSymbols => "document-symbols",
            Self::Diagnostics => "diagnostics",
            Self::SemanticTokens => "semantic-tokens",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Behavior {
    StableOrdering,
    ExactByteRange,
    TypeDetail,
    CrossFile,
    DeclarationFile,
    ReadOnly,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExpectedParity {
    Equal,
    ExpectedDifference {
        id: &'static str,
        remove_by: &'static str,
        reason: &'static str,
    },
    Unsupported {
        remove_by: &'static str,
        reason: &'static str,
    },
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Anchor {
    None,
    Word(&'static str, usize),
    Member(&'static str),
    Path(&'static str),
    References(&'static str, usize, bool),
    Rename(&'static str, usize, &'static str),
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum FixtureInput {
    Single(Anchor),
    Workspace { root: &'static str, anchor: Anchor },
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Fixture {
    pub(crate) id: &'static str,
    pub(crate) query: QueryKind,
    pub(crate) source: &'static str,
    pub(crate) oracle: &'static str,
    pub(crate) input: FixtureInput,
    pub(crate) behaviors: &'static [Behavior],
    pub(crate) expectation: ExpectedParity,
}

const COMPLETION_UNSUPPORTED: ExpectedParity = ExpectedParity::Unsupported {
    remove_by: "4B.9",
    reason: "Analysis::completions is not available",
};
const HOVER_UNSUPPORTED: ExpectedParity = ExpectedParity::Unsupported {
    remove_by: "4B.9",
    reason: "Analysis::hover is not available",
};
const GOTO_UNSUPPORTED: ExpectedParity = ExpectedParity::Unsupported {
    remove_by: "4B.9",
    reason: "Analysis::goto_definition is not available",
};
const REFERENCES_UNSUPPORTED: ExpectedParity = ExpectedParity::Unsupported {
    remove_by: "4B.9",
    reason: "Analysis::references is not available",
};
const RENAME_UNSUPPORTED: ExpectedParity = ExpectedParity::Unsupported {
    remove_by: "4B.9",
    reason: "Analysis::rename is not available",
};
const DIAGNOSTICS_UNSUPPORTED: ExpectedParity = ExpectedParity::Unsupported {
    remove_by: "4B.8",
    reason: "Analysis::diagnostics only reports parse errors",
};

pub(crate) const FIXTURES: &[Fixture] = &[
    Fixture {
        id: "ide.completion.local",
        query: QueryKind::Completion,
        source: "tests/golden/ide/completion_local.rua",
        oracle: "tests/golden/ide/completion_local.snap",
        input: FixtureInput::Single(Anchor::Word("local_value", 1)),
        behaviors: &[Behavior::StableOrdering, Behavior::TypeDetail],
        expectation: COMPLETION_UNSUPPORTED,
    },
    Fixture {
        id: "ide.completion.member-struct",
        query: QueryKind::Completion,
        source: "tests/golden/ide/completion_member_struct.rua",
        oracle: "tests/golden/ide/completion_member_struct.snap",
        input: FixtureInput::Single(Anchor::Member("point")),
        behaviors: &[Behavior::StableOrdering, Behavior::TypeDetail],
        expectation: COMPLETION_UNSUPPORTED,
    },
    Fixture {
        id: "ide.completion.member-trait",
        query: QueryKind::Completion,
        source: "tests/golden/ide/completion_member_trait.rua",
        oracle: "tests/golden/ide/completion_member_trait.snap",
        input: FixtureInput::Single(Anchor::Member("job")),
        behaviors: &[Behavior::StableOrdering, Behavior::TypeDetail],
        expectation: COMPLETION_UNSUPPORTED,
    },
    Fixture {
        id: "ide.completion.module-path",
        query: QueryKind::Completion,
        source: "tests/golden/ide/completion_module_path.rua",
        oracle: "tests/golden/ide/completion_module_path.snap",
        input: FixtureInput::Single(Anchor::Path("math")),
        behaviors: &[Behavior::StableOrdering, Behavior::TypeDetail],
        expectation: COMPLETION_UNSUPPORTED,
    },
    Fixture {
        id: "ruai.completion.members",
        query: QueryKind::Completion,
        source: "tests/golden/ruai/completion_members/workspace/main.rua",
        oracle: "tests/golden/ruai/completion_members/result.ide.golden",
        input: FixtureInput::Workspace {
            root: "tests/golden/ruai/completion_members/workspace",
            anchor: Anchor::Member("client"),
        },
        behaviors: &[
            Behavior::StableOrdering,
            Behavior::TypeDetail,
            Behavior::DeclarationFile,
        ],
        expectation: COMPLETION_UNSUPPORTED,
    },
    Fixture {
        id: "ide.hover.function-signature",
        query: QueryKind::Hover,
        source: "tests/golden/ide/hover_function_signature.rua",
        oracle: "tests/golden/ide/hover_function_signature.snap",
        input: FixtureInput::Single(Anchor::Word("add", 1)),
        behaviors: &[Behavior::ExactByteRange, Behavior::TypeDetail],
        expectation: HOVER_UNSUPPORTED,
    },
    Fixture {
        id: "ide.hover.local-type",
        query: QueryKind::Hover,
        source: "tests/golden/ide/hover_local_type.rua",
        oracle: "tests/golden/ide/hover_local_type.snap",
        input: FixtureInput::Single(Anchor::Word("total", 1)),
        behaviors: &[Behavior::ExactByteRange, Behavior::TypeDetail],
        expectation: HOVER_UNSUPPORTED,
    },
    Fixture {
        id: "ruai.hover.signature",
        query: QueryKind::Hover,
        source: "tests/golden/ruai/goto_hover_signature/workspace/main.rua",
        oracle: "tests/golden/ruai/goto_hover_signature/result.ide.golden",
        input: FixtureInput::Workspace {
            root: "tests/golden/ruai/goto_hover_signature/workspace",
            anchor: Anchor::Word("log", 0),
        },
        behaviors: &[
            Behavior::ExactByteRange,
            Behavior::TypeDetail,
            Behavior::DeclarationFile,
        ],
        expectation: HOVER_UNSUPPORTED,
    },
    Fixture {
        id: "ide.goto.local",
        query: QueryKind::GotoDefinition,
        source: "tests/golden/ide/goto_local.rua",
        oracle: "tests/golden/ide/goto_local.snap",
        input: FixtureInput::Single(Anchor::Word("value", 1)),
        behaviors: &[Behavior::ExactByteRange, Behavior::TypeDetail],
        expectation: GOTO_UNSUPPORTED,
    },
    Fixture {
        id: "ide.goto.cross-file",
        query: QueryKind::GotoDefinition,
        source: "tests/golden/ide/goto_cross_file/workspace/main.rua",
        oracle: "tests/golden/ide/goto_cross_file.snap",
        input: FixtureInput::Workspace {
            root: "tests/golden/ide/goto_cross_file/workspace",
            anchor: Anchor::Word("area", 0),
        },
        behaviors: &[
            Behavior::ExactByteRange,
            Behavior::TypeDetail,
            Behavior::CrossFile,
        ],
        expectation: GOTO_UNSUPPORTED,
    },
    Fixture {
        id: "ruai.goto.signature",
        query: QueryKind::GotoDefinition,
        source: "tests/golden/ruai/goto_hover_signature/workspace/main.rua",
        oracle: "tests/golden/ruai/goto_hover_signature/result.ide.golden",
        input: FixtureInput::Workspace {
            root: "tests/golden/ruai/goto_hover_signature/workspace",
            anchor: Anchor::Word("log", 0),
        },
        behaviors: &[
            Behavior::ExactByteRange,
            Behavior::TypeDetail,
            Behavior::DeclarationFile,
        ],
        expectation: GOTO_UNSUPPORTED,
    },
    Fixture {
        id: "ide.references.local",
        query: QueryKind::References,
        source: "tests/golden/ide/references_local.rua",
        oracle: "tests/golden/ide/references_local.snap",
        input: FixtureInput::Single(Anchor::References("value", 1, true)),
        behaviors: &[Behavior::StableOrdering, Behavior::ExactByteRange],
        expectation: REFERENCES_UNSUPPORTED,
    },
    Fixture {
        id: "ide.references.cross-file",
        query: QueryKind::References,
        source: "tests/golden/ide/references_cross_file/workspace/main.rua",
        oracle: "tests/golden/ide/references_cross_file.snap",
        input: FixtureInput::Workspace {
            root: "tests/golden/ide/references_cross_file/workspace",
            anchor: Anchor::References("area", 0, true),
        },
        behaviors: &[
            Behavior::StableOrdering,
            Behavior::ExactByteRange,
            Behavior::CrossFile,
        ],
        expectation: REFERENCES_UNSUPPORTED,
    },
    Fixture {
        id: "ruai.references.include-declaration",
        query: QueryKind::References,
        source: "tests/golden/ruai/references_include_declaration/workspace/main.rua",
        oracle: "tests/golden/ruai/references_include_declaration/result.ide.golden",
        input: FixtureInput::Workspace {
            root: "tests/golden/ruai/references_include_declaration/workspace",
            anchor: Anchor::References("log", 0, true),
        },
        behaviors: &[
            Behavior::StableOrdering,
            Behavior::ExactByteRange,
            Behavior::CrossFile,
            Behavior::DeclarationFile,
        ],
        expectation: REFERENCES_UNSUPPORTED,
    },
    Fixture {
        id: "ide.rename.local",
        query: QueryKind::Rename,
        source: "tests/golden/ide/rename_local.rua",
        oracle: "tests/golden/ide/rename_local.snap",
        input: FixtureInput::Single(Anchor::Rename("value", 1, "total")),
        behaviors: &[Behavior::StableOrdering, Behavior::ExactByteRange],
        expectation: RENAME_UNSUPPORTED,
    },
    Fixture {
        id: "ide.rename.cross-file",
        query: QueryKind::Rename,
        source: "tests/golden/ide/rename_cross_file/workspace/main.rua",
        oracle: "tests/golden/ide/rename_cross_file.snap",
        input: FixtureInput::Workspace {
            root: "tests/golden/ide/rename_cross_file/workspace",
            anchor: Anchor::Rename("area", 0, "surface"),
        },
        behaviors: &[
            Behavior::StableOrdering,
            Behavior::ExactByteRange,
            Behavior::CrossFile,
        ],
        expectation: RENAME_UNSUPPORTED,
    },
    Fixture {
        id: "ruai.rename.readonly-rejected",
        query: QueryKind::Rename,
        source: "tests/golden/ruai/rename_readonly_rejected/workspace/main.rua",
        oracle: "tests/golden/ruai/rename_readonly_rejected/result.ide.golden",
        input: FixtureInput::Workspace {
            root: "tests/golden/ruai/rename_readonly_rejected/workspace",
            anchor: Anchor::Rename("log", 0, "debug"),
        },
        behaviors: &[
            Behavior::ReadOnly,
            Behavior::DeclarationFile,
            Behavior::Error,
        ],
        expectation: RENAME_UNSUPPORTED,
    },
    Fixture {
        id: "ide.document-symbols",
        query: QueryKind::DocumentSymbols,
        source: "tests/golden/ide/document_symbols.rua",
        oracle: "tests/golden/ide/document_symbols.snap",
        input: FixtureInput::Single(Anchor::None),
        behaviors: &[Behavior::StableOrdering, Behavior::ExactByteRange],
        expectation: ExpectedParity::ExpectedDifference {
            id: "native-document-symbol-members",
            remove_by: "4B.9",
            reason: "native symbols omit field, variant, impl, and method records",
        },
    },
    Fixture {
        id: "ide.diagnostics.fast",
        query: QueryKind::Diagnostics,
        source: "tests/golden/ide/diagnostics_fast.rua",
        oracle: "tests/golden/ide/diagnostics_fast.snap",
        input: FixtureInput::Single(Anchor::None),
        behaviors: &[
            Behavior::StableOrdering,
            Behavior::ExactByteRange,
            Behavior::Error,
        ],
        expectation: DIAGNOSTICS_UNSUPPORTED,
    },
    Fixture {
        id: "ruai.diagnostics.type-error",
        query: QueryKind::Diagnostics,
        source: "tests/golden/ruai/declaration_type_error/workspace/main.rua",
        oracle: "tests/golden/ruai/declaration_type_error/workspace/main.diag.golden",
        input: FixtureInput::Workspace {
            root: "tests/golden/ruai/declaration_type_error/workspace",
            anchor: Anchor::None,
        },
        behaviors: &[
            Behavior::StableOrdering,
            Behavior::ExactByteRange,
            Behavior::DeclarationFile,
            Behavior::Error,
        ],
        expectation: DIAGNOSTICS_UNSUPPORTED,
    },
    Fixture {
        id: "ide.closure.completion",
        query: QueryKind::Completion,
        source: "tests/golden/ide/closure_iterator.rua",
        oracle: "tests/golden/ide/closure_iterator.snap",
        input: FixtureInput::Single(Anchor::Word("item", 1)),
        behaviors: &[Behavior::StableOrdering, Behavior::TypeDetail],
        expectation: COMPLETION_UNSUPPORTED,
    },
    Fixture {
        id: "ide.closure.goto",
        query: QueryKind::GotoDefinition,
        source: "tests/golden/ide/closure_iterator.rua",
        oracle: "tests/golden/ide/closure_iterator.snap",
        input: FixtureInput::Single(Anchor::Word("item", 1)),
        behaviors: &[Behavior::ExactByteRange, Behavior::TypeDetail],
        expectation: GOTO_UNSUPPORTED,
    },
    Fixture {
        id: "ide.closure.references",
        query: QueryKind::References,
        source: "tests/golden/ide/closure_iterator.rua",
        oracle: "tests/golden/ide/closure_iterator.snap",
        input: FixtureInput::Single(Anchor::References("item", 1, true)),
        behaviors: &[Behavior::StableOrdering, Behavior::ExactByteRange],
        expectation: REFERENCES_UNSUPPORTED,
    },
    Fixture {
        id: "ide.closure.rename",
        query: QueryKind::Rename,
        source: "tests/golden/ide/closure_iterator.rua",
        oracle: "tests/golden/ide/closure_iterator.snap",
        input: FixtureInput::Single(Anchor::Rename("item", 1, "element")),
        behaviors: &[Behavior::StableOrdering, Behavior::ExactByteRange],
        expectation: RENAME_UNSUPPORTED,
    },
    Fixture {
        id: "ide.closure.semantic-tokens",
        query: QueryKind::SemanticTokens,
        source: "tests/golden/ide/closure_iterator.rua",
        oracle: "tests/golden/ide/closure_iterator.snap",
        input: FixtureInput::Single(Anchor::None),
        behaviors: &[Behavior::StableOrdering, Behavior::ExactByteRange],
        expectation: ExpectedParity::Equal,
    },
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct NormalizedOutput(Vec<String>);

impl NormalizedOutput {
    fn from_text(text: &str) -> Self {
        let root = workspace_root().to_string_lossy().replace('\\', "/");
        Self(
            text.replace("\r\n", "\n")
                .replace('\\', "/")
                .replace(&root, "<workspace>")
                .lines()
                .map(str::trim_end)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect(),
        )
    }
}

#[derive(Debug)]
enum NativeResult {
    Supported(NormalizedOutput),
    Unsupported(&'static str),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ParityStatus {
    Equal,
    ExpectedDifference(&'static str),
    Unsupported(&'static str),
}

pub(crate) fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonical workspace root")
}

pub(crate) fn workspace_path(relative: &str) -> PathBuf {
    workspace_root().join(relative)
}

pub(crate) fn read_fixture(relative: &str) -> String {
    let path = workspace_path(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

pub(crate) fn registered_oracles_under(relative: &str, file_name: &str) -> BTreeSet<String> {
    let root = workspace_path(relative);
    let mut paths = Vec::new();
    discover_files(&root, &mut paths);
    paths
        .into_iter()
        .filter(|path| path.file_name().is_some_and(|name| name == file_name))
        .map(relative_path)
        .collect()
}

pub(crate) fn registered_ide_snapshots() -> BTreeSet<String> {
    let root = workspace_path("tests/golden/ide");
    fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("read {}: {error}", root.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "snap")
        })
        .map(relative_path)
        .collect()
}

fn relative_path(path: PathBuf) -> String {
    path.strip_prefix(workspace_root())
        .unwrap_or(&path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn discover_files(directory: &Path, paths: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()));
    for entry in entries {
        let path = entry.expect("read directory entry").path();
        if path.is_dir() {
            discover_files(&path, paths);
        } else {
            paths.push(path);
        }
    }
}

pub(crate) struct Phase4aCase {
    pub(crate) id: String,
    pub(crate) source: PathBuf,
    pub(crate) oracle: PathBuf,
    pub(crate) accepted: bool,
    pub(crate) remove_by: &'static str,
    pub(crate) unsupported_reason: &'static str,
}

pub(crate) fn phase4a_cases() -> Vec<Phase4aCase> {
    let mut cases = Vec::new();
    for (directory, accepted, extension) in [
        ("compile-pass", true, "lua.golden"),
        ("compile-fail", false, "diag.golden"),
    ] {
        let root = workspace_path(&format!("tests/golden/phase4a/{directory}"));
        let mut sources = fs::read_dir(&root)
            .unwrap_or_else(|error| panic!("read {}: {error}", root.display()))
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|value| value == "rua"))
            .collect::<Vec<_>>();
        sources.sort();
        for source in sources {
            let name = source
                .file_stem()
                .expect("Phase 4A source stem")
                .to_string_lossy();
            cases.push(Phase4aCase {
                id: format!("phase4a.{directory}.{name}"),
                oracle: source.with_extension(extension),
                source,
                accepted,
                remove_by: if accepted { "4B.7" } else { "4B.8" },
                unsupported_reason: if accepted {
                    "native closure and iterator inference is not available"
                } else {
                    "native type diagnostics are not available"
                },
            });
        }
    }
    cases
}

pub(crate) fn classify_fixture(fixture: &Fixture) -> Result<ParityStatus, String> {
    let legacy = legacy_result(fixture);
    let native = native_result(fixture);
    match (fixture.expectation, native) {
        (ExpectedParity::Equal, NativeResult::Supported(native)) if legacy == native => {
            Ok(ParityStatus::Equal)
        }
        (
            ExpectedParity::ExpectedDifference {
                id,
                remove_by,
                reason,
            },
            NativeResult::Supported(native),
        ) if expected_difference_matches(id, &legacy, &native) => {
            validate_tracking(id, remove_by, reason)?;
            Ok(ParityStatus::ExpectedDifference(id))
        }
        (
            ExpectedParity::Unsupported { remove_by, reason },
            NativeResult::Unsupported(actual_reason),
        ) if reason == actual_reason => {
            validate_tracking("unsupported", remove_by, reason)?;
            Ok(ParityStatus::Unsupported(remove_by))
        }
        (expected, actual) => Err(format!(
            "{} baseline changed: expected {expected:?}, got {actual:?}",
            fixture.id
        )),
    }
}

fn expected_difference_matches(
    id: &str,
    legacy: &NormalizedOutput,
    native: &NormalizedOutput,
) -> bool {
    match id {
        "native-document-symbol-members" => {
            let expected_native = legacy
                .0
                .iter()
                .filter(|record| {
                    !["|Field|", "|Variant|", "|Impl|", "|Method|"]
                        .iter()
                        .any(|kind| record.contains(kind))
                })
                .cloned()
                .collect::<Vec<_>>();
            legacy != native && expected_native == native.0
        }
        _ => false,
    }
}

fn validate_tracking(id: &str, remove_by: &str, reason: &str) -> Result<(), String> {
    if id.is_empty() || reason.is_empty() || !remove_by.starts_with(['4', '5']) {
        return Err(format!(
            "invalid parity tracking: id={id:?}, remove_by={remove_by:?}, reason={reason:?}"
        ));
    }
    Ok(())
}

fn native_result(fixture: &Fixture) -> NativeResult {
    match fixture.id {
        "ide.document-symbols" => {
            NativeResult::Supported(native_document_symbols(&read_fixture(fixture.source)))
        }
        "ide.closure.semantic-tokens" => {
            NativeResult::Supported(native_semantic_tokens(&read_fixture(fixture.source)))
        }
        _ => NativeResult::Unsupported(match fixture.query {
            QueryKind::Completion => "Analysis::completions is not available",
            QueryKind::Hover => "Analysis::hover is not available",
            QueryKind::GotoDefinition => "Analysis::goto_definition is not available",
            QueryKind::References => "Analysis::references is not available",
            QueryKind::Rename => "Analysis::rename is not available",
            QueryKind::Diagnostics => "Analysis::diagnostics only reports parse errors",
            QueryKind::DocumentSymbols | QueryKind::SemanticTokens => {
                "fixture has no native adapter"
            }
        }),
    }
}

fn legacy_result(fixture: &Fixture) -> NormalizedOutput {
    match fixture.id {
        "ide.document-symbols" => {
            let source = read_fixture(fixture.source);
            assert_legacy_oracle(fixture, &legacy_document_symbols_snapshot(&source));
            legacy_document_symbols(&source)
        }
        "ide.closure.semantic-tokens" => {
            let oracle = read_fixture(fixture.oracle);
            let tokens = oracle
                .lines()
                .filter(|line| line.starts_with("token: "))
                .collect::<Vec<_>>()
                .join("\n");
            NormalizedOutput::from_text(&tokens)
        }
        _ => {
            let actual = legacy_snapshot(fixture);
            assert_legacy_oracle(fixture, &actual);
            NormalizedOutput::from_text(&actual)
        }
    }
}

fn assert_legacy_oracle(fixture: &Fixture, actual: &str) {
    let expected = oracle_projection(fixture, &read_fixture(fixture.oracle));
    assert_eq!(
        NormalizedOutput::from_text(&expected),
        NormalizedOutput::from_text(actual),
        "legacy oracle drifted for {}",
        fixture.id
    );
}

fn oracle_projection(fixture: &Fixture, oracle: &str) -> String {
    let prefix = match fixture.id {
        "ide.closure.completion" => Some("completion: "),
        "ide.closure.goto" => Some("definition: "),
        "ide.closure.references" => Some("reference: "),
        "ide.closure.rename" => Some("rename: "),
        "ide.closure.semantic-tokens" => Some("token: "),
        _ => None,
    };
    match prefix {
        Some(prefix) => oracle
            .lines()
            .filter(|line| line.starts_with(prefix))
            .collect::<Vec<_>>()
            .join("\n"),
        None => oracle.to_string(),
    }
}

fn legacy_snapshot(fixture: &Fixture) -> String {
    match fixture.query {
        QueryKind::Completion => legacy_completion(fixture),
        QueryKind::Hover | QueryKind::GotoDefinition => legacy_navigation(fixture),
        QueryKind::References => legacy_references(fixture),
        QueryKind::Rename => legacy_rename(fixture),
        QueryKind::Diagnostics => legacy_diagnostics(fixture),
        QueryKind::DocumentSymbols | QueryKind::SemanticTokens => {
            panic!("{} uses a dedicated legacy adapter", fixture.id)
        }
    }
}

fn legacy_completion(fixture: &Fixture) -> String {
    let source = read_fixture(fixture.source);
    match fixture.input {
        FixtureInput::Single(Anchor::Word(word, occurrence)) => {
            let offset = nth_word(&source, word, occurrence);
            let analysis = rua_syntax::analysis::Analysis::new(&source);
            if fixture.id == "ide.closure.completion" {
                let local = analysis
                    .scope_locals(offset)
                    .into_iter()
                    .find(|local| local.name == word)
                    .unwrap_or_else(|| panic!("{} returned no local {word:?}", fixture.id));
                return format!("completion: {} {:?}\n", local.name, local.detail);
            }

            let mut locals = analysis.scope_locals(offset);
            locals.sort_by(|left, right| left.name.cmp(&right.name));
            let mut output = format!("query: {word}\n");
            for local in locals {
                writeln!(output, "completion: {} {:?}", local.name, local.detail).unwrap();
            }
            output
        }
        FixtureInput::Single(Anchor::Member(receiver)) => {
            let offset = member_offset(&source, receiver);
            let analysis = rua_syntax::analysis::Analysis::new(&source);
            let mut members = analysis
                .member_completions(offset)
                .unwrap_or_else(|| panic!("{} returned no member context", fixture.id));
            members.sort_by(|left, right| left.name.cmp(&right.name));
            let mut output = format!("query: {receiver}.\n");
            for member in members {
                writeln!(
                    output,
                    "completion: {} {:?} {:?}",
                    member.name, member.kind, member.detail
                )
                .unwrap();
            }
            output
        }
        FixtureInput::Single(Anchor::Path(receiver)) => {
            let offset = path_offset(&source, receiver);
            let analysis = rua_syntax::analysis::Analysis::new(&source);
            let mut symbols = analysis
                .path_completions(offset)
                .unwrap_or_else(|| panic!("{} returned no path context", fixture.id));
            symbols.sort_by(|left, right| left.name.cmp(&right.name));
            let mut output = format!("query: {receiver}::\n");
            for symbol in symbols {
                writeln!(
                    output,
                    "completion: {} {:?} {:?}",
                    symbol.name, symbol.kind, symbol.detail
                )
                .unwrap();
            }
            output
        }
        FixtureInput::Workspace {
            root,
            anchor: Anchor::Member(receiver),
        } => {
            let root = workspace_path(root);
            let main = workspace_path(fixture.source);
            let offset = member_offset(&source, receiver);
            let mut workspace = Workspace::new(DiskLoader);
            workspace.index_root(&root);
            let mut members = workspace
                .member_completions(&main, offset)
                .unwrap_or_else(|| panic!("{} returned no member context", fixture.id));
            members.sort_by(|left, right| left.name.cmp(&right.name));
            let mut output = format!("query: {receiver}.\n");
            for member in members {
                writeln!(
                    output,
                    "member: {} {:?} {:?}",
                    member.name, member.kind, member.detail
                )
                .unwrap();
            }
            output
        }
        input => panic!("invalid completion input for {}: {input:?}", fixture.id),
    }
}

fn legacy_navigation(fixture: &Fixture) -> String {
    let source = read_fixture(fixture.source);
    match fixture.input {
        FixtureInput::Single(Anchor::Word(word, occurrence)) => {
            let offset = nth_word(&source, word, occurrence);
            let analysis = rua_syntax::analysis::Analysis::new(&source);
            let resolution = analysis
                .definition_at(offset)
                .unwrap_or_else(|| panic!("{} returned no definition", fixture.id));
            if fixture.id == "ide.closure.goto" {
                return format!(
                    "definition: {}..{} {:?}\n",
                    resolution.target_range.0, resolution.target_range.1, resolution.detail
                );
            }
            format!(
                "query: {word}\ntarget: {}..{} {:?}\ntext: {:?}\ndetail: {:?}\n",
                resolution.target_range.0,
                resolution.target_range.1,
                resolution.kind,
                source_slice(&source, resolution.target_range),
                resolution.detail
            )
        }
        FixtureInput::Workspace {
            root,
            anchor: Anchor::Word(word, occurrence),
        } => {
            let root = workspace_path(root);
            let main = workspace_path(fixture.source);
            let offset = nth_word(&source, word, occurrence);
            let mut workspace = Workspace::new(DiskLoader);
            workspace.index_root(&root);
            let (target, range, kind, detail) = workspace
                .goto_definition(&main, offset)
                .unwrap_or_else(|| panic!("{} returned no definition", fixture.id));
            let target_source = fs::read_to_string(&target)
                .unwrap_or_else(|error| panic!("read {}: {error}", target.display()));
            if fixture.id.starts_with("ruai.") {
                let hover = workspace
                    .hover(&main, offset)
                    .unwrap_or_else(|| panic!("{} returned no hover", fixture.id));
                return format!(
                    "query: moon::log\ntarget: {} {}..{} {:?}\ntext: {:?}\ndetail: {:?}\nhover: {:?}\n",
                    path_relative_to(&target, &root),
                    range.0,
                    range.1,
                    kind,
                    source_slice(&target_source, range),
                    detail,
                    hover
                );
            }
            format!(
                "query: geometry::area\ntarget: {} {}..{} {:?}\ntext: {:?}\ndetail: {:?}\n",
                path_relative_to(&target, &root),
                range.0,
                range.1,
                kind,
                source_slice(&target_source, range),
                detail
            )
        }
        input => panic!("invalid navigation input for {}: {input:?}", fixture.id),
    }
}

fn legacy_references(fixture: &Fixture) -> String {
    let source = read_fixture(fixture.source);
    match fixture.input {
        FixtureInput::Single(Anchor::References(word, occurrence, _)) => {
            let offset = nth_word(&source, word, occurrence);
            let analysis = rua_syntax::analysis::Analysis::new(&source);
            let references = analysis.references_at(offset);
            let mut output = if fixture.id == "ide.closure.references" {
                String::new()
            } else {
                format!("query: {word}\n")
            };
            for range in references {
                writeln!(
                    output,
                    "reference: {}..{} {:?}",
                    range.0,
                    range.1,
                    source_slice(&source, range)
                )
                .unwrap();
            }
            output
        }
        FixtureInput::Workspace {
            root,
            anchor: Anchor::References(word, occurrence, include_declaration),
        } => {
            let root = workspace_path(root);
            let main = workspace_path(fixture.source);
            let offset = nth_word(&source, word, occurrence);
            let mut workspace = Workspace::new(DiskLoader);
            workspace.index_root(&root);
            let references = workspace.references(&main, offset, include_declaration);
            let query = if fixture.id.starts_with("ruai.") {
                "moon::log"
            } else {
                "geometry::area"
            };
            let mut output = format!("query: {query}\n");
            for (path, range) in references {
                let target_source = fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
                writeln!(
                    output,
                    "reference: {} {}..{} {:?}",
                    path_relative_to(&path, &root),
                    range.0,
                    range.1,
                    source_slice(&target_source, range)
                )
                .unwrap();
            }
            output
        }
        input => panic!("invalid references input for {}: {input:?}", fixture.id),
    }
}

fn legacy_rename(fixture: &Fixture) -> String {
    let source = read_fixture(fixture.source);
    match fixture.input {
        FixtureInput::Single(Anchor::Rename(word, occurrence, replacement)) => {
            let offset = nth_word(&source, word, occurrence);
            let analysis = rua_syntax::analysis::Analysis::new(&source);
            let edits = analysis
                .rename_edits(offset, replacement)
                .unwrap_or_else(|error| panic!("{} rename failed: {error:?}", fixture.id));
            let mut output = if fixture.id == "ide.closure.rename" {
                String::new()
            } else {
                format!("query: {word} -> {replacement}\n")
            };
            let prefix = if fixture.id == "ide.closure.rename" {
                "rename"
            } else {
                "edit"
            };
            for (start, end, replacement) in edits {
                writeln!(
                    output,
                    "{prefix}: {start}..{end} {:?} -> {:?}",
                    source_slice(&source, (start, end)),
                    replacement
                )
                .unwrap();
            }
            output
        }
        FixtureInput::Workspace {
            root,
            anchor: Anchor::Rename(word, occurrence, replacement),
        } => {
            let root = workspace_path(root);
            let main = workspace_path(fixture.source);
            let offset = nth_word(&source, word, occurrence);
            let mut workspace = Workspace::new(DiskLoader);
            workspace.index_root(&root);
            if fixture.id == "ruai.rename.readonly-rejected" {
                return match workspace.rename_edits(&main, offset, replacement) {
                    Ok(edits) => panic!(
                        "{} unexpectedly produced edits for {} files",
                        fixture.id,
                        edits.len()
                    ),
                    Err(error) => format!("query: moon::log\nrename: rejected {error:?}\n"),
                };
            }

            let edits = workspace
                .rename_edits(&main, offset, replacement)
                .unwrap_or_else(|error| panic!("{} rename failed: {error:?}", fixture.id));
            let mut edits = edits.into_iter().collect::<Vec<_>>();
            edits.sort_by(|left, right| left.0.cmp(&right.0));
            let mut output = format!("query: geometry::area -> {replacement}\n");
            for (path, mut file_edits) in edits {
                let target_source = fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
                file_edits.sort_by_key(|(start, _, _)| *start);
                for (start, end, replacement) in file_edits {
                    writeln!(
                        output,
                        "edit: {} {start}..{end} {:?} -> {:?}",
                        path_relative_to(&path, &root),
                        source_slice(&target_source, (start, end)),
                        replacement
                    )
                    .unwrap();
                }
            }
            output
        }
        input => panic!("invalid rename input for {}: {input:?}", fixture.id),
    }
}

fn legacy_diagnostics(fixture: &Fixture) -> String {
    if fixture.id == "ruai.diagnostics.type-error" {
        let source = workspace_path(fixture.source);
        let error = ruac::compile_path(&source)
            .err()
            .unwrap_or_else(|| panic!("{} unexpectedly compiled", fixture.id));
        return stable_compiler_output(&error);
    }

    let source = read_fixture(fixture.source);
    let (diagnostics, _) = ruac::check_diags(&source);
    let mut output = String::new();
    for diagnostic in diagnostics {
        let range = (diagnostic.start, diagnostic.start + diagnostic.len);
        let text = if diagnostic.len == 0 {
            ""
        } else {
            source_slice(&source, range)
        };
        writeln!(
            output,
            "diagnostic: line={} range={}..{} text={:?} message={:?}",
            diagnostic.line, range.0, range.1, text, diagnostic.msg
        )
        .unwrap();
    }
    output
}

fn nth_word(source: &str, word: &str, occurrence: usize) -> usize {
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
            if seen == occurrence {
                return start;
            }
            seen += 1;
        }
        cursor = start + 1;
    }
    panic!("word {word:?} occurrence {occurrence} not found")
}

fn member_offset(source: &str, receiver: &str) -> usize {
    let query = format!("{receiver}.");
    source
        .rfind(&query)
        .map(|start| start + query.len())
        .unwrap_or_else(|| panic!("member query {query:?} not found"))
}

fn path_offset(source: &str, receiver: &str) -> usize {
    let query = format!("{receiver}::");
    source
        .rfind(&query)
        .map(|start| start + query.len())
        .unwrap_or_else(|| panic!("path query {query:?} not found"))
}

fn source_slice(source: &str, range: (usize, usize)) -> &str {
    source
        .get(range.0..range.1)
        .unwrap_or_else(|| panic!("range {range:?} is outside source"))
}

fn path_relative_to(path: &Path, root: &Path) -> String {
    let path = normalize_path(path);
    let root = normalize_path(root);
    path.strip_prefix(&root)
        .unwrap_or(&path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn stable_compiler_output(output: &str) -> String {
    let normalized = output.replace('\\', "/");
    let golden = workspace_path("tests/golden")
        .to_string_lossy()
        .replace('\\', "/");
    normalized.replace(&golden, "<golden>")
}

fn analysis_for_source(source: &str) -> (rua_analysis::Analysis, FileId) {
    let root_id = SourceRootId::new(0);
    let file_id = FileId::new(0);
    let mut change = Change::new();
    change.set_source_root(root_id, SourceRootKind::Workspace);
    change.set_file_with_path(file_id, root_id, FileKind::Source, "main.rua", source);
    let mut host = AnalysisHost::new();
    host.apply_change(change);
    (host.analysis(), file_id)
}

fn legacy_document_symbols(source: &str) -> NormalizedOutput {
    let analysis = rua_syntax::analysis::Analysis::new(source);
    let mut output = String::new();
    for symbol in analysis.symbols() {
        let container = if symbol.container.is_empty() {
            "<root>".to_string()
        } else {
            symbol.container.join("::")
        };
        output.push_str(&format!(
            "{}|{:?}|{}|{}..{}|{}..{}\n",
            symbol.name,
            symbol.kind,
            container,
            symbol.name_range.0,
            symbol.name_range.1,
            symbol.full_range.0,
            symbol.full_range.1
        ));
    }
    NormalizedOutput::from_text(&output)
}

fn legacy_document_symbols_snapshot(source: &str) -> String {
    let analysis = rua_syntax::analysis::Analysis::new(source);
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
        .unwrap();
    }
    output
}

fn native_document_symbols(source: &str) -> NormalizedOutput {
    let (analysis, file_id) = analysis_for_source(source);
    let mut output = String::new();
    flatten_native_symbols(
        &analysis.document_symbols(file_id, file_id),
        &mut Vec::new(),
        &mut output,
    );
    NormalizedOutput::from_text(&output)
}

fn flatten_native_symbols(
    symbols: &[DocumentSymbol],
    container: &mut Vec<String>,
    output: &mut String,
) {
    for symbol in symbols {
        let parent = if container.is_empty() {
            "<root>".to_string()
        } else {
            container.join("::")
        };
        let selection = symbol.selection_range();
        let range = symbol.range();
        output.push_str(&format!(
            "{}|{:?}|{}|{}..{}|{}..{}\n",
            symbol.name(),
            symbol.kind(),
            parent,
            selection.start(),
            selection.end(),
            range.start(),
            range.end()
        ));
        container.push(symbol.name().to_string());
        flatten_native_symbols(symbol.children(), container, output);
        container.pop();
    }
}

fn native_semantic_tokens(source: &str) -> NormalizedOutput {
    let (analysis, file_id) = analysis_for_source(source);
    let mut output = String::new();
    for token in analysis.semantic_tokens(file_id) {
        let range = token.range();
        output.push_str(&format!(
            "token: {:?} {}..{} text={:?} declaration={}\n",
            token.kind(),
            range.start(),
            range.end(),
            &source[range.start() as usize..range.end() as usize],
            token.is_declaration()
        ));
    }
    NormalizedOutput::from_text(&output)
}
