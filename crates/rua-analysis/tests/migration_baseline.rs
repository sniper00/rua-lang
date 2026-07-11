//! Executable legacy/native inventory for the native analysis migration.

mod support;

use std::collections::BTreeSet;

use support::migration::{
    Behavior, FIXTURES, ParityStatus, QueryKind, classify_fixture, phase4a_cases, read_fixture,
    registered_ide_snapshots, registered_oracles_under, workspace_root,
};

#[test]
fn migration_baseline_manifest_is_complete() {
    let ids = FIXTURES
        .iter()
        .map(|fixture| fixture.id)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        ids.len(),
        FIXTURES.len(),
        "migration fixture IDs must be unique"
    );

    let queries = FIXTURES
        .iter()
        .map(|fixture| fixture.query)
        .collect::<BTreeSet<_>>();
    assert_eq!(queries, QueryKind::ALL.into_iter().collect());

    let registered = FIXTURES
        .iter()
        .map(|fixture| fixture.oracle.to_string())
        .collect::<BTreeSet<_>>();
    assert!(registered_ide_snapshots().is_subset(&registered));
    assert!(
        registered_oracles_under("tests/golden/ruai", "result.ide.golden").is_subset(&registered)
    );

    for query in QueryKind::ALL {
        assert!(
            FIXTURES.iter().any(|fixture| fixture.query == query),
            "{} has no migration oracle",
            query.label()
        );
    }
}

#[test]
fn migration_baseline_oracles_are_stable() {
    let checkout = workspace_root().to_string_lossy().replace('\\', "/");
    for fixture in FIXTURES {
        let source = read_fixture(fixture.source);
        let oracle = read_fixture(fixture.oracle);
        assert!(!source.is_empty(), "{} source is empty", fixture.id);
        assert!(!oracle.is_empty(), "{} oracle is empty", fixture.id);
        assert!(
            !oracle.replace('\\', "/").contains(&checkout),
            "{} oracle contains an absolute checkout path",
            fixture.id
        );
        assert!(
            !fixture.behaviors.is_empty(),
            "{} must record at least one behavior",
            fixture.id
        );
    }

    let readonly = FIXTURES
        .iter()
        .find(|fixture| fixture.id == "ruai.rename.readonly-rejected")
        .expect("readonly rename fixture");
    assert!(readonly.behaviors.contains(&Behavior::ReadOnly));
    assert_eq!(
        read_fixture(readonly.oracle),
        "query: moon::log\nrename: rejected InvalidName\n"
    );

    for ordered_query in [
        QueryKind::Completion,
        QueryKind::References,
        QueryKind::Rename,
        QueryKind::DocumentSymbols,
        QueryKind::Diagnostics,
        QueryKind::SemanticTokens,
    ] {
        assert!(FIXTURES.iter().any(|fixture| {
            fixture.query == ordered_query && fixture.behaviors.contains(&Behavior::StableOrdering)
        }));
    }
}

#[test]
fn migration_baseline_classifies_native_queries() {
    let mut statuses = BTreeSet::new();
    for fixture in FIXTURES {
        let status =
            classify_fixture(fixture).unwrap_or_else(|error| panic!("{}: {error}", fixture.id));
        match status {
            ParityStatus::Equal => {
                statuses.insert("equal");
            }
            ParityStatus::ExpectedDifference(id) => {
                assert_eq!(id, "native-document-symbol-members");
                statuses.insert("expected-difference");
            }
            ParityStatus::Unsupported(remove_by) => {
                assert!(matches!(remove_by, "4B.8" | "4B.9"));
                statuses.insert("unsupported");
            }
        }
    }
    assert_eq!(
        statuses,
        BTreeSet::from(["equal", "expected-difference", "unsupported"])
    );
}

#[test]
fn migration_baseline_phase4a_corpus_is_complete() {
    let cases = phase4a_cases();
    assert_eq!(cases.iter().filter(|case| case.accepted).count(), 12);
    assert_eq!(cases.iter().filter(|case| !case.accepted).count(), 9);
    assert_eq!(
        cases
            .iter()
            .map(|case| &case.id)
            .collect::<BTreeSet<_>>()
            .len(),
        cases.len()
    );

    for case in cases {
        let oracle = std::fs::read_to_string(&case.oracle)
            .unwrap_or_else(|error| panic!("read {}: {error}", case.oracle.display()));
        assert!(!oracle.is_empty(), "{} oracle is empty", case.id);
        if case.accepted {
            assert_eq!(case.remove_by, "4B.7");
            assert_eq!(
                case.unsupported_reason,
                "native closure and iterator inference is not available"
            );
        } else {
            assert_eq!(case.remove_by, "4B.8");
            assert_eq!(
                case.unsupported_reason,
                "native type diagnostics are not available"
            );
        }

        let actual = if case.accepted {
            ruac::compile_path(&case.source)
                .unwrap_or_else(|error| panic!("compiler oracle rejected {}: {error}", case.id))
        } else {
            let error = ruac::compile_path(&case.source)
                .err()
                .unwrap_or_else(|| panic!("compiler oracle unexpectedly accepted {}", case.id));
            let normalized = error.replace('\\', "/");
            let golden = workspace_root()
                .join("tests/golden")
                .to_string_lossy()
                .replace('\\', "/");
            normalized.replace(&golden, "<golden>")
        };
        assert_eq!(actual, oracle, "compiler oracle drifted for {}", case.id);
    }
}
