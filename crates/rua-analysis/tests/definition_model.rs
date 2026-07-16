use rua_analysis::{
    AnalysisHost, Change, DefKind, DefMap, FileId, FileKind, ItemSignature, ItemSourceKind,
    ProjectData, ProjectId, ProjectRoot, ReceiverKind, SourceRootId, SourceRootKind, VariantKind,
};

fn single_file_host(source: &str, file_kind: FileKind) -> (AnalysisHost, FileId) {
    let root_id = SourceRootId::new(0);
    let file_id = FileId::new(0);
    let mut change = Change::new();
    change.set_source_root(root_id, SourceRootKind::Workspace);
    change.set_file_with_path(file_id, root_id, file_kind, "main.rua", source);
    let mut host = AnalysisHost::new();
    host.apply_change(change);
    (host, file_id)
}

#[test]
fn definition_identity_survives_body_and_unrelated_item_edits() {
    let original = "fn target(value: i64) -> i64 { value + 1 }\n";
    let (mut host, file_id) = single_file_host(original, FileKind::Source);
    let stable_file = FileId::new(1);
    let mut add_module = Change::new();
    add_module.set_file_with_path(
        stable_file,
        SourceRootId::new(0),
        FileKind::Source,
        "stable.rua",
        "fn child() {}\n",
    );
    host.apply_change(add_module);
    let before_snapshot = host.analysis();
    let before_map = before_snapshot.def_map(file_id);
    let before = before_map
        .resolve_name(before_map.root(), "target")
        .expect("target before edit");
    let id = before.id();
    let fingerprint = before.signature_fingerprint();
    let old_source = before.source();
    let stable_module = module(&before_map, "stable");
    let stable_child = before_map
        .resolve_name(stable_module, "child")
        .expect("stable module child")
        .id();

    let body_edit = concat!(
        "fn unrelated() {}\n",
        "fn target(value: i64) -> i64 { let doubled = value * 2; doubled }\n",
    );
    let mut change = Change::new();
    change.set_file_text(file_id, body_edit);
    host.apply_change(change);
    let body_snapshot = host.analysis();
    let body_map = body_snapshot.def_map(file_id);
    let after_body = body_map
        .resolve_name(body_map.root(), "target")
        .expect("target after body edit");

    assert_eq!(after_body.id(), id);
    assert_eq!(after_body.signature_fingerprint(), fingerprint);
    assert_ne!(after_body.source(), old_source);
    let stable_module_after = module(&body_map, "stable");
    assert_eq!(stable_module_after, stable_module);
    assert_eq!(
        body_map
            .resolve_name(stable_module_after, "child")
            .expect("stable child after edit")
            .id(),
        stable_child
    );
    assert_eq!(
        before_map.definition(id).expect("old snapshot").name(),
        "target"
    );

    let mut signature_edit = Change::new();
    signature_edit.set_file_text(
        file_id,
        concat!(
            "fn unrelated() {}\n",
            "fn target(value: String) -> i64 { 1 }\n",
        ),
    );
    host.apply_change(signature_edit);
    let signature_map = host.analysis().def_map(file_id);
    let after_signature = signature_map
        .resolve_name(signature_map.root(), "target")
        .expect("target after signature edit");
    assert_eq!(after_signature.id(), id);
    assert_ne!(after_signature.signature_fingerprint(), fingerprint);

    let mut deletion = Change::new();
    deletion.set_file_text(file_id, "fn replacement() {}\n");
    host.apply_change(deletion);
    let deletion_map = host.analysis().def_map(file_id);
    assert!(deletion_map.definition(id).is_none());
    assert_ne!(
        deletion_map
            .resolve_name(deletion_map.root(), "replacement")
            .expect("replacement")
            .id(),
        id
    );
}

