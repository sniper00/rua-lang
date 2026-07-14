use crate::compile_str;

#[test]
fn parser_preserves_top_level_chunk_order() {
    use crate::ast::ChunkEntry;

    let program = crate::parser::parse(
        "let before = 1; fn value() -> i64 { before } println!(\"{}\", value());",
    )
    .expect("parse executable chunk");
    assert_eq!(
        program.source_order,
        [
            ChunkEntry::Statement(0),
            ChunkEntry::Item(0),
            ChunkEntry::Statement(1),
        ]
    );
    assert_eq!(program.items.len(), 1);
    assert_eq!(program.chunk.stmts.len(), 2);
}

#[test]
fn strict_parser_preserves_normalized_api_documentation() {
    let program = crate::parser::parse(
        r#"
        /// Function docs.
        fn documented() {}

        /// Detached docs.

        fn plain() {}

        /** Structure docs. */
        struct Point {
            /// X coordinate.
            x: i64,
        }

        enum Color {
            /// Red variant.
            Red,
        }

        mod api {
            //! Module docs.
            /// Nested function.
            fn call() {}
        }

        extern "lua" {
            /// Host function.
            fn host();
        }
        "#,
    )
    .unwrap();

    let crate::ast::Item::Fn(documented) = &program.items[0] else {
        panic!("expected function")
    };
    assert_eq!(documented.documentation.as_deref(), Some("Function docs."));
    let crate::ast::Item::Fn(plain) = &program.items[1] else {
        panic!("expected function")
    };
    assert_eq!(plain.documentation, None);
    let crate::ast::Item::Struct(point) = &program.items[2] else {
        panic!("expected struct")
    };
    assert_eq!(point.documentation.as_deref(), Some("Structure docs."));
    assert_eq!(
        point.fields[0].documentation.as_deref(),
        Some("X coordinate.")
    );
    let crate::ast::Item::Enum(color) = &program.items[3] else {
        panic!("expected enum")
    };
    assert_eq!(
        color.variants[0].documentation.as_deref(),
        Some("Red variant.")
    );
    let crate::ast::Item::Mod(api) = &program.items[4] else {
        panic!("expected module")
    };
    assert_eq!(api.documentation.as_deref(), Some("Module docs."));
    let crate::ast::Item::Fn(call) = &api.items[0] else {
        panic!("expected nested function")
    };
    assert_eq!(call.documentation.as_deref(), Some("Nested function."));
    let crate::ast::Item::Extern(block) = &program.items[5] else {
        panic!("expected extern block")
    };
    assert_eq!(
        block.fns[0].documentation.as_deref(),
        Some("Host function.")
    );
}

#[test]
fn strict_parser_enforces_token_and_nesting_budgets() {
    let token_error = crate::parser::parse_with_budget(
        "fn main() {}",
        crate::parser::ParseBudget {
            max_tokens: 2,
            max_nesting: 512,
        },
    )
    .expect_err("small token budget must reject input");
    assert_eq!(
        token_error.diagnostic().code,
        rua_core::DiagnosticCode::ParseResourceLimit
    );

    let nested = format!("{}1{};", "(".repeat(64), ")".repeat(64));
    let nesting_error = crate::parser::parse_with_budget(
        &nested,
        crate::parser::ParseBudget {
            max_tokens: 1_000,
            max_nesting: 16,
        },
    )
    .expect_err("small nesting budget must reject input");
    assert_eq!(
        nesting_error.diagnostic().code,
        rua_core::DiagnosticCode::ParseResourceLimit
    );
}

#[test]
fn lua_result_extern_requires_non_variadic_builtin_result_return() {
    for source in [
        r#"extern "lua-result" { fn bad() -> i64; }"#,
        r#"extern "lua-result" { fn bad(value: i64, ...) -> Result<i64, String>; }"#,
        r#"
            struct Result<T, E> { value: T }
            extern "lua-result" { fn bad() -> Result<i64, String>; }
        "#,
    ] {
        let (diagnostics, _) = crate::check_diagnostics(source);
        assert!(
            diagnostics.iter().any(
                |diagnostic| diagnostic.code == rua_core::DiagnosticCode::TypeInvalidFfiAdapter
            ),
            "missing invalid FFI adapter diagnostic for:\n{source}\n{diagnostics:#?}"
        );
    }
}

#[test]
fn parser_closure_supports_expression_typed_block_and_empty_params() {
    use crate::ast::{ClosureBody, ExprKind, Item, Stmt};

    let program = crate::parser::parse(concat!(
        "fn main() {\n",
        "  let add = |left, right| left + right;\n",
        "  let typed = |value: i64| -> i64 { value + 1 };\n",
        "  let empty = || 42;\n",
        "}\n",
    ))
    .expect("parse closures");
    let Item::Fn(function) = &program.items[0] else {
        panic!("expected function");
    };
    let closures: Vec<_> = function
        .body
        .stmts
        .iter()
        .map(|statement| {
            let Stmt::Let { init, .. } = statement else {
                panic!("expected closure binding");
            };
            let ExprKind::Closure { params, ret, body } = &init.kind else {
                panic!("expected closure expression");
            };
            (params, ret, body)
        })
        .collect();

    assert_eq!(closures[0].0.len(), 2);
    assert!(closures[0].0.iter().all(|parameter| parameter.ty.is_none()));
    assert!(closures[0].1.is_none());
    assert!(matches!(closures[0].2, ClosureBody::Expr(_)));
    assert_eq!(closures[1].0.len(), 1);
    assert!(closures[1].0[0].ty.is_some());
    assert!(closures[1].1.is_some());
    assert!(matches!(closures[1].2, ClosureBody::Block(_)));
    assert!(closures[2].0.is_empty());
    assert!(matches!(closures[2].2, ClosureBody::Expr(_)));
}

#[test]
fn parser_supports_callable_types() {
    use crate::ast::{Item, Type};

    let program = crate::parser::parse(
        "fn apply(value: i64, callback: fn(i64) -> String) -> String { callback(value) }",
    )
    .expect("parse callable type");
    let Item::Fn(function) = &program.items[0] else {
        panic!("expected function");
    };
    assert!(matches!(function.params[1].ty, Type::Function { .. }));
}

#[test]
fn parser_supports_tuple_types() {
    use crate::ast::{Item, Type};

    let program = crate::parser::parse("fn pair() -> (i64, String) {}").unwrap();
    let Item::Fn(function) = &program.items[0] else {
        panic!("expected function");
    };
    assert!(matches!(function.ret, Some(Type::Tuple(ref items)) if items.len() == 2));
}

#[test]
fn parser_range_keeps_exclusive_and_inclusive_forms() {
    use crate::ast::{ExprKind, Item, Stmt};

    let program = crate::parser::parse("fn main() { for x in 0..3 {} for y in 0..=3 {} }")
        .expect("parse ranges");
    let Item::Fn(function) = &program.items[0] else {
        panic!("expected function");
    };
    let inclusive: Vec<_> = function
        .body
        .stmts
        .iter()
        .map(|statement| {
            let Stmt::For { iter, .. } = statement else {
                panic!("expected for statement");
            };
            let ExprKind::Range { inclusive, .. } = iter.kind else {
                panic!("expected range iterator");
            };
            inclusive
        })
        .collect();

    assert_eq!(inclusive, [false, true]);
}

#[test]
fn closure_typeck_infers_calls_blocks_and_read_captures() {
    let cases = [
        r#"
fn main() -> i64 {
    let increment = |value| value + 1;
    increment(41)
}
"#,
        r#"
fn main() -> i64 {
    let increment = |value: i64| -> i64 {
        return value + 1;
    };
    increment(41)
}
"#,
        r#"
fn main() -> i64 {
    let factor = 3;
    let scale = |value| value * factor;
    scale(14)
}
"#,
    ];

    for source in cases {
        let (diagnostics, _) = crate::check_diags(source);
        assert!(
            diagnostics.is_empty(),
            "valid closure produced diagnostics: {diagnostics:?}"
        );
    }
}

#[test]
fn closure_typeck_checks_signature_and_call_arguments() {
    let source = r#"
fn main() {
    let stringify = |value: String| -> bool { value };
    stringify(1);
}
"#;
    let (diagnostics, _) = crate::check_diags(source);
    let messages: Vec<_> = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.msg.as_str())
        .collect();
    assert!(
        messages.iter().any(|message| {
            message.contains("closure expects return type `bool`, found `String`")
        })
    );
    assert!(messages.iter().any(|message| {
        message.contains("argument 1 of closure `stringify` expects `String`, found `i64`")
    }));
}

#[test]
fn closure_typeck_uses_iterator_adapter_item_context() {
    let source = r#"
fn main() {
    let values = vec![1, 2, 3];
    let count = values
        .iter()
        .map(|value| value + 1)
        .filter(|value| value > 1)
        .count();
}
"#;
    let (diagnostics, _) = crate::check_diags(source);
    assert!(
        diagnostics.is_empty(),
        "iterator closure context produced diagnostics: {diagnostics:?}"
    );
}

#[test]
fn closure_typeck_reports_unknown_mutable_capture_and_escape() {
    let cases = [
        (
            "fn main() { let unknown = |value| value; }",
            "cannot infer type of closure parameter `value`",
        ),
        (
            concat!(
                "fn main() {\n",
                "  let mut total = 0;\n",
                "  let update = |value: i64| { total = total + value; };\n",
                "  update(1);\n",
                "}\n",
            ),
            "mutable capture of `total` is only supported in a fused iterator consumer",
        ),
        (
            "fn main() { let escaped = |value: i64| value; }",
            "closure escape is not supported yet",
        ),
    ];

    for (source, expected) in cases {
        let (diagnostics, _) = crate::check_diags(source);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.msg.contains(expected)),
            "missing {expected:?} in {diagnostics:?}"
        );
    }
}

fn iterator_type_info(source: &str) -> crate::typeck::TypeInfo {
    let mut program = crate::parser::parse(source).expect("parse iterator source");
    let mut files = vec![String::new()];
    crate::resolve::resolve_modules(&mut program.items, None, &mut files)
        .expect("resolve iterator source");
    let hir = crate::hir::resolve(&program);
    crate::check::check_resolved(&program, &hir).expect("structurally check iterator source");
    crate::typeck::check_resolved(&program, &hir).expect("type-check iterator source")
}

#[test]
fn iterator_typeck_supports_sources_adapters_and_consumers() {
    use crate::typeck::{IterAdapterKind as A, IterConsumerKind as C, IterSourceKind as S};

    let source = r#"
fn main() {
    let values = vec![1, 2, 3, 4];
    let collected = values
        .iter()
        .map(|value| value + 1)
        .filter(|value| value > 1)
        .filter_map(|value| Some(value))
        .enumerate()
        .skip(1)
        .take(2)
        .collect::<Vec<_>>();
    let total = values.into_iter().fold(0, |acc, value| acc + value);
    let count = (0..4).count();
    let any = (0..=4).any(|value| value == 3);
    let all = values.iter().all(|value| value > 0);
    let found = values.iter().find(|value| value == 2);
    for value in values {}
}
"#;
    let info = iterator_type_info(source);
    let plans: Vec<_> = info.iter_plans().collect();
    assert_eq!(plans.len(), 7);

    let collect = plans
        .iter()
        .find(|plan| plan.consumer == C::CollectVec)
        .expect("collect plan");
    assert_eq!(collect.source.kind, S::VecIter);
    assert_eq!(
        collect
            .adapters
            .iter()
            .map(|adapter| adapter.kind)
            .collect::<Vec<_>>(),
        [
            A::Map,
            A::Filter,
            A::FilterMap,
            A::Enumerate,
            A::Skip,
            A::Take
        ]
    );
    assert_eq!(collect.item_type, "(i64, i64)");
    assert_eq!(collect.output_type, "Vec<(i64, i64)>");

    assert!(plans.iter().any(|plan| {
        plan.source.kind == S::VecIntoIter && plan.consumer == C::Fold && plan.output_type == "i64"
    }));
    assert!(
        plans
            .iter()
            .any(|plan| { plan.source.kind == S::ExclusiveRange && plan.consumer == C::Count })
    );
    assert!(
        plans
            .iter()
            .any(|plan| { plan.source.kind == S::InclusiveRange && plan.consumer == C::Any })
    );
    assert!(plans.iter().any(|plan| plan.consumer == C::All));
    assert!(
        plans
            .iter()
            .any(|plan| { plan.consumer == C::Find && plan.output_type == "Option<i64>" })
    );
    assert!(
        plans
            .iter()
            .any(|plan| { plan.source.kind == S::Vec && plan.consumer == C::For })
    );
}

