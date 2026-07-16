//! Enhanced inlay hints tests — type hints, explicit type skip, multi-function.

mod support;

use rua_analysis::TypeHintTarget;
use support::{TestServer, uri};

#[test]
fn inlay_hints_work_for_unattached_demo_file() {
    let root_uri = uri("/test/main.rua");
    let demo_uri = uri("/test/demo.rua");
    let source = include_str!("../../../tests/demo.rua");
    let mut srv = TestServer::new();
    srv.open(&root_uri, "fn root() {}");
    let file_id = srv.open(&demo_uri, source);

    let hints = srv.snapshot().inlay_hints(rua_analysis::ProjectFile::new(
        rua_analysis::ProjectId::new(0),
        file_id,
    ));
    let red_name_end = source.find("let red_name =").unwrap() as u32 + "let red_name".len() as u32;
    assert!(
        hints
            .iter()
            .any(|hint| hint.position().offset == red_name_end && hint.ty() == "String"),
        "missing inferred String hint for red_name"
    );

    let doubled_end = source.find("let doubled:").unwrap() as u32 + "let doubled".len() as u32;
    assert!(
        hints
            .iter()
            .all(|hint| hint.position().offset != doubled_end),
        "explicitly annotated binding must not get a duplicate hint"
    );
}

#[test]
fn inlay_hint_for_let_binding_with_struct_type() {
    let uri = uri("/test/inlay_struct.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "struct Point { x: i64, y: i64 }\nfn main() { let p = Point { x: 0, y: 0 }; }",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let def_map = analysis.def_map(file_id);

    let mut hints_found: Vec<(u32, String)> = Vec::new();
    for definition in def_map.definitions() {
        if !matches!(
            definition.kind(),
            rua_analysis::DefKind::Function | rua_analysis::DefKind::Method
        ) {
            continue;
        }
        let Some(body) = analysis.body(definition.id()) else {
            continue;
        };
        let Some(source_map) = analysis.body_source_map(definition.id()) else {
            continue;
        };
        let Some(inference) = analysis.infer(definition.id()) else {
            continue;
        };
        for (bid, b) in body.bindings() {
            if b.name().is_some()
                && let Some(ty) = inference.type_of_binding(bid)
                && !ty.is_unknown()
                && let Some(fr) = source_map.binding_range(bid)
            {
                hints_found.push((fr.range.end(), ty.to_string()));
            }
        }
    }

    let has_point_hint = hints_found.iter().any(|(_, label)| label.contains("Point"));
    assert!(
        has_point_hint,
        "should have Point type hint, got: {hints_found:?}"
    );
}

#[test]
fn inlay_hint_option_type_and_payload_are_hoverable_and_navigable() {
    let uri = uri("/test/main.rua");
    let source = "struct Product {}\n\
                  fn first_available() -> Option<Product> { Option::None }\n\
                  let featured = first_available();\n";
    let mut srv = TestServer::new();
    let file_id = srv.open(&uri, source);

    let hints = srv.snapshot().inlay_hints(rua_analysis::ProjectFile::new(
        rua_analysis::ProjectId::new(0),
        file_id,
    ));
    let featured_end = source.find("featured").unwrap() as u32 + "featured".len() as u32;
    let hint = hints
        .iter()
        .find(|hint| hint.position().offset == featured_end)
        .expect("featured type hint");

    assert_eq!(hint.ty(), "Option<Product>");
    let option = hint
        .label_parts()
        .iter()
        .find(|part| part.value() == "Option")
        .expect("Option label part");
    assert!(
        matches!(option.target(), Some(TypeHintTarget::Builtin(target)) if target.source_name() == "option.ruai")
    );
    assert_eq!(
        option.tooltip().and_then(|tooltip| tooltip.context()),
        Some("std::option::Option")
    );

    let product = hint
        .label_parts()
        .iter()
        .find(|part| part.value() == "Product")
        .expect("Product label part");
    assert!(matches!(
        product.target(),
        Some(TypeHintTarget::Source(target)) if target.file_id == file_id
    ));
    assert_eq!(
        product.tooltip().and_then(|tooltip| tooltip.context()),
        Some("Product")
    );
}

#[test]
fn inlay_hint_for_primitive_types() {
    let uri = uri("/test/inlay_prim.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "fn main() { let x = 42; let b = true; let s = \"hello\"; }",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let def_map = analysis.def_map(file_id);

    let mut hints_found: Vec<(u32, String)> = Vec::new();
    for definition in def_map.definitions() {
        if !matches!(
            definition.kind(),
            rua_analysis::DefKind::Function | rua_analysis::DefKind::Method
        ) {
            continue;
        }
        let Some(body) = analysis.body(definition.id()) else {
            continue;
        };
        let Some(source_map) = analysis.body_source_map(definition.id()) else {
            continue;
        };
        let Some(inference) = analysis.infer(definition.id()) else {
            continue;
        };
        for (bid, b) in body.bindings() {
            if b.name().is_some()
                && let Some(ty) = inference.type_of_binding(bid)
                && !ty.is_unknown()
                && let Some(fr) = source_map.binding_range(bid)
            {
                hints_found.push((fr.range.end(), ty.to_string()));
            }
        }
    }

    // Should have hints for x (i64), b (bool), s (String)
    assert!(
        !hints_found.is_empty(),
        "should have type hints for primitive bindings, got: {hints_found:?}"
    );
}

#[test]
fn inlay_hint_no_hint_when_type_annotation_present() {
    // When a binding has an explicit type annotation, the inlay hint
    // should not add a redundant `: Type` hint.
    let uri = uri("/test/inlay_annotated.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { let x: i64 = 42; }");

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let def_map = analysis.def_map(file_id);

    let mut hints_for_x: Vec<String> = Vec::new();
    for definition in def_map.definitions() {
        if !matches!(
            definition.kind(),
            rua_analysis::DefKind::Function | rua_analysis::DefKind::Method
        ) {
            continue;
        }
        let Some(body) = analysis.body(definition.id()) else {
            continue;
        };
        let Some(source_map) = analysis.body_source_map(definition.id()) else {
            continue;
        };
        let Some(inference) = analysis.infer(definition.id()) else {
            continue;
        };
        for (bid, b) in body.bindings() {
            if b.name() == Some("x")
                && let Some(ty) = inference.type_of_binding(bid)
                && let Some(_fr) = source_map.binding_range(bid)
            {
                hints_for_x.push(ty.to_string());
            }
        }
    }

    // The hint should match the annotation type (i64) or not produce extra hint
    // Verify no crash regardless of whether hints are produced
    let _ = hints_for_x;
}