#[test]
fn item_signature_records_generics_bounds_receivers_where_and_extern() {
    let source = concat!(
        "pub struct Box<T: Clone> where T: Named { pub value: T }\n",
        "pub enum Message<T> { Quit, Value(T), Named { value: T } }\n",
        "pub trait Read<T> where T: Clone {\n",
        "  fn borrow<U: Named>(&self, value: U) -> T where U: Clone;\n",
        "  fn take(self, value: T) -> T { value }\n",
        "}\n",
        "impl<T: Clone> Read<T> for Box<T> where T: Named {\n",
        "  pub fn replace(&mut self, value: T) -> T { value }\n",
        "}\n",
        "extern \"lua\" { pub fn format<F: Clone>(template: String, ...) -> String; }\n",
    );
    let (host, file_id) = single_file_host(source, FileKind::Declaration);
    let analysis = host.analysis();
    let tree = analysis.item_tree(file_id);

    let structure = tree
        .items()
        .iter()
        .find(|item| item.name() == "Box")
        .unwrap();
    let ItemSignature::Aggregate(signature) = structure.signature() else {
        panic!("struct aggregate signature")
    };
    assert_eq!(signature.generic_clause(), Some("<T:Clone>"));
    assert_eq!(signature.generic_params()[0].declaration(), "T:Clone");
    assert_eq!(
        signature.generic_params()[0].bounds()[0].syntax(),
        Some("Clone")
    );
    assert_eq!(signature.where_predicates()[0].declaration(), "T:Named");
    assert_eq!(signature.where_predicates()[0].target().syntax(), Some("T"));
    assert_eq!(
        signature.where_predicates()[0].bounds()[0].syntax(),
        Some("Named")
    );
    let ItemSignature::Field(field_type) = structure.children()[0].signature() else {
        panic!("field type")
    };
    assert_eq!(field_type.syntax(), Some("T"));

    let enumeration = tree
        .items()
        .iter()
        .find(|item| item.name() == "Message")
        .unwrap();
    let ItemSignature::Variant(tuple) = enumeration.children()[1].signature() else {
        panic!("tuple variant")
    };
    assert_eq!(tuple.kind(), VariantKind::Tuple);
    assert_eq!(tuple.tuple_types()[0].syntax(), Some("T"));
    let ItemSignature::Variant(record) = enumeration.children()[2].signature() else {
        panic!("record variant")
    };
    assert_eq!(record.kind(), VariantKind::Struct);

    let trait_item = tree
        .items()
        .iter()
        .find(|item| item.name() == "Read")
        .unwrap();
    let borrow = &trait_item.children()[0];
    let ItemSignature::Callable(borrow_signature) = borrow.signature() else {
        panic!("borrow signature")
    };
    assert_eq!(borrow_signature.receiver(), Some(ReceiverKind::SharedRef));
    assert_eq!(borrow_signature.generic_clause(), Some("<U:Named>"));
    assert_eq!(borrow.source_kind(), ItemSourceKind::TraitSignature);
    assert_eq!(
        trait_item.children()[1].source_kind(),
        ItemSourceKind::TraitDefault
    );

    let implementation = tree
        .items()
        .iter()
        .find(|item| item.kind() == rua_analysis::ItemKind::Impl)
        .unwrap();
    let ItemSignature::Callable(replace) = implementation.children()[0].signature() else {
        panic!("impl method")
    };
    assert_eq!(replace.receiver(), Some(ReceiverKind::MutRef));

    let external = tree
        .items()
        .iter()
        .find(|item| item.kind() == rua_analysis::ItemKind::ExternFunction)
        .unwrap();
    let ItemSignature::Callable(external) = external.signature() else {
        panic!("extern signature")
    };
    assert_eq!(external.abi(), Some("lua"));
    assert!(external.is_variadic());
    assert_eq!(external.generic_params()[0].name(), Some("F"));
    assert_eq!(
        external.generic_params()[0].bounds()[0].syntax(),
        Some("Clone")
    );

    let map = analysis.def_map(file_id);
    assert!(
        map.definitions()
            .all(|definition| definition.source_kind().file_kind() == FileKind::Declaration)
    );
}