#[test]
fn iterator_plan_escape_stays_lazy_and_is_rejected() {
    let source = r#"
fn main() {
    let values = vec![1, 2, 3];
    let pending = values.iter().map(|value| value + 1).skip(1);
}
"#;
    let (diagnostics, _) = crate::check_diags(source);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.msg == "iterator escape is not supported yet")
    );
}

#[test]
fn iterator_plan_records_for_range_and_vec_iter_sources() {
    use crate::typeck::{IterConsumerKind as C, IterSourceKind as S};

    let source = r#"
fn main() {
    let values = vec![1, 2];
    for left in 0..2 {}
    for right in values.iter() {}
}
"#;
    let info = iterator_type_info(source);
    let plans: Vec<_> = info.iter_plans().collect();
    assert_eq!(plans.len(), 2);
    assert!(plans.iter().all(|plan| plan.consumer == C::For));
    assert!(
        plans
            .iter()
            .any(|plan| plan.source.kind == S::ExclusiveRange)
    );
    assert!(plans.iter().any(|plan| plan.source.kind == S::VecIter));
}

#[test]
fn iterator_codegen_does_not_fall_through_to_legacy_method_calls() {
    let lua = crate::compile_str("fn main() -> i64 { (0..4).count() }")
        .expect("compile fused range count");
    assert!(lua.contains("for __t"), "missing numeric for: {lua}");
    assert!(!lua.contains(":count("), "legacy method call leaked: {lua}");
}

#[test]
fn iterator_codegen_fuses_adapters_into_one_vec_loop() {
    let source = r#"
fn main() -> Vec<i64> {
    vec![1, 2, 3, 4]
        .iter()
        .map(|value| value * 2)
        .filter(|value| value > 4)
        .skip(1)
        .take(2)
        .collect::<Vec<i64>>()
}
"#;
    let lua = crate::compile_str(source).expect("compile fused iterator chain");
    assert_eq!(lua.matches("for ").count(), 1, "expected one loop: {lua}");
    assert!(lua.contains(".n - 1 do"), "Vec loop must use `.n`: {lua}");
    for forbidden in [
        ":iter(",
        ":map(",
        ":filter(",
        ":skip(",
        ":take(",
        ":collect(",
        "coroutine",
    ] {
        assert!(!lua.contains(forbidden), "found {forbidden:?}: {lua}");
    }
    assert!(
        !lua.contains("function("),
        "per-item closure emitted: {lua}"
    );
    assert_eq!(
        lua.matches("rt.vec(").count(),
        2,
        "expected source Vec plus one collected Vec: {lua}"
    );
}

#[test]
fn iterator_collect_preallocates_only_when_length_is_exact() {
    let exact = crate::compile_str(
        "fn main() -> Vec<i64> { vec![1, 2, 3].iter().map(|value| value * 2).collect() }",
    )
    .expect("compile exact-size collect");
    assert!(
        exact.contains("local __rua_table_create = table.create"),
        "missing Lua 5.5 capacity helper: {exact}"
    );
    assert!(
        exact.contains("rt.vec(__rua_table_create(") && exact.contains(".n, 2)"),
        "exact-size collect was not preallocated: {exact}"
    );

    let filtered = crate::compile_str(
        "fn main() -> Vec<i64> { vec![1, 2, 3].iter().filter(|value| value > 1).collect() }",
    )
    .expect("compile filtered collect");
    assert!(
        !filtered.contains("__rua_table_create"),
        "filter output length is not exact and must not reserve the source length: {filtered}"
    );
}

#[test]
fn unused_pure_iterator_pipeline_is_eliminated_but_trapping_expression_is_kept() {
    let iterator = crate::compile_str(
        "fn main() { let unused = vec![1, 2, 3].iter().map(|value| value * 2).collect::<Vec<i64>>(); }",
    )
    .expect("compile removable iterator");
    assert!(
        !iterator.contains("for "),
        "unused loop survived: {iterator}"
    );
    assert!(
        !iterator.contains("rua_rt"),
        "unused Vec survived: {iterator}"
    );

    let division = crate::compile_str("fn main() { let unused = 1 / 0; }")
        .expect("compile potentially trapping division");
    assert!(
        division.contains("local unused = rt.idiv(1, 0)"),
        "potential division-by-zero was removed: {division}"
    );
}

#[test]
fn iterator_codegen_handles_fold_filter_map_enumerate_and_early_exit() {
    let sources = [
        "fn main() -> i64 { (0..4).fold(0, |total, value| total + value) }",
        "fn main() -> i64 { vec![1, 2].iter().filter_map(|value| Some(value)).enumerate().count() }",
        "fn main() -> bool { (0..4).any(|value| value == 2) }",
        "fn main() -> bool { (0..4).all(|value| value < 4) }",
        "fn main() -> Option<i64> { (0..4).find(|value| value == 2) }",
    ];
    for source in sources {
        let lua = crate::compile_str(source).expect("compile iterator consumer");
        assert_eq!(lua.matches("for ").count(), 1, "expected one loop: {lua}");
        assert!(!lua.contains("coroutine"));
        assert!(!lua.contains("function("));
    }
}

#[test]
fn iterator_codegen_inlines_block_closure_returns() {
    let source = r#"
fn main() -> Vec<i64> {
    (0..4).map(|value| -> i64 {
        if value > 1 { return value * 2; }
        value
    }).collect::<Vec<_>>()
}
"#;
    let lua = crate::compile_str(source).expect("compile block iterator closure");
    assert!(
        lua.contains("goto __t"),
        "closure return was not lowered: {lua}"
    );
    assert!(
        lua.contains("_done::"),
        "closure return label missing: {lua}"
    );
    assert!(
        !lua.contains("function("),
        "per-item closure emitted: {lua}"
    );
}

#[test]
fn iterator_typeck_reports_invalid_sources_predicates_and_collects() {
    let cases = [
        (
            "fn main() { for value in 42 {} }",
            "type `i64` is not iterable",
        ),
        (
            "fn main() { let values = vec![1]; let n = values.iter().filter(|value| value + 1).count(); }",
            "iterator filter predicate must be `bool`, found `i64`",
        ),
        (
            "fn main() { let values = vec![1]; let out = values.iter().collect::<Vec<String>>(); }",
            "collect target element type `String` is incompatible with iterator item `i64`",
        ),
        (
            "fn main() { let values = vec![1]; let out = values.iter().map(42).count(); }",
            "iterator map argument must be a closure",
        ),
    ];

    for (source, expected) in cases {
        let (diagnostics, _) = crate::check_diags(source);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.msg.contains(expected)),
            "missing {expected:?} in {diagnostics:?}"
        );
    }
}

#[test]
fn iterator_typeck_allows_mutable_capture_in_consumed_plan() {
    let source = r#"
fn main() {
    let mut total = 0;
    let values = vec![1, 2, 3];
    let count = values.iter().map(|value| {
        total = total + value;
        value
    }).count();
}
"#;
    let (diagnostics, _) = crate::check_diags(source);
    assert!(
        diagnostics.is_empty(),
        "fused mutable capture should type-check: {diagnostics:?}"
    );
}

#[test]
fn iterator_escape_reports_stored_passed_returned_and_assigned_plans() {
    let cases = [
        "fn main() { let pending = (0..4).map(|value| value + 1); }",
        concat!(
            "fn consume<T>(value: T) {}\n",
            "fn main() { consume((0..4).map(|value| value + 1)); }\n",
        ),
        "fn escaped() { return (0..4).map(|value| value + 1); }",
        concat!(
            "fn main() {\n",
            "  let mut slot = 0;\n",
            "  slot = (0..4).map(|value| value + 1);\n",
            "}\n",
        ),
    ];

    for source in cases {
        let (diagnostics, _) = crate::check_diags(source);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.msg == "iterator escape is not supported yet"),
            "missing iterator escape diagnostic in {diagnostics:?}"
        );
    }
}

#[test]
fn iterator_escape_does_not_reject_immediate_consumers() {
    let source = "fn main() -> i64 { (0..4).map(|value| value + 1).count() }";
    let (diagnostics, _) = crate::check_diags(source);
    assert!(
        diagnostics.is_empty(),
        "consumer was treated as escape: {diagnostics:?}"
    );
}

fn compile(src: &str) -> String {
    compile_str(src).unwrap_or_else(|e| panic!("compile error: {}", e))
}

#[test]
fn empty_fn() {
    let lua = compile("fn main() {}");
    assert!(lua.contains("function main()"));
    assert!(!lua.trim_end().ends_with("main()"));
}

#[test]
fn let_and_arithmetic() {
    let lua = compile(
        r#"
        fn f(a: i64, b: i64) -> i64 {
            let x = a + b * 2;
            x
        }
    "#,
    );
    assert!(lua.contains("function f(a, b)"));
    assert!(lua.contains("local x = a + b * 2"));
    assert!(lua.contains("return x"));
}

#[test]
fn immutable_assignment_requires_mutable_binding_identity() {
    for source in [
        "let value = 1; value = 2;",
        "struct Point { x: i64 } let point = Point { x: 1 }; point.x = 2;",
        "let values = vec![1]; values[0] = 2;",
        "struct Point { x: i64 } impl Point { fn update(&self) { self.x = 2; } }",
    ] {
        let diagnostics = compile_str(source).unwrap_err();
        assert!(
            diagnostics.contains("cannot assign to immutable binding"),
            "source: {source}\ndiagnostics: {diagnostics}"
        );
    }

    compile_str(
        "struct Point { x: i64 }\n\
         impl Point { fn update(&mut self) { self.x = 2; } }\n\
         let mut point = Point { x: 1 };\n\
         point.x = 2;\n\
         point.update();",
    )
    .expect("mutable locals and &mut self receivers accept assignment");
}

#[test]
fn precedence() {
    // `1 + 2 * 3 == 7 && true` -> ((1 + (2*3)) == 7) and true
    let lua = compile("fn f() -> bool { 1 + 2 * 3 == 7 && true }");
    assert!(lua.contains("1 + 2 * 3 == 7 and true"), "got: {lua}");
}

#[test]
fn if_expression_hoists_temp() {
    let lua = compile(
        r#"
        fn abs(x: i64) -> i64 {
            let y = if x < 0 { -x } else { x };
            y
        }
    "#,
    );
    assert!(lua.contains("local y"));
    assert!(lua.contains("if x < 0 then"));
    assert!(lua.contains("y = -x") || lua.contains("y = (-x)"));
    assert!(lua.contains("y = x"));
}

#[test]
fn if_as_return_tail() {
    let lua = compile("fn m(a: i64, b: i64) -> i64 { if a > b { a } else { b } }");
    assert!(lua.contains("if a > b then"));
    assert!(lua.contains("return a"));
    assert!(lua.contains("return b"));
}

#[test]
fn while_loop_break_continue() {
    let lua = compile(
        r#"
        fn f() {
            let mut i = 0;
            while i < 10 {
                if i == 5 { break; }
                i = i + 1;
                continue;
            }
        }
    "#,
    );
    assert!(lua.contains("while i < 10 do"));
    assert!(lua.contains("break"));
    assert!(lua.contains("goto continue"));
    assert!(lua.contains("::continue::"));
}

#[test]
fn boolean_and_comparison_ops() {
    let lua = compile("fn f(a: i64, b: i64) -> bool { a != b || !(a < b) }");
    assert!(lua.contains("~="));
    assert!(lua.contains(" or "));
    assert!(lua.contains("not "));
}

#[test]
fn mutual_recursion_predeclared() {
    let lua = compile(
        r#"
        fn is_even(n: i64) -> bool { if n == 0 { true } else { is_odd(n - 1) } }
        fn is_odd(n: i64) -> bool { if n == 0 { false } else { is_even(n - 1) } }
    "#,
    );
    // both names predeclared as locals before either body
    assert!(lua.contains("local is_even, is_odd"));
}

#[test]
fn parse_error_reports_line() {
    let err = compile_str("fn f( {}").unwrap_err();
    assert!(
        err.contains(":"),
        "expected line-prefixed error, got: {err}"
    );
}

#[test]
fn parser_error_exposes_structured_diagnostic() {
    let error = crate::parser::parse("fn broken( {}").unwrap_err();
    assert_eq!(
        error.diagnostic().code,
        rua_core::DiagnosticCode::ParseUnexpectedToken
    );
    assert!(error.diagnostic().range.is_some());
}

// --- P2: struct / enum / match / Option / Result / ? / impl ---------------

