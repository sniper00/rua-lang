//! Iterator type parity tests — validates adapter chain intermediate types
//! and consumer return types through native inference.

use rua_analysis::{
    AnalysisHost, Body, BodySourceMap, Change, DefKind, FileId, FileKind, InferenceResult,
    SourceRootId, SourceRootKind, Ty,
};
use std::sync::Arc;

struct Fixture {
    body: Arc<Body>,
    #[allow(dead_code)]
    source_map: Arc<BodySourceMap>,
    inference: Arc<InferenceResult>,
}

fn single_file_fixture(source: &'static str, owner_name: &str) -> Fixture {
    let root_id = SourceRootId::new(0);
    let file_id = FileId::new(0);
    let mut change = Change::new();
    change.set_source_root(root_id, SourceRootKind::Workspace);
    change.set_file_with_path(file_id, root_id, FileKind::Source, "main.rua", source);
    let mut host = AnalysisHost::new();
    host.apply_change(change);
    let analysis = host.analysis();
    assert!(
        analysis.parse(file_id).errors().is_empty(),
        "fixture must parse: {:?}",
        analysis.parse(file_id).errors()
    );
    let owner = analysis
        .def_map(file_id)
        .definitions()
        .find(|d| d.name() == owner_name && d.kind() == DefKind::Function)
        .unwrap_or_else(|| panic!("missing function `{owner_name}`"))
        .id();
    Fixture {
        body: analysis.body(owner).expect("body"),
        source_map: analysis.body_source_map(owner).expect("source map"),
        inference: analysis.infer(owner).expect("inference"),
    }
}

fn binding_ty(fixture: &Fixture, name: &str) -> Ty {
    fixture
        .body
        .bindings()
        .find_map(|(id, binding)| (binding.name() == Some(name)).then_some(id))
        .and_then(|id| fixture.inference.type_of_binding(id).cloned())
        .unwrap_or(Ty::Unknown)
}

#[test]
fn iterator_type_parity_map_collect_chain_infers_correct_types() {
    const SOURCE: &str = r#"
fn main() -> i64 {
    let values: Vec<i64> = [1, 2, 3];
    let doubled: Vec<i64> = values.iter().map(|v| v + 1).collect();
    0
}
"#;
    let fixture = single_file_fixture(SOURCE, "main");
    assert_eq!(binding_ty(&fixture, "doubled"), Ty::Vec(Box::new(Ty::I64)));
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn iterator_type_parity_filter_count_returns_i64() {
    const SOURCE: &str = r#"
fn main() -> i64 {
    let values: Vec<i64> = [1, 2, 3];
    values.iter().filter(|v| v > 0).count()
}
"#;
    let fixture = single_file_fixture(SOURCE, "main");
    // The function body tail should be i64.
    assert_eq!(
        fixture.inference.type_of_expr(fixture.body.root_expr()),
        Some(&Ty::I64)
    );
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn iterator_type_parity_filter_map_collect_unwraps_option() {
    const SOURCE: &str = r#"
fn main() -> Vec<i64> {
    let values: Vec<Option<i64>> = [Some(1), None];
    values.iter().filter_map(|v| v).collect()
}
"#;
    let fixture = single_file_fixture(SOURCE, "main");
    assert_eq!(
        fixture.inference.type_of_expr(fixture.body.root_expr()),
        Some(&Ty::Vec(Box::new(Ty::I64)))
    );
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn iterator_type_parity_enumerate_fold_produces_correct_type() {
    const SOURCE: &str = r#"
fn main() -> i64 {
    let values: Vec<i64> = [1, 2, 3];
    values.iter().enumerate().fold(0, |acc, _pair| acc + 1)
}
"#;
    let fixture = single_file_fixture(SOURCE, "main");
    assert_eq!(
        fixture.inference.type_of_expr(fixture.body.root_expr()),
        Some(&Ty::I64)
    );
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn iterator_type_parity_take_skip_preserve_item_type() {
    const SOURCE: &str = r#"
fn main() -> i64 {
    let values: Vec<i64> = [1, 2, 3, 4, 5];
    let filtered: Vec<i64> = values.iter().skip(1).take(3).collect();
    0
}
"#;
    let fixture = single_file_fixture(SOURCE, "main");
    assert_eq!(binding_ty(&fixture, "filtered"), Ty::Vec(Box::new(Ty::I64)));
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn iterator_type_parity_any_all_return_bool() {
    const SOURCE: &str = r#"
fn main() -> bool {
    let values: Vec<i64> = [1, 2, 3];
    values.iter().any(|v| v > 0) && values.iter().all(|v| v > 0)
}
"#;
    let fixture = single_file_fixture(SOURCE, "main");
    assert_eq!(
        fixture.inference.type_of_expr(fixture.body.root_expr()),
        Some(&Ty::BOOL)
    );
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn iterator_type_parity_find_returns_option_item() {
    const SOURCE: &str = r#"
fn main() -> Option<i64> {
    let values: Vec<i64> = [1, 2, 3];
    values.iter().find(|v| v > 1)
}
"#;
    let fixture = single_file_fixture(SOURCE, "main");
    assert_eq!(
        fixture.inference.type_of_expr(fixture.body.root_expr()),
        Some(&Ty::Option(Box::new(Ty::I64)))
    );
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn iterator_type_parity_range_produces_iterator_i64() {
    const SOURCE: &str = r#"
fn main() -> i64 {
    let sum: i64 = (0..10).fold(0, |acc, v| acc + v);
    sum
}
"#;
    let fixture = single_file_fixture(SOURCE, "main");
    assert_eq!(binding_ty(&fixture, "sum"), Ty::I64);
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn iterator_type_parity_unknown_adaptor_does_not_panic() {
    const SOURCE: &str = r#"
fn main() -> i64 {
    let values: Vec<i64> = [1, 2, 3];
    values.iter().unknown_adaptor().count()
}
"#;
    let fixture = single_file_fixture(SOURCE, "main");
    // When an unknown adapter is encountered, the chain degrades to Unknown
    // but does not panic.
    assert_eq!(
        fixture.inference.type_of_expr(fixture.body.root_expr()),
        Some(&Ty::Unknown)
    );
}

#[test]
fn iterator_type_parity_closure_captures_outer_variable() {
    const SOURCE: &str = r#"
fn main() -> i64 {
    let multiplier: i64 = 2;
    let values: Vec<i64> = [1, 2, 3];
    let result: Vec<i64> = values.iter().map(|v| v * multiplier).collect();
    0
}
"#;
    let fixture = single_file_fixture(SOURCE, "main");
    assert_eq!(binding_ty(&fixture, "result"), Ty::Vec(Box::new(Ty::I64)));
    assert!(fixture.inference.diagnostics().is_empty());
}
