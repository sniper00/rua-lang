use std::sync::Arc;

use rua_analysis::{
    Analysis, AnalysisHost, BindingId, Body, BodySourceMap, BuiltinMemberId, Change, DefId,
    DefKind, DefMap, Expr, ExprId, FileId, FileKind, InferenceDiagnostic, InferenceResult,
    ItemSignature, MemberIndex, MemberKind, MemberResolution, MemberTarget, NameRefId, NameRefKind,
    NamedTy, SourceRootId, SourceRootKind, Ty,
};

struct Fixture {
    source: &'static str,
    file_id: FileId,
    body: Arc<Body>,
    source_map: Arc<BodySourceMap>,
    inference: Arc<InferenceResult>,
    member_index: Arc<MemberIndex>,
    def_map: Arc<DefMap>,
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

fn definition(analysis: &Analysis, root_file: FileId, name: &str, kind: DefKind) -> DefId {
    analysis
        .def_map(root_file)
        .definitions()
        .find(|definition| definition.name() == name && definition.kind() == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} definition `{name}`"))
        .id()
}

fn member_definition(analysis: &Analysis, root_file: FileId, owner: DefId, name: &str) -> DefId {
    analysis
        .def_map(root_file)
        .members(owner)
        .find(|definition| definition.name() == name)
        .unwrap_or_else(|| panic!("missing member `{name}` on {owner:?}"))
        .id()
}

fn mapped_definition(def_map: &DefMap, name: &str, kind: DefKind) -> DefId {
    def_map
        .definitions()
        .find(|definition| definition.name() == name && definition.kind() == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} definition `{name}`"))
        .id()
}

fn mapped_member(def_map: &DefMap, owner: DefId, name: &str) -> DefId {
    def_map
        .members(owner)
        .find(|definition| definition.name() == name)
        .unwrap_or_else(|| panic!("missing member `{name}` on {owner:?}"))
        .id()
}

fn fixture(source: &'static str, owner_name: &str) -> Fixture {
    fixture_with_kind(source, owner_name, DefKind::Function)
}

fn fixture_with_kind(source: &'static str, owner_name: &str, owner_kind: DefKind) -> Fixture {
    let (host, file_id) = single_file_host(source);
    let analysis = host.analysis();
    assert!(
        analysis.parse(file_id).errors().is_empty(),
        "fixture must parse: {:?}",
        analysis.parse(file_id).errors()
    );
    let owner = definition(&analysis, file_id, owner_name, owner_kind);
    Fixture {
        source,
        file_id,
        body: analysis.body(owner).expect("fixture body"),
        source_map: analysis
            .body_source_map(owner)
            .expect("fixture body source map"),
        inference: analysis.infer(owner).expect("fixture inference result"),
        member_index: analysis.member_index(file_id),
        def_map: analysis.def_map(file_id),
    }
}

fn marker_offset(source: &str, marker: &str) -> u32 {
    u32::try_from(source.find(marker).expect("fixture marker") + marker.len())
        .expect("fixture offset fits u32")
}

fn name_ref_at(fixture: &Fixture, marker: &str, kind: NameRefKind) -> NameRefId {
    let offset = marker_offset(fixture.source, marker);
    fixture
        .body
        .name_refs()
        .find_map(|(id, name_ref)| {
            fixture
                .source_map
                .name_ref_range(id)
                .filter(|range| {
                    range.file_id == fixture.file_id
                        && range.range.start() == offset
                        && name_ref.kind() == kind
                })
                .map(|_| id)
        })
        .unwrap_or_else(|| panic!("no {kind:?} name reference at {marker}"))
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

fn member_expr_at(fixture: &Fixture, marker: &str, kind: NameRefKind) -> ExprId {
    let name_ref = name_ref_at(fixture, marker, kind);
    fixture
        .body
        .exprs()
        .find_map(|(id, expr)| match expr {
            Expr::MethodCall { method, .. } if *method == name_ref => Some(id),
            Expr::Field { field, .. } if *field == name_ref => Some(id),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no member expression at {marker}"))
}

fn call_for_name_ref(fixture: &Fixture, name_ref: NameRefId) -> ExprId {
    fixture
        .body
        .exprs()
        .find_map(|(id, expr)| {
            let Expr::Call { callee, .. } = expr else {
                return None;
            };
            let Expr::Path(path) = &fixture.body[*callee] else {
                return None;
            };
            path.contains(&name_ref).then_some(id)
        })
        .unwrap_or_else(|| panic!("no call for name reference {name_ref:?}"))
}

fn resolution_at<'a>(
    fixture: &'a Fixture,
    marker: &str,
    kind: NameRefKind,
) -> &'a MemberResolution {
    let name_ref = name_ref_at(fixture, marker, kind);
    fixture
        .inference
        .member_resolution(name_ref)
        .unwrap_or_else(|| panic!("no member resolution at {marker}"))
}

fn assert_named(ty: &Ty, definition: DefId, args: &[Ty]) {
    let Ty::Named(named) = ty else {
        panic!("expected named type, got {ty}");
    };
    assert_eq!(named.definition(), definition);
    assert_eq!(named.args(), args);
}

fn assert_definition_target(resolution: &MemberResolution, target: DefId, kind: MemberKind) {
    assert_eq!(resolution.target(), MemberTarget::Definition(target));
    assert_eq!(resolution.kind(), kind);
}

#[test]
fn adt_inference_instantiates_generic_struct_fields_and_enum_variants() {
    const SOURCE: &str = r#"
struct Wrapper<T> {
    value: T,
}

enum Message<T> {
    Empty,
    Value(T),
}

fn adt_case() -> i64 {
    let wrapped = /*wrapper_literal*/Wrapper { /*literal_field*/value: 7 };
    let extracted = wrapped./*field_use*/value;
    let message = Message::/*variant_use*/Value(extracted);
    let empty: Message<i64> = Message::Empty;
    extracted
}
"#;
    let fixture = fixture(SOURCE, "adt_case");
    let wrapper = mapped_definition(&fixture.def_map, "Wrapper", DefKind::Struct);
    let message = mapped_definition(&fixture.def_map, "Message", DefKind::Enum);
    let wrapper_field = mapped_member(&fixture.def_map, wrapper, "value");
    let value_variant = mapped_member(&fixture.def_map, message, "Value");

    let literal = expr_at(&fixture, "/*wrapper_literal*/", |expr| {
        matches!(expr, Expr::StructLiteral { .. })
    });
    assert_named(
        fixture
            .inference
            .type_of_expr(literal)
            .expect("struct literal type"),
        wrapper,
        &[Ty::I64],
    );

    let literal_field = resolution_at(&fixture, "/*literal_field*/", NameRefKind::StructField);
    assert_definition_target(literal_field, wrapper_field, MemberKind::Field);
    assert_eq!(literal_field.ty(), &Ty::I64);
    assert!(!literal_field.substitution().is_empty());

    let field_use = resolution_at(&fixture, "/*field_use*/", NameRefKind::Field);
    assert_definition_target(field_use, wrapper_field, MemberKind::Field);
    assert_eq!(field_use.ty(), &Ty::I64);
    let field_expr = member_expr_at(&fixture, "/*field_use*/", NameRefKind::Field);
    assert_eq!(fixture.inference.type_of_expr(field_expr), Some(&Ty::I64));

    let variant_name_ref = name_ref_at(&fixture, "/*variant_use*/", NameRefKind::Path);
    let variant = fixture
        .inference
        .member_resolution(variant_name_ref)
        .expect("variant member resolution");
    assert_definition_target(variant, value_variant, MemberKind::Variant);
    let variant_expr = call_for_name_ref(&fixture, variant_name_ref);
    assert_named(
        fixture
            .inference
            .type_of_expr(variant_expr)
            .expect("variant constructor type"),
        message,
        &[Ty::I64],
    );
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn adt_inference_index_substitutes_generic_impls_and_struct_variant_fields() {
    const SOURCE: &str = r#"
struct Box<T> {
    value: T,
}

impl<T> Box<T> {
    fn get(&self) -> T { self.value }
}

impl Box<i64> {
    fn from_i64(value: i64) -> Box<i64> { Box { value } }
}

enum Packet<T> {
    Named { payload: T },
}

fn index_owner() {}
"#;
    let (host, file_id) = single_file_host(SOURCE);
    let analysis = host.analysis();
    assert!(analysis.parse(file_id).errors().is_empty());
    let def_map = analysis.def_map(file_id);
    let index = analysis.member_index(file_id);
    let boxed = mapped_definition(&def_map, "Box", DefKind::Struct);
    let value = mapped_member(&def_map, boxed, "value");
    let implementation = mapped_definition(&def_map, "impl Box<T>", DefKind::Impl);
    let ItemSignature::Impl(impl_signature) = def_map
        .definition(implementation)
        .expect("generic impl definition")
        .signature()
    else {
        panic!("expected impl signature");
    };
    assert_eq!(impl_signature.target_type().syntax(), Some("Box<T>"));
    let get = mapped_member(&def_map, implementation, "get");
    let packet = mapped_definition(&def_map, "Packet", DefKind::Enum);
    let named = mapped_member(&def_map, packet, "Named");
    let payload = mapped_member(&def_map, named, "payload");

    let box_i64 = Ty::Named(NamedTy::new(boxed, "Box", vec![Ty::I64]));
    let field = index
        .resolve_field(&box_i64, "value")
        .expect("generic field");
    assert_definition_target(&field, value, MemberKind::Field);
    assert_eq!(field.ty(), &Ty::I64);
    assert!(!field.substitution().is_empty());
    let method = index
        .resolve_method(&box_i64, "get")
        .expect("generic impl method");
    assert_definition_target(&method, get, MemberKind::Method);
    assert_eq!(
        method.callable().expect("get callable").return_ty(),
        &Ty::I64
    );
    let specialized_impl = mapped_definition(&def_map, "impl Box<i64>", DefKind::Impl);
    let from_i64 = mapped_member(&def_map, specialized_impl, "from_i64");
    let associated = index
        .resolve_associated(boxed, "from_i64")
        .expect("unique specialized associated function");
    assert_definition_target(&associated, from_i64, MemberKind::AssociatedFunction);
    assert_eq!(
        associated
            .callable()
            .expect("specialized associated callable")
            .return_ty(),
        &box_i64
    );

    let packet_i64 = Ty::Named(NamedTy::new(packet, "Packet", vec![Ty::I64]));
    let variant_field = index
        .resolve_variant_field(named, &packet_i64, "payload")
        .expect("generic struct variant field");
    assert_definition_target(&variant_field, payload, MemberKind::Field);
    assert_eq!(variant_field.ty(), &Ty::I64);
}

#[test]
fn member_lookup_resolves_self_fields_methods_and_associated_functions() {
    const SOURCE: &str = r#"
struct Counter {
    value: i64,
}

impl Counter {
    fn make(value: i64) -> Counter {
        Counter { value }
    }

    fn increment(&mut self, amount: i64) {
        self.value = self.value + amount;
    }

    fn read(&self) -> i64 {
        self.value
    }

    fn exercise(&mut /*self_def*/self) -> i64 {
        self./*self_method*/increment(1);
        let other = Counter::/*assoc_use*/make(self./*self_field*/value);
        other./*method_use*/read()
    }
}
"#;
    let fixture = fixture_with_kind(SOURCE, "exercise", DefKind::Method);
    let counter = mapped_definition(&fixture.def_map, "Counter", DefKind::Struct);
    let field = mapped_member(&fixture.def_map, counter, "value");
    let implementation = mapped_definition(&fixture.def_map, "impl Counter", DefKind::Impl);
    let make = mapped_member(&fixture.def_map, implementation, "make");
    let increment = mapped_member(&fixture.def_map, implementation, "increment");
    let read = mapped_member(&fixture.def_map, implementation, "read");

    assert_named(
        fixture
            .inference
            .type_of_binding(binding_at(&fixture, "/*self_def*/"))
            .expect("self binding type"),
        counter,
        &[],
    );
    assert_definition_target(
        resolution_at(&fixture, "/*self_method*/", NameRefKind::Method),
        increment,
        MemberKind::Method,
    );
    assert_definition_target(
        resolution_at(&fixture, "/*self_field*/", NameRefKind::Field),
        field,
        MemberKind::Field,
    );
    assert_definition_target(
        resolution_at(&fixture, "/*assoc_use*/", NameRefKind::Path),
        make,
        MemberKind::AssociatedFunction,
    );
    assert_definition_target(
        resolution_at(&fixture, "/*method_use*/", NameRefKind::Method),
        read,
        MemberKind::Method,
    );

    let counter_ty = Ty::Named(NamedTy::new(counter, "Counter", Vec::new()));
    assert_eq!(
        fixture
            .member_index
            .resolve_field(&counter_ty, "value")
            .expect("field index resolution")
            .target(),
        MemberTarget::Definition(field)
    );
    assert_eq!(
        fixture
            .member_index
            .resolve_method(&counter_ty, "read")
            .expect("method index resolution")
            .target(),
        MemberTarget::Definition(read)
    );
    assert_eq!(
        fixture
            .member_index
            .resolve_associated_ty(&counter_ty, "make")
            .expect("associated index resolution")
            .target(),
        MemberTarget::Definition(make)
    );
    let candidates = fixture.member_index.instance_candidates(&counter_ty);
    for expected in ["value", "increment", "read", "exercise"] {
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.name() == expected),
            "missing completion candidate `{expected}`"
        );
    }
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn member_lookup_trait_default_body_resolves_trait_self_methods() {
    const SOURCE: &str = r#"
trait Arithmetic {
    fn base(&self) -> i64;
    fn twice(&self) -> i64 { self./*default_base*/base() * 2 }
}
"#;
    let fixture = fixture_with_kind(SOURCE, "twice", DefKind::Method);
    let arithmetic = mapped_definition(&fixture.def_map, "Arithmetic", DefKind::Trait);
    let base = mapped_member(&fixture.def_map, arithmetic, "base");
    let resolution = resolution_at(&fixture, "/*default_base*/", NameRefKind::Method);
    assert_definition_target(resolution, base, MemberKind::Method);
    let call = member_expr_at(&fixture, "/*default_base*/", NameRefKind::Method);
    assert_eq!(fixture.inference.type_of_expr(call), Some(&Ty::I64));
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn generic_trait_inference_substitutes_method_generics_for_inline_and_where_bounds() {
    const SOURCE: &str = r#"
trait Identity {
    fn keep<U>(&self, value: U) -> U;
    fn label(&self) -> String { "identity" }
}

struct Holder<T> { value: T }
impl<T> Holder<T> where T: Identity {
    fn describe(&self) -> String { self./*holder_value*/value./*holder_label*/label() }
}

fn inline<T: Identity>(value: T) -> String {
    value./*inline_keep*/keep(value./*inline_label*/label())
}

fn where_clause<T>(value: T) -> i64
where
    T: Identity,
{
    value./*where_keep*/keep(7)
}


fn through_impl<T: Identity>(holder: Holder<T>) -> String {
    holder./*impl_bound_method*/describe()
}
"#;
    let inline = fixture(SOURCE, "inline");
    let where_clause = fixture(SOURCE, "where_clause");
    let through_impl = fixture(SOURCE, "through_impl");
    let describe_body = fixture_with_kind(SOURCE, "describe", DefKind::Method);
    let identity = mapped_definition(&inline.def_map, "Identity", DefKind::Trait);
    let keep = mapped_member(&inline.def_map, identity, "keep");
    let label = mapped_member(&inline.def_map, identity, "label");

    let inline_keep = resolution_at(&inline, "/*inline_keep*/", NameRefKind::Method);
    assert_definition_target(inline_keep, keep, MemberKind::Method);
    assert_eq!(inline_keep.generic_params().len(), 1);
    assert_definition_target(
        resolution_at(&inline, "/*inline_label*/", NameRefKind::Method),
        label,
        MemberKind::Method,
    );
    let inline_call = member_expr_at(&inline, "/*inline_keep*/", NameRefKind::Method);
    let inline_call_info = inline
        .inference
        .call_info(inline_call)
        .expect("inline method call info");
    assert_eq!(inline_call_info.return_type(), &Ty::STRING);
    assert!(
        inline_call_info
            .substitution()
            .iter()
            .any(|(_, ty)| ty == &Ty::STRING),
        "method generic must be inferred from its argument"
    );
    assert_eq!(
        inline.inference.type_of_expr(inline_call),
        Some(&Ty::STRING)
    );

    let where_keep = resolution_at(&where_clause, "/*where_keep*/", NameRefKind::Method);
    assert_definition_target(where_keep, keep, MemberKind::Method);
    let where_call = member_expr_at(&where_clause, "/*where_keep*/", NameRefKind::Method);
    let where_call_info = where_clause
        .inference
        .call_info(where_call)
        .expect("where-bound method call info");
    assert_eq!(where_call_info.return_type(), &Ty::I64);
    assert!(
        where_call_info
            .substitution()
            .iter()
            .any(|(_, ty)| ty == &Ty::I64)
    );
    assert_eq!(
        where_clause.inference.type_of_expr(where_call),
        Some(&Ty::I64)
    );
    let holder_impl = mapped_definition(&through_impl.def_map, "impl Holder<T>", DefKind::Impl);
    let describe = mapped_member(&through_impl.def_map, holder_impl, "describe");
    assert_definition_target(
        resolution_at(&through_impl, "/*impl_bound_method*/", NameRefKind::Method),
        describe,
        MemberKind::Method,
    );
    let holder = mapped_definition(&describe_body.def_map, "Holder", DefKind::Struct);
    let holder_value = mapped_member(&describe_body.def_map, holder, "value");
    let identity = mapped_definition(&describe_body.def_map, "Identity", DefKind::Trait);
    let identity_label = mapped_member(&describe_body.def_map, identity, "label");
    assert_definition_target(
        resolution_at(&describe_body, "/*holder_value*/", NameRefKind::Field),
        holder_value,
        MemberKind::Field,
    );
    assert_definition_target(
        resolution_at(&describe_body, "/*holder_label*/", NameRefKind::Method),
        identity_label,
        MemberKind::Method,
    );
    assert!(inline.inference.diagnostics().is_empty());
    assert!(where_clause.inference.diagnostics().is_empty());
    assert!(through_impl.inference.diagnostics().is_empty());
    assert!(describe_body.inference.diagnostics().is_empty());
}

#[test]
fn generic_trait_inference_merges_defaults_overrides_and_preserves_ambiguity() {
    const SOURCE: &str = r#"
trait Named {
    fn label(&self) -> String { "default" }
}

trait Left {
    fn clash(&self) -> i64 { 1 }
}

trait Right {
    fn clash(&self) -> i64 { 2 }
}

struct Defaulted { value: i64 }
struct Overridden { value: i64 }
struct Ambiguous { value: i64 }

impl Named for Defaulted {}

impl Named for Overridden {
    fn label(&self) -> String { "override" }
}

impl Left for Ambiguous {}
impl Right for Ambiguous {}

fn index_owner() {}
"#;
    let (host, file_id) = single_file_host(SOURCE);
    let analysis = host.analysis();
    assert!(analysis.parse(file_id).errors().is_empty());
    let index = analysis.member_index(file_id);
    let named_trait = definition(&analysis, file_id, "Named", DefKind::Trait);
    let trait_label = member_definition(&analysis, file_id, named_trait, "label");
    let defaulted = definition(&analysis, file_id, "Defaulted", DefKind::Struct);
    let overridden = definition(&analysis, file_id, "Overridden", DefKind::Struct);
    let ambiguous = definition(&analysis, file_id, "Ambiguous", DefKind::Struct);
    let overridden_impl = definition(
        &analysis,
        file_id,
        "impl Named for Overridden",
        DefKind::Impl,
    );
    let override_label = member_definition(&analysis, file_id, overridden_impl, "label");

    let defaulted_ty = Ty::Named(NamedTy::new(defaulted, "Defaulted", Vec::new()));
    let overridden_ty = Ty::Named(NamedTy::new(overridden, "Overridden", Vec::new()));
    let ambiguous_ty = Ty::Named(NamedTy::new(ambiguous, "Ambiguous", Vec::new()));
    assert_eq!(
        index
            .resolve_method(&defaulted_ty, "label")
            .expect("trait default")
            .target(),
        MemberTarget::Definition(trait_label)
    );
    assert_eq!(
        index
            .resolve_method(&overridden_ty, "label")
            .expect("trait override")
            .target(),
        MemberTarget::Definition(override_label)
    );
    assert!(index.resolve_method(&ambiguous_ty, "clash").is_none());
    assert_eq!(
        index
            .method_candidates(&ambiguous_ty)
            .iter()
            .filter(|candidate| candidate.name() == "clash")
            .count(),
        2,
        "ambiguous trait candidates must be retained without guessing"
    );
}

#[test]
fn generic_trait_inference_reports_unsatisfied_free_and_method_bounds() {
    const SOURCE: &str = r#"
trait Show { fn show(&self) -> String; }

struct Displayed {}
impl Show for Displayed { fn show(&self) -> String { "ok" } }

struct Plain {}
struct Registry {}
impl Registry {
    fn store<T: Show>(&self, value: T) { value; }
}

fn require<T>(value: T) where T: Show { value; }

fn bound_calls() {
    let registry = Registry {};
    require(Displayed {});
    registry.store(Displayed {});
    require(Plain {});
    registry.store(Plain {});
}
"#;
    let fixture = fixture(SOURCE, "bound_calls");
    let show = mapped_definition(&fixture.def_map, "Show", DefKind::Trait);
    let plain = mapped_definition(&fixture.def_map, "Plain", DefKind::Struct);
    let failures = fixture
        .inference
        .diagnostics()
        .iter()
        .filter(|diagnostic| {
            matches!(
                diagnostic,
                InferenceDiagnostic::UnsatisfiedTraitBound {
                    actual: Ty::Named(named),
                    trait_id,
                    ..
                } if named.definition() == plain && *trait_id == show
            )
        })
        .count();
    assert_eq!(
        failures, 2,
        "free and method generic bounds must both be checked"
    );
    assert_eq!(
        fixture.inference.diagnostics().len(),
        2,
        "implemented bounds must not produce false positives: {:?}",
        fixture.inference.diagnostics()
    );
    assert!(!ruac::check_diags(SOURCE).0.is_empty());
}

#[test]
fn generic_trait_inference_method_local_where_does_not_hide_siblings() {
    const SOURCE: &str = r#"
trait Show { fn show(&self) -> String; }
struct Plain {}
struct Bucket<T> { value: T }
impl<T> Bucket<T> {
    fn always(&self) -> i64 { 1 }
    fn constrained(&self) where T: Show {
        self./*constrained_value*/value./*constrained_show*/show();
    }
}
fn index_owner() {}
"#;
    let (host, file_id) = single_file_host(SOURCE);
    let analysis = host.analysis();
    assert!(analysis.parse(file_id).errors().is_empty());
    let map = analysis.def_map(file_id);
    let index = analysis.member_index(file_id);
    let bucket = mapped_definition(&map, "Bucket", DefKind::Struct);
    let plain = mapped_definition(&map, "Plain", DefKind::Struct);
    let implementation = mapped_definition(&map, "impl Bucket<T>", DefKind::Impl);
    let always = mapped_member(&map, implementation, "always");
    let receiver = Ty::Named(NamedTy::new(
        bucket,
        "Bucket",
        vec![Ty::Named(NamedTy::new(plain, "Plain", Vec::new()))],
    ));
    let resolution = index
        .resolve_method(&receiver, "always")
        .expect("a method-local bound must not constrain sibling methods");
    assert_definition_target(&resolution, always, MemberKind::Method);

    let constrained = fixture_with_kind(SOURCE, "constrained", DefKind::Method);
    let bucket = mapped_definition(&constrained.def_map, "Bucket", DefKind::Struct);
    let value = mapped_member(&constrained.def_map, bucket, "value");
    let show_trait = mapped_definition(&constrained.def_map, "Show", DefKind::Trait);
    let show = mapped_member(&constrained.def_map, show_trait, "show");
    let constrained_method =
        mapped_definition(&constrained.def_map, "constrained", DefKind::Method);
    assert_definition_target(
        resolution_at(&constrained, "/*constrained_value*/", NameRefKind::Field),
        value,
        MemberKind::Field,
    );
    assert_definition_target(
        resolution_at(&constrained, "/*constrained_show*/", NameRefKind::Method),
        show,
        MemberKind::Method,
    );
    let generic_receiver =
        resolution_at(&constrained, "/*constrained_value*/", NameRefKind::Field).ty();
    assert!(
        constrained
            .member_index
            .method_candidates_in(generic_receiver, constrained_method)
            .iter()
            .any(|candidate| candidate.target() == MemberTarget::Definition(show)),
        "scoped completion must see methods supplied by the callable where bound"
    );
    assert!(constrained.inference.diagnostics().is_empty());
}

#[test]
fn generic_trait_inference_proves_recursive_generic_impl_requirements() {
    const SOURCE: &str = r#"
trait Base {}
trait Wrap {}
trait Outer { fn out(&self) -> i64; }
struct Holder<T> { value: T }
struct Envelope<T> { value: T }
impl<T: Base> Wrap for Holder<T> {}
impl<T: Wrap> Outer for Envelope<T> {
    fn out(&self) -> i64 { 1 }
}
fn nested<T: Base>(value: Envelope<Holder<T>>) -> i64 {
    value./*recursive_bound*/out()
}
"#;
    let fixture = fixture(SOURCE, "nested");
    let implementation = mapped_definition(
        &fixture.def_map,
        "impl Outer for Envelope<T>",
        DefKind::Impl,
    );
    let out = mapped_member(&fixture.def_map, implementation, "out");
    assert_definition_target(
        resolution_at(&fixture, "/*recursive_bound*/", NameRefKind::Method),
        out,
        MemberKind::Method,
    );
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn member_lookup_builtin_metadata_covers_core_containers_and_strings() {
    const SOURCE: &str = "fn builtin_index_owner() {}\n";
    let (host, file_id) = single_file_host(SOURCE);
    let index = host.analysis().member_index(file_id);

    let vec_ty = Ty::Vec(Box::new(Ty::I64));
    let vec_get = index.resolve_method(&vec_ty, "get").expect("Vec::get");
    assert_eq!(
        vec_get.target(),
        MemberTarget::Builtin(BuiltinMemberId::VecGet)
    );
    assert_eq!(vec_get.kind(), MemberKind::Method);
    assert_eq!(
        vec_get.callable().expect("Vec::get callable").return_ty(),
        &Ty::Option(Box::new(Ty::I64))
    );
    assert_eq!(
        index
            .resolve_associated_ty(&Ty::Vec(Box::new(Ty::Unknown)), "new")
            .expect("Vec::new")
            .target(),
        MemberTarget::Builtin(BuiltinMemberId::VecNew)
    );

    let map_ty = Ty::HashMap(Box::new(Ty::STRING), Box::new(Ty::I64));
    let map_get = index.resolve_method(&map_ty, "get").expect("HashMap::get");
    assert_eq!(
        map_get.target(),
        MemberTarget::Builtin(BuiltinMemberId::HashMapGet)
    );
    assert_eq!(
        map_get
            .callable()
            .expect("HashMap::get callable")
            .return_ty(),
        &Ty::Option(Box::new(Ty::I64))
    );

    let uppercase = index
        .resolve_method(&Ty::STRING, "to_uppercase")
        .expect("String::to_uppercase");
    assert_eq!(
        uppercase.target(),
        MemberTarget::Builtin(BuiltinMemberId::StringToUppercase)
    );
    assert_eq!(
        uppercase
            .callable()
            .expect("String::to_uppercase callable")
            .return_ty(),
        &Ty::STRING
    );

    let option_names = index
        .associated_candidates(&Ty::Option(Box::new(Ty::I64)))
        .into_iter()
        .map(|candidate| candidate.name().to_string())
        .collect::<Vec<_>>();
    assert_eq!(option_names, ["None", "Some"]);
    assert_eq!(
        index
            .resolve_associated_ty(&Ty::Option(Box::new(Ty::I64)), "Some")
            .expect("Option::Some")
            .target(),
        MemberTarget::Builtin(BuiltinMemberId::OptionSome)
    );
    let option_methods = index
        .instance_candidates(&Ty::Option(Box::new(Ty::I64)))
        .into_iter()
        .map(|candidate| candidate.name().to_string())
        .collect::<Vec<_>>();
    assert_eq!(option_methods, ["map"]);
    assert_eq!(
        index
            .resolve_method(&Ty::Option(Box::new(Ty::I64)), "map")
            .expect("Option::map")
            .target(),
        MemberTarget::Builtin(BuiltinMemberId::OptionMap)
    );

    let result_names = index
        .associated_candidates(&Ty::Result(Box::new(Ty::I64), Box::new(Ty::STRING)))
        .into_iter()
        .map(|candidate| candidate.name().to_string())
        .collect::<Vec<_>>();
    assert_eq!(result_names, ["Err", "Ok"]);
    assert_eq!(
        index
            .resolve_associated_ty(&Ty::Result(Box::new(Ty::I64), Box::new(Ty::STRING)), "Err",)
            .expect("Result::Err")
            .target(),
        MemberTarget::Builtin(BuiltinMemberId::ResultErr)
    );
}

#[test]
fn member_lookup_builtin_calls_feed_inference_and_resolution_facts() {
    const SOURCE: &str = r#"
fn builtin_calls() -> i64 {
    let mut values = vec![1, 2];
    values./*vec_push*/push(3);
    let length = values./*vec_len*/len();
    let upper = "rua"./*string_upper*/to_uppercase();
    let option: Option<i64> = Option::Some(length);
    let mapped = option./*option_map*/map(|value| value > 0);
    let mut map = HashMap::/*map_new*/new();
    map./*map_insert*/insert(upper, length);
    map./*map_len*/len()
}
"#;
    let fixture = fixture(SOURCE, "builtin_calls");
    for (marker, target, ty) in [
        ("/*vec_push*/", BuiltinMemberId::VecPush, Ty::UNIT),
        ("/*vec_len*/", BuiltinMemberId::VecLen, Ty::I64),
        (
            "/*string_upper*/",
            BuiltinMemberId::StringToUppercase,
            Ty::STRING,
        ),
        (
            "/*option_map*/",
            BuiltinMemberId::OptionMap,
            Ty::Option(Box::new(Ty::BOOL)),
        ),
        ("/*map_insert*/", BuiltinMemberId::HashMapInsert, Ty::UNIT),
        ("/*map_len*/", BuiltinMemberId::HashMapLen, Ty::I64),
    ] {
        let resolution = resolution_at(&fixture, marker, NameRefKind::Method);
        assert_eq!(resolution.target(), MemberTarget::Builtin(target));
        assert_eq!(resolution.kind(), MemberKind::Method);
        let expr = member_expr_at(&fixture, marker, NameRefKind::Method);
        assert_eq!(fixture.inference.type_of_expr(expr), Some(&ty));
    }
    let map_new = resolution_at(&fixture, "/*map_new*/", NameRefKind::Path);
    assert_eq!(
        map_new.target(),
        MemberTarget::Builtin(BuiltinMemberId::HashMapNew)
    );
    assert_eq!(map_new.kind(), MemberKind::AssociatedFunction);
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn member_lookup_wrong_arity_builtin_keeps_resolution_fact() {
    const SOURCE: &str = r#"
fn bad_builtin() {
    let map = HashMap::/*bad_new*/new(1);
    map;
}
"#;
    let fixture = fixture(SOURCE, "bad_builtin");
    let resolution = resolution_at(&fixture, "/*bad_new*/", NameRefKind::Path);
    assert_eq!(
        resolution.target(),
        MemberTarget::Builtin(BuiltinMemberId::HashMapNew)
    );
    assert!(
        fixture
            .inference
            .diagnostics()
            .iter()
            .any(|diagnostic| { matches!(diagnostic, InferenceDiagnostic::ArgumentCount { .. }) })
    );
}

#[test]
fn ruai_member_lookup_uses_declaration_fields_methods_and_associated_functions() {
    const MAIN: &str = r#"
mod api;

fn declaration_client() -> i64 {
    let remote = api::Remote::/*ruai_assoc*/make(7);
    let direct = remote./*ruai_field*/value;
    direct + remote./*ruai_method*/get()
}
"#;
    const API: &str = r#"
pub struct Remote {
    pub value: i64,
}

impl Remote {
    pub fn make(value: i64) -> Remote { Remote { value } }
    pub fn get(&self) -> i64 { self.value }
}
"#;
    let root_id = SourceRootId::new(0);
    let main_file = FileId::new(0);
    let declaration_file = FileId::new(1);
    let mut change = Change::new();
    change.set_source_root(root_id, SourceRootKind::Workspace);
    change.set_file_with_path(main_file, root_id, FileKind::Source, "main.rua", MAIN);
    change.set_file_with_path(
        declaration_file,
        root_id,
        FileKind::Declaration,
        "api.ruai",
        API,
    );
    let mut host = AnalysisHost::new();
    host.apply_change(change);
    let analysis = host.analysis();
    assert!(analysis.parse(main_file).errors().is_empty());
    assert!(analysis.parse(declaration_file).errors().is_empty());
    let owner = definition(
        &analysis,
        main_file,
        "declaration_client",
        DefKind::Function,
    );
    let fixture = Fixture {
        source: MAIN,
        file_id: main_file,
        body: analysis.body(owner).expect("declaration client body"),
        source_map: analysis
            .body_source_map(owner)
            .expect("declaration client source map"),
        inference: analysis.infer(owner).expect("declaration client inference"),
        member_index: analysis.member_index(main_file),
        def_map: analysis.def_map(main_file),
    };
    let remote = definition(&analysis, main_file, "Remote", DefKind::Struct);
    let remote_field = member_definition(&analysis, main_file, remote, "value");
    let implementation = definition(&analysis, main_file, "impl Remote", DefKind::Impl);
    let make = member_definition(&analysis, main_file, implementation, "make");
    let get = member_definition(&analysis, main_file, implementation, "get");

    for (marker, kind, target, member_kind) in [
        (
            "/*ruai_assoc*/",
            NameRefKind::Path,
            make,
            MemberKind::AssociatedFunction,
        ),
        (
            "/*ruai_field*/",
            NameRefKind::Field,
            remote_field,
            MemberKind::Field,
        ),
        (
            "/*ruai_method*/",
            NameRefKind::Method,
            get,
            MemberKind::Method,
        ),
    ] {
        let resolution = resolution_at(&fixture, marker, kind);
        assert_definition_target(resolution, target, member_kind);
        let source = analysis
            .def_map(main_file)
            .definition(target)
            .expect("cross-file target")
            .source();
        assert_eq!(source.name_range().file_id, declaration_file);
    }
    let root = fixture.body.root_expr();
    assert_eq!(fixture.inference.type_of_expr(root), Some(&Ty::I64));
    assert!(fixture.inference.diagnostics().is_empty());
}

#[test]
fn ruai_member_lookup_cache_invalidates_on_declaration_signature_change() {
    const MAIN: &str = "mod api; fn index_owner() {}\n";
    const BEFORE: &str = "pub struct Remote { pub value: i64 }\n";
    const AFTER: &str = "pub struct Remote { pub value: String }\n";
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
        "api.ruai",
        BEFORE,
    );
    let mut host = AnalysisHost::new();
    host.apply_change(load);

    let old_analysis = host.analysis();
    let old_map = old_analysis.def_map(main_file);
    let remote = mapped_definition(&old_map, "Remote", DefKind::Struct);
    let old_index = old_analysis.member_index(main_file);
    let remote_ty = Ty::Named(NamedTy::new(remote, "Remote", Vec::new()));
    assert_eq!(
        old_index
            .resolve_field(&remote_ty, "value")
            .expect("old declaration field")
            .ty(),
        &Ty::I64
    );

    let mut edit = Change::new();
    edit.set_file_text(declaration_file, AFTER);
    host.apply_change(edit);
    let new_analysis = host.analysis();
    let new_map = new_analysis.def_map(main_file);
    assert_eq!(
        mapped_definition(&new_map, "Remote", DefKind::Struct),
        remote,
        "declaration-only signature edits keep stable definition identity"
    );
    let new_index = new_analysis.member_index(main_file);
    assert!(!Arc::ptr_eq(&old_index, &new_index));
    assert_eq!(
        new_index
            .resolve_field(&remote_ty, "value")
            .expect("updated declaration field")
            .ty(),
        &Ty::STRING
    );
    assert_eq!(
        old_index
            .resolve_field(&remote_ty, "value")
            .expect("old snapshot remains isolated")
            .ty(),
        &Ty::I64
    );
}

#[test]
fn member_targets_resolve_to_exact_native_definitions() {
    const SOURCE: &str = r#"
struct Item {
    value: i64,
}

impl Item {
    fn read(&self) -> i64 { self.value }
}

fn parity() -> i64 {
    let item = Item { value: 7 };
    item./*field_parity*/value + item./*method_parity*/read()
}
"#;
    let fixture = fixture(SOURCE, "parity");
    let (compiler_diagnostics, _) = ruac::check_diags(SOURCE);
    assert!(compiler_diagnostics.is_empty());
    assert!(fixture.inference.diagnostics().is_empty());

    for (marker, name_ref_kind, native_kind, expected_definition) in [
        (
            "/*field_parity*/",
            NameRefKind::Field,
            MemberKind::Field,
            "value",
        ),
        (
            "/*method_parity*/",
            NameRefKind::Method,
            MemberKind::Method,
            "read",
        ),
    ] {
        let native = resolution_at(&fixture, marker, name_ref_kind);
        assert_eq!(native.kind(), native_kind);
        let MemberTarget::Definition(target) = native.target() else {
            panic!("user member must resolve to a definition");
        };
        let definition = fixture
            .def_map
            .definition(target)
            .expect("native target definition");
        let range = definition.name_range();
        assert_eq!(
            &SOURCE[range.start() as usize..range.end() as usize],
            expected_definition
        );
        assert_eq!(definition.file_id(), fixture.file_id);
    }
}
