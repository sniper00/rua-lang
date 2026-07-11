use std::sync::Arc;

use rua_analysis::{
    Analysis, AnalysisHost, BindingId, Body, BodyId, BodyResolution, BodyScopes, BodySourceMap,
    Change, DefId, DefKind, Expr, ExprId, FileId, FileKind, FilePosition, LocalBindingId,
    LocalResolveResult, LocalUseKind, NameRefId, NameRefKind, ScopeId, ScopeKind, SourceRootId,
    SourceRootKind,
};

struct Fixture {
    source: &'static str,
    file_id: FileId,
    body: Arc<Body>,
    source_map: Arc<BodySourceMap>,
    scopes: Arc<BodyScopes>,
    resolution: Arc<BodyResolution>,
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

fn fixture(source: &'static str, owner_name: &str, owner_kind: DefKind) -> Fixture {
    let (host, file_id) = single_file_host(source);
    let analysis = host.analysis();
    let owner = body_owner(&analysis, file_id, owner_name, owner_kind);
    Fixture {
        source,
        file_id,
        body: analysis.body(owner).expect("fixture body"),
        source_map: analysis
            .body_source_map(owner)
            .expect("fixture body source map"),
        scopes: analysis.body_scopes(owner).expect("fixture body scopes"),
        resolution: analysis
            .body_resolution(owner)
            .expect("fixture body resolution"),
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

fn name_ref_at(fixture: &Fixture, marker: &str, kind: NameRefKind) -> NameRefId {
    let offset = marker_offset(fixture.source, marker);
    fixture
        .body
        .name_refs()
        .find_map(|(id, name_ref)| {
            (name_ref.kind() == kind)
                .then(|| fixture.source_map.name_ref_range(id))
                .flatten()
                .filter(|range| range.file_id == fixture.file_id && range.range.start() == offset)
                .map(|_| id)
        })
        .unwrap_or_else(|| panic!("no {kind:?} name reference at {marker}"))
}

fn expr_at<F>(fixture: &Fixture, marker: &str, predicate: F) -> ExprId
where
    F: Fn(&Expr) -> bool,
{
    let offset = marker_offset(fixture.source, marker);
    fixture
        .body
        .exprs()
        .find_map(|(id, expr)| {
            (predicate(expr)
                && fixture
                    .source_map
                    .expr_range(id)
                    .is_some_and(|range| range.range.start() == offset))
            .then_some(id)
        })
        .unwrap_or_else(|| panic!("no matching expression at {marker}"))
}

fn resolved(fixture: &Fixture, name_ref: NameRefId, binding: BindingId) -> LocalBindingId {
    let result = fixture
        .resolution
        .resolve(name_ref)
        .unwrap_or_else(|| panic!("name reference {name_ref:?} has no resolution fact"));
    let LocalResolveResult::Resolved(local) = result else {
        panic!("expected local {binding:?}, got {result:?}")
    };
    assert_eq!(local.owner(), fixture.body.id());
    assert_eq!(local.binding(), binding);
    local
}

fn assert_non_local(fixture: &Fixture, name_ref: NameRefId) {
    assert_eq!(
        fixture.resolution.resolve(name_ref),
        Some(LocalResolveResult::NonLocal)
    );
}

fn binding_scope(fixture: &Fixture, binding: BindingId) -> ScopeId {
    fixture
        .scopes
        .scope_for_binding(binding)
        .unwrap_or_else(|| panic!("binding {binding:?} has no scope"))
}

fn assert_scope_contains(fixture: &Fixture, binding: BindingId) -> ScopeId {
    let scope = binding_scope(fixture, binding);
    assert!(
        fixture
            .scopes
            .scope(scope)
            .expect("scope data")
            .bindings()
            .contains(&binding)
    );
    scope
}

fn assert_use(fixture: &Fixture, local: LocalBindingId, name_ref: NameRefId, kind: LocalUseKind) {
    assert!(
        fixture
            .resolution
            .uses_for(local)
            .any(|local_use| local_use.name_ref() == name_ref && local_use.kind() == kind),
        "missing {kind:?} use {name_ref:?} for {local:?}"
    );
}

const ORDERING: &str = r#"
fn ordering(/*param_def*/param: i64) -> i64 {
    /*before*/future;
    let /*future_def*/future = 1;
    /*after*/future;
    let /*shadow_def*/param = /*rhs_param*/param;
    /*shadow_after*/param;
    {
        /*inner_before*/param;
        let /*inner_def*/param = /*inner_rhs*/param;
        /*inner_after*/param;
    }
    /*outer_after*/param
}
"#;

const BRANCHES: &str = r#"
enum Maybe { Some(i64), None }

fn branches(opt: Maybe) -> i64 {
    if let Maybe::Some(/*if_def*/if_value) = opt {
        /*if_use*/if_value;
    } else {
        /*if_else*/if_value;
    }
    /*if_after*/if_value;

    while let Maybe::Some(/*while_def*/while_value) = opt {
        /*while_use*/while_value;
        break;
    }
    /*while_after*/while_value;

    match opt {
        Maybe::Some(/*arm_one_def*/arm) if /*arm_guard*/arm > 0 => /*arm_one_use*/arm,
        Maybe::Some(/*arm_two_def*/arm) => /*arm_two_use*/arm,
        Maybe::None => /*arm_leak*/arm,
    }
}
"#;

const LOCAL_KINDS: &str = r#"
struct Snapshot { item: i64 }
struct Harness { item: i64 }

impl Harness {
    fn kinds(&/*self_def*/self, /*param_def*/param: i64) -> i64 {
        let /*outer_item_def*/item = param;
        for /*for_item_def*/item in /*iter_item*/item {
            /*plain_self*/self;
            /*plain_param*/param;
            /*for_item_use*/item;
            /*field_receiver*/self./*field_name*/item;
            /*method_receiver*/self./*method_name*/method(/*method_arg*/param);
            let short = Snapshot { /*short_field*/item };
            let qualified = /*module_self*/self::Snapshot { /*qualified_short*/item };
            /*macro_name*/println!("{}", /*macro_arg*/item);
        }
        /*after_item*/item
    }
}
"#;

const CLOSURES: &str = r#"
fn closures(/*outer_def*/outer: i64) -> i64 {
    let /*value_def*/value = outer;
    let first = /*first_closure*/|/*closure_param_def*/value| {
        /*closure_param_use*/value + /*capture_outer*/outer
    };
    let second = /*second_closure*/|| /*capture_value*/value;
    /*outside_value*/value
}
"#;

const AMBIGUOUS_BINDINGS: &str = r#"
enum Maybe { Some(i64), None }

fn ambiguous(/*first_param*/same: i64, /*second_param*/same: i64, input: Maybe) -> i64 {
    /*param_use*/same;
    match input {
        Maybe::Some(/*left_def*/value) | Maybe::Some(/*right_def*/value) => /*or_use*/value,
        Maybe::None => 0,
    }
}
"#;

const NESTED_CLOSURES: &str = r#"
fn nested(/*outer_def*/outer: i64) -> i64 {
    let first = /*outer_closure*/|| {
        let second = /*inner_closure*/|| /*nested_use*/outer;
        second()
    };
    first()
}
"#;

const MALFORMED: &str = r#"
enum Maybe { Some(i64), None }

fn broken(input: Maybe) -> i64 {
    let /*kept_def*/kept = ;
    match input {
        => /*empty_arm_kept*/kept,
        Maybe::Some(/*valid_def*/valid) => /*valid_use*/valid,
        Maybe::None => /*sibling_leak*/valid,
    }
}

fn other() -> i64 { /*cross_body*/kept }
"#;

const SEMANTICS_FACADE: &str = r#"
struct Subject { field: i64 }

impl Subject {
    fn inspect(&/*self_def*/self, /*param_def*/param: i64) -> i64 {
        let mut /*value_def*/value = /*param_use*/param;
        /*value_write*/value = /*value_read*/value + 1;
        /*self_field_receiver*/self./*member_field*/field;
        /*self_method_receiver*/self./*member_method*/method();
        /*value_tail*/value
    }
}

fn field() -> i64 { 0 }
fn method() -> i64 { 0 }
"#;

#[test]
fn local_scope_tree_maps_callable_blocks_loops_arms_and_closures() {
    let branches = fixture(BRANCHES, "branches", DefKind::Function);
    assert!(branches.scopes.scope(branches.scopes.root()).is_some());
    assert!(
        branches
            .scopes
            .scope(branches.scopes.root())
            .expect("root scope")
            .parent()
            .is_none()
    );

    let if_binding = binding_at(&branches, "/*if_def*/");
    let while_binding = binding_at(&branches, "/*while_def*/");
    let arm_one = binding_at(&branches, "/*arm_one_def*/");
    let arm_two = binding_at(&branches, "/*arm_two_def*/");
    let if_scope = assert_scope_contains(&branches, if_binding);
    let while_scope = assert_scope_contains(&branches, while_binding);
    let arm_one_scope = assert_scope_contains(&branches, arm_one);
    let arm_two_scope = assert_scope_contains(&branches, arm_two);
    assert_ne!(if_scope, while_scope);
    assert_ne!(arm_one_scope, arm_two_scope);
    assert!(matches!(
        branches
            .scopes
            .scope(arm_one_scope)
            .expect("first arm scope")
            .kind(),
        ScopeKind::MatchArm
    ));
    assert!(matches!(
        branches
            .scopes
            .scope(arm_two_scope)
            .expect("second arm scope")
            .kind(),
        ScopeKind::MatchArm
    ));

    for marker in [
        "/*if_use*/",
        "/*while_use*/",
        "/*arm_guard*/",
        "/*arm_one_use*/",
    ] {
        let name_ref = name_ref_at(&branches, marker, NameRefKind::Path);
        assert!(branches.scopes.scope_for_name_ref(name_ref).is_some());
    }

    let closures = fixture(CLOSURES, "closures", DefKind::Function);
    let closure_param = binding_at(&closures, "/*closure_param_def*/");
    let closure_scope = assert_scope_contains(&closures, closure_param);
    let outer_scope = assert_scope_contains(&closures, binding_at(&closures, "/*outer_def*/"));
    assert_ne!(closure_scope, outer_scope);
    let first_closure = expr_at(&closures, "/*first_closure*/", |expr| {
        matches!(expr, Expr::Closure { .. })
    });
    assert!(closures.scopes.scope_for_expr(first_closure).is_some());
}

#[test]
fn local_resolution_respects_declaration_order_and_shadowing() {
    let fixture = fixture(ORDERING, "ordering", DefKind::Function);
    let param = binding_at(&fixture, "/*param_def*/");
    let future = binding_at(&fixture, "/*future_def*/");
    let shadow = binding_at(&fixture, "/*shadow_def*/");
    let inner = binding_at(&fixture, "/*inner_def*/");

    assert_non_local(
        &fixture,
        name_ref_at(&fixture, "/*before*/", NameRefKind::Path),
    );
    resolved(
        &fixture,
        name_ref_at(&fixture, "/*after*/", NameRefKind::Path),
        future,
    );
    resolved(
        &fixture,
        name_ref_at(&fixture, "/*rhs_param*/", NameRefKind::Path),
        param,
    );
    for marker in [
        "/*shadow_after*/",
        "/*inner_before*/",
        "/*inner_rhs*/",
        "/*outer_after*/",
    ] {
        resolved(
            &fixture,
            name_ref_at(&fixture, marker, NameRefKind::Path),
            shadow,
        );
    }
    resolved(
        &fixture,
        name_ref_at(&fixture, "/*inner_after*/", NameRefKind::Path),
        inner,
    );
}

#[test]
fn local_resolution_handles_params_self_for_and_name_ref_kinds() {
    let fixture = fixture(LOCAL_KINDS, "kinds", DefKind::Method);
    let self_binding = binding_at(&fixture, "/*self_def*/");
    let param = binding_at(&fixture, "/*param_def*/");
    let outer_item = binding_at(&fixture, "/*outer_item_def*/");
    let for_item = binding_at(&fixture, "/*for_item_def*/");

    for marker in [
        "/*plain_self*/",
        "/*field_receiver*/",
        "/*method_receiver*/",
    ] {
        resolved(
            &fixture,
            name_ref_at(&fixture, marker, NameRefKind::Path),
            self_binding,
        );
    }
    for marker in ["/*plain_param*/", "/*method_arg*/"] {
        resolved(
            &fixture,
            name_ref_at(&fixture, marker, NameRefKind::Path),
            param,
        );
    }
    resolved(
        &fixture,
        name_ref_at(&fixture, "/*iter_item*/", NameRefKind::Path),
        outer_item,
    );
    resolved(
        &fixture,
        name_ref_at(&fixture, "/*after_item*/", NameRefKind::Path),
        outer_item,
    );
    for marker in [
        "/*for_item_use*/",
        "/*short_field*/",
        "/*qualified_short*/",
        "/*macro_arg*/",
    ] {
        resolved(
            &fixture,
            name_ref_at(&fixture, marker, NameRefKind::Path),
            for_item,
        );
    }

    assert_non_local(
        &fixture,
        name_ref_at(&fixture, "/*field_name*/", NameRefKind::Field),
    );
    assert_non_local(
        &fixture,
        name_ref_at(&fixture, "/*method_name*/", NameRefKind::Method),
    );
    assert_non_local(
        &fixture,
        name_ref_at(&fixture, "/*short_field*/", NameRefKind::StructField),
    );
    assert_non_local(
        &fixture,
        name_ref_at(&fixture, "/*module_self*/", NameRefKind::StructPath),
    );
    assert_non_local(
        &fixture,
        name_ref_at(&fixture, "/*macro_name*/", NameRefKind::Macro),
    );
}

#[test]
fn local_resolution_obeys_if_while_match_guard_and_arm_boundaries() {
    let fixture = fixture(BRANCHES, "branches", DefKind::Function);
    let if_binding = binding_at(&fixture, "/*if_def*/");
    let while_binding = binding_at(&fixture, "/*while_def*/");
    let arm_one = binding_at(&fixture, "/*arm_one_def*/");
    let arm_two = binding_at(&fixture, "/*arm_two_def*/");

    resolved(
        &fixture,
        name_ref_at(&fixture, "/*if_use*/", NameRefKind::Path),
        if_binding,
    );
    for marker in ["/*if_else*/", "/*if_after*/"] {
        assert_non_local(&fixture, name_ref_at(&fixture, marker, NameRefKind::Path));
    }
    resolved(
        &fixture,
        name_ref_at(&fixture, "/*while_use*/", NameRefKind::Path),
        while_binding,
    );
    assert_non_local(
        &fixture,
        name_ref_at(&fixture, "/*while_after*/", NameRefKind::Path),
    );
    for marker in ["/*arm_guard*/", "/*arm_one_use*/"] {
        resolved(
            &fixture,
            name_ref_at(&fixture, marker, NameRefKind::Path),
            arm_one,
        );
    }
    resolved(
        &fixture,
        name_ref_at(&fixture, "/*arm_two_use*/", NameRefKind::Path),
        arm_two,
    );
    assert_non_local(
        &fixture,
        name_ref_at(&fixture, "/*arm_leak*/", NameRefKind::Path),
    );
}

#[test]
fn local_resolution_tracks_closure_shadowing_and_captures() {
    let fixture = fixture(CLOSURES, "closures", DefKind::Function);
    let outer = binding_at(&fixture, "/*outer_def*/");
    let value = binding_at(&fixture, "/*value_def*/");
    let closure_param = binding_at(&fixture, "/*closure_param_def*/");

    resolved(
        &fixture,
        name_ref_at(&fixture, "/*closure_param_use*/", NameRefKind::Path),
        closure_param,
    );
    let outer_local = resolved(
        &fixture,
        name_ref_at(&fixture, "/*capture_outer*/", NameRefKind::Path),
        outer,
    );
    let value_local = resolved(
        &fixture,
        name_ref_at(&fixture, "/*capture_value*/", NameRefKind::Path),
        value,
    );
    resolved(
        &fixture,
        name_ref_at(&fixture, "/*outside_value*/", NameRefKind::Path),
        value,
    );

    let first = expr_at(&fixture, "/*first_closure*/", |expr| {
        matches!(expr, Expr::Closure { .. })
    });
    let second = expr_at(&fixture, "/*second_closure*/", |expr| {
        matches!(expr, Expr::Closure { .. })
    });
    assert_eq!(
        fixture
            .resolution
            .captures_for(first)
            .map(|capture| capture.binding())
            .collect::<Vec<_>>(),
        [outer_local]
    );
    assert_eq!(
        fixture
            .resolution
            .captures_for(second)
            .map(|capture| capture.binding())
            .collect::<Vec<_>>(),
        [value_local]
    );
}

#[test]
fn local_resolution_tracks_each_crossed_nested_closure() {
    let fixture = fixture(NESTED_CLOSURES, "nested", DefKind::Function);
    let outer = binding_at(&fixture, "/*outer_def*/");
    let outer_closure = expr_at(&fixture, "/*outer_closure*/", |expr| {
        matches!(expr, Expr::Closure { .. })
    });
    let inner_closure = expr_at(&fixture, "/*inner_closure*/", |expr| {
        matches!(expr, Expr::Closure { .. })
    });
    let nested_use = name_ref_at(&fixture, "/*nested_use*/", NameRefKind::Path);
    let target = resolved(&fixture, nested_use, outer);
    let local_use = fixture
        .resolution
        .uses_for(target)
        .find(|local_use| local_use.name_ref() == nested_use)
        .expect("nested capture use");
    assert_eq!(local_use.captured_by(), [outer_closure, inner_closure]);
    assert_eq!(
        fixture
            .resolution
            .captures()
            .iter()
            .map(|capture| (capture.closure(), capture.binding()))
            .collect::<Vec<_>>(),
        [(outer_closure, target), (inner_closure, target)]
    );
}

#[test]
fn local_resolution_marks_duplicate_and_bindingful_or_patterns_ambiguous() {
    let fixture = fixture(AMBIGUOUS_BINDINGS, "ambiguous", DefKind::Function);
    assert_eq!(
        fixture
            .resolution
            .resolve(name_ref_at(&fixture, "/*param_use*/", NameRefKind::Path,)),
        Some(LocalResolveResult::Ambiguous)
    );
    assert_eq!(
        fixture
            .resolution
            .resolve(name_ref_at(&fixture, "/*or_use*/", NameRefKind::Path,)),
        Some(LocalResolveResult::Ambiguous)
    );
}

#[test]
fn local_resolution_malformed_bodies_do_not_leak_bindings() {
    let (host, file_id) = single_file_host(MALFORMED);
    let analysis = host.analysis();
    assert!(!analysis.parse(file_id).errors().is_empty());

    let broken_owner = body_owner(&analysis, file_id, "broken", DefKind::Function);
    let broken = Fixture {
        source: MALFORMED,
        file_id,
        body: analysis.body(broken_owner).expect("broken body"),
        source_map: analysis
            .body_source_map(broken_owner)
            .expect("broken source map"),
        scopes: analysis
            .body_scopes(broken_owner)
            .expect("broken body scopes"),
        resolution: analysis
            .body_resolution(broken_owner)
            .expect("broken body resolution"),
    };
    let kept = binding_at(&broken, "/*kept_def*/");
    let valid = binding_at(&broken, "/*valid_def*/");
    resolved(
        &broken,
        name_ref_at(&broken, "/*empty_arm_kept*/", NameRefKind::Path),
        kept,
    );
    resolved(
        &broken,
        name_ref_at(&broken, "/*valid_use*/", NameRefKind::Path),
        valid,
    );
    assert_non_local(
        &broken,
        name_ref_at(&broken, "/*sibling_leak*/", NameRefKind::Path),
    );

    let other_owner = body_owner(&analysis, file_id, "other", DefKind::Function);
    let other_body = analysis.body(other_owner).expect("other body");
    let other_map = analysis
        .body_source_map(other_owner)
        .expect("other source map");
    let cross_offset = marker_offset(MALFORMED, "/*cross_body*/");
    let cross_ref = other_body
        .name_refs()
        .find_map(|(id, name_ref)| {
            (name_ref.kind() == NameRefKind::Path
                && other_map
                    .name_ref_range(id)
                    .is_some_and(|range| range.range.start() == cross_offset))
            .then_some(id)
        })
        .expect("cross-body kept reference");
    assert_eq!(
        analysis
            .body_resolution(other_owner)
            .expect("other body resolution")
            .resolve(cross_ref),
        Some(LocalResolveResult::NonLocal)
    );
}

#[test]
fn local_resolution_semantics_facade_resolves_definition_use_and_self() {
    let (host, file_id) = single_file_host(SEMANTICS_FACADE);
    let analysis = host.analysis();
    assert!(analysis.parse(file_id).errors().is_empty());
    let semantics = analysis.semantics(file_id);
    let position = |marker| FilePosition::new(file_id, marker_offset(SEMANTICS_FACADE, marker));

    let LocalResolveResult::Resolved(value_from_definition) =
        semantics.resolve_local_at(position("/*value_def*/"))
    else {
        panic!("value declaration must resolve to itself")
    };
    let LocalResolveResult::Resolved(value_from_use) =
        semantics.resolve_local_at(position("/*value_read*/"))
    else {
        panic!("value use must resolve")
    };
    assert_eq!(value_from_definition, value_from_use);
    let value_definition = semantics
        .local_definition(value_from_definition)
        .expect("value definition range");
    assert_eq!(
        value_definition,
        semantics
            .local_definition_at(position("/*value_def*/"))
            .expect("definition at declaration")
    );
    assert_eq!(
        value_definition,
        semantics
            .local_definition_at(position("/*value_read*/"))
            .expect("definition at use")
    );
    assert_eq!(
        value_definition.range.start(),
        marker_offset(SEMANTICS_FACADE, "/*value_def*/")
    );

    let LocalResolveResult::Resolved(param_from_definition) =
        semantics.resolve_local_at(position("/*param_def*/"))
    else {
        panic!("parameter declaration must resolve")
    };
    let LocalResolveResult::Resolved(param_from_use) =
        semantics.resolve_local_at(position("/*param_use*/"))
    else {
        panic!("parameter use must resolve")
    };
    assert_eq!(param_from_definition, param_from_use);

    let LocalResolveResult::Resolved(self_from_definition) =
        semantics.resolve_local_at(position("/*self_def*/"))
    else {
        panic!("self declaration must resolve")
    };
    for marker in ["/*self_field_receiver*/", "/*self_method_receiver*/"] {
        assert_eq!(
            semantics.resolve_local_at(position(marker)),
            LocalResolveResult::Resolved(self_from_definition)
        );
        assert_eq!(
            semantics.local_definition_at(position(marker)),
            semantics.local_definition(self_from_definition)
        );
    }
}

#[test]
fn local_resolution_semantics_member_names_do_not_fall_back_to_items() {
    let (host, file_id) = single_file_host(SEMANTICS_FACADE);
    let analysis = host.analysis();
    let semantics = analysis.semantics(file_id);
    let position = |marker| FilePosition::new(file_id, marker_offset(SEMANTICS_FACADE, marker));

    for marker in ["/*member_field*/", "/*member_method*/"] {
        let position = position(marker);
        assert_eq!(
            semantics.resolve_local_at(position),
            LocalResolveResult::NonLocal
        );
        assert!(semantics.local_definition_at(position).is_none());
        assert!(semantics.find_def_at(position).is_none());
        assert!(semantics.local_references_at(position, true).is_empty());
    }
}

#[test]
fn local_reference_index_tracks_reads_writes_and_source_order() {
    const SOURCE: &str = r#"
fn refs(/*seed_def*/seed: i64) -> i64 {
    let mut /*value_def*/value = /*seed_init*/seed;
    /*write_value*/value = /*read_value*/value + /*seed_rhs*/seed;
    println!("{}", /*macro_value*/value);
    /*tail_value*/value
}
"#;
    let fixture = fixture(SOURCE, "refs", DefKind::Function);
    let value = binding_at(&fixture, "/*value_def*/");
    let write = name_ref_at(&fixture, "/*write_value*/", NameRefKind::Path);
    let read = name_ref_at(&fixture, "/*read_value*/", NameRefKind::Path);
    let macro_use = name_ref_at(&fixture, "/*macro_value*/", NameRefKind::Path);
    let tail = name_ref_at(&fixture, "/*tail_value*/", NameRefKind::Path);
    let local = resolved(&fixture, write, value);
    for name_ref in [read, macro_use, tail] {
        resolved(&fixture, name_ref, value);
    }

    assert_use(&fixture, local, write, LocalUseKind::Write);
    for name_ref in [read, macro_use, tail] {
        assert_use(&fixture, local, name_ref, LocalUseKind::Read);
    }
    let uses = fixture.resolution.uses_for(local).collect::<Vec<_>>();
    assert_eq!(uses.len(), 4);
    let starts = uses
        .into_iter()
        .map(|local_use| {
            fixture
                .source_map
                .name_ref_range(local_use.name_ref())
                .expect("use source")
                .range
                .start()
        })
        .collect::<Vec<_>>();
    assert!(starts.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(fixture.resolution.uses().len(), 6);
}

#[test]
fn local_reference_index_semantics_facade_includes_declaration_on_request() {
    let (host, file_id) = single_file_host(SEMANTICS_FACADE);
    let analysis = host.analysis();
    let semantics = analysis.semantics(file_id);
    let position = |marker| FilePosition::new(file_id, marker_offset(SEMANTICS_FACADE, marker));
    let LocalResolveResult::Resolved(value) =
        semantics.resolve_local_at(position("/*value_read*/"))
    else {
        panic!("value use must resolve")
    };

    let uses = semantics.local_uses(value);
    assert_eq!(uses.len(), 3);
    assert_eq!(
        uses.iter()
            .filter(|reference| reference.kind() == LocalUseKind::Write)
            .count(),
        1
    );
    assert_eq!(
        uses.iter()
            .filter(|reference| reference.kind() == LocalUseKind::Read)
            .count(),
        2
    );

    let without_declaration = semantics.local_references(value, false);
    let with_declaration = semantics.local_references(value, true);
    assert_eq!(without_declaration.len(), 3);
    assert_eq!(with_declaration.len(), 4);
    let declaration = semantics
        .local_definition(value)
        .expect("value declaration");
    assert!(!without_declaration.contains(&declaration));
    assert!(with_declaration.contains(&declaration));
    assert_eq!(
        semantics.local_references_at(position("/*value_def*/"), true),
        with_declaration
    );
    assert_eq!(
        semantics.local_references_at(position("/*value_read*/"), true),
        with_declaration
    );
}

#[test]
fn local_reference_index_partitions_shadowed_bindings() {
    let fixture = fixture(ORDERING, "ordering", DefKind::Function);
    let shadow = binding_at(&fixture, "/*shadow_def*/");
    let inner = binding_at(&fixture, "/*inner_def*/");
    let shadow_after = name_ref_at(&fixture, "/*shadow_after*/", NameRefKind::Path);
    let shadow_local = resolved(&fixture, shadow_after, shadow);
    let inner_after = name_ref_at(&fixture, "/*inner_after*/", NameRefKind::Path);
    let inner_local = resolved(&fixture, inner_after, inner);

    let shadow_refs = fixture
        .resolution
        .uses_for(shadow_local)
        .map(|local_use| local_use.name_ref())
        .collect::<Vec<_>>();
    for marker in [
        "/*shadow_after*/",
        "/*inner_before*/",
        "/*inner_rhs*/",
        "/*outer_after*/",
    ] {
        assert!(shadow_refs.contains(&name_ref_at(&fixture, marker, NameRefKind::Path)));
    }
    assert_eq!(shadow_refs.len(), 4);
    let inner_uses = fixture.resolution.uses_for(inner_local).collect::<Vec<_>>();
    assert_eq!(inner_uses.len(), 1);
    assert_eq!(inner_uses[0].name_ref(), inner_after);
}

#[test]
fn local_reference_index_filters_shorthand_members_and_multipaths() {
    let fixture = fixture(LOCAL_KINDS, "kinds", DefKind::Method);
    let for_item = binding_at(&fixture, "/*for_item_def*/");
    let short_path = name_ref_at(&fixture, "/*short_field*/", NameRefKind::Path);
    let short_label = name_ref_at(&fixture, "/*short_field*/", NameRefKind::StructField);
    let local = resolved(&fixture, short_path, for_item);
    assert_use(&fixture, local, short_path, LocalUseKind::Read);
    assert_non_local(&fixture, short_label);
    assert!(
        !fixture
            .resolution
            .uses_for(local)
            .any(|local_use| local_use.name_ref() == short_label)
    );
    assert!(
        fixture
            .resolution
            .uses_for(local)
            .all(|local_use| fixture.body[local_use.name_ref()].kind() == NameRefKind::Path)
    );

    for (marker, kind) in [
        ("/*field_name*/", NameRefKind::Field),
        ("/*method_name*/", NameRefKind::Method),
        ("/*module_self*/", NameRefKind::StructPath),
        ("/*macro_name*/", NameRefKind::Macro),
    ] {
        let name_ref = name_ref_at(&fixture, marker, kind);
        assert_non_local(&fixture, name_ref);
        assert!(
            !fixture
                .resolution
                .uses()
                .iter()
                .any(|local_use| local_use.name_ref() == name_ref)
        );
    }
}

#[test]
fn local_reference_index_is_keyed_by_body_owner() {
    const SOURCE: &str = r#"
fn first() -> i64 { let /*first_def*/shared = 1; /*first_use*/shared }
fn second() -> i64 { let /*second_def*/shared = 2; /*second_use*/shared }
"#;
    let (host, file_id) = single_file_host(SOURCE);
    let analysis = host.analysis();
    let first_owner = body_owner(&analysis, file_id, "first", DefKind::Function);
    let second_owner = body_owner(&analysis, file_id, "second", DefKind::Function);

    let make_fixture = |owner| Fixture {
        source: SOURCE,
        file_id,
        body: analysis.body(owner).expect("owner body"),
        source_map: analysis.body_source_map(owner).expect("owner source map"),
        scopes: analysis.body_scopes(owner).expect("owner body scopes"),
        resolution: analysis
            .body_resolution(owner)
            .expect("owner body resolution"),
    };
    let first = make_fixture(first_owner);
    let second = make_fixture(second_owner);
    let first_binding = binding_at(&first, "/*first_def*/");
    let second_binding = binding_at(&second, "/*second_def*/");
    let first_ref = name_ref_at(&first, "/*first_use*/", NameRefKind::Path);
    let second_ref = name_ref_at(&second, "/*second_use*/", NameRefKind::Path);
    assert_eq!(first_binding.index(), second_binding.index());
    assert_eq!(first_ref.index(), second_ref.index());

    let first_local = resolved(&first, first_ref, first_binding);
    let second_local = resolved(&second, second_ref, second_binding);
    assert_ne!(first_local, second_local);
    assert_eq!(first_local.owner(), BodyId::new(first_owner));
    assert_eq!(second_local.owner(), BodyId::new(second_owner));
    assert_eq!(first.resolution.uses_for(first_local).count(), 1);
    assert_eq!(second.resolution.uses_for(second_local).count(), 1);

    let first_use = first
        .resolution
        .uses_for(first_local)
        .next()
        .expect("first local use");
    let first_range = first
        .source_map
        .name_ref_range(first_use.name_ref())
        .expect("first use range");
    let second_use = second
        .resolution
        .uses_for(second_local)
        .next()
        .expect("second local use");
    let second_range = second
        .source_map
        .name_ref_range(second_use.name_ref())
        .expect("second use range");
    assert!(
        first
            .source_map
            .body_range()
            .range
            .contains_range(first_range.range)
    );
    assert!(
        second
            .source_map
            .body_range()
            .range
            .contains_range(second_range.range)
    );
    assert_ne!(first_range, second_range);
}
