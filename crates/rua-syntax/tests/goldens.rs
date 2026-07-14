//! Golden snapshot tests for the Rua formatter (P5e-B4).
//!
//! Each example `.rua` corpus file has a corresponding `.golden` file in
//! `tests/goldens/` containing the expected byte-exact formatted output.
//!
//! Tests:
//!   `golden_matches` — asserts every golden file matches the current formatter
//!   `regenerate_goldens` (`#[ignore]`) — overwrites golden files with current
//!     output, for intentional format changes
//!
//! Regenerate with:
//!   cargo test -p rua-syntax --test goldens regenerate_goldens -- --ignored

use rua_syntax::format::format_str;

/// Corpus files and their golden file names. Kept in sync with
/// `tests/format.rs::CORPUS`.
const CORPUS: &[&str] = &[
    "example_rua.rua",
    "example_rua_p2.rua",
    "example_rua_p3.rua",
    "example_rua_p4.rua",
    "example_rua_p4b.rua",
    "example_rua_p4c.rua",
    "example_rua_p4c_mod.rua",
    "example_rua_p4c_types.rua",
    "example_rua_p5.rua",
    "example_rua_std.rua",
];

fn corpus_path(name: &str) -> String {
    format!(
        "{}/../../tests/fixtures/examples/{name}",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn golden_path(name: &str) -> String {
    format!("{}/tests/goldens/{name}.golden", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn golden_matches() {
    for name in CORPUS {
        let src_path = corpus_path(name);
        let golden_path = golden_path(name);

        let src = std::fs::read_to_string(&src_path).unwrap_or_else(|e| {
            panic!("cannot read corpus file {src_path}: {e}");
        });
        let expected = std::fs::read_to_string(&golden_path).unwrap_or_else(|e| {
            panic!(
                "cannot read golden file {golden_path}: {e}\n\
                 (re-run with: cargo test -p rua-syntax --test goldens \
                 regenerate_goldens -- --ignored)"
            );
        });
        let actual = format_str(&src);
        assert_eq!(
            actual, expected,
            "golden mismatch for {name}\n\
             to update, run: cargo test -p rua-syntax --test goldens \
             regenerate_goldens -- --ignored",
        );
    }
}

#[test]
#[ignore]
fn regenerate_goldens() {
    for name in CORPUS {
        let src_path = corpus_path(name);
        let golden_path = golden_path(name);

        let src = std::fs::read_to_string(&src_path)
            .unwrap_or_else(|e| panic!("cannot read corpus file {src_path}: {e}"));
        let formatted = format_str(&src);
        std::fs::write(&golden_path, &formatted)
            .unwrap_or_else(|e| panic!("cannot write golden file {golden_path}: {e}"));
        println!("wrote {name}.golden");
    }
}
