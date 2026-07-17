//! Rua source formatter (P5e-B).
//!
//! Two-tree design: the formatter operates on this crate's lossless rowan CST
//! (not the compiler's owned AST), so comments and layout are available. The CST
//! is lowered to a [`doc::Doc`] IR and printed within a target width.
//!
//! Pipeline: `parse_source_file(src)` → CST → [`lower`] → [`doc::Doc`] → [`doc::print`].

pub mod comment;
pub mod doc;
pub mod lower;

pub use doc::{DEFAULT_WIDTH, Doc};

/// Format Rua source text, returning the reformatted source. Malformed input
/// (parse errors) is returned unchanged so the formatter never corrupts a file
/// it cannot fully understand.
pub fn format_str(src: &str) -> String {
    format_str_width(src, DEFAULT_WIDTH)
}

/// Format with an explicit target line width (mainly for tests).
pub fn format_str_width(src: &str, width: usize) -> String {
    let parsed = crate::parse_source_file(src);
    if !parsed.errors.is_empty() {
        return src.to_string();
    }
    let file = parsed.tree;
    let doc = lower::lower_source_file(&file);
    let mut out = doc::print(&doc, width);
    // Source files end with exactly one trailing newline.
    while out.ends_with('\n') {
        out.pop();
    }
    out.push('\n');
    out
}

/// Check whether `src` is already formatted — i.e. `format_str(src)` would be a
/// no-op. Returns `true` when the source is unchanged by formatting (including
/// parse-error inputs, which the formatter cannot safely modify).
///
/// This is the function backing a `--check` / `--check-format` CLI flag: use it
/// in CI to reject unformatted files without mutating them.
pub fn check_format(src: &str) -> bool {
    check_format_width(src, DEFAULT_WIDTH)
}

