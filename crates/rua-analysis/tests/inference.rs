use std::{fs, path::Path, sync::Arc};

use rua_analysis::{
    Analysis, AnalysisHost, BindingId, Body, BodySourceMap, CallTarget, Change, DefId, DefKind,
    Expr, ExprId, FileId, FileKind, InferenceDiagnostic, InferenceResult, PatId, PrimitiveTy,
    SourceRootId, SourceRootKind, Ty,
};

struct Fixture {
    source: &'static str,
    file_id: FileId,
    body: Arc<Body>,
    source_map: Arc<BodySourceMap>,
    inference: Arc<InferenceResult>,
}

fn single_file_host(source: &str) -> (AnalysisHost, FileId) {
    let root_id = SourceRootId::new(0);
    let file_id = FileId::new(0);
    let mut change = Change::new();
    change.set_source_root(root_id, SourceRootKind::Workspace);
    change.set_file_with_path(file_id, root_id, FileKind::Source, "main.rua", source);
    let mut host = AnalysisHost::new();
    host.apply_change(change);
    (host, file_id)
}

fn body_owner(analysis: &Analysis, root_file: FileId, name: &str, kind: DefKind) -> DefId {
    analysis
        .def_map(root_file)
        .definitions()
        .find(|definition| definition.name() == name && definition.kind() == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} definition `{name}`"))
        .id()
}

fn fixture(source: &'static str, owner_name: &str) -> Fixture {
    let (host, file_id) = single_file_host(source);
    let analysis = host.analysis();
    assert!(
        analysis.parse(file_id).errors().is_empty(),
        "fixture must parse: {:?}",
        analysis.parse(file_id).errors()
    );
    let owner = body_owner(&analysis, file_id, owner_name, DefKind::Function);
    Fixture {
        source,
        file_id,
        body: analysis.body(owner).expect("fixture body"),
        source_map: analysis
            .body_source_map(owner)
            .expect("fixture body source map"),
        inference: analysis.infer(owner).expect("fixture inference result"),
    }
}

fn marker_offset(source: &str, marker: &str) -> u32 {
    u32::try_from(source.find(marker).expect("fixture marker") + marker.len())
        .expect("fixture offset fits u32")
}

fn binding_at(fixture: &Fixture, marker: &str) -> BindingId {
    let offset = marker_offset(fixture.source, marker);
    fixture
        .body
        .bindings()
        .find_map(|(id, _)| {
            fixture
                .source_map
                .binding_range(id)
                .filter(|range| range.file_id == fixture.file_id && range.range.start() == offset)
                .map(|_| id)
        })
        .unwrap_or_else(|| panic!("no binding at {marker}"))
}

fn expr_at<F>(fixture: &Fixture, marker: &str, predicate: F) -> ExprId
where
    F: Fn(&Expr) -> bool,
{
    let offset = marker_offset(fixture.source, marker);
    fixture
        .body
        .exprs()
        .filter_map(|(id, expr)| {
            let range = fixture.source_map.expr_range(id)?;
            (predicate(expr) && range.file_id == fixture.file_id && range.range.start() == offset)
                .then_some((range.range.len(), id))
        })
        .max_by_key(|(length, _)| *length)
        .map(|(_, id)| id)
        .unwrap_or_else(|| panic!("no matching expression at {marker}"))
}

fn pattern_at(fixture: &Fixture, marker: &str) -> PatId {
    let offset = marker_offset(fixture.source, marker);
    fixture
        .body
        .patterns()
        .find_map(|(id, _)| {
            fixture
                .source_map
                .pat_range(id)
                .filter(|range| range.file_id == fixture.file_id && range.range.start() == offset)
                .map(|_| id)
        })
        .unwrap_or_else(|| panic!("no pattern at {marker}"))
}

fn primitive(kind: PrimitiveTy) -> Ty {
    Ty::Primitive(kind)
}

fn assert_expr_ty(fixture: &Fixture, marker: &str, expected: &Ty) -> ExprId {
    let expr = expr_at(fixture, marker, |_| true);
    assert_eq!(fixture.inference.type_of_expr(expr), Some(expected));
    expr
}

fn assert_binding_ty(fixture: &Fixture, marker: &str, expected: &Ty) -> BindingId {
    let binding = binding_at(fixture, marker);
    assert_eq!(fixture.inference.type_of_binding(binding), Some(expected));
    binding
}

const PRIMITIVES: &str = r#"
fn primitives() -> bool {
    let /*int_def*/integer = /*int_expr*/42;
    let /*float_def*/floating = /*float_expr*/3.5;
    let /*text_def*/text = /*text_expr*/"rua";
    let /*truth_def*/truth = /*truth_expr*/true;
    let /*neg_def*/negative = /*neg_expr*/-integer;
    let /*not_def*/inverted = /*not_expr*/!truth;
    let /*sum_def*/sum = /*sum_expr*/integer + 1;
    let /*cmp_def*/compared = /*cmp_expr*/sum < 100;
    let /*eq_def*/equal = /*eq_expr*/text == "rua";
    /*tail_expr*/compared && equal && inverted == false
}
"#;

