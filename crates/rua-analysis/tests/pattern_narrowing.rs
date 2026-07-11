//! Pattern narrowing type inference tests for match, if-let, and while-let.
//!
//! Validates that pattern bindings receive narrowed types instead of Unknown
//! when the scrutinee type is known.

use std::sync::Arc;

use rua_analysis::{
    Analysis, AnalysisHost, BindingId, Body, BodySourceMap, Change, DefId, DefKind, Expr, ExprId,
    FileId, FileKind, InferenceResult, SourceRootId, SourceRootKind, Ty,
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
            (predicate(expr)
                && range.file_id == fixture.file_id
                && range.range.start() == offset)
                .then_some((range.range.len(), id))
        })
        .max_by_key(|(length, _)| *length)
        .map(|(_, id)| id)
        .unwrap_or_else(|| panic!("no matching expression at {marker}"))
}

fn assert_expr_ty(fixture: &Fixture, marker: &str, expected: &Ty) {
    let expr = expr_at(fixture, marker, |_| true);
    let actual = fixture.inference.type_of_expr(expr);
    eprintln!(
        "assert_expr_ty: marker={marker} expr_id={:?} actual={:?} expected={:?}",
        expr.index(),
        actual,
        expected
    );
    assert_eq!(actual, Some(expected), "expression at {marker}");
}

fn assert_binding_ty(fixture: &Fixture, marker: &str, expected: &Ty) {
    let binding = binding_at(fixture, marker);
    let actual = fixture.inference.type_of_binding(binding);
    eprintln!("assert_binding_ty: marker={marker} binding_id={:?} name={:?} actual={:?} expected={:?}",
        binding.index(), fixture.body.binding(binding).and_then(|b| b.name()), actual, expected);
    assert_eq!(actual, Some(expected), "binding at {marker}");
}

fn i64_ty() -> Ty {
    Ty::I64
}