/// Check with an explicit target line width.
pub fn check_format_width(src: &str, width: usize) -> bool {
    format_str_width(src, width) == src
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_comments_preserved() {
        let src = "// file header\nfn foo() {}\n";
        let out = format_str(src);
        assert!(
            out.contains("// file header"),
            "leading comment preserved\n---\n{out}"
        );
        assert!(out.contains("fn foo() {}"), "item present\n---\n{out}");
    }

    #[test]
    fn attributes_stay_on_lines_before_their_targets() {
        let src = "#[cfg(all(feature=\"http\",runtime=\"moon\"))]\nfn serve(){}\n";
        let out = format_str(src);
        assert_eq!(
            out,
            "#[cfg(all(feature = \"http\", runtime = \"moon\"))]\nfn serve() {}\n"
        );
    }

    #[test]
    fn trailing_comment_on_statement() {
        let src = "fn foo() {\n    let x = 1; // trail\n}\n";
        let out = format_str(src);
        assert!(
            out.contains("// trail"),
            "trailing comment preserved\n---\n{out}"
        );
    }

    #[test]
    fn comment_between_items() {
        let src = "fn a() {}\n// between\nfn b() {}\n";
        let out = format_str(src);
        assert!(
            out.contains("// between"),
            "between comment preserved\n---\n{out}"
        );
        assert!(out.contains("fn a() {}"), "first item\n---\n{out}");
        assert!(out.contains("fn b() {}"), "second item\n---\n{out}");
    }

    #[test]
    fn standalone_comment_in_block() {
        let src = "fn foo() {\n    // note\n    let x = 1;\n}\n";
        let out = format_str(src);
        assert!(
            out.contains("// note"),
            "block comment preserved\n---\n{out}"
        );
    }

    #[test]
    fn trailing_comment_at_eof() {
        let src = "fn foo() {}\n// eof\n";
        let out = format_str(src);
        assert!(out.contains("// eof"), "eof comment preserved\n---\n{out}");
        assert!(out.ends_with("// eof\n"), "eof comment at end\n---\n{out}");
    }

    #[test]
    fn block_comment_preserved() {
        let src = "/* header */\nfn foo() {}\n";
        let out = format_str(src);
        assert!(
            out.contains("/* header */"),
            "block comment preserved\n---\n{out}"
        );
    }

    #[test]
    fn comments_preserve_idempotence() {
        let src = "// top\nfn foo() {\n    let x = 1; // trail\n    // mid\n    let y = 2;\n}\n// bottom\n";
        let once = format_str(src);
        let twice = format_str(&once);
        assert_eq!(
            once, twice,
            "idempotent with comments:\n--- once ---\n{once}--- twice ---\n{twice}"
        );
    }

    #[test]
    fn no_comments_idempotence_unchanged() {
        // Input without comments should still be idempotent.
        let src = "fn foo() {\n    let x = 1;\n}\n";
        let once = format_str(src);
        let twice = format_str(&once);
        assert_eq!(
            once, twice,
            "idempotent without comments:\n---\n{once}---\n{twice}"
        );
    }

    // --- B3 wrapping tests ---------------------------------------------------

    #[test]
    fn short_call_fits_on_one_line() {
        let src = "fn foo() { bar(a, b, c); }\n";
        let out = format_str(src);
        assert!(out.contains("bar(a, b, c)"), "short call flat\n---\n{out}");
    }

    #[test]
    fn long_call_wraps_at_narrow_width() {
        let src = "fn foo() { bar(aaaaa, bbbbb, ccccc, ddddd, eeeee, fffff); }\n";
        let out = format_str_width(src, 30);
        // Should contain newlines inside the argument list.
        let after_paren = out.split('(').nth(1).unwrap_or("");
        assert!(
            after_paren.contains('\n'),
            "long call wraps at width 30\n---\n{out}"
        );
        assert!(out.contains("aaaaa,"), "first arg\n---\n{out}");
        assert!(
            out.contains("fffff,"),
            "last arg with trailing comma\n---\n{out}"
        );
    }

    #[test]
    fn short_struct_literal_fits_flat() {
        let src = "fn foo() { Point { x: 1, y: 2 }; }\n";
        let out = format_str(src);
        assert!(
            out.contains("Point { x: 1, y: 2 }"),
            "short struct flat\n---\n{out}"
        );
    }

    #[test]
    fn long_struct_literal_wraps() {
        let src =
            "fn foo() { Point { xxxxx: 11111, yyyyy: 22222, zzzzz: 33333, wwwww: 44444 }; }\n";
        let out = format_str_width(src, 30);
        let after_brace = out.split('{').nth(1).unwrap_or("");
        assert!(
            after_brace.contains('\n'),
            "long struct wraps at width 30\n---\n{out}"
        );
    }

    #[test]
    fn empty_call_no_wrapping() {
        let src = "fn foo() { bar(); }\n";
        let out = format_str(src);
        assert!(out.contains("bar()"), "empty call\n---\n{out}");
    }

    #[test]
    fn idempotent_with_wrapping() {
        let src = "fn foo() { bar(aaaaa, bbbbb, ccccc, ddddd, eeeee); }\n";
        let once = format_str_width(src, 40);
        let twice = format_str_width(&once, 40);
        assert_eq!(
            once, twice,
            "idempotent with wrapping:\n--- once ---\n{once}--- twice ---\n{twice}"
        );
    }

    // --- B3 blank-line preservation -----------------------------------------

    #[test]
    fn blank_line_between_statements_preserved() {
        let src = "fn foo() {\n    let x = 1;\n\n    let y = 2;\n}\n";
        let out = format_str(src);
        // Should contain a blank line between the two lets.
        let after_x = out.split("let x").nth(1).unwrap_or("");
        assert!(
            after_x.contains("\n\n"),
            "blank line between statements preserved\n---\n{out}"
        );
        assert!(
            out.contains("let y"),
            "second statement present\n---\n{out}"
        );
    }

    #[test]
    fn no_blank_line_when_not_present() {
        let src = "fn foo() {\n    let x = 1;\n    let y = 2;\n}\n";
        let out = format_str(src);
        // Two lets on adjacent lines — no blank line between them.
        let once = format_str(&out);
        assert_eq!(out, once, "idempotent without blank lines\n---\n{out}");
    }

    #[test]
    fn blank_line_preserves_idempotence() {
        let src = "fn foo() {\n    let x = 1;\n\n    let y = 2;\n}\n";
        let once = format_str(src);
        let twice = format_str(&once);
        assert_eq!(
            once, twice,
            "idempotent with blank lines:\n--- once ---\n{once}--- twice ---\n{twice}"
        );
    }

    #[test]
    fn blank_line_between_fns_preserved() {
        let src = "fn a() {}\n\nfn b() {}\n";
        let out = format_str(src);
        // Two non-use fns always get a blank line.
        let once = format_str(&out);
        assert_eq!(out, once, "idempotent between fns\n---\n{out}");
    }

    #[test]
    fn multiple_blank_lines_collapsed_to_one() {
        let src = "fn foo() {\n    let x = 1;\n\n\n\n    let y = 2;\n}\n";
        let out = format_str(src);
        // Multiple blank lines collapse to one.
        let twice = format_str(&out);
        assert_eq!(out, twice, "idempotent after collapsing\n---\n{out}");
    }

    // --- B3 blank lines & comments in field/variant lists -------------------

    #[test]
    fn blank_line_between_struct_fields_preserved() {
        let src = "struct S {\n    a: i64,\n\n    b: i64,\n}\n";
        let out = format_str(src);
        let after_a = out.split("a: i64,").nth(1).unwrap_or("");
        assert!(
            after_a.contains("\n\n"),
            "blank between struct fields\n---\n{out}"
        );
        assert_eq!(out, format_str(&out), "idempotent\n---\n{out}");
    }

    #[test]
    fn blank_line_between_enum_variants_preserved() {
        let src = "enum E {\n    A,\n\n    B,\n}\n";
        let out = format_str(src);
        let after_a = out.split("A,").nth(1).unwrap_or("");
        assert!(
            after_a.contains("\n\n"),
            "blank between enum variants\n---\n{out}"
        );
        assert_eq!(out, format_str(&out), "idempotent\n---\n{out}");
    }

    #[test]
    fn leading_comment_on_field_survives() {
        let src = "struct S {\n    a: i64,\n    // note on b\n    b: i64,\n}\n";
        let out = format_str(src);
        assert!(
            out.contains("// note on b"),
            "field leading comment kept\n---\n{out}"
        );
        assert_eq!(out, format_str(&out), "idempotent\n---\n{out}");
    }

    #[test]
    fn trailing_comment_on_field_survives() {
        let src = "struct S {\n    a: i64, // trailing a\n    b: i64,\n}\n";
        let out = format_str(src);
        assert!(
            out.contains("// trailing a"),
            "field trailing comment kept\n---\n{out}"
        );
        // The comment stays on field a's line.
        let a_line = out.lines().find(|l| l.contains("a: i64")).unwrap_or("");
        assert!(
            a_line.contains("// trailing a"),
            "trailing on field a's line\n---\n{out}"
        );
        assert_eq!(out, format_str(&out), "idempotent\n---\n{out}");
    }

    #[test]
    fn leading_comment_on_enum_variant_survives() {
        let src = "enum E {\n    A,\n    // about B\n    B,\n}\n";
        let out = format_str(src);
        assert!(
            out.contains("// about B"),
            "variant leading comment kept\n---\n{out}"
        );
        assert_eq!(out, format_str(&out), "idempotent\n---\n{out}");
    }

    // --- B3 blank-line normalization ----------------------------------------

    #[test]
    fn header_comment_blank_before_first_item_preserved() {
        let src = "// file banner\n\nfn foo() {}\n";
        let out = format_str(src);
        assert_eq!(
            out, "// file banner\n\nfn foo() {}\n",
            "banner keeps its blank\n---\n{out}"
        );
        assert_eq!(out, format_str(&out), "idempotent\n---\n{out}");
    }

    #[test]
    fn header_comment_without_blank_stays_glued() {
        let src = "// doc\nfn foo() {}\n";
        let out = format_str(src);
        assert_eq!(out, "// doc\nfn foo() {}\n", "no blank added\n---\n{out}");
    }

    #[test]
    fn no_blank_line_right_after_block_open_brace() {
        let src = "fn f() {\n\n    let x = 1;\n}\n";
        let out = format_str(src);
        assert!(
            !out.contains("{\n\n"),
            "blank immediately after `{{` is stripped\n---\n{out}"
        );
        assert_eq!(out, format_str(&out), "idempotent\n---\n{out}");
    }

    #[test]
    fn no_blank_line_right_after_struct_open_brace() {
        let src = "struct S {\n\n    a: i64,\n}\n";
        let out = format_str(src);
        assert!(
            !out.contains("{\n\n"),
            "blank immediately after struct `{{` is stripped\n---\n{out}"
        );
        assert_eq!(out, format_str(&out), "idempotent\n---\n{out}");
    }

    // --- B4 check mode -------------------------------------------------------

    #[test]
    fn check_already_formatted_returns_true() {
        // Already-clean source should pass check.
        let src = "fn foo() {\n    let x = 1;\n}\n";
        let formatted = format_str(src);
        assert!(check_format(&formatted), "already formatted passes check");
    }

    #[test]
    fn check_unformatted_returns_false() {
        // Extra whitespace in non-significant positions should be flagged.
        let src = "fn foo()  {\n    let x = 1  ;\n}\n";
        assert!(!check_format(src), "unformatted source fails check");
        // After formatting, it should pass.
        assert!(check_format(&format_str(src)), "after format passes check");
    }

    #[test]
    fn check_parse_error_returns_true() {
        // Unparseable source is returned unchanged → check passes (safe default).
        let src = "fn foo {";
        assert!(
            check_format(src),
            "parse-error source is unchanged → passes check"
        );
    }

    #[test]
    fn check_with_comments() {
        let src = "// header\nfn foo() {} // trail\n";
        let formatted = format_str(src);
        assert!(
            check_format(&formatted),
            "formatted with comments passes check"
        );
    }

    #[test]
    fn check_with_blank_lines() {
        let src = "fn foo() {\n    let x = 1;\n\n    let y = 2;\n}\n";
        let formatted = format_str(src);
        assert!(
            check_format(&formatted),
            "formatted with blank lines passes check"
        );
    }

    #[test]
    fn check_blank_line_before_first_item_not_introduced() {
        // The formatter should NOT introduce a blank line before the first item
        // where the author only had a single newline.
        let src = "// doc\nfn foo() {}\n";
        let formatted = format_str(src);
        // Check that formatted output equals original (no blank added).
        assert_eq!(formatted, "// doc\nfn foo() {}\n");
        assert!(check_format(&formatted));
    }

    #[test]
    fn formats_annotation_declarations_and_schema_attributes() {
        let source =
            "#[targets(function,method)]\npub annotation Route(method:String,path:String);\n";
        let expected =
            "#[targets(function, method)]\npub annotation Route(method: String, path: String);\n";
        let formatted = format_str(source);
        assert_eq!(formatted, expected);
        assert_eq!(format_str(&formatted), expected);
    }

    #[test]
    fn formats_native_vec_literals() {
        let source = "fn values() -> Vec<i64> { [1,2,3] }\n";
        let expected = "fn values() -> Vec<i64> {\n    [1, 2, 3]\n}\n";
        assert_eq!(format_str(source), expected);
    }
}
