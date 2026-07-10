//! Conformance net for the IDE/LSP CST: ensure the rowan parser is lossless and
//! the lexer covers every byte across the whole example corpus.
//!
//! For every example the compiler ships, we assert two invariants:
//!   1. **Lossless**: `parse_source_file(src).syntax_node().text() == src`.
//!   2. **Total lexing**: the flat token stream covers every byte.

use rua_syntax::{lex, parse_source_file};

/// Every `.rua` the compiler accepts; must also parse cleanly here.
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
    "rua_multi/main.rua",
    "rua_multi/math.rua",
    "rua_multi/util.rua",
    "rua_multi/math/trig.rua",
    "rua_moon/main.rua",
];

fn read_example(rel: &str) -> String {
    let path = format!("{}/../../tests/fixtures/examples/{}", env!("CARGO_MANIFEST_DIR"), rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

#[test]
fn corpus_is_lossless() {
    for rel in CORPUS {
        let src = read_example(rel);
        assert_eq!(
            parse_source_file(&src).syntax_node().text().to_string(),
            src,
            "round-trip failed for {rel}"
        );
    }
}

#[test]
fn corpus_lexing_covers_every_byte() {
    for rel in CORPUS {
        let src = read_example(rel);
        let total: usize = lex(&src).iter().map(|t| t.len).sum();
        assert_eq!(total, src.len(), "lexer left a gap in {rel}");
    }
}