#[test]
fn inference_primitives_assigns_expr_and_binding_types_without_false_positives() {
    let fixture = fixture(PRIMITIVES, "primitives");
    let i64_ty = primitive(PrimitiveTy::I64);
    let f64_ty = primitive(PrimitiveTy::F64);
    let string_ty = primitive(PrimitiveTy::String);
    let bool_ty = primitive(PrimitiveTy::Bool);

    for (expr, binding, ty) in [
        ("/*int_expr*/", "/*int_def*/", &i64_ty),
        ("/*float_expr*/", "/*float_def*/", &f64_ty),
        ("/*text_expr*/", "/*text_def*/", &string_ty),
        ("/*truth_expr*/", "/*truth_def*/", &bool_ty),
        ("/*neg_expr*/", "/*neg_def*/", &i64_ty),
        ("/*not_expr*/", "/*not_def*/", &bool_ty),
        ("/*sum_expr*/", "/*sum_def*/", &i64_ty),
        ("/*cmp_expr*/", "/*cmp_def*/", &bool_ty),
        ("/*eq_expr*/", "/*eq_def*/", &bool_ty),
    ] {
        assert_expr_ty(&fixture, expr, ty);
        assert_binding_ty(&fixture, binding, ty);
    }
    assert_expr_ty(&fixture, "/*tail_expr*/", &bool_ty);
    assert_eq!(
        fixture.inference.type_of_expr(fixture.body.root_expr()),
        Some(&bool_ty)
    );
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn inference_primitives_reports_core_mismatches() {
    const SOURCE: &str = r#"
fn invalid_primitives() -> i64 {
    let invalid_not = /*bad_not*/!1;
    let invalid_add = /*bad_add*/true + 1;
    let annotated: bool = /*bad_annotation*/1;
    /*bad_if*/if 1 { 0 } else { 1 }
}
"#;
    let fixture = fixture(SOURCE, "invalid_primitives");
    assert!(
        fixture
            .inference
            .diagnostics()
            .iter()
            .any(|diagnostic| matches!(diagnostic, InferenceDiagnostic::InvalidUnary { .. }))
    );
    assert!(
        fixture
            .inference
            .diagnostics()
            .iter()
            .any(|diagnostic| matches!(diagnostic, InferenceDiagnostic::InvalidBinary { .. }))
    );
    assert!(
        fixture
            .inference
            .diagnostics()
            .iter()
            .any(|diagnostic| matches!(diagnostic, InferenceDiagnostic::TypeMismatch { .. }))
    );
    assert!(
        fixture
            .inference
            .diagnostics()
            .iter()
            .any(|diagnostic| matches!(diagnostic, InferenceDiagnostic::ExpectedBool { .. }))
    );
}

#[test]
fn inference_control_flow_types_blocks_loops_for_and_patterns() {
    const SOURCE: &str = r#"
fn control(/*limit_def*/limit: i64) -> i64 {
    let mut /*total_def*/total: i64 = 0;
    let /*selected_def*/selected = /*if_expr*/if /*if_condition*/limit > 0 {
        /*then_expr*/limit
    } else {
        /*else_expr*/0
    };
    while /*while_condition*/total < selected {
        total = total + 1;
    }
    for /*item_def*/item in /*range_expr*/0..limit {
        total = total + item;
    }
    let /*matched_def*/matched = /*match_expr*/match selected {
        /*pattern_def*/value => /*pattern_use*/value,
    };
    /*control_tail*/total + matched
}
"#;
    let fixture = fixture(SOURCE, "control");
    let i64_ty = primitive(PrimitiveTy::I64);
    let bool_ty = primitive(PrimitiveTy::Bool);
    for marker in [
        "/*limit_def*/",
        "/*total_def*/",
        "/*selected_def*/",
        "/*item_def*/",
        "/*matched_def*/",
    ] {
        assert_binding_ty(&fixture, marker, &i64_ty);
    }
    for marker in [
        "/*if_expr*/",
        "/*then_expr*/",
        "/*else_expr*/",
        "/*match_expr*/",
        "/*control_tail*/",
    ] {
        assert_expr_ty(&fixture, marker, &i64_ty);
    }
    for marker in ["/*if_condition*/", "/*while_condition*/"] {
        assert_expr_ty(&fixture, marker, &bool_ty);
    }
    let pattern = pattern_at(&fixture, "/*pattern_def*/");
    assert_eq!(fixture.inference.type_of_pattern(pattern), Some(&i64_ty));
    let pattern_binding = binding_at(&fixture, "/*pattern_def*/");
    assert_eq!(
        fixture.inference.type_of_binding(pattern_binding),
        Some(&i64_ty)
    );
    assert_eq!(
        fixture.inference.type_of_expr(fixture.body.root_expr()),
        Some(&i64_ty)
    );
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn inference_control_flow_reports_non_bool_and_non_iterable_inputs() {
    const SOURCE: &str = r#"
fn invalid_control() {
    if 1 {}
    while "yes" {}
    for item in true { item; }
}
"#;
    let fixture = fixture(SOURCE, "invalid_control");
    assert!(
        fixture
            .inference
            .diagnostics()
            .iter()
            .filter(|diagnostic| matches!(diagnostic, InferenceDiagnostic::ExpectedBool { .. }))
            .count()
            >= 2
    );
    assert!(
        fixture
            .inference
            .diagnostics()
            .iter()
            .any(|diagnostic| matches!(diagnostic, InferenceDiagnostic::NotIterable { .. }))
    );
}

#[test]
fn inference_control_flow_explicit_returns_do_not_report_tail_mismatches() {
    const SOURCE: &str = r#"
fn early() -> i64 { return 1; }

fn closure_early() -> i64 {
    let produce = /*closure_expr*/|| -> i64 { return 2; };
    /*closure_call*/produce()
}
"#;
    let (host, file_id) = single_file_host(SOURCE);
    let analysis = host.analysis();
    assert!(analysis.parse(file_id).errors().is_empty());

    let early = body_owner(&analysis, file_id, "early", DefKind::Function);
    let early_inference = analysis.infer(early).expect("early inference");
    assert!(
        early_inference.diagnostics().is_empty(),
        "an explicit function return must not leave a false tail mismatch: {:?}",
        early_inference.diagnostics()
    );

    let fixture = fixture(SOURCE, "closure_early");
    let closure = expr_at(&fixture, "/*closure_expr*/", |expr| {
        matches!(expr, Expr::Closure { .. })
    });
    assert!(matches!(
        fixture.inference.type_of_expr(closure),
        Some(Ty::Closure(_))
    ));
    assert_expr_ty(&fixture, "/*closure_call*/", &primitive(PrimitiveTy::I64));
    assert!(
        fixture.inference.diagnostics().is_empty(),
        "an explicit closure return must not leave a false tail mismatch: {:?}",
        fixture.inference.diagnostics()
    );
}

#[test]
fn inference_control_flow_propagates_never_without_unreachable_noise() {
    const SOURCE: &str = r#"
fn dead_tail() -> i64 { return 1; "unreachable" }
fn forever() -> i64 { loop {} }
fn broken_loop() -> i64 { loop { break; } 1 }
fn never_binary() -> i64 { panic!("stop") + 1 }
fn inferred_closure() -> i64 {
    let produce = /*inferred_closure*/|| { return 3; };
    /*inferred_call*/produce()
}
"#;
    let (host, file_id) = single_file_host(SOURCE);
    let analysis = host.analysis();
    for name in ["dead_tail", "forever", "broken_loop", "never_binary"] {
        let owner = body_owner(&analysis, file_id, name, DefKind::Function);
        let inference = analysis.infer(owner).expect("flow inference");
        assert!(
            inference.diagnostics().is_empty(),
            "{name} produced unreachable noise: {:?}",
            inference.diagnostics()
        );
    }

    let fixture = fixture(SOURCE, "inferred_closure");
    let closure = expr_at(&fixture, "/*inferred_closure*/", |expr| {
        matches!(expr, Expr::Closure { .. })
    });
    let Some(Ty::Closure(callable)) = fixture.inference.type_of_expr(closure) else {
        panic!("expected inferred closure type")
    };
    assert_eq!(callable.return_ty(), &Ty::I64);
    assert_expr_ty(&fixture, "/*inferred_call*/", &Ty::I64);
    assert!(fixture.inference.diagnostics().is_empty());
}

const CALLS: &str = r#"
fn add(left: i64, right: i64) -> i64 { left + right }
fn positive(value: i64) -> bool { value > 0 }

fn calls(/*seed_def*/seed: i64) -> bool {
    let /*sum_def*/sum = /*add_call*/add(seed, 1);
    let predicate = /*closure_expr*/|value: i64| -> bool { value > 0 };
    let /*closure_result_def*/closure_result = /*closure_call*/predicate(sum);
    /*positive_call*/positive(sum) && closure_result
}
"#;

#[test]
fn inference_calls_records_definition_and_closure_call_types() {
    let fixture = fixture(CALLS, "calls");
    let i64_ty = primitive(PrimitiveTy::I64);
    let bool_ty = primitive(PrimitiveTy::Bool);
    let (host, file_id) = single_file_host(CALLS);
    let analysis = host.analysis();
    let add = body_owner(&analysis, file_id, "add", DefKind::Function);
    let positive = body_owner(&analysis, file_id, "positive", DefKind::Function);

    let add_call = expr_at(&fixture, "/*add_call*/", |expr| {
        matches!(expr, Expr::Call { .. })
    });
    let add_info = fixture
        .inference
        .call_info(add_call)
        .expect("add call info");
    assert_eq!(add_info.target(), CallTarget::Definition(add));
    assert_eq!(add_info.parameters(), &[i64_ty.clone(), i64_ty.clone()]);
    assert_eq!(add_info.return_type(), &i64_ty);
    assert!(add_info.substitution().iter().next().is_none());
    assert_eq!(fixture.inference.type_of_expr(add_call), Some(&i64_ty));
    assert_binding_ty(&fixture, "/*sum_def*/", &i64_ty);

    let closure = expr_at(&fixture, "/*closure_expr*/", |expr| {
        matches!(expr, Expr::Closure { .. })
    });
    let closure_call = expr_at(&fixture, "/*closure_call*/", |expr| {
        matches!(expr, Expr::Call { .. })
    });
    let closure_info = fixture
        .inference
        .call_info(closure_call)
        .expect("closure call info");
    assert_eq!(closure_info.target(), CallTarget::Closure(closure));
    assert_eq!(closure_info.parameters(), std::slice::from_ref(&i64_ty));
    assert_eq!(closure_info.return_type(), &bool_ty);
    assert_eq!(fixture.inference.type_of_expr(closure_call), Some(&bool_ty));
    assert_binding_ty(&fixture, "/*closure_result_def*/", &bool_ty);

    let positive_call = expr_at(&fixture, "/*positive_call*/", |expr| {
        matches!(expr, Expr::Call { .. })
    });
    assert_eq!(
        fixture
            .inference
            .call_info(positive_call)
            .expect("positive call info")
            .target(),
        CallTarget::Definition(positive)
    );
    assert_eq!(
        fixture.inference.type_of_expr(positive_call),
        Some(&bool_ty)
    );
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn inference_calls_reports_arity_type_and_not_callable_errors() {
    const SOURCE: &str = r#"
fn expects(value: i64) -> bool { value > 0 }

fn invalid_calls() -> bool {
    let arity = /*arity_call*/expects();
    let wrong = /*wrong_call*/expects("text");
    let unknown = /*unknown_call*/mystery(1);
    let not_callable = /*not_callable*/(1)(2);
    unknown;
    false
}
"#;
    let fixture = fixture(SOURCE, "invalid_calls");
    assert!(
        fixture
            .inference
            .diagnostics()
            .iter()
            .any(|diagnostic| matches!(diagnostic, InferenceDiagnostic::ArgumentCount { .. }))
    );
    assert!(
        fixture
            .inference
            .diagnostics()
            .iter()
            .any(|diagnostic| matches!(diagnostic, InferenceDiagnostic::TypeMismatch { .. }))
    );
    assert!(
        fixture
            .inference
            .diagnostics()
            .iter()
            .any(|diagnostic| matches!(diagnostic, InferenceDiagnostic::NotCallable { .. }))
    );

    let unknown_call = expr_at(&fixture, "/*unknown_call*/", |expr| {
        matches!(expr, Expr::Call { .. })
    });
    assert_eq!(
        fixture.inference.type_of_expr(unknown_call),
        Some(&Ty::Unknown)
    );
    assert_eq!(
        fixture
            .inference
            .call_info(unknown_call)
            .expect("unresolved call info")
            .target(),
        CallTarget::Unresolved
    );
}

#[test]
fn inference_calls_unknown_types_suppress_secondary_noise() {
    const SOURCE: &str = r#"
fn unknown_suppression() -> bool {
    let /*unknown_def*/unknown = /*unknown_call*/external_value();
    let annotated: bool = unknown;
    if unknown { annotated } else { unknown }
}
"#;
    let fixture = fixture(SOURCE, "unknown_suppression");
    let call = expr_at(&fixture, "/*unknown_call*/", |expr| {
        matches!(expr, Expr::Call { .. })
    });
    assert_eq!(fixture.inference.type_of_expr(call), Some(&Ty::Unknown));
    assert_binding_ty(&fixture, "/*unknown_def*/", &Ty::Unknown);
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn inference_calls_respects_local_shadowing_before_builtin_constructors() {
    const SOURCE: &str = r#"
fn Some(value: i64) -> i64 { value }
struct Vec {}
impl Vec { fn new() -> i64 { 1 } }

fn builtin_shadow() -> i64 {
    let Some = /*some_closure*/|value: i64| -> i64 { value };
    /*some_call*/Some(1)
}
fn global_shadow() -> i64 { /*global_call*/Some(1) }
fn associated_shadow() -> i64 { /*associated_call*/Vec::new() }
"#;
    let local_fixture = fixture(SOURCE, "builtin_shadow");
    let closure = expr_at(&local_fixture, "/*some_closure*/", |expr| {
        matches!(expr, Expr::Closure { .. })
    });
    let call = expr_at(&local_fixture, "/*some_call*/", |expr| {
        matches!(expr, Expr::Call { .. })
    });
    assert_eq!(
        local_fixture
            .inference
            .call_info(call)
            .map(|info| info.target()),
        Some(CallTarget::Closure(closure))
    );
    assert_eq!(local_fixture.inference.type_of_expr(call), Some(&Ty::I64));
    assert!(local_fixture.inference.diagnostics().is_empty());

    let global = fixture(SOURCE, "global_shadow");
    let global_call = expr_at(&global, "/*global_call*/", |expr| {
        matches!(expr, Expr::Call { .. })
    });
    assert!(matches!(
        global
            .inference
            .call_info(global_call)
            .map(|info| info.target()),
        Some(CallTarget::Definition(_))
    ));

    let associated = fixture(SOURCE, "associated_shadow");
    let associated_call = expr_at(&associated, "/*associated_call*/", |expr| {
        matches!(expr, Expr::Call { .. })
    });
    assert_eq!(
        associated
            .inference
            .call_info(associated_call)
            .map(|info| info.target()),
        Some(CallTarget::Unresolved)
    );
    assert_eq!(
        associated.inference.type_of_expr(associated_call),
        Some(&Ty::Unknown)
    );
    assert!(associated.inference.diagnostics().is_empty());
}

#[test]
fn inference_calls_reports_expected_branch_constructor_vec_and_pattern_mismatches() {
    const SOURCE: &str = r#"
fn bad_branch(flag: bool) -> i64 { if flag { "text" } else { 1 } }
fn bad_some() -> Option<i64> { Some("text") }
fn bad_vec() { let values: Vec<i64> = vec!["text"]; values; }
fn bad_pattern() -> i64 { match 1 { "text" => 0, _ => 1 } }
fn bad_arity() { Some(); }
fn unknown_arithmetic() { let value = external_value() + 1; value; }
"#;
    let (host, file_id) = single_file_host(SOURCE);
    let analysis = host.analysis();

    for name in ["bad_branch", "bad_some", "bad_vec"] {
        let owner = body_owner(&analysis, file_id, name, DefKind::Function);
        assert!(
            analysis
                .infer(owner)
                .expect("mismatch inference")
                .diagnostics()
                .iter()
                .any(|diagnostic| matches!(diagnostic, InferenceDiagnostic::TypeMismatch { .. })),
            "missing mismatch for {name}"
        );
    }

    let pattern_owner = body_owner(&analysis, file_id, "bad_pattern", DefKind::Function);
    assert!(
        analysis
            .infer(pattern_owner)
            .expect("pattern inference")
            .diagnostics()
            .iter()
            .any(|diagnostic| matches!(
                diagnostic,
                InferenceDiagnostic::TypeMismatch {
                    source: rua_analysis::InferenceSource::Pattern(_),
                    ..
                }
            ))
    );

    let arity_owner = body_owner(&analysis, file_id, "bad_arity", DefKind::Function);
    assert!(
        analysis
            .infer(arity_owner)
            .expect("arity inference")
            .diagnostics()
            .iter()
            .any(|diagnostic| matches!(diagnostic, InferenceDiagnostic::ArgumentCount { .. }))
    );

    let unknown_owner = body_owner(&analysis, file_id, "unknown_arithmetic", DefKind::Function);
    assert!(
        analysis
            .infer(unknown_owner)
            .expect("unknown arithmetic inference")
            .diagnostics()
            .is_empty()
    );
}

#[test]
fn inference_calls_instantiates_generic_identity_and_records_substitution() {
    const SOURCE: &str = r#"
fn identity<T>(value: T) -> T { value }
fn choose<T>(left: T, right: T) -> T { left }

fn generic_call() -> i64 {
    let /*value_def*/value = /*identity_call*/identity(7);
    let conflict = /*conflict_call*/choose(1, "text");
    conflict;
    value
}
"#;
    let fixture = fixture(SOURCE, "generic_call");
    let i64_ty = primitive(PrimitiveTy::I64);
    let call = expr_at(&fixture, "/*identity_call*/", |expr| {
        matches!(expr, Expr::Call { .. })
    });
    let info = fixture
        .inference
        .call_info(call)
        .expect("identity call info");
    let CallTarget::Definition(identity) = info.target() else {
        panic!("identity must resolve to a definition: {:?}", info.target());
    };

    assert_eq!(fixture.inference.type_of_expr(call), Some(&i64_ty));
    assert_binding_ty(&fixture, "/*value_def*/", &i64_ty);
    assert_eq!(info.parameters(), std::slice::from_ref(&i64_ty));
    assert_eq!(info.return_type(), &i64_ty);
    let substitutions = info.substitution().iter().collect::<Vec<_>>();
    assert_eq!(substitutions.len(), 1);
    assert_eq!(substitutions[0].0.owner(), identity);
    assert_eq!(substitutions[0].0.index(), 0);
    assert_eq!(substitutions[0].1, &i64_ty);

    let conflict_call = expr_at(&fixture, "/*conflict_call*/", |expr| {
        matches!(expr, Expr::Call { .. })
    });
    let conflict = fixture
        .inference
        .call_info(conflict_call)
        .expect("conflicting generic call info");
    assert_eq!(conflict.return_type(), &Ty::Unknown);
    assert!(
        conflict
            .substitution()
            .iter()
            .all(|(_, ty)| ty == &Ty::Unknown)
    );
    assert!(
        fixture
            .inference
            .diagnostics()
            .iter()
            .any(|diagnostic| matches!(diagnostic, InferenceDiagnostic::TypeMismatch { .. }))
    );
}

#[test]
fn inference_control_flow_types_containers_ranges_and_index_without_unknown_noise() {
    const SOURCE: &str = r#"
fn containers() -> i64 {
    let /*values_def*/values: Vec<i64> = /*vec_expr*/vec![1, 2, 3];
    let /*option_def*/optional: Option<i64> = /*some_call*/Some(/*unknown_call*/external_value());
    let /*none_def*/none: Option<i64> = /*none_expr*/None;
    let /*result_def*/result: Result<i64, String> = /*ok_call*/Ok(1);
    let /*map_def*/map: HashMap<String, i64> = /*map_call*/HashMap::new();
    let /*range_def*/range = /*range_expr*/0..3;
    optional;
    none;
    result;
    map;
    range;
    /*index_expr*/values[0]
}
"#;
    let fixture = fixture(SOURCE, "containers");
    let i64_ty = primitive(PrimitiveTy::I64);
    let string_ty = primitive(PrimitiveTy::String);
    let vec_ty = Ty::Vec(Box::new(i64_ty.clone()));
    let option_ty = Ty::Option(Box::new(i64_ty.clone()));
    let result_ty = Ty::Result(Box::new(i64_ty.clone()), Box::new(string_ty.clone()));
    let map_ty = Ty::HashMap(Box::new(string_ty), Box::new(i64_ty.clone()));
    let range_ty = Ty::Iterator(Box::new(i64_ty.clone()));

    assert_binding_ty(&fixture, "/*values_def*/", &vec_ty);
    assert_binding_ty(&fixture, "/*option_def*/", &option_ty);
    assert_binding_ty(&fixture, "/*none_def*/", &option_ty);
    assert_binding_ty(&fixture, "/*result_def*/", &result_ty);
    assert_binding_ty(&fixture, "/*map_def*/", &map_ty);
    assert_binding_ty(&fixture, "/*range_def*/", &range_ty);
    assert_expr_ty(&fixture, "/*vec_expr*/", &vec_ty);
    assert_expr_ty(&fixture, "/*some_call*/", &option_ty);
    assert_expr_ty(&fixture, "/*none_expr*/", &option_ty);
    assert_expr_ty(&fixture, "/*ok_call*/", &result_ty);
    assert_expr_ty(
        &fixture,
        "/*map_call*/",
        &Ty::HashMap(Box::new(Ty::Unknown), Box::new(Ty::Unknown)),
    );
    assert_expr_ty(&fixture, "/*range_expr*/", &range_ty);
    assert_expr_ty(&fixture, "/*index_expr*/", &i64_ty);
    assert_expr_ty(&fixture, "/*unknown_call*/", &Ty::Unknown);
    for marker in ["/*some_call*/", "/*ok_call*/", "/*map_call*/"] {
        let call = expr_at(&fixture, marker, |expr| matches!(expr, Expr::Call { .. }));
        assert_eq!(
            fixture
                .inference
                .call_info(call)
                .expect("builtin call info")
                .target(),
            CallTarget::Builtin
        );
    }
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn inference_calls_cache_reuses_hot_results_and_invalidates_on_signature_change() {
    const BEFORE: &str = concat!(
        "fn callee(value: i64) -> i64 { value }\n",
        "fn caller() -> i64 { /*call*/callee(1) }\n",
    );
    const AFTER: &str = concat!(
        "fn callee(value: String) -> bool { true }\n",
        "fn caller() -> i64 { /*call*/callee(1) }\n",
    );
    let (mut host, file_id) = single_file_host(BEFORE);
    let before = host.analysis();
    let caller = body_owner(&before, file_id, "caller", DefKind::Function);
    let callee = body_owner(&before, file_id, "callee", DefKind::Function);
    let first = before.infer(caller).expect("first inference");
    let hot = before.infer(caller).expect("hot inference");
    assert!(Arc::ptr_eq(&first, &hot));

    let mut change = Change::new();
    change.set_file_text(file_id, AFTER);
    host.apply_change(change);
    let after = host.analysis();
    assert_eq!(
        body_owner(&after, file_id, "caller", DefKind::Function),
        caller
    );
    assert_eq!(
        body_owner(&after, file_id, "callee", DefKind::Function),
        callee
    );
    let changed = after.infer(caller).expect("changed inference");
    assert!(!Arc::ptr_eq(&first, &changed));

    let body = after.body(caller).expect("caller body");
    let source_map = after.body_source_map(caller).expect("caller source map");
    let call_offset = marker_offset(AFTER, "/*call*/");
    let call = body
        .exprs()
        .find_map(|(id, expr)| {
            (matches!(expr, Expr::Call { .. })
                && source_map
                    .expr_range(id)
                    .is_some_and(|range| range.range.start() == call_offset))
            .then_some(id)
        })
        .expect("caller call expression");
    let call_info = changed.call_info(call).expect("changed call info");
    assert_eq!(call_info.target(), CallTarget::Definition(callee));
    assert_eq!(call_info.return_type(), &primitive(PrimitiveTy::Bool));
    assert!(
        changed
            .diagnostics()
            .iter()
            .any(|diagnostic| matches!(diagnostic, InferenceDiagnostic::TypeMismatch { .. }))
    );
}

#[test]
fn inference_calls_cache_invalidates_when_declaration_module_signature_changes() {
    const MAIN: &str = "mod dep;\nfn caller() -> i64 { /*call*/dep::callee(1) }\n";
    const BEFORE_DECLARATION: &str = "extern \"lua\" { pub fn callee(value: i64) -> i64; }\n";
    const AFTER_DECLARATION: &str = "extern \"lua\" { pub fn callee(value: String) -> bool; }\n";
    let root_id = SourceRootId::new(0);
    let main_file = FileId::new(0);
    let declaration_file = FileId::new(1);
    let mut load = Change::new();
    load.set_source_root(root_id, SourceRootKind::Workspace);
    load.set_file_with_path(main_file, root_id, FileKind::Source, "main.rua", MAIN);
    load.set_file_with_path(
        declaration_file,
        root_id,
        FileKind::Declaration,
        "dep.ruai",
        BEFORE_DECLARATION,
    );
    let mut host = AnalysisHost::new();
    host.apply_change(load);

    let before = host.analysis();
    assert!(before.parse(main_file).errors().is_empty());
    assert!(before.parse(declaration_file).errors().is_empty());
    let caller = body_owner(&before, main_file, "caller", DefKind::Function);
    let first = before.infer(caller).expect("first cross-file inference");
    let hot = before.infer(caller).expect("hot cross-file inference");
    assert!(Arc::ptr_eq(&first, &hot));
    let before_body = before.body(caller).expect("caller body");
    let before_map = before.body_source_map(caller).expect("caller source map");
    let call_offset = marker_offset(MAIN, "/*call*/");
    let call = before_body
        .exprs()
        .find_map(|(id, expr)| {
            (matches!(expr, Expr::Call { .. })
                && before_map
                    .expr_range(id)
                    .is_some_and(|range| range.range.start() == call_offset))
            .then_some(id)
        })
        .expect("cross-file call expression");
    let before_info = first.call_info(call).expect("initial call info");
    let target = before_info.target();
    assert!(matches!(target, CallTarget::Definition(_)));
    assert_eq!(before_info.return_type(), &primitive(PrimitiveTy::I64));
    assert!(first.diagnostics().is_empty());

    let mut signature_change = Change::new();
    signature_change.set_file_text(declaration_file, AFTER_DECLARATION);
    host.apply_change(signature_change);
    let after = host.analysis();
    assert_eq!(
        body_owner(&after, main_file, "caller", DefKind::Function),
        caller
    );
    let changed = after.infer(caller).expect("changed cross-file inference");
    assert!(!Arc::ptr_eq(&first, &changed));
    let changed_info = changed.call_info(call).expect("changed call info");
    assert_eq!(changed_info.target(), target);
    assert_eq!(
        changed_info.parameters(),
        std::slice::from_ref(&primitive(PrimitiveTy::String))
    );
    assert_eq!(changed_info.return_type(), &primitive(PrimitiveTy::Bool));
    assert!(
        changed
            .diagnostics()
            .iter()
            .any(|diagnostic| matches!(diagnostic, InferenceDiagnostic::TypeMismatch { .. }))
    );
}

#[test]
fn type_parity_supported_binding_types_match_compiler_oracle() {
    const SOURCE: &str = r#"
fn parity(flag: bool, count: i64) -> i64 {
    let integer = 1;
    let floating = 1.5;
    let text = "rua";
    let truth = flag;
    let selected = if truth { count } else { integer };
    selected
}
"#;
    let fixture = fixture(SOURCE, "parity");
    let compiler = ruac::binding_types(SOURCE);
    assert!(fixture.inference.diagnostics().is_empty());
    let (compiler_diagnostics, _) = ruac::check_diags(SOURCE);
    assert!(compiler_diagnostics.is_empty());

    for (binding, data) in fixture.body.bindings() {
        let Some(name) = data.name() else {
            continue;
        };
        let range = fixture
            .source_map
            .binding_range(binding)
            .expect("binding source range");
        let compiler_binding = compiler
            .at(0, range.range.start() as usize)
            .unwrap_or_else(|| panic!("compiler has no type for `{name}`"));
        let compiler_type = compiler_binding
            .display
            .rsplit_once(": ")
            .map(|(_, ty)| ty)
            .expect("compiler binding display contains a type");
        let native_type = fixture
            .inference
            .type_of_binding(binding)
            .unwrap_or_else(|| panic!("native inference has no type for `{name}`"));
        assert_eq!(native_type.to_string(), compiler_type, "binding `{name}`");
    }
}

#[test]
fn type_parity_compile_pass_corpus_has_no_native_false_positives() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for relative in [
        "tests/golden/compile-pass",
        "tests/golden/phase4a/compile-pass",
    ] {
        let mut paths = Vec::new();
        collect_rua_files(&workspace.join(relative), &mut paths);
        paths.sort();
        assert!(!paths.is_empty(), "missing corpus directory {relative}");
        for path in paths {
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            let (host, file_id) = single_file_host(&source);
            let analysis = host.analysis();
            assert!(
                analysis.parse(file_id).errors().is_empty(),
                "rowan parser rejected compile-pass fixture {}: {:?}",
                path.display(),
                analysis.parse(file_id).errors()
            );
            let map = analysis.def_map(file_id);
            for definition in map.definitions().filter(|definition| {
                definition.file_id() == file_id
                    && matches!(definition.kind(), DefKind::Function | DefKind::Method)
            }) {
                let Some(inference) = analysis.infer(definition.id()) else {
                    continue;
                };
                assert!(
                    inference.diagnostics().is_empty(),
                    "native false positive in {} for {}: {:?}",
                    path.display(),
                    definition.name(),
                    inference.diagnostics()
                );
            }
        }
    }
}

#[test]
fn type_parity_core_compile_fail_corpus_matches_diagnostic_categories() {
    #[derive(Clone, Copy)]
    enum Expected {
        Bool,
        Mismatch,
        Unary,
        Binary,
        Arity,
    }

    let cases = [
        ("type_if_condition_non_bool.rua", Expected::Bool),
        ("type_while_condition_non_bool.rua", Expected::Bool),
        ("type_let_annotation_mismatch.rua", Expected::Mismatch),
        ("type_return_mismatch.rua", Expected::Mismatch),
        ("type_unary_invalid.rua", Expected::Unary),
        ("type_binary_invalid.rua", Expected::Binary),
        ("call_wrong_arity_fn.rua", Expected::Arity),
        ("call_wrong_type_fn.rua", Expected::Mismatch),
    ];
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden/compile-fail");
    for (file_name, expected) in cases {
        let path = root.join(file_name);
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        assert!(
            !ruac::check_diags(&source).0.is_empty(),
            "compiler oracle unexpectedly accepted {file_name}"
        );
        let (host, file_id) = single_file_host(&source);
        let analysis = host.analysis();
        let map = analysis.def_map(file_id);
        let found = map
            .definitions()
            .filter(|definition| {
                definition.file_id() == file_id
                    && matches!(definition.kind(), DefKind::Function | DefKind::Method)
            })
            .filter_map(|definition| analysis.infer(definition.id()))
            .any(|inference| {
                inference
                    .diagnostics()
                    .iter()
                    .any(|diagnostic| match expected {
                        Expected::Bool => {
                            matches!(diagnostic, InferenceDiagnostic::ExpectedBool { .. })
                        }
                        Expected::Mismatch => {
                            matches!(diagnostic, InferenceDiagnostic::TypeMismatch { .. })
                        }
                        Expected::Unary => {
                            matches!(diagnostic, InferenceDiagnostic::InvalidUnary { .. })
                        }
                        Expected::Binary => {
                            matches!(diagnostic, InferenceDiagnostic::InvalidBinary { .. })
                        }
                        Expected::Arity => {
                            matches!(diagnostic, InferenceDiagnostic::ArgumentCount { .. })
                        }
                    })
            });
        assert!(found, "missing native {file_name} diagnostic category");
    }
}

fn collect_rua_files(directory: &Path, paths: &mut Vec<std::path::PathBuf>) {
    let entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read directory {}: {error}", directory.display()));
    for entry in entries {
        let path = entry.expect("read corpus entry").path();
        if path.is_dir() {
            collect_rua_files(&path, paths);
        } else if path.extension().is_some_and(|extension| extension == "rua") {
            paths.push(path);
        }
    }
}