#[test]
fn struct_decl_and_literal() {
    let lua = compile(
        r#"
        struct Point { x: f64, y: f64 }
        fn origin() -> Point { Point { x: 0.0, y: 0.0 } }
    "#,
    );
    assert!(lua.contains("---@class Point"), "got: {lua}");
    assert!(!lua.contains("local Point ="), "got: {lua}");
    assert!(!lua.contains("Point.__index = Point"), "got: {lua}");
    assert!(!lua.contains("setmetatable"), "got: {lua}");
    assert!(lua.contains("x = 0.0"), "got: {lua}");
}

#[test]
fn impl_methods_use_colon() {
    let lua = compile(
        r#"
        struct Point { x: f64, y: f64 }
        impl Point {
            fn new(x: f64, y: f64) -> Point { Point { x: x, y: y } }
            fn sum(&self) -> f64 { self.x + self.y }
        }
    "#,
    );
    assert!(lua.contains("function Point.new(x, y)"), "got: {lua}");
    assert!(lua.contains("function Point:sum()"), "got: {lua}");
    assert!(lua.contains("return self.x + self.y"));
}

#[test]
fn concrete_method_call_uses_static_identity() {
    let lua = compile(
        r#"
        struct P { x: f64 }
        impl P { fn get(&self) -> f64 { self.x } }
        fn f(p: P) -> f64 { p.get() }
    "#,
    );
    assert!(lua.contains("return P.get(p)"), "got: {lua}");
}

