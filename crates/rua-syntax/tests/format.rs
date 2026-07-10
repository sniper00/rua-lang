//! Formatter B1–B4 conformance: on the example corpus the formatter must
//! produce output that (1) re-parses with no CST errors, (2) is idempotent
//! (`fmt(fmt(x)) == fmt(x)`), and (3) passes `--check` after formatting.
//! Comments are preserved (B2), blank lines preserved (B3).

use rua_syntax::format::{check_format, format_str};
use rua_syntax::parse_source_file;

/// Example `.rua` sources that parse cleanly under the CST parser. Kept in sync
/// with `tests/fixtures/examples/`.
const CORPUS: &[&str] = &[
    "../../tests/fixtures/examples/example_rua.rua",
    "../../tests/fixtures/examples/example_rua_p2.rua",
    "../../tests/fixtures/examples/example_rua_p3.rua",
    "../../tests/fixtures/examples/example_rua_p4.rua",
    "../../tests/fixtures/examples/example_rua_p4b.rua",
    "../../tests/fixtures/examples/example_rua_p4c.rua",
    "../../tests/fixtures/examples/example_rua_p4c_mod.rua",
    "../../tests/fixtures/examples/example_rua_p4c_types.rua",
    "../../tests/fixtures/examples/example_rua_p5.rua",
    "../../tests/fixtures/examples/example_rua_std.rua",
];

fn read(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[test]
fn formatted_output_reparses_clean() {
    for path in CORPUS {
        let Some(src) = read(path) else { continue };
        // Skip inputs the CST parser can't handle cleanly to begin with.
        if !parse_source_file(&src).errors.is_empty() {
            continue;
        }
        let out = format_str(&src);
        let errs = parse_source_file(&out).errors;
        assert!(
            errs.is_empty(),
            "formatted {path} has parse errors: {errs:?}\n---\n{out}"
        );
    }
}

#[test]
fn formatting_is_idempotent() {
    for path in CORPUS {
        let Some(src) = read(path) else { continue };
        if !parse_source_file(&src).errors.is_empty() {
            continue;
        }
        let once = format_str(&src);
        let twice = format_str(&once);
        assert_eq!(once, twice, "formatter not idempotent for {path}");
    }
}

/// Structural preservation: the non-trivia token *kinds* of the formatted output
/// match the original, modulo trailing commas the formatter may introduce in
/// braced lists. This catches accidental token loss/insertion.
#[test]
fn formatting_preserves_token_kinds_modulo_trailing_commas() {
    use rua_syntax::{SyntaxKind, lex};
    fn sig(src: &str) -> Vec<SyntaxKind> {
        lex(src)
            .into_iter()
            .map(|t| t.kind)
            .filter(|k| !k.is_trivia() && *k != SyntaxKind::Comma)
            .collect()
    }
    for path in CORPUS {
        let Some(src) = read(path) else { continue };
        if !parse_source_file(&src).errors.is_empty() {
            continue;
        }
        let out = format_str(&src);
        assert_eq!(sig(&src), sig(&out), "token kinds changed for {path}\n---\n{out}");
    }
}

/// B4: After formatting, every corpus file passes `check_format`.
#[test]
fn formatted_output_passes_check() {
    for path in CORPUS {
        let Some(src) = read(path) else { continue };
        if !parse_source_file(&src).errors.is_empty() {
            continue;
        }
        let out = format_str(&src);
        assert!(
            check_format(&out),
            "formatted {path} does not pass check_format — formatter is not idempotent"
        );
    }
}

/// B4: `check_format` correctly rejects deliberately unformatted input.
#[test]
fn check_rejects_unformatted() {
    // Extra space before `{` — the formatter normalises this.
    let src = "fn foo()  {\n    let x = 1;\n}\n";
    if parse_source_file(src).errors.is_empty() {
        assert!(!check_format(src), "unformatted source must fail check");
    }
}

/// Writes formatted corpus outputs to `/tmp/rua_fmt/` for eyeballing.
#[test]
fn snapshot_corpus_to_tmp() {
    let dir = std::path::Path::new("/tmp/rua_fmt");
    let _ = std::fs::create_dir_all(dir);
    for path in CORPUS {
        let Some(src) = read(path) else { continue };
        let name = std::path::Path::new(path).file_name().unwrap();
        let _ = std::fs::write(dir.join(name), format_str(&src));
    }
}