#[test]
fn pattern_narrowing_match_option_some_extracts_inner_type() {
    const SOURCE: &str = r#"
fn narrow_option() -> i64 {
    let opt: Option<i64> = Some(42);
    let result = /*match_expr*/match opt {
        Some(/*v_def*/v) => /*v_use*/v,
        None => 0,
    };
    result
}
"#;
    let fixture = fixture(SOURCE, "narrow_option");
    assert_binding_ty(&fixture, "/*v_def*/", &i64_ty());
    assert_expr_ty(&fixture, "/*v_use*/", &i64_ty());
    assert_expr_ty(&fixture, "/*match_expr*/", &i64_ty());
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn pattern_narrowing_if_let_option_narrows_then_branch() {
    const SOURCE: &str = r#"
fn if_let_narrow() -> i64 {
    let opt: Option<i64> = Some(42);
    if let Some(/*inner_def*/inner) = opt {
        /*then_expr*/inner
    } else {
        0
    }
}
"#;
    let fixture = fixture(SOURCE, "if_let_narrow");
    assert_binding_ty(&fixture, "/*inner_def*/", &i64_ty());
    assert_expr_ty(&fixture, "/*then_expr*/", &i64_ty());
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn pattern_narrowing_while_let_narrows_loop_body() {
    const SOURCE: &str = r#"
fn while_let_narrow() {
    let mut items: Vec<i64> = vec![1, 2];
    while let Some(/*item_def*/item) = items.pop() {
        let _used: i64 = /*item_use*/item;
    }
}
"#;
    let fixture = fixture(SOURCE, "while_let_narrow");
    assert_binding_ty(&fixture, "/*item_def*/", &i64_ty());
    assert_expr_ty(&fixture, "/*item_use*/", &i64_ty());
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn pattern_narrowing_nested_option_result_pattern() {
    const SOURCE: &str = r#"
fn nested_narrow() -> i64 {
    let value: Option<Result<i64, String>> = Some(Ok(42));
    match value {
        Some(Ok(/*nested_def*/nested)) => /*nested_use*/nested,
        _ => 0,
    }
}
"#;
    let fixture = fixture(SOURCE, "nested_narrow");
    assert_binding_ty(&fixture, "/*nested_def*/", &i64_ty());
    assert_expr_ty(&fixture, "/*nested_use*/", &i64_ty());
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn pattern_narrowing_enum_struct_variant_narrows_field_bindings() {
    const SOURCE: &str = r#"
enum Shape {
    Rect { width: i64, height: i64 },
    Circle(i64),
}

fn struct_variant_narrow() -> i64 {
    let shape: Shape = Rect { width: 3, height: 4 };
    let result = /*match_expr*/match shape {
        Rect { width: /*w_def*/w, height: /*h_def*/h } => /*add_use*/w + h,
        Circle(/*r_def*/r) => /*mul_use*/r * 2,
    };
    result
}
"#;
    let fixture = fixture(SOURCE, "struct_variant_narrow");
    assert_binding_ty(&fixture, "/*w_def*/", &i64_ty());
    assert_binding_ty(&fixture, "/*h_def*/", &i64_ty());
    assert_binding_ty(&fixture, "/*r_def*/", &i64_ty());
    assert_expr_ty(&fixture, "/*add_use*/", &i64_ty());
    assert_expr_ty(&fixture, "/*mul_use*/", &i64_ty());
    assert_expr_ty(&fixture, "/*match_expr*/", &i64_ty());
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn pattern_narrowing_incomplete_match_does_not_panic() {
    const SOURCE: &str = r#"
fn incomplete_match() -> i64 {
    let opt: Option<i64> = Some(1);
    match opt {
        Some(/*v_def*/v) => /*v_use*/v,
    }
}
"#;
    let fixture = fixture(SOURCE, "incomplete_match");
    // Incomplete pattern still narrows the binding correctly.
    assert_binding_ty(&fixture, "/*v_def*/", &i64_ty());
    assert_expr_ty(&fixture, "/*v_use*/", &i64_ty());
    // Result is Unknown because the match is non-exhaustive.
    // We only require that it doesn't panic.
}

#[test]
fn pattern_narrowing_match_bindings_do_not_leak_between_arms() {
    const SOURCE: &str = r#"
fn arm_isolation(flag: bool) -> i64 {
    let opt: Option<i64> = if flag { Some(1) } else { None };
    match opt {
        Some(/*arm1_def*/a) => /*arm1_expr*/a,
        None => {
            let /*arm2_def*/default: i64 = 0;
            /*arm2_expr*/default
        }
    }
}
"#;
    let fixture = fixture(SOURCE, "arm_isolation");
    assert_binding_ty(&fixture, "/*arm1_def*/", &i64_ty());
    assert_expr_ty(&fixture, "/*arm1_expr*/", &i64_ty());
    assert_binding_ty(&fixture, "/*arm2_def*/", &i64_ty());
    assert_expr_ty(&fixture, "/*arm2_expr*/", &i64_ty());
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn pattern_narrowing_deeply_nested_tuple_variants() {
    const SOURCE: &str = r#"
fn deep_nest() -> i64 {
    let triple: Option<Option<Option<i64>>> = Some(Some(Some(1)));
    match triple {
        Some(Some(Some(/*deep_def*/deep))) => /*deep_use*/deep,
        _ => 0,
    }
}
"#;
    let fixture = fixture(SOURCE, "deep_nest");
    assert_binding_ty(&fixture, "/*deep_def*/", &i64_ty());
    assert_expr_ty(&fixture, "/*deep_use*/", &i64_ty());
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn pattern_narrowing_result_ok_err_variants() {
    const SOURCE: &str = r#"
fn result_narrow(flag: bool) -> String {
    let res: Result<i64, String> = if flag { Ok(42) } else { Err("failed".to_string()) };
    match res {
        Ok(/*ok_def*/value) => /*ok_use*/value.to_string(),
        Err(/*err_def*/msg) => /*err_use*/msg,
    }
}
"#;
    let fixture = fixture(SOURCE, "result_narrow");
    assert_binding_ty(&fixture, "/*ok_def*/", &i64_ty());
    assert_binding_ty(&fixture, "/*err_def*/", &Ty::STRING);
    // The return type of the match would be String (join of i64.to_string() -> String and String -> String)
    assert!(fixture.inference.diagnostics().is_empty());
}