#[test]
fn enum_construction_tagged() {
    let lua = compile(
        r#"
        enum Shape { Circle(f64), Rect { w: f64, h: f64 }, Unit }
        fn a() -> Shape { Shape::Circle(2.0) }
        fn b() -> Shape { Shape::Rect { w: 3.0, h: 4.0 } }
        fn c() -> Shape { Shape::Unit }
    "#,
    );
    assert!(!lua.contains("setmetatable"), "got: {lua}");
    assert!(lua.contains(r#"tag = "Circle""#), "got: {lua}");
    assert!(lua.contains(r#"tag = "Rect""#), "got: {lua}");
    assert!(lua.contains(r#"tag = "Unit""#), "got: {lua}");
}

#[test]
fn option_pure_nil() {
    let lua = compile(
        r#"
        fn some_v() -> Option<i64> { let x = Some(5); x }
        fn none_v() -> Option<i64> { let y = None; y }
    "#,
    );
    assert!(
        lua.contains("local x = 5"),
        "Some(5) should be bare 5; got: {lua}"
    );
    assert!(
        lua.contains("local y = nil"),
        "None should be nil; got: {lua}"
    );
}

#[test]
fn result_and_try() {
    let lua = compile(
        r#"
        fn parse() -> Result<i64, String> { Ok(3) }
        fn use_it() -> Result<i64, String> {
            let v = parse()?;
            Ok(v + 1)
        }
    "#,
    );
    assert!(lua.contains("rt.result_ok(3)"), "got: {lua}");
    assert!(lua.contains(".tag == \"err\" then return"), "got: {lua}");
}

#[test]
fn match_on_enum() {
    let lua = compile(
        r#"
        enum Shape { Circle(f64), Rect { w: f64, h: f64 } }
        fn area(s: Shape) -> f64 {
            match s {
                Shape::Circle(r) => 3.14159 * r * r,
                Shape::Rect { w, h } => w * h,
            }
        }
    "#,
    );
    assert!(lua.contains(r#"s.tag == "Circle""#), "got: {lua}");
    assert!(lua.contains("local r ="), "binds tuple field; got: {lua}");
    assert!(!lua.contains(r#"== "Rect""#), "got: {lua}");
    assert!(lua.contains("else"), "got: {lua}");
    assert!(lua.contains("local w ="));
    assert!(lua.contains("local h ="));
}

#[test]
fn match_option_and_literals() {
    let lua = compile(
        r#"
        fn classify(n: i64) -> i64 {
            match n {
                0 => 10,
                1 | 2 => 20,
                _ => 30,
            }
        }
    "#,
    );
    assert!(lua.contains("== 0"), "got: {lua}");
    assert!(lua.contains(" or "), "or-pattern combines; got: {lua}");
}

// --- P3: traits, default methods, operator overloading, checker -----------

#[test]
fn operator_overload_add() {
    let lua = compile(
        r#"
        struct V2 { x: f64, y: f64 }
        impl Add for V2 {
            fn add(self, o: V2) -> V2 { V2 { x: self.x + o.x, y: self.y + o.y } }
        }
    "#,
    );
    assert!(lua.contains("function V2:add(o)"), "got: {lua}");
    assert!(
        lua.contains("V2.__add = V2.add"),
        "operator alias; got: {lua}"
    );
}

#[test]
fn trait_default_method_inherited() {
    let lua = compile(
        r#"
        trait Named {
            fn name(&self) -> String { "thing" }
            fn describe(&self) -> String;
        }
        struct Cat {}
        impl Named for Cat {
            fn describe(&self) -> String { "a cat" }
        }
    "#,
    );
    // `describe` provided by impl, `name` inherited as default
    assert!(lua.contains("function Cat:describe()"), "got: {lua}");
    assert!(
        lua.contains("function Cat:name()"),
        "default inherited; got: {lua}"
    );
    assert!(lua.contains("\"thing\""));
}

#[test]
fn trait_default_not_emitted_when_overridden() {
    let lua = compile(
        r#"
        trait Named { fn name(&self) -> String { "thing" } }
        struct Dog {}
        impl Named for Dog { fn name(&self) -> String { "dog" } }
    "#,
    );
    assert!(lua.contains("\"dog\""));
    assert!(
        !lua.contains("\"thing\""),
        "overridden default must not be emitted; got: {lua}"
    );
}

#[test]
fn checker_unknown_struct_field() {
    let err = compile_str(
        r#"
        struct Point { x: f64, y: f64 }
        fn f() -> Point { Point { x: 1.0, z: 2.0 } }
    "#,
    )
    .unwrap_err();
    assert!(err.contains("no field `z`"), "got: {err}");
}

#[test]
fn checker_missing_struct_field() {
    let err = compile_str(
        r#"
        struct Point { x: f64, y: f64 }
        fn f() -> Point { Point { x: 1.0 } }
    "#,
    )
    .unwrap_err();
    assert!(err.contains("missing field `y`"), "got: {err}");
}

#[test]
fn checker_variant_arity() {
    let err = compile_str(
        r#"
        enum E { Pair(i64, i64) }
        fn f() -> E { E::Pair(1) }
    "#,
    )
    .unwrap_err();
    assert!(err.contains("expects 2 argument"), "got: {err}");
}

#[test]
fn checker_unknown_variant() {
    let err = compile_str(
        r#"
        enum E { A, B }
        fn f() -> E { E::C }
    "#,
    )
    .unwrap_err();
    assert!(err.contains("no variant `C`"), "got: {err}");
}

#[test]
fn checker_duplicate_definition() {
    let err = compile_str("fn f() {} fn f() {}").unwrap_err();
    assert!(
        err.contains("duplicate top-level definition `f`"),
        "got: {err}"
    );
}

#[test]
fn checker_allows_declared_external_names() {
    let lua = compile(
        r#"
        extern "lua" { fn print(value: String); }
        fn main() { print("hi"); }
    "#,
    );
    assert!(lua.contains("print(\"hi\")"));
}

#[test]
fn checker_rejects_unresolved_call_target() {
    let err = compile_str("fn main() { missing(\"hi\"); }").unwrap_err();
    assert!(err.contains("cannot resolve name `missing`"), "got: {err}");
}

#[test]
fn match_guard() {
    let lua = compile(
        r#"
        fn f(n: i64) -> i64 {
            match n {
                x if x > 0 => 1,
                _ => 0,
            }
        }
    "#,
    );
    assert!(
        !lua.contains("local x ="),
        "stable binding was copied: {lua}"
    );
    assert!(
        lua.contains("if __t1 > 0 then"),
        "guard uses binding; got: {lua}"
    );
}

// --- P4: for / range / index / macros / Vec -------------------------------

#[test]
fn for_range_exclusive() {
    let lua = compile("fn f() { for i in 0..10 { print!(\"{}\", i); } }");
    assert!(lua.contains("for i = 0, 9 do"), "got: {lua}");
}

#[test]
fn for_range_inclusive() {
    let lua = compile("fn f() { for i in 1..=5 { print!(\"{}\", i); } }");
    assert!(lua.contains("for i = 1, 5 do"), "got: {lua}");
}

#[test]
fn empty_pure_range_loop_is_eliminated_but_effectful_bounds_are_kept() {
    let pure = compile("fn f() { for _ in 0..5 {} }");
    assert!(!pure.contains("for _ ="), "got: {pure}");

    let effectful = compile("fn bound() -> i64 { 5 } fn f() { for _ in bound()..bound() {} }");
    assert!(effectful.contains("bound()"), "got: {effectful}");
    assert!(effectful.contains("for _ ="), "got: {effectful}");
}

#[test]
fn for_over_vec() {
    let lua = compile(
        r#"
        fn f() {
            let v = vec![1, 2, 3];
            for x in v { print!("{}", x); }
        }
    "#,
    );
    assert!(lua.contains(".n - 1 do"), "iterates by length; got: {lua}");
}

#[test]
fn vec_macro_zero_based() {
    let lua = compile("fn f() -> i64 { let v = vec![10, 20]; v[0] }");
    assert!(
        lua.contains("rt.vec({ [0] = 10, 20, n = 2 })"),
        "got: {lua}"
    );
    assert!(
        lua.contains("local rt = require(\"rua_rt\")"),
        "emits require; got: {lua}"
    );
}

#[test]
fn index_expr() {
    let lua = compile("fn f() -> i64 { let v = vec![1, 2]; let a = v[0]; a }");
    assert!(lua.contains("local a = v[0]"), "0-based index; got: {lua}");
}

#[test]
fn macro_println_format() {
    let lua = compile(r#"fn main() { println!("x = {}", 42); }"#);
    assert!(lua.contains(r#"rt.println("x = {}", 42)"#), "got: {lua}");
}

#[test]
fn macro_panic() {
    let lua = compile(r#"fn f() { panic!("boom"); }"#);
    assert!(lua.contains("rt.panic(rt.format(\"boom\"))"), "got: {lua}");
}

#[test]
fn no_require_when_rt_unused() {
    let lua = compile("fn main() { let x = 1; }");
    assert!(
        !lua.contains("require(\"rua_rt\")"),
        "no require without rt; got: {lua}"
    );
}

#[test]
fn vec_method_call() {
    // `.push()` / `.len()` lower to method calls resolved by the Vec metatable.
    let lua = compile("fn f() { let v = vec![1]; v.push(2); let n = v.len(); }");
    assert!(lua.contains("v:push(2)"), "got: {lua}");
    assert!(lua.contains("v:len()"), "got: {lua}");
}

// --- P4b: extern "lua" + HashMap ------------------------------------------

#[test]
fn extern_block_emits_no_code() {
    let lua = compile(
        r#"
        extern "lua" {
            fn print(msg: String);
            fn tostring(v: i64) -> String;
        }
        fn main() { print("hi"); }
    "#,
    );
    // Externs bind the host global and fail fast when the host contract is absent.
    assert!(
        lua.contains("local print = assert(_G[\"print\"], \"missing Lua extern `print`\")"),
        "got: {lua}"
    );
    assert!(lua.contains("print(\"hi\")"), "got: {lua}");
}

#[test]
fn extern_variadic_parses() {
    // `...` in an extern signature is accepted.
    let lua = compile(
        r#"
        extern "lua" {
            fn format(fmt: String, ...) -> String;
        }
        fn main() { let s = format("{} {}", 1, 2); }
    "#,
    );
    assert!(lua.contains("format(\"{} {}\", 1, 2)"), "got: {lua}");
}

#[test]
fn extern_duplicate_of_fn_is_error() {
    let err = compile_str(
        r#"
        extern "lua" { fn foo(); }
        fn foo() {}
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("duplicate top-level definition `foo`"),
        "got: {err}"
    );
}

#[test]
fn hashmap_new_and_methods() {
    let lua = compile(
        r#"
        fn main() {
            let mut m = HashMap::new();
            m.insert("a", 1);
            let v = m.get("a");
            let has = m.contains_key("a");
            let n = m.len();
        }
    "#,
    );
    assert!(lua.contains("local m = rt.map()"), "got: {lua}");
    assert!(lua.contains("m:insert(\"a\", 1)"), "got: {lua}");
    assert!(lua.contains("m:get(\"a\")"), "got: {lua}");
    assert!(lua.contains("m:contains_key(\"a\")"), "got: {lua}");
    assert!(lua.contains("local rt = require(\"rua_rt\")"), "got: {lua}");
}

#[test]
fn vec_new_intrinsic() {
    let lua = compile("fn f() { let v = Vec::new(); v.push(1); }");
    assert!(lua.contains("local v = rt.vec({ n = 0 })"), "got: {lua}");
}

// --- P5: AST spans -> line-accurate diagnostics ---------------------------

#[test]
fn diagnostic_reports_line_number() {
    // The bad struct literal is on line 4 (1-based, counting the leading \n).
    let err = compile_str(
        "\nstruct Point { x: f64, y: f64 }\nfn f() -> Point {\n    Point { x: 1.0, z: 2.0 }\n}\n",
    )
    .unwrap_err();
    assert!(err.contains("no field `z`"), "got: {err}");
    assert!(
        err.starts_with("4:"),
        "error should be prefixed with line 4; got: {err}"
    );
}

#[test]
fn diagnostic_line_for_variant_arity() {
    let err =
        compile_str("enum E { Pair(i64, i64) }\nfn f() -> E {\n    E::Pair(1)\n}\n").unwrap_err();
    assert!(err.contains("expects 2 argument"), "got: {err}");
    assert!(err.starts_with("3:"), "arity error on line 3; got: {err}");
}

// --- P5b: conservative type checker ---------------------------------------

#[test]
fn typeck_non_bool_if_condition() {
    let err = compile_str("fn f() { if 1 { } }").unwrap_err();
    assert!(err.contains("`if` condition must be `bool`"), "got: {err}");
}

#[test]
fn typeck_non_bool_while_condition() {
    let err = compile_str("fn f() { while 3 { } }").unwrap_err();
    assert!(
        err.contains("`while` condition must be `bool`"),
        "got: {err}"
    );
}

#[test]
fn typeck_return_type_mismatch() {
    let err = compile_str("fn f() -> bool { 1 }").unwrap_err();
    assert!(err.contains("expected return type `bool`"), "got: {err}");
}

#[test]
fn typeck_let_annotation_mismatch() {
    let err = compile_str("fn f() { let x: bool = 1; }").unwrap_err();
    assert!(err.contains("annotated as `bool`"), "got: {err}");
}

#[test]
fn typeck_fn_arity() {
    let err = compile_str("fn g(a: i64, b: i64) -> i64 { a + b }\nfn f() { g(1); }").unwrap_err();
    assert!(
        err.contains("function `g` expects 2 argument"),
        "got: {err}"
    );
}

#[test]
fn typeck_fn_arg_type() {
    let err = compile_str("fn g(a: bool) {}\nfn f() { g(5); }").unwrap_err();
    assert!(
        err.contains("argument 1 of `g` expects `bool`"),
        "got: {err}"
    );
}

#[test]
fn typeck_arithmetic_on_bool() {
    let err = compile_str("fn f() { let x = true + 1; }").unwrap_err();
    assert!(
        err.contains("arithmetic operator applied to `bool`"),
        "got: {err}"
    );
}

#[test]
fn typeck_field_access_unknown() {
    let err = compile_str("struct P { x: i64 }\nfn f(p: P) -> i64 { p.y }").unwrap_err();
    assert!(err.contains("struct `P` has no field `y`"), "got: {err}");
}

#[test]
fn typeck_numeric_mixing_is_allowed() {
    // i64 + f64 is intentionally lenient (Lua unifies numbers) -> no error.
    let lua = compile("fn f() -> f64 { let a = 1; let b = 2.0; a + b }");
    assert!(lua.contains("a + b"), "got: {lua}");
}

#[test]
fn typeck_extern_calls_not_flagged() {
    // Declared extern names participate in resolution without type false positives.
    let lua = compile(
        r#"
        extern "lua" { fn thing(x: i64) -> bool; fn other(); }
        fn f() { if thing(1) { } let y = other(); }
    "#,
    );
    assert!(lua.contains("thing(1)"), "got: {lua}");
}

#[test]
fn typeck_method_arity() {
    let err = compile_str(
        "struct P { x: i64 }\nimpl P { fn get(self) -> i64 { self.x } }\nfn f(p: P) -> i64 { p.get(5) }",
    )
    .unwrap_err();
    assert!(
        err.contains("method `P::get` expects 0 argument"),
        "got: {err}"
    );
}

#[test]
fn typeck_method_arg_type() {
    let err = compile_str(
        "struct P { x: i64 }\nimpl P { fn set(self, v: bool) {} }\nfn f(p: P) { p.set(3); }",
    )
    .unwrap_err();
    assert!(
        err.contains("argument 1 of `P::set` expects `bool`"),
        "got: {err}"
    );
}

#[test]
fn typeck_method_return_feeds_inference() {
    let err = compile_str(
        "struct P { x: i64 }\nimpl P { fn flag(self) -> bool { true } }\nfn f(p: P) -> i64 { p.flag() }",
    )
    .unwrap_err();
    assert!(err.contains("expected return type `i64`"), "got: {err}");
}

#[test]
fn typeck_inherited_default_method_ok() {
    // `twice` is a trait default inherited by S; calling it must type-check.
    let lua = compile(
        r#"
        trait T {
            fn base(&self) -> i64;
            fn twice(&self) -> i64 { self.base() * 2 }
        }
        struct S { v: i64 }
        impl T for S { fn base(&self) -> i64 { self.v } }
        fn f(s: S) -> i64 { s.twice() }
    "#,
    );
    assert!(
        lua.contains("S.twice") || lua.contains("S:twice"),
        "got: {lua}"
    );
}

#[test]
fn typeck_inherited_default_method_arity_checked() {
    let err = compile_str(
        r#"
        trait T {
            fn base(&self) -> i64;
            fn twice(&self) -> i64 { self.base() * 2 }
        }
        struct S { v: i64 }
        impl T for S { fn base(&self) -> i64 { self.v } }
        fn f(s: S) -> i64 { s.twice(1) }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("method `S::twice` expects 0 argument"),
        "got: {err}"
    );
}

#[test]
fn typeck_vec_and_map_methods_not_flagged() {
    // Receiver types for Vec/HashMap are Unknown -> method calls never flagged.
    let lua = compile(
        "fn f() { let v = vec![1]; v.push(2); let m = HashMap::new(); m.insert(\"a\", 1); }",
    );
    assert!(lua.contains("v:push(2)"), "got: {lua}");
    assert!(lua.contains("m:insert"), "got: {lua}");
}

#[test]
fn typeck_vec_element_index_type() {
    // v[0] : i64, used as an `if` condition -> non-bool error (element tracked).
    let err = compile_str("fn f() { let v = vec![1, 2]; if v[0] { } }").unwrap_err();
    assert!(err.contains("`if` condition must be `bool`"), "got: {err}");
}

#[test]
fn typeck_vec_annotation_mismatch() {
    let err = compile_str("fn f() { let x: i64 = vec![1]; }").unwrap_err();
    assert!(err.contains("annotated as `i64`"), "got: {err}");
    assert!(err.contains("Vec<i64>"), "shows element type; got: {err}");
}

#[test]
fn typeck_for_over_vec_binds_element() {
    // x : bool (from Vec<bool>), annotated as i64 -> mismatch.
    let err =
        compile_str("fn f() { let v = vec![true]; for x in v { let y: i64 = x; } }").unwrap_err();
    assert!(err.contains("annotated as `i64`"), "got: {err}");
}

#[test]
fn typeck_try_unwraps_result_element() {
    let err = compile_str(
        r#"
        fn p() -> Result<i64, String> { Ok(1) }
        fn f() -> Result<i64, String> { let v = p()?; if v { } Ok(v) }
    "#,
    )
    .unwrap_err();
    assert!(err.contains("`if` condition must be `bool`"), "got: {err}");
}

#[test]
fn typeck_map_contains_key_is_bool_ok() {
    let lua = compile(r#"fn f() { let m = HashMap::new(); if m.contains_key("a") { } }"#);
    assert!(lua.contains("m:contains_key"), "got: {lua}");
}

#[test]
fn typeck_option_result_return_ok() {
    // Correct Option/Result returns must type-check cleanly.
    let lua = compile(
        r#"
        fn a() -> Option<i64> { Some(1) }
        fn b() -> Result<i64, String> { Ok(2) }
        fn c() -> Result<i64, String> { Err("x") }
    "#,
    );
    assert!(lua.contains("rt.result_ok(2)"), "got: {lua}");
    assert!(lua.contains("rt.result_err(\"x\")"), "got: {lua}");
}

#[test]
fn codegen_integer_division() {
    // Constant i64 division is folded without loading the runtime helper.
    let lua = compile("fn f() -> i64 { 7 / 2 }");
    assert!(lua.contains("return 3"), "got: {lua}");
    assert!(!lua.contains("rt.idiv"), "got: {lua}");
}

#[test]
fn codegen_float_division_stays_slash() {
    let lua = compile("fn f() -> f64 { 7.0 / 2.0 }");
    assert!(lua.contains("7.0 / 2.0"), "got: {lua}");
    assert!(
        !lua.contains("//"),
        "should not use integer division; got: {lua}"
    );
}

#[test]
fn codegen_mixed_division_stays_slash() {
    // i64 / f64 is not integer division.
    let lua = compile("fn f(a: i64, b: f64) -> f64 { a / b }");
    assert!(lua.contains("a / b"), "got: {lua}");
    assert!(!lua.contains("//"), "got: {lua}");
}

#[test]
fn generic_division_requires_operator_bound() {
    let error = crate::compile_str("fn f<T>(a: T, b: T) { let x = a / b; }").unwrap_err();
    assert!(
        error.contains("cannot apply arithmetic operator to `T` and `T`"),
        "got: {error}"
    );
}

#[test]
fn codegen_int_division_via_params() {
    // `i64 / i64` lowers to the truncating helper (matches Rust for negatives),
    // not Lua's floored `//`.
    let lua = compile("fn f(a: i64, b: i64) -> i64 { a / b }");
    assert!(lua.contains("rt.idiv(a, b)"), "got: {lua}");
    assert!(!lua.contains("//"), "got: {lua}");
}

#[test]
fn codegen_int_remainder_via_params() {
    // `i64 % i64` lowers to the truncating remainder helper.
    let lua = compile("fn f(a: i64, b: i64) -> i64 { a % b }");
    assert!(lua.contains("rt.irem(a, b)"), "got: {lua}");
}

#[test]
fn codegen_nonnegative_range_remainder_uses_lua_operator() {
    let lua = compile(
        "fn f() -> i64 { let mut total = 0; for i in 0..10 { total = total + i % 2; } total }",
    );
    assert!(lua.contains("i % 2"), "got: {lua}");
    assert!(!lua.contains("rt.irem"), "got: {lua}");
}

#[test]
fn codegen_mixed_remainder_stays_percent() {
    // A non-integer remainder keeps the plain Lua `%`.
    let lua = compile("fn f(a: f64, b: f64) -> f64 { a % b }");
    assert!(lua.contains("a % b"), "got: {lua}");
    assert!(!lua.contains("rt.irem"), "got: {lua}");
}

// --- std shims: String methods + concatenation ------------------------------

#[test]
fn codegen_string_method_routes_through_rt_str() {
    let lua = compile(r#"fn f(s: String) -> String { s.to_uppercase() }"#);
    assert!(lua.contains("rt.str[\"to_uppercase\"](s)"), "got: {lua}");
}

#[test]
fn codegen_string_method_on_literal_uses_call_form() {
    // Method on a string literal must not use the `literal:method()` form.
    let lua = compile(r#"fn f() -> bool { "abc".contains("b") }"#);
    assert!(
        lua.contains("rt.str[\"contains\"](\"abc\", \"b\")"),
        "got: {lua}"
    );
}

#[test]
fn codegen_string_concat_uses_dotdot() {
    let lua = compile(r#"fn f(a: String, b: String) -> String { a + b }"#);
    assert!(lua.contains("(a .. b)"), "got: {lua}");
}

#[test]
fn typeck_string_len_is_i64() {
    // `s.len()` yields `i64`, so binding it to `bool` is a type error.
    let err = compile_str(r#"fn f(s: String) { let _n: bool = s.len(); }"#).unwrap_err();
    assert!(
        err.contains("annotated as `bool` but initialized with `i64`"),
        "got: {err}"
    );
}

#[test]
fn typeck_string_split_returns_vec_of_string() {
    // `split` yields `Vec<String>`; indexing gives `String`, so `if el { }` fails.
    let err =
        compile_str(r#"fn f(s: String) { let v = s.split(","); if v.get(0) { } }"#).unwrap_err();
    assert!(err.contains("must be `bool`"), "got: {err}");
}

#[test]
fn codegen_unknown_string_method_stays_colon_form() {
    // An unrecognized method on a String is left as a plain method call.
    let lua = compile(r#"fn f(s: String) { s.frobnicate(); }"#);
    assert!(lua.contains("s:frobnicate()"), "got: {lua}");
    assert!(!lua.contains("rt.str"), "got: {lua}");
}

#[test]
fn typeck_operator_overload_not_flagged() {
    // `a + b` on a user struct with `impl Add` must not be flagged as bad arithmetic.
    let lua = compile(
        r#"
        struct V { x: f64 }
        impl Add for V {
            fn add(self, o: V) -> V { V { x: self.x + o.x } }
        }
        fn f(a: V, b: V) -> V { a + b }
    "#,
    );
    assert!(lua.contains("V.__add"), "got: {lua}");
}

#[test]
fn user_trait_named_like_operator_does_not_enable_operator_lowering() {
    let error = compile_str(
        r#"
        trait Add { fn add(&self, other: Point) -> Point; }
        struct Point { x: i64 }
        impl Add for Point {
            fn add(&self, other: Point) -> Point { Point { x: self.x + other.x } }
        }
        fn combine(left: Point, right: Point) -> Point { left + right }
        "#,
    )
    .unwrap_err();
    assert!(
        error.contains("does not implement operator trait `Add`"),
        "got: {error}"
    );
}

// --- P4c: if let / while let ------------------------------------------------

#[test]
fn if_let_some_binds_and_tests_nil() {
    // Pure-nil Option: `if let Some(x) = opt` tests `~= nil` and binds x = subject.
    let lua = compile(
        r#"
        fn f(opt: Option<i64>) {
            if let Some(x) = opt {
                println!("{}", x);
            }
        }
    "#,
    );
    assert!(lua.contains("~= nil then"), "got: {lua}");
    assert!(!lua.contains("local x ="), "got: {lua}");
    assert!(lua.contains("rt.println(\"{}\", opt)"), "got: {lua}");
}

#[test]
fn if_let_with_else_branch() {
    let lua = compile(
        r#"
        fn f(opt: Option<i64>) -> i64 {
            if let Some(x) = opt { x } else { 0 }
        }
    "#,
    );
    assert!(lua.contains("else"), "got: {lua}");
    // Used in tail position, so both branches feed the return.
    assert!(lua.contains("return"), "got: {lua}");
}

#[test]
fn if_let_ok_unwraps_result() {
    let lua = compile(
        r#"
        fn f(r: Result<i64, String>) {
            if let Ok(v) = r {
                println!("{}", v);
            }
        }
    "#,
    );
    assert!(lua.contains(".tag == \"ok\""), "got: {lua}");
    assert!(lua.contains(".value"), "got: {lua}");
}

#[test]
fn if_let_as_statement_needs_no_semicolon() {
    // Two consecutive if-let statements must parse (block-like, no `;`).
    let lua = compile(
        r#"
        fn f(a: Option<i64>, b: Option<i64>) {
            if let Some(x) = a {
                println!("{}", x);
            }
            if let Some(y) = b {
                println!("{}", y);
            }
        }
    "#,
    );
    assert!(lua.contains("function f(a, b)"), "got: {lua}");
}

#[test]
fn while_let_drains_with_break() {
    let lua = compile(
        r#"
        fn f(stack: Vec<i64>) {
            while let Some(x) = stack.pop() {
                println!("{}", x);
            }
        }
    "#,
    );
    assert!(lua.contains("while true do"), "got: {lua}");
    assert!(lua.contains("stack:pop()"), "got: {lua}");
    assert!(lua.contains("else"), "got: {lua}");
    assert!(lua.contains("break"), "got: {lua}");
}

// --- P4c: module system (slice 1: inline mod / use / pub) -------------------

#[test]
fn module_emits_one_table_identity() {
    let lua = compile(
        r#"
        mod math {
            pub fn add(a: i64, b: i64) -> i64 { a + b }
        }
        fn main() { println!("{}", math::add(1, 2)); }
    "#,
    );
    assert!(
        lua.contains("local math = __rua_table_create(0, 1)"),
        "got: {lua}"
    );
    assert!(lua.contains("function math.add(a, b)"), "got: {lua}");
    // Qualified call `math::add` lowers to `math.add`.
    assert!(lua.contains("math.add(1, 2)"), "got: {lua}");
}

#[test]
fn module_sibling_call_is_block_local() {
    // The sibling call resolves to the module field identity.
    let lua = compile(
        r#"
        mod m {
            pub fn add(a: i64, b: i64) -> i64 { a + b }
            pub fn double(x: i64) -> i64 { add(x, x) }
        }
        fn main() {}
    "#,
    );
    assert!(lua.contains("function m.add"), "got: {lua}");
    assert!(lua.contains("return m.add(x, x)"), "got: {lua}");
}

#[test]
fn nested_module_qualified_access() {
    let lua = compile(
        r#"
        mod outer {
            pub mod inner {
                pub fn f() -> i64 { 1 }
            }
        }
        fn main() { println!("{}", outer::inner::f()); }
    "#,
    );
    assert!(
        lua.contains("outer.inner = __rua_table_create(0, 1)"),
        "got: {lua}"
    );
    assert!(lua.contains("function outer.inner.f()"), "got: {lua}");
    assert!(lua.contains("outer.inner.f()"), "got: {lua}");
}

#[test]
fn use_import_desugars_to_qualified_path() {
    // `use` introduces no runtime local; the call site is fully qualified.
    let lua = compile(
        r#"
        mod m { pub fn f() -> i64 { 1 } }
        use m::f;
        fn main() { println!("{}", f()); }
    "#,
    );
    assert!(lua.contains("m.f()"), "got: {lua}");
    assert!(!lua.contains("f = m.f"), "got: {lua}");
}

#[test]
fn use_alias_and_group_desugar() {
    let lua = compile(
        r#"
        mod m {
            pub fn a() -> i64 { 1 }
            pub fn b() -> i64 { 2 }
        }
        use m::a as first;
        use m::{b};
        fn main() {
            let _x = first();
            let _y = b();
        }
    "#,
    );
    assert!(lua.contains("m.a()"), "got: {lua}");
    assert!(lua.contains("m.b()"), "got: {lua}");
    // No alias locals are emitted.
    assert!(!lua.contains("first ="), "got: {lua}");
}

#[test]
fn use_alias_keeps_structural_checks_identity_driven() {
    let missing_field = compile_str(
        r#"
        mod model { pub struct Point { x: i64, y: i64 } }
        use model::Point as P;
        let point = P { x: 1 };
        "#,
    )
    .unwrap_err();
    assert!(
        missing_field.contains("missing field `y`"),
        "{missing_field}"
    );

    let wrong_variant_arity = compile_str(
        r#"
        mod model { pub enum Pair { Both(i64, i64) } }
        use model::Pair::Both as B;
        let pair = B(1);
        "#,
    )
    .unwrap_err();
    assert!(
        wrong_variant_arity.contains("expects 2 argument(s), got 1"),
        "{wrong_variant_arity}"
    );
}

#[test]
fn use_alias_keeps_type_checks_identity_driven() {
    let error = compile_str(
        r#"
        mod math { pub fn double(value: i64) -> i64 { value + value } }
        use math::double as twice;
        let value = twice(true);
        "#,
    )
    .unwrap_err();
    assert!(error.contains("argument 1"), "{error}");
    assert!(error.contains("expects `i64`, found `bool`"), "{error}");
}

#[test]
fn same_fn_name_across_modules_no_dup_error() {
    // Two modules may each define `f` without a duplicate-definition error.
    let lua = compile(
        r#"
        mod a { pub fn f() -> i64 { 1 } }
        mod b { pub fn f() -> i64 { 2 } }
        fn main() {}
    "#,
    );
    assert!(
        lua.contains("local a = __rua_table_create(0, 1)"),
        "got: {lua}"
    );
    assert!(
        lua.contains("local b = __rua_table_create(0, 1)"),
        "got: {lua}"
    );
}

#[test]
fn module_struct_qualified_literal_and_assoc_fn() {
    let lua = compile(
        r#"
        mod geo {
            pub struct Point { x: f64, y: f64 }
            impl Point {
                pub fn new(x: f64, y: f64) -> Point { Point { x: x, y: y } }
            }
        }
        fn main() {
            let p = geo::Point::new(1.0, 2.0);
            let q = geo::Point { x: 3.0, y: 4.0 };
            println!("{}", q.x);
        }
    "#,
    );
    // The class table has one module-owned backend place.
    assert!(
        lua.contains("geo.Point = __rua_table_create(0, 2)"),
        "got: {lua}"
    );
    assert!(lua.contains("geo.Point.__index = geo.Point"), "got: {lua}");
    assert!(lua.contains("function geo.Point.new(x, y)"), "got: {lua}");
    // Cross-module associated fn + struct literal use the qualified table.
    assert!(lua.contains("geo.Point.new(1.0, 2.0)"), "got: {lua}");
    assert!(
        lua.contains("setmetatable({ x = 3.0, y = 4.0 }, geo.Point)"),
        "got: {lua}"
    );
}

#[test]
fn module_enum_variants_qualified() {
    let lua = compile(
        r#"
        mod geo {
            pub enum Shape { Circle(f64), Rect { w: f64, h: f64 }, Dot }
        }
        fn consume(_a: geo::Shape, _b: geo::Shape, _c: geo::Shape) {}
        fn main() {
            let a = geo::Shape::Circle(5.0);
            let b = geo::Shape::Rect { w: 2.0, h: 3.0 };
            let c = geo::Shape::Dot;
            consume(a, b, c);
        }
    "#,
    );
    assert!(!lua.contains("setmetatable"), "got: {lua}");
    assert!(lua.contains("{ tag = \"Circle\", 5.0 }"), "got: {lua}");
    assert!(
        lua.contains("{ tag = \"Rect\", w = 2.0, h = 3.0 }"),
        "got: {lua}"
    );
    assert!(lua.contains("{ tag = \"Dot\" }"), "got: {lua}");
}

#[test]
fn module_method_and_self_call() {
    let lua = compile(
        r#"
        mod geo {
            pub struct P { x: f64 }
            impl P {
                pub fn get(&self) -> f64 { self.x }
            }
        }
        fn main() {
            let p = geo::P { x: 9.0 };
            println!("{}", p.get());
        }
    "#,
    );
    assert!(lua.contains("function geo.P:get()"), "got: {lua}");
    assert!(lua.contains("geo.P.get(p)"), "got: {lua}");
}

#[test]
fn module_trait_default_method() {
    let lua = compile(
        r#"
        mod shapes {
            pub trait Named {
                fn tag(&self) -> i64 { 0 }
            }
            pub struct Sq { s: f64 }
            impl Named for Sq {}
        }
        fn main() {}
    "#,
    );
    // Inherited default method is emitted onto the module-local class table.
    assert!(lua.contains("function shapes.Sq:tag()"), "got: {lua}");
}

#[test]
fn same_type_name_across_modules_no_dup_error() {
    // Distinct modules may each define a `Point` without a duplicate error.
    let lua = compile(
        r#"
        mod a { pub struct Point { x: i64 } }
        mod b { pub struct Point { y: i64 } }
        fn main() {}
    "#,
    );
    assert!(lua.contains("a.Point = {}"), "got: {lua}");
    assert!(!lua.contains("a.Point.__index = a.Point"), "got: {lua}");
    assert!(lua.contains("b.Point = {}"), "got: {lua}");
    assert!(!lua.contains("b.Point.__index = b.Point"), "got: {lua}");
}

#[test]
fn private_fn_cross_module_rejected() {
    let err = compile_str(
        r#"
        mod m { fn secret() -> i64 { 42 } }
        fn main() { println!("{}", m::secret()); }
    "#,
    )
    .unwrap_err();
    assert!(err.contains("`secret` is private"), "got: {err}");
}

#[test]
fn private_submodule_cross_access_rejected() {
    let err = compile_str(
        r#"
        mod a {
            mod b { pub fn f() -> i64 { 1 } }
        }
        fn main() { println!("{}", a::b::f()); }
    "#,
    )
    .unwrap_err();
    assert!(err.contains("`b` is private"), "got: {err}");
}

#[test]
fn private_struct_cross_module_rejected() {
    let err = compile_str(
        r#"
        mod geo { struct Point { x: i64 } }
        fn main() { let p = geo::Point { x: 1 }; }
    "#,
    )
    .unwrap_err();
    assert!(err.contains("`Point` is private"), "got: {err}");
}

#[test]
fn use_private_item_rejected() {
    // Importing a private item via `use` is itself a visibility error.
    let err = compile_str(
        r#"
        mod m { fn secret() -> i64 { 42 } }
        use m::secret;
        fn main() {}
    "#,
    )
    .unwrap_err();
    assert!(err.contains("`secret` is private"), "got: {err}");
}

#[test]
fn use_pub_item_ok() {
    let lua = compile(
        r#"
        mod m { pub fn ok() -> i64 { 1 } }
        use m::ok;
        fn main() { let _x = ok(); }
    "#,
    );
    assert!(lua.contains("m.ok()"), "got: {lua}");
}

#[test]
fn pub_item_cross_module_ok() {
    // A fully public path must not be flagged.
    let lua = compile(
        r#"
        mod a {
            pub mod b { pub fn f() -> i64 { 1 } }
        }
        fn main() { println!("{}", a::b::f()); }
    "#,
    );
    assert!(lua.contains("a.b.f()"), "got: {lua}");
}

#[test]
fn root_private_item_visible_everywhere() {
    // Root-level private items are visible crate-wide (including from modules).
    let lua = compile(
        r#"
        fn helper() -> i64 { 7 }
        mod m {
            pub fn use_root() -> i64 { helper() }
        }
        fn main() { println!("{}", m::use_root()); }
    "#,
    );
    assert!(lua.contains("m.use_root()"), "got: {lua}");
}

#[test]
fn private_item_same_module_ok() {
    // A module may use its own private items through its resolved table place.
    let lua = compile(
        r#"
        mod m {
            fn secret() -> i64 { 42 }
            pub fn reveal() -> i64 { secret() }
        }
        fn main() { println!("{}", m::reveal()); }
    "#,
    );
    assert!(lua.contains("return m.secret()"), "got: {lua}");
}

#[test]
fn use_inside_module_desugars_within_scope() {
    // A `use` inside a module rewrites references in that module's functions,
    // and its scope does not leak to sibling modules or the root.
    let lua = compile(
        r#"
        mod util { pub fn helper() -> i64 { 7 } }
        mod m {
            use util::helper as h;
            pub fn go() -> i64 { h() }
        }
        fn main() { println!("{}", m::go()); }
    "#,
    );
    assert!(lua.contains("return util.helper()"), "got: {lua}");
    // No runtime alias local for `h`.
    assert!(!lua.contains(" h ="), "got: {lua}");
}

#[test]
fn use_scope_does_not_leak_to_root() {
    // An alias declared inside a module must not affect a bare name at the root.
    let lua = compile(
        r#"
        mod m {
            use m::inner::deep as helper;
            pub mod inner { pub fn deep() -> i64 { 1 } }
        }
        fn helper() -> i64 { 42 }
        fn main() { println!("{}", helper()); }
    "#,
    );
    // Root `helper()` stays bare (not rewritten to the module alias target).
    assert!(lua.contains("rt.println(\"{}\", helper())"), "got: {lua}");
}

#[test]
fn use_alias_shadowed_by_local_not_rewritten() {
    // A local binding that shadows a `use` alias suppresses rewriting.
    let lua = compile(
        r#"
        mod m { pub fn f() -> i64 { 1 } }
        use m::f;
        fn main() {
            let f = 99;
            println!("{}", f);
        }
    "#,
    );
    // The shadowed reference stays bare rather than becoming `m.f`.
    assert!(lua.contains("rt.println(\"{}\", f)"), "got: {lua}");
}

#[test]
fn duplicate_fn_in_module_is_rejected() {
    let err = compile_str(
        r#"
        mod m {
            pub fn f() -> i64 { 1 }
            pub fn f() -> i64 { 2 }
        }
        fn main() {}
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("duplicate definition `f` in module `m`"),
        "got: {err}"
    );
}

#[test]
fn file_module_via_string_errors() {
    // `mod m;` parses, but resolving it needs a base directory (a file compile).
    let err = compile_str("mod m;\nfn main() {}").unwrap_err();
    assert!(err.contains("requires compiling from a file"), "got: {err}");
}

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut d = std::env::temp_dir();
    d.push(format!(
        "ruac_test_{}_{}_{}",
        tag,
        std::process::id(),
        nanos
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn file_module_resolution() {
    let dir = tmp_dir("filemod");
    std::fs::write(
        dir.join("main.rua"),
        "mod util;\nfn main() { println!(\"{}\", util::f()); }\n",
    )
    .unwrap();
    std::fs::write(dir.join("util.rua"), "pub fn f() -> i64 { 7 }\n").unwrap();
    let lua = crate::compile_path(&dir.join("main.rua")).unwrap();
    assert!(
        lua.contains("local util = __rua_table_create(0, 1)"),
        "got: {lua}"
    );
    assert!(lua.contains("function util.f()"), "got: {lua}");
    assert!(lua.contains("util.f()"), "got: {lua}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn nested_file_module_resolution() {
    let dir = tmp_dir("nested");
    std::fs::write(
        dir.join("main.rua"),
        "mod math;\nfn main() { println!(\"{}\", math::trig::triple(2)); }\n",
    )
    .unwrap();
    std::fs::write(dir.join("math.rua"), "pub mod trig;\n").unwrap();
    std::fs::create_dir_all(dir.join("math")).unwrap();
    std::fs::write(
        dir.join("math").join("trig.rua"),
        "pub fn triple(x: i64) -> i64 { x * 3 }\n",
    )
    .unwrap();
    let lua = crate::compile_path(&dir.join("main.rua")).unwrap();
    assert!(lua.contains("math.trig.triple(2)"), "got: {lua}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mod_dir_style_resolution() {
    // `mod foo;` may also resolve to `foo/mod.rua`.
    let dir = tmp_dir("moddir");
    std::fs::write(
        dir.join("main.rua"),
        "mod foo;\nfn main() { println!(\"{}\", foo::g()); }\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("foo")).unwrap();
    std::fs::write(dir.join("foo").join("mod.rua"), "pub fn g() -> i64 { 1 }\n").unwrap();
    let lua = crate::compile_path(&dir.join("main.rua")).unwrap();
    assert!(lua.contains("foo.g()"), "got: {lua}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ambiguous_file_module_layout_is_rejected() {
    let dir = tmp_dir("ambiguous_mod");
    std::fs::write(dir.join("main.rua"), "mod api;\n").unwrap();
    std::fs::write(dir.join("api.rua"), "pub fn flat() {}\n").unwrap();
    std::fs::create_dir_all(dir.join("api")).unwrap();
    std::fs::write(dir.join("api/mod.rua"), "pub fn nested() {}\n").unwrap();
    let error = crate::compile_path(&dir.join("main.rua")).unwrap_err();
    assert!(error.contains("ambiguous module `api`"), "got: {error}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_file_module_errors() {
    let dir = tmp_dir("missing");
    std::fs::write(dir.join("main.rua"), "mod nope;\nfn main() {}\n").unwrap();
    let err = crate::compile_path(&dir.join("main.rua")).unwrap_err();
    assert!(
        err.contains("cannot find file for module `nope`"),
        "got: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn file_module_visibility_enforced() {
    // Privacy still applies across file modules.
    let dir = tmp_dir("filevis");
    std::fs::write(
        dir.join("main.rua"),
        "mod m;\nfn main() { println!(\"{}\", m::secret()); }\n",
    )
    .unwrap();
    std::fs::write(dir.join("m.rua"), "fn secret() -> i64 { 42 }\n").unwrap();
    let err = crate::compile_path(&dir.join("main.rua")).unwrap_err();
    assert!(err.contains("`secret` is private"), "got: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn type_error_in_child_file_attributes_file_and_line() {
    // A type error in a loaded child file must report that file's path + line,
    // not a bare line number relative to the merged root.
    let dir = tmp_dir("attrib");
    std::fs::write(
        dir.join("main.rua"),
        "mod util;\nfn main() { let _ = util::f(); }\n",
    )
    .unwrap();
    // Return type mismatch on line 2 of util.rua.
    std::fs::write(dir.join("util.rua"), "pub fn f() -> bool {\n    1 + 2\n}\n").unwrap();
    let err = crate::compile_path(&dir.join("main.rua")).unwrap_err();
    assert!(
        err.contains("util.rua:2:"),
        "expected file:line attribution; got: {err}"
    );
    assert!(
        err.contains("expected return type `bool`, found `i64`"),
        "got: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn type_error_in_root_file_attributes_root() {
    // Errors in the root file are attributed to the root path.
    let dir = tmp_dir("rootattrib");
    std::fs::write(
        dir.join("main.rua"),
        "fn main() {\n    let _x: bool = 1 + 2;\n}\n",
    )
    .unwrap();
    let err = crate::compile_path(&dir.join("main.rua")).unwrap_err();
    assert!(err.contains("main.rua:2:"), "got: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn if_let_binding_is_scoped_no_false_positive() {
    // The binding introduced by if-let is usable in the then-block and must
    // not trigger a type error.
    let lua = compile(
        r#"
        fn f(opt: Option<i64>) -> i64 {
            if let Some(x) = opt {
                x + 1
            } else {
                0
            }
        }
    "#,
    );
    assert!(lua.contains("opt + 1"), "got: {lua}");
}

// --- P5c-4 generics + bounds ------------------------------------------------

#[test]
fn generic_identity_fn_erases_to_plain_lua() {
    let lua = compile("fn id<T>(x: T) -> T { x }\nfn main() {}");
    assert!(lua.contains("function id(x)"), "got: {lua}");
    assert!(lua.contains("return x"), "got: {lua}");
}

#[test]
fn generic_struct_and_enum_parse_and_erase() {
    let lua = compile(
        r#"
        struct Wrapper<T> { value: T }
        enum Either<L, R> { Left(L), Right(R) }
        fn main() {}
    "#,
    );
    assert!(lua.contains("---@class Wrapper"), "got: {lua}");
    assert!(lua.contains("---@class Either"), "got: {lua}");
    assert!(!lua.contains("local Wrapper ="), "got: {lua}");
    assert!(!lua.contains("local Either ="), "got: {lua}");
}

#[test]
fn generic_bound_method_call_ok() {
    // A method provided by the trait bound resolves and type-checks.
    let lua = compile(
        r#"
        trait Animal { fn speak(&self) -> String; }
        fn describe<T: Animal>(a: T) -> String { a.speak() }
        fn main() {}
    "#,
    );
    assert!(lua.contains("function describe(a)"), "got: {lua}");
    assert!(lua.contains("getmetatable(a).speak(a)"), "got: {lua}");
}

#[test]
fn generic_bound_method_wrong_arity_rejected() {
    let err = compile_str(
        r#"
        trait Animal { fn speak(&self) -> String; }
        fn describe<T: Animal>(a: T) -> String { a.speak(1) }
        fn main() {}
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("`Animal::speak` expects 0 argument(s), got 1"),
        "got: {err}"
    );
}

#[test]
fn generic_bound_method_wrong_arg_type_rejected() {
    let err = compile_str(
        r#"
        trait Adder { fn add_i(&self, x: i64) -> i64; }
        fn use_it<T: Adder>(a: T) -> i64 { a.add_i(true) }
        fn main() {}
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("argument 1 of `Adder::add_i` expects `i64`, found `bool`"),
        "got: {err}"
    );
}

#[test]
fn method_not_in_bound_is_silent() {
    // A method the bound does not declare stays Unknown (no false positive).
    let lua = compile(
        r#"
        trait Animal { fn speak(&self) -> String; }
        fn describe<T: Animal>(a: T) { a.wander(1, 2, 3); }
        fn main() {}
    "#,
    );
    assert!(lua.contains("a:wander(1, 2, 3)"), "got: {lua}");
}

#[test]
fn unknown_trait_bound_rejected() {
    let err = compile_str("fn f<T: Bogus>(x: T) {}\nfn main() {}").unwrap_err();
    assert!(err.contains("unknown trait `Bogus`"), "got: {err}");
}

#[test]
fn builtin_trait_bounds_ok() {
    let lua = compile("fn f<T: Clone + Debug>(x: T) {}\nfn main() {}");
    assert!(lua.contains("function f(x)"), "got: {lua}");
}

#[test]
fn generic_return_type_no_false_positive() {
    // `T` return with a `T` tail must not raise a return-type mismatch.
    let lua = compile(
        r#"
        fn first<T>(a: T, b: T) -> T { a }
        fn main() {}
    "#,
    );
    assert!(lua.contains("function first(a, b)"), "got: {lua}");
    assert!(lua.contains("return a"), "got: {lua}");
}

#[test]
fn impl_generic_bound_in_module_ok() {
    // Generics + bounds resolve inside modules too.
    let lua = compile(
        r#"
        mod z {
            pub trait Named { fn name(&self) -> String; }
            pub fn label<T: Named>(x: T) -> String { x.name() }
        }
        fn main() {}
    "#,
    );
    assert!(lua.contains("function z.label(x)"), "got: {lua}");
    assert!(lua.contains("getmetatable(x).name(x)"), "got: {lua}");
}

// --- P5c-5: `where` clauses + call-site constraint satisfaction -------------

#[test]
fn where_clause_bounds_merge_and_resolve() {
    // A `where`-declared bound behaves exactly like an inline `<T: Animal>`.
    let lua = compile(
        r#"
        trait Animal { fn speak(&self) -> String; }
        fn describe<T>(a: T) -> String where T: Animal { a.speak() }
        fn main() {}
    "#,
    );
    assert!(lua.contains("function describe(a)"), "got: {lua}");
    assert!(lua.contains("getmetatable(a).speak(a)"), "got: {lua}");
}

#[test]
fn where_clause_unknown_trait_rejected() {
    let err = compile_str("fn f<T>(x: T) where T: Bogus {}\nfn main() {}").unwrap_err();
    assert!(err.contains("unknown trait `Bogus`"), "got: {err}");
}

#[test]
fn where_clause_on_impl_ok() {
    let lua = compile(
        r#"
        trait Named { fn name(&self) -> String; }
        struct Pair<T> { a: T, b: T }
        impl<T> Pair<T> where T: Named {
            fn describe(&self) -> String { self.a.name() }
        }
        fn main() {}
    "#,
    );
    assert!(lua.contains("function Pair:describe()"), "got: {lua}");
}

#[test]
fn call_site_bound_satisfied_ok() {
    let lua = compile(
        r#"
        trait Animal { fn speak(&self) -> String; }
        struct Dog { n: i64 }
        impl Animal for Dog { fn speak(&self) -> String { format!("woof") } }
        fn describe<T: Animal>(a: T) -> String { a.speak() }
        fn main() { let _ = describe(Dog { n: 1 }); }
    "#,
    );
    assert!(lua.contains("describe("), "got: {lua}");
}

#[test]
fn call_site_bound_unsatisfied_rejected() {
    let err = compile_str(
        r#"
        trait Animal { fn speak(&self) -> String; }
        struct Cat { n: i64 }
        fn describe<T: Animal>(a: T) {}
        fn main() { describe(Cat { n: 1 }); }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("type `Cat` does not implement trait `Animal`"),
        "got: {err}"
    );
}

#[test]
fn call_site_where_bound_unsatisfied_rejected() {
    // The `where` form is enforced at call sites just like the inline form.
    let err = compile_str(
        r#"
        trait Animal { fn speak(&self) -> String; }
        struct Cat { n: i64 }
        fn describe<T>(a: T) where T: Animal {}
        fn main() { describe(Cat { n: 1 }); }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("type `Cat` does not implement trait `Animal`"),
        "got: {err}"
    );
}

#[test]
fn call_site_builtin_bound_not_checked() {
    // Builtin traits cannot be verified, so no false positive even without impl.
    let lua = compile(
        r#"
        struct Thing { n: i64 }
        fn f<T: Clone>(x: T) {}
        fn main() { f(Thing { n: 1 }); }
    "#,
    );
    assert!(lua.contains("function f(x)"), "got: {lua}");
}

#[test]
fn call_site_scalar_arg_not_checked() {
    // A non-user-type argument (scalar) is never flagged (conservative).
    let lua = compile(
        r#"
        trait Animal { fn speak(&self) -> String; }
        fn describe<T: Animal>(a: T) {}
        fn main() { describe(5); }
    "#,
    );
    assert!(lua.contains("describe(5)"), "got: {lua}");
}

// --- method-level generics + call-site checking -----------------------------

#[test]
fn generic_method_bound_satisfied_ok() {
    let lua = compile(
        r#"
        trait Shape { fn area(&self) -> f64; }
        struct Circle { r: f64 }
        impl Shape for Circle { fn area(&self) -> f64 { 1.0 } }
        struct Registry { n: i64 }
        impl Registry {
            fn add<T: Shape>(&self, s: T) {}
        }
        fn main() {
            let reg = Registry { n: 0 };
            reg.add(Circle { r: 1.0 });
        }
    "#,
    );
    assert!(lua.contains("function Registry:add(s)"), "got: {lua}");
}

#[test]
fn generic_method_bound_unsatisfied_rejected() {
    let err = compile_str(
        r#"
        trait Shape { fn area(&self) -> f64; }
        struct Square { s: f64 }
        struct Registry { n: i64 }
        impl Registry {
            fn add<T: Shape>(&self, s: T) {}
        }
        fn main() {
            let reg = Registry { n: 0 };
            reg.add(Square { s: 1.0 });
        }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("type `Square` does not implement trait `Shape`"),
        "got: {err}"
    );
}

#[test]
fn generic_method_return_substituted() {
    let err = compile_str(
        r#"
        struct Boxx { n: i64 }
        impl Boxx {
            fn wrap<U>(&self, x: U) -> U { x }
        }
        fn main() {
            let b = Boxx { n: 0 };
            let _y: bool = b.wrap(1);
        }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("annotated as `bool` but initialized with `i64`"),
        "got: {err}"
    );
}

#[test]
fn generic_method_builtin_bound_not_checked() {
    let lua = compile(
        r#"
        struct Thing { n: i64 }
        struct Reg { n: i64 }
        impl Reg {
            fn add<T: Clone>(&self, x: T) {}
        }
        fn main() {
            let r = Reg { n: 0 };
            r.add(Thing { n: 1 });
        }
    "#,
    );
    assert!(lua.contains("function Reg:add(x)"), "got: {lua}");
}

// --- cross-module type-name dedup -------------------------------------------

#[test]
fn same_struct_name_in_two_modules_no_false_positive() {
    // Resolved identities keep the two field schemas separate.
    let lua = compile(
        r#"
        mod a { pub struct Point { pub x: i64 } }
        mod b { pub struct Point { pub y: i64 } }
        fn main() {
            let _p = a::Point { x: 1 };
            let _q = b::Point { y: 2 };
        }
    "#,
    );
    // Runtime uses distinct qualified class tables.
    assert!(lua.contains("a.Point"), "got: {lua}");
    assert!(lua.contains("b.Point"), "got: {lua}");
}

#[test]
fn unique_struct_field_still_checked() {
    // Regression: a uniquely-named struct still gets field validation.
    let err = compile_str(
        r#"
        struct Solo { x: i64 }
        fn main() { let _ = Solo { y: 1 }; }
    "#,
    )
    .unwrap_err();
    assert!(err.contains("has no field `y`"), "got: {err}");
}

#[test]
fn same_fn_name_in_two_modules_no_false_positive() {
    // Same free-fn name in two modules resolves to two independent signatures.
    let lua = compile(
        r#"
        mod a { pub fn f(x: i64) -> i64 { x } }
        mod b { pub fn f(x: i64, y: i64) -> i64 { x + y } }
        fn main() {
            let _ = a::f(1);
            let _ = b::f(1, 2);
        }
    "#,
    );
    assert!(lua.contains("a.f"), "got: {lua}");
    assert!(lua.contains("b.f"), "got: {lua}");
}

#[test]
fn same_fn_name_in_two_modules_checks_each_resolved_signature() {
    let err = compile_str(
        r#"
        mod a { pub fn f(x: i64) -> i64 { x } }
        mod b { pub fn f(x: i64, y: i64) -> i64 { x + y } }
        fn main() {
            let _ = a::f(1, 2);
            let _ = b::f(1);
        }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("function `a::f` expects 1 argument(s), got 2"),
        "got: {err}"
    );
    assert!(
        err.contains("function `b::f` expects 2 argument(s), got 1"),
        "got: {err}"
    );
}

#[test]
fn same_type_name_in_two_modules_preserves_return_and_field_identity() {
    let err = compile_str(
        r#"
        mod a {
            pub struct Point { pub x: i64 }
            pub fn make() -> Point { Point { x: 1 } }
        }
        mod b {
            pub struct Point { pub y: bool }
            pub fn make() -> Point { Point { y: true } }
        }
        fn main() {
            let p = a::make();
            if p.x {}
            let q = b::make();
            let _ = q.y + 1;
        }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("`if` condition must be `bool`, found `i64`"),
        "got: {err}"
    );
    assert!(
        err.contains("arithmetic operator applied to `bool`"),
        "got: {err}"
    );
}

#[test]
fn same_type_name_in_two_modules_checks_each_resolved_method() {
    let err = compile_str(
        r#"
        mod a {
            pub struct Value { pub n: i64 }
            impl Value { pub fn add(&self, x: i64) -> i64 { self.n + x } }
        }
        mod b {
            pub struct Value { pub n: i64 }
            impl Value { pub fn add(&self, x: i64, y: i64) -> i64 { self.n + x + y } }
        }
        fn main() {
            let a_value = a::Value { n: 1 };
            let b_value = b::Value { n: 2 };
            let _ = a_value.add(1, 2);
            let _ = b_value.add(1);
        }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("method `Value::add` expects 1 argument(s), got 2"),
        "got: {err}"
    );
    assert!(
        err.contains("method `Value::add` expects 2 argument(s), got 1"),
        "got: {err}"
    );
}

#[test]
fn qualified_trait_bounds_use_trait_and_type_identity() {
    let source = r#"
        mod left {
            pub trait Gate {}
            pub struct Token {}
            impl Gate for Token {}
        }
        mod right {
            pub trait Gate {}
            pub struct Token {}
            impl Gate for Token {}
        }
        fn require_left<T: left::Gate>(value: T) -> T { value }
        let _ok = require_left(left::Token {});
    "#;
    compile_str(source).unwrap();

    let failing = format!("{}\nlet _bad = require_left(right::Token {{}});", source);
    let error = compile_str(&failing).unwrap_err();
    assert!(
        error.contains("does not implement trait `Gate`"),
        "got: {error}"
    );
}

#[test]
fn generic_receiver_method_uses_qualified_trait_identity() {
    let lua = compile(
        r#"
        mod left {
            pub trait Value { fn value(&self) -> i64; }
            pub struct Number { value: i64 }
            impl Value for Number {
                fn value(&self) -> i64 { self.value }
            }
        }
        mod right {
            pub trait Value { fn value(&self, fallback: bool) -> bool; }
        }
        fn read<T: left::Value>(item: T) -> i64 { item.value() }
        let answer = read(left::Number { value: 42 });
        "#,
    );
    assert!(lua.contains("read(setmetatable("), "{lua}");
    assert!(!lua.contains("local answer"), "{lua}");
}

#[test]
fn associated_function_uses_resolved_method_identity() {
    let err = compile_str(
        r#"
        mod a {
            pub struct Value { pub n: i64 }
            impl Value { pub fn make(n: i64) -> Value { Value { n: n } } }
        }
        mod b {
            pub struct Value { pub n: i64 }
            impl Value { pub fn make(n: i64, extra: i64) -> Value { Value { n: n + extra } } }
        }
        fn main() {
            let _ = a::Value::make(1, 2);
            let _ = b::Value::make(1);
        }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("function `a::Value::make` expects 1 argument(s), got 2"),
        "got: {err}"
    );
    assert!(
        err.contains("function `b::Value::make` expects 2 argument(s), got 1"),
        "got: {err}"
    );
}

#[test]
fn generic_return_type_substituted_at_call_site() {
    // `id(1)` returns `i64`, so binding it to a `bool` must be a mismatch.
    let err = compile_str(
        r#"
        fn id<T>(x: T) -> T { x }
        fn main() { let _y: bool = id(1); }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("annotated as `bool` but initialized with `i64`"),
        "got: {err}"
    );
}

// --- P4c-7: `.ruai` declaration files + qualified function-call checking ---

#[test]
fn qualified_fn_call_arity_checked() {
    // A module-qualified call is now checked against the callee's signature.
    let err = compile_str(
        r#"
        mod m { pub fn add(a: i64, b: i64) -> i64 { a + b } }
        fn main() { let _x = m::add(1); }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("function `m::add` expects 2 argument"),
        "got: {err}"
    );
}

#[test]
fn qualified_extern_call_arity_checked() {
    let err = compile_str(
        r#"
        mod host { extern "lua" { fn log(msg: String); } }
        fn main() { host::log("a", "b"); }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("function `host::log` expects 1 argument"),
        "got: {err}"
    );
}

#[test]
fn qualified_extern_arg_type_checked() {
    let err = compile_str(
        r#"
        mod host { extern "lua" { fn log(msg: String); } }
        fn main() { host::log(42); }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("argument 1 of `host::log` expects `String`"),
        "got: {err}"
    );
}

#[test]
fn qualified_extern_ok_and_return_type_flows() {
    // Correct calls compile; the declared `-> i64` return type flows into `let`.
    let lua = compile(
        r#"
        mod host { extern "lua" { fn log(msg: String); fn time() -> i64; } }
        fn main() { host::log("hi"); let _t: i64 = host::time(); }
    "#,
    );
    assert!(lua.contains("host.log(\"hi\")"), "got: {lua}");
    // A return-type mismatch on the same signature must be rejected.
    let err = compile_str(
        r#"
        mod host { extern "lua" { fn time() -> i64; } }
        fn main() { let _t: bool = host::time(); }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("annotated as `bool` but initialized with `i64`"),
        "got: {err}"
    );
}

#[test]
fn variadic_extern_not_arity_checked() {
    // Variadic extern fns are left unchecked (any arity accepted).
    let lua = compile(
        r#"
        mod host { extern "lua" { fn printf(fmt: String, ...); } }
        fn main() { host::printf("%d %d", 1, 2); }
    "#,
    );
    assert!(lua.contains("host.printf(\"%d %d\", 1, 2)"), "got: {lua}");
}

#[test]
fn ruai_module_omitted_from_codegen() {
    // A `.ruai` file is a declaration-only module: the checker knows its
    // signatures, but codegen emits no Lua and references hit the host global.
    let dir = tmp_dir("ruai_codegen");
    std::fs::write(
        dir.join("main.rua"),
        "mod moon;\nfn main() { moon::log(\"hi\"); }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("moon.ruai"),
        "extern \"lua\" { fn log(msg: String); }\n",
    )
    .unwrap();
    let lua = crate::compile_path(&dir.join("main.rua")).unwrap();
    assert!(lua.contains("moon.log(\"hi\")"), "got: {lua}");
    // The module must NOT be declared as a local nor defined as a table.
    assert!(!lua.contains("moon = {}"), "got: {lua}");
    assert!(!lua.contains("local main, moon"), "got: {lua}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ruai_root_is_declaration_only_and_validates_loaded_descendants() {
    let dir = tmp_dir("ruai_root_declaration");
    let root = dir.join("api.ruai");
    std::fs::write(&root, "pub fn answer() -> i64 {}\n").unwrap();
    let artifact = crate::compile_path_artifact(&root).expect("compile declaration root");
    assert!(artifact.source_map.is_empty(), "{artifact:#?}");
    assert!(
        !artifact.source.contains("function answer"),
        "{}",
        artifact.source
    );

    std::fs::write(&root, "pub fn answer() -> i64 { 42 }\n").unwrap();
    let failure = crate::compile_path_artifact(&root).unwrap_err();
    assert_eq!(
        failure.diagnostics[0].code,
        rua_core::DiagnosticCode::NameInvalidDeclaration
    );

    std::fs::write(&root, "mod child;\n").unwrap();
    std::fs::write(dir.join("child.rua"), "pub fn answer() -> i64 { 42 }\n").unwrap();
    let failure = crate::compile_path_artifact(&root).unwrap_err();
    assert_eq!(
        failure.diagnostics[0].code,
        rua_core::DiagnosticCode::NameInvalidDeclaration
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn ruai_declaration_type_checked() {
    // Calls against a `.ruai` declaration are arity/type checked.
    let dir = tmp_dir("ruai_check");
    std::fs::write(
        dir.join("main.rua"),
        "mod moon;\nfn main() { moon::log(1, 2); }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("moon.ruai"),
        "extern \"lua\" { fn log(msg: String); }\n",
    )
    .unwrap();
    let err = crate::compile_path(&dir.join("main.rua")).unwrap_err();
    assert!(
        err.contains("function `moon::log` expects 1 argument"),
        "got: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// --- P5c-7: trait-method-level generics ---

#[test]
fn trait_method_generic_bound_satisfied_ok() {
    // `store<U: Show>` called with a type that DOES implement `Show`.
    let lua = compile(
        r#"
        trait Show { fn show(&self) -> String; }
        trait Container { fn store<U: Show>(&self, x: U); }
        struct Plain {}
        impl Show for Plain { fn show(&self) -> String { "p" } }
        fn use_it<T: Container>(c: T, p: Plain) { c.store(p); }
        fn main() {}
    "#,
    );
    assert!(lua.contains("function use_it(c, p)"), "got: {lua}");
}

#[test]
fn trait_method_generic_bound_unsatisfied_rejected() {
    // `store<U: Show>` called with `Plain`, which does NOT implement `Show`.
    let err = compile_str(
        r#"
        trait Show { fn show(&self) -> String; }
        trait Container { fn store<U: Show>(&self, x: U); }
        struct Plain {}
        fn use_it<T: Container>(c: T, p: Plain) { c.store(p); }
        fn main() {}
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("type `Plain` does not implement trait `Show`")
            && err.contains("Container::store"),
        "got: {err}"
    );
}

#[test]
fn trait_method_generic_return_substituted() {
    // `wrap<U>(x: U) -> U` — passing `1` yields `i64`, so a `bool` binding fails.
    let err = compile_str(
        r#"
        trait Wrapper { fn wrap<U>(&self, x: U) -> U; }
        fn use_it<T: Wrapper>(w: T) { let _y: bool = w.wrap(1); }
        fn main() {}
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("annotated as `bool` but initialized with `i64`"),
        "got: {err}"
    );
}

#[test]
fn trait_method_generic_builtin_bound_not_checked() {
    // A builtin bound (`Clone`) on a trait-method generic is never verified,
    // so no false positive even though `Plain` has no explicit impl.
    let lua = compile(
        r#"
        trait Container { fn store<U: Clone>(&self, x: U); }
        struct Plain {}
        fn use_it<T: Container>(c: T, p: Plain) { c.store(p); }
        fn main() {}
    "#,
    );
    assert!(lua.contains("function use_it(c, p)"), "got: {lua}");
}

#[test]
fn trait_method_generic_unknown_bound_rejected() {
    // An unknown trait in a trait-method generic bound is now caught (previously
    // the method generics were dropped at parse time).
    let err = compile_str(
        r#"
        trait Foo { fn bar<U: Bogus>(&self, x: U); }
        fn main() {}
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("unknown trait `Bogus` in bound `U: Bogus`"),
        "got: {err}"
    );
}

// --- check_diags: byte-offset spans for LSP -------------------------------

#[test]
fn check_diags_reports_located_span_for_struct_field() {
    // A struct literal with an unknown field has an expression span, so the
    // diagnostic must carry a precise byte range that points at the source.
    let src = "struct P { x: i64 }\nfn main() { let _ = P { y: 1 }; }";
    let (diags, _files) = crate::check_diags(src);
    let d = diags
        .iter()
        .find(|d| d.msg.contains("has no field `y`"))
        .expect("expected a `has no field` diagnostic");
    assert!(
        !d.is_empty(),
        "located diag should carry a non-empty span: {d:?}"
    );
    // The span must fall inside the source and cover part of the literal.
    assert!(
        d.start() + d.len() <= src.len(),
        "span out of bounds: {d:?}"
    );
    assert!(d.line >= 1, "line should be 1-based: {d:?}");
    // The byte range should intersect the struct-literal text on line 2.
    let snippet = &src[d.start()..d.start() + d.len()];
    assert!(
        snippet.contains('P') || snippet.contains('y') || snippet.contains('{'),
        "span {snippet:?} should point at the struct literal"
    );
}

#[test]
fn check_diags_bare_for_duplicate_definition() {
    // Duplicate top-level definitions have no AST span, so the diagnostic is
    // bare: start == len == 0 (rendered whole-file / at file top by the LSP).
    let src = "fn dup() {}\nfn dup() {}\nfn main() {}";
    let (diags, _files) = crate::check_diags(src);
    let d = diags
        .iter()
        .find(|d| d.msg.contains("duplicate top-level definition `dup`"))
        .expect("expected a duplicate-definition diagnostic");
    assert_eq!(d.start(), 0, "bare diag has no start: {d:?}");
    assert_eq!(d.len(), 0, "bare diag has no length: {d:?}");
    assert_eq!(d.code, rua_core::DiagnosticCode::NameDuplicateDefinition);
}

#[test]
fn compiler_diagnostic_api_preserves_parse_name_and_type_codes() {
    let (parse, _) = crate::check_diagnostics("fn broken( {");
    assert!(
        parse
            .iter()
            .any(|diagnostic| diagnostic.code.category() == rua_core::DiagnosticCategory::Parse)
    );

    let (name, _) = crate::check_diagnostics("fn run() { missing(); }");
    assert!(
        name.iter()
            .any(|diagnostic| diagnostic.code == rua_core::DiagnosticCode::NameUnresolved)
    );

    let (ty, _) = crate::check_diagnostics("fn run() -> i64 { true }");
    assert!(
        ty.iter()
            .any(|diagnostic| diagnostic.code == rua_core::DiagnosticCode::TypeMismatch)
    );
}

#[test]
fn check_diags_locates_match_pattern_error() {
    // Pattern diagnostics carry the enclosing match arm's span as a fallback,
    // so they no longer degrade to a bare (file-top) diagnostic.
    let src = "enum E { A, B(i64) }\nfn main() { let e = E::A; match e { A(x) => x, _ => 0 }; }";
    let (diags, _files) = crate::check_diags(src);
    let d = diags
        .iter()
        .find(|d| d.msg.contains("has no tuple payload"))
        .expect("expected a unit-variant payload diagnostic");
    assert!(!d.is_empty(), "pattern diag should now be located: {d:?}");
    assert!(
        d.start() + d.len() <= src.len(),
        "span out of bounds: {d:?}"
    );
}

#[test]
fn check_diags_clean_program_has_no_diags() {
    let src = "fn add(a: i64, b: i64) -> i64 { a + b }\nfn main() { let _ = add(1, 2); }";
    let (diags, _files) = crate::check_diags(src);
    assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
}

// --- B1: member_index (LSP member resolution) --------------------------------

#[test]
fn hashmap_insert_value_type_mismatch_is_reported() {
    // scores inferred as HashMap<String, i64>; inserting a String value errors.
    let src = "fn main() {\n    let mut scores = HashMap::new();\n    scores.insert(\"alice\", 10);\n    scores.insert(\"bob\", \"world\");\n}";
    let (diags, _) = crate::check_diags(src);
    assert!(
        diags
            .iter()
            .any(|d| d.msg.contains("HashMap value type mismatch")
                && d.msg.contains("expected `i64`")
                && d.msg.contains("found `String`")),
        "expected a HashMap value mismatch, got {:?}",
        diags.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
}

#[test]
fn hashmap_insert_matching_types_ok() {
    let src = "fn main() {\n    let mut scores = HashMap::new();\n    scores.insert(\"alice\", 10);\n    scores.insert(\"bob\", 20);\n}";
    let (diags, _) = crate::check_diags(src);
    assert!(
        diags.iter().all(|d| !d.msg.contains("mismatch")),
        "matching inserts must not error, got {:?}",
        diags.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
}

#[test]
fn hashmap_key_type_mismatch_is_reported() {
    let src = "fn main() {\n    let mut m = HashMap::new();\n    m.insert(\"a\", 1);\n    m.insert(2, 3);\n}";
    let (diags, _) = crate::check_diags(src);
    assert!(
        diags
            .iter()
            .any(|d| d.msg.contains("HashMap key type mismatch")),
        "expected a HashMap key mismatch, got {:?}",
        diags.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
}

#[test]
fn vec_push_type_mismatch_is_reported() {
    let src =
        "fn main() {\n    let mut xs = Vec::new();\n    xs.push(1);\n    xs.push(\"nope\");\n}";
    let (diags, _) = crate::check_diags(src);
    assert!(
        diags
            .iter()
            .any(|d| d.msg.contains("Vec element type mismatch")
                && d.msg.contains("expected `i64`")
                && d.msg.contains("found `String`")),
        "expected a Vec element mismatch, got {:?}",
        diags.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
}

#[test]
fn uninferred_hashmap_does_not_flag_inserts() {
    // No first insert to fix the value type: stays HashMap<String, ?>, so mixed
    // value types are not flagged (no false positives on `?`).
    let src = "fn f(m: HashMap<String, i64>) {}\nfn main() {\n    let mut m = HashMap::new();\n    m.insert(\"a\", true);\n}";
    // The single insert fixes value = bool; only later conflicting inserts would
    // error. A lone insert never mismatches itself.
    let (diags, _) = crate::check_diags(src);
    assert!(
        diags.iter().all(|d| !d.msg.contains("mismatch")),
        "a single insert cannot mismatch itself, got {:?}",
        diags.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
}