#[test]
fn member_definition_map_covers_fields_variants_methods_impls_and_externs() {
    let source = concat!(
        "struct Point { x: i64 }\n",
        "enum Message { Ready, Move { amount: i64 } }\n",
        "trait Named { fn name(&self) -> String; }\n",
        "impl Named for Point { fn name(&self) -> String { \"point\" } }\n",
        "extern \"lua\" { fn clock() -> i64; }\n",
    );
    let (host, file_id) = single_file_host(source, FileKind::Source);
    let map = host.analysis().def_map(file_id);

    let point = map.resolve_name(map.root(), "Point").expect("Point");
    let field = map.resolve_member(point.id(), "x").expect("field x");
    assert_eq!(field.kind(), DefKind::Field);
    assert_eq!(field.owner(), Some(point.id()));
    assert_eq!(field.member_id().unwrap().def_id(), field.id());

    let message = map.resolve_name(map.root(), "Message").expect("Message");
    let ready = map.resolve_member(message.id(), "Ready").expect("Ready");
    let movement = map.resolve_member(message.id(), "Move").expect("Move");
    assert_eq!(ready.kind(), DefKind::Variant);
    assert_eq!(
        map.resolve_member(movement.id(), "amount")
            .expect("variant field")
            .kind(),
        DefKind::Field
    );

    let named = map.resolve_name(map.root(), "Named").expect("Named");
    assert_eq!(
        map.resolve_member(named.id(), "name")
            .expect("trait method")
            .kind(),
        DefKind::Method
    );
    let implementation = map
        .definitions()
        .find(|definition| definition.kind() == DefKind::Impl)
        .expect("impl block");
    assert_eq!(
        map.resolve_member(implementation.id(), "name")
            .expect("impl method")
            .kind(),
        DefKind::Method
    );
    assert!(
        map.resolve_name(map.root(), implementation.name())
            .is_none()
    );
    assert_eq!(
        map.resolve_name(map.root(), "clock")
            .expect("extern function")
            .kind(),
        DefKind::ExternFunction
    );

    for definition in map.definitions() {
        let source_map = map
            .definition_source(definition.id())
            .expect("every definition has a source entry");
        assert_eq!(source_map, definition.source());
        let name = &source[source_map.name_range().range.start() as usize
            ..source_map.name_range().range.end() as usize];
        if definition.kind() == DefKind::Chunk {
            assert!(name.is_empty());
        } else if definition.kind() != DefKind::Impl {
            assert_eq!(name, definition.name());
        }
    }
}

fn module(map: &DefMap, name: &str) -> rua_analysis::ModuleId {
    map.resolve_name(map.root(), name)
        .unwrap_or_else(|| panic!("module {name}"))
        .target_module()
        .expect("module target")
}

#[test]
fn module_resolution_uses_flat_directory_and_virtual_path_modules() {
    let root = SourceRootId::new(0);
    let main = FileId::new(0);
    let flat = FileId::new(1);
    let nested = FileId::new(2);
    let inline_child = FileId::new(3);
    let mut change = Change::new();
    change.set_source_root(root, SourceRootKind::Workspace);
    change.set_file_with_path(main, root, FileKind::Source, "src/main.rua", "");
    change.set_file_with_path(flat, root, FileKind::Source, "src/flat.rua", "");
    change.set_file_with_path(
        nested,
        root,
        FileKind::Source,
        "src/flat/nested.rua",
        "fn from_flat_child() {}",
    );
    change.set_file_with_path(
        inline_child,
        root,
        FileKind::Source,
        "src/inline/child.rua",
        "fn from_inline_child() {}",
    );
    let mut host = AnalysisHost::new();
    host.apply_change(change);
    let map = host.analysis().def_map(main);

    let flat_module = module(&map, "flat");
    let nested_module = map
        .resolve_name(flat_module, "nested")
        .expect("flat module child")
        .target_module()
        .unwrap();
    assert_eq!(map.module(nested_module).unwrap().file_id(), Some(nested));
    assert!(map.resolve_name(nested_module, "from_flat_child").is_some());

    let inline_module = module(&map, "inline");
    let child_module = map
        .resolve_name(inline_module, "child")
        .expect("inline module child")
        .target_module()
        .unwrap();
    assert_eq!(
        map.module(child_module).unwrap().file_id(),
        Some(inline_child)
    );
    assert!(
        map.resolve_name(child_module, "from_inline_child")
            .is_some()
    );
}

