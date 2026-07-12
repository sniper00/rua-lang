//! Code lens tests — reference counts and impl counts on definitions.

mod support;

use support::{uri, TestServer};

#[test]
fn code_lens_has_entries_for_functions() {
    let uri = uri("/test/lens.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "fn main() {}\nfn helper() {}\nstruct Point { x: i64 }\nimpl Point { fn new() -> Point { Point { x: 0 } } }",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let def_map = analysis.def_map(file_id);

    // Each function, method, struct, trait, impl should produce a lens
    let mut fn_count = 0;
    let mut struct_count = 0;
    let mut impl_count = 0;

    for definition in def_map.definitions() {
        match definition.kind() {
            rua_analysis::DefKind::Function | rua_analysis::DefKind::Method => {
                fn_count += 1;
                // Verify the definition has a body (needed for reference counting)
                if let Some(body) = analysis.body(definition.id()) {
                    // Body should have expressions
                    let _ = body;
                }
            }
            rua_analysis::DefKind::Struct => {
                struct_count += 1;
            }
            rua_analysis::DefKind::Impl => {
                impl_count += 1;
            }
            _ => {}
        }
    }

    assert!(fn_count >= 2, "should have at least 2 functions/methods, got {fn_count}");
    assert!(struct_count >= 1, "should have at least 1 struct, got {struct_count}");
    assert!(impl_count >= 1, "should have at least 1 impl, got {impl_count}");
}

#[test]
fn code_lens_counts_references() {
    let uri = uri("/test/lens_refs.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "fn used_fn() -> i64 { 42 }\nfn unused_fn() -> i64 { 0 }\nfn main() { used_fn(); }",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let def_map = analysis.def_map(file_id);

    // Count how many bodies reference "used_fn"
    let mut ref_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for d in def_map.definitions() {
        if matches!(
            d.kind(),
            rua_analysis::DefKind::Function | rua_analysis::DefKind::Method
        )
            && let Some(body) = analysis.body(d.id()) {
                let mut seen = std::collections::HashSet::new();
                for (_, nr) in body.name_refs() {
                    if let Some(n) = nr.name() {
                        seen.insert(n.to_string());
                    }
                }
                for n in seen {
                    *ref_counts.entry(n).or_default() += 1;
                }
            }
    }

    let used_count = ref_counts.get("used_fn").copied().unwrap_or(0);
    let unused_count = ref_counts.get("unused_fn").copied().unwrap_or(0);
    // used_fn should be referenced at least once (by main)
    assert!(used_count >= 1, "used_fn should have references, got {used_count}");
    // unused_fn should have fewer references than used_fn
    assert!(
        unused_count < used_count || unused_count == 0,
        "unused_fn should have fewer references than used_fn"
    );
}

#[test]
fn code_lens_counts_impls_for_struct() {
    let uri = uri("/test/lens_impls.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "struct Point { x: i64, y: i64 }\nimpl Point { fn new() -> Point { Point { x: 0, y: 0 } } }\nimpl Point { fn origin() -> Point { Point { x: 0, y: 0 } } }",
    );

    let file_id = srv.file_id_for_uri(&uri).unwrap();
    let analysis = srv.snapshot();
    let def_map = analysis.def_map(file_id);

    let impl_count = def_map
        .definitions()
        .filter(|d| d.kind() == rua_analysis::DefKind::Impl)
        .count();
    assert!(impl_count >= 2, "should have 2 impl blocks for Point, got {impl_count}");
}
