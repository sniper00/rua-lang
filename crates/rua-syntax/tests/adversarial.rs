use proptest::prelude::*;

use rua_syntax::parse_source_file;
use ruac::parser::{ParseBudget, parse_with_budget};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn arbitrary_unicode_is_bounded_lossless_and_range_safe(source in any::<String>()) {
        let tolerant = parse_source_file(&source);
        prop_assert_eq!(tolerant.syntax_node().text().to_string(), source.as_str());
        for error in tolerant.errors() {
            prop_assert!(error.offset <= source.len());
            prop_assert!(source.is_char_boundary(error.offset));
        }

        let strict = parse_with_budget(
            &source,
            ParseBudget {
                max_tokens: 4_096,
                max_nesting: 128,
            },
        );
        if let Err(error) = strict {
            let range = error.diagnostic().range.expect("parser errors carry a range");
            prop_assert!(range.start() <= range.end());
            prop_assert!((range.end() as usize) <= source.len());
            prop_assert!(source.is_char_boundary(range.start() as usize));
            prop_assert!(source.is_char_boundary(range.end() as usize));
        }
    }
}

#[test]
fn strict_parser_rejects_adversarial_depth_and_token_volume_without_panicking() {
    let deeply_nested = format!("{}1{}", "(".repeat(2_048), ")".repeat(2_048));
    let error = parse_with_budget(
        &deeply_nested,
        ParseBudget {
            max_tokens: 10_000,
            max_nesting: 64,
        },
    )
    .expect_err("nesting budget must reject deeply nested input");
    assert_eq!(
        error.diagnostic().code,
        rua_core::DiagnosticCode::ParseResourceLimit
    );

    let token_heavy = "+ ".repeat(10_000);
    let error = parse_with_budget(
        &token_heavy,
        ParseBudget {
            max_tokens: 128,
            max_nesting: 64,
        },
    )
    .expect_err("token budget must reject token-heavy input");
    assert_eq!(
        error.diagnostic().code,
        rua_core::DiagnosticCode::ParseResourceLimit
    );
}