#[test]
fn multi_root_identity_is_project_scoped_and_uses_logical_bases() {
    let project_a = ProjectId::new(1);
    let project_b = ProjectId::new(2);
    let project_shared = ProjectId::new(3);
    let root_a = SourceRootId::new(90);
    let root_b = SourceRootId::new(1);
    let library_one = SourceRootId::new(40);
    let library_two = SourceRootId::new(30);
    let std_root = SourceRootId::new(0);
    let main_a = FileId::new(0);
    let main_b = FileId::new(1);
    let shadow_a = FileId::new(2);
    let foreign_b = FileId::new(3);
    let foo_one = FileId::new(4);
    let foo_two = FileId::new(5);
    let foo_std = FileId::new(6);
    let nested_a = FileId::new(7);
    let nested_leaf = FileId::new(8);
    let foo_child = FileId::new(9);

    let mut change = Change::new();
    for (root, kind) in [
        (root_a, SourceRootKind::Workspace),
        (root_b, SourceRootKind::Workspace),
        (library_one, SourceRootKind::Library),
        (library_two, SourceRootKind::Library),
        (std_root, SourceRootKind::Std),
    ] {
        change.set_source_root(root, kind);
    }
    change.set_file_with_path(main_a, root_a, FileKind::Source, "src/main.rua", "");
    change.set_file_with_path(main_b, root_b, FileKind::Source, "src/main.rua", "");
    change.set_file_with_path(
        shadow_a,
        root_a,
        FileKind::Declaration,
        "src/shadow.ruai",
        "fn workspace_shadow() {}",
    );
    change.set_file_with_path(
        foreign_b,
        root_b,
        FileKind::Source,
        "src/foreign.rua",
        "fn only_in_b() {}",
    );
    change.set_file_with_path(
        foo_one,
        library_one,
        FileKind::Declaration,
        "foo.ruai",
        "fn from_one() {}",
    );
    change.set_file_with_path(
        foo_two,
        library_two,
        FileKind::Declaration,
        "foo.rua",
        "fn from_two() {}",
    );
    change.set_file_with_path(
        foo_std,
        std_root,
        FileKind::Source,
        "foo.rua",
        "fn from_std() {}",
    );
    change.set_file_with_path(nested_a, root_a, FileKind::Source, "src/nested/mod.rua", "");
    change.set_file_with_path(
        nested_leaf,
        library_two,
        FileKind::Declaration,
        "nested/leaf.ruai",
        "fn from_nested_library() {}",
    );
    change.set_file_with_path(
        foo_child,
        library_two,
        FileKind::Declaration,
        "foo/child.ruai",
        "fn from_dependency_child() {}",
    );
    change.set_project(
        project_a,
        ProjectData::new(
            main_a,
            [ProjectRoot::new(root_a, "src")],
            [
                ProjectRoot::at_root(library_two),
                ProjectRoot::at_root(library_one),
                ProjectRoot::at_root(std_root),
            ],
        ),
    );
    change.set_project(
        project_b,
        ProjectData::new(main_b, [ProjectRoot::new(root_b, "src")], []),
    );
    change.set_project(
        project_shared,
        ProjectData::new(
            main_b,
            [ProjectRoot::new(root_b, "src")],
            [ProjectRoot::at_root(library_two)],
        ),
    );

    let mut host = AnalysisHost::new();
    host.apply_change(change);
    let analysis = host.analysis();

    let map_a = analysis
        .def_map_for_project(project_a)
        .expect("project A map");
    assert!(
        map_a
            .resolve_name(module(&map_a, "foo"), "from_two")
            .is_some()
    );
    let foo_module = module(&map_a, "foo");
    let dependency_child = map_a
        .resolve_name(foo_module, "child")
        .expect("dependency flat-module child")
        .target_module()
        .unwrap();
    assert!(
        map_a
            .resolve_name(dependency_child, "from_dependency_child")
            .is_some()
    );
    let nested_module = module(&map_a, "nested");
    let leaf_module = map_a
        .resolve_name(nested_module, "leaf")
        .expect("nested library module")
        .target_module()
        .expect("leaf target");
    assert!(
        map_a
            .resolve_name(leaf_module, "from_nested_library")
            .is_some()
    );
    assert!(map_a.resolve_name(map_a.root(), "foreign").is_none());

    let map_shared = analysis
        .def_map_for_project(project_shared)
        .expect("shared dependency map");
    let shared_from_a = map_a
        .resolve_name(module(&map_a, "foo"), "from_two")
        .unwrap();
    let shared_from_b = map_shared
        .resolve_name(module(&map_shared, "foo"), "from_two")
        .unwrap();
    assert_ne!(shared_from_a.id(), shared_from_b.id());
}

#[test]
fn multi_root_identity_rejects_missing_or_removed_project_roots() {
    let project = ProjectId::new(7);
    let root = SourceRootId::new(7);
    let main = FileId::new(7);
    let mut host = AnalysisHost::new();
    let mut missing = Change::new();
    missing.set_project(
        project,
        ProjectData::new(main, [ProjectRoot::at_root(root)], []),
    );
    host.apply_change(missing);
    assert!(host.analysis().def_map_for_project(project).is_none());

    let mut load = Change::new();
    load.set_source_root(root, SourceRootKind::Workspace);
    load.set_file_with_path(main, root, FileKind::Source, "main.rua", "fn loaded() {}");
    host.apply_change(load);
    let loaded_snapshot = host.analysis();
    let loaded_map = loaded_snapshot
        .def_map_for_project(project)
        .expect("loaded project map");
    assert!(
        loaded_map
            .resolve_name(loaded_map.root(), "loaded")
            .is_some()
    );

    let mut remove = Change::new();
    remove.remove_source_root(root);
    host.apply_change(remove);
    assert!(host.analysis().def_map_for_project(project).is_none());
    assert!(loaded_snapshot.def_map_for_project(project).is_some());
}
