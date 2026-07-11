use std::sync::Arc;

use rua_analysis::{
    Analysis, AnalysisHost, BindingKind, Body, BodySourceId, BodySourceMap, Change, Condition,
    DefId, DefKind, Expr, FileId, FileKind, NameRefKind, Pat, SourceRootId, SourceRootKind,
    Statement,
};

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

fn body_and_map(analysis: &Analysis, owner: DefId) -> (Arc<Body>, Arc<BodySourceMap>) {
    (
        analysis.body(owner).expect("owner has a lowered body"),
        analysis
            .body_source_map(owner)
            .expect("owner has a body source map"),
    )
}

#[test]
fn body_lowering_covers_callable_control_flow_expressions_and_patterns() {
    let source = r#"
struct Snapshot { value: i64 }
struct Harness { values: Vec<i64> }
enum Message {
    Quit,
    Code(i64),
    Point { x: i64, y: i64 },
}

fn helper(value: i64) -> i64 { value }

impl Harness {
    fn exercise(&mut self, input: i64, message: Message) -> i64 {
        let mut total: i64 = input;
        let value = helper(total);
        let snapshot = Snapshot { value };
        let qualified = self::Snapshot { value };
        let field = snapshot.value;
        let indexed = self.values[0];
        self.values.push(field);
        let negated = -indexed;
        let transform = |item: i64| -> i64 { item + negated };
        let counted = (0..4).map(|item| transform(item)).count();

        if total > counted {
            return total;
        } else {
            total = total + 1;
        }

        while total < 10 {
            total = total + 1;
            continue;
        }
        loop { break; }
        for step in 1..=3 { total = total + step; }

        let numeric = match input {
            0 => 0,
            1 | 2 => 1,
            3..=9 => 2,
            candidate if candidate > 10 => candidate,
            _ => -1,
        };
        let decoded = match message {
            Message::Quit => 0,
            Message::Code(code) => code,
            Message::Point { x, y: _, .. } => x,
        };
        println!("{}", decoded);
        total + numeric + field + indexed + counted
    }
}
"#;
    let (host, file_id) = single_file_host(source);
    let analysis = host.analysis();
    assert!(
        analysis.parse(file_id).errors().is_empty(),
        "mega fixture must remain valid syntax: {:?}",
        analysis.parse(file_id).errors()
    );

    let owner = body_owner(&analysis, file_id, "exercise", DefKind::Method);
    let body = analysis.body(owner).expect("method body");
    assert_eq!(body.owner(), owner);

    let params = body
        .params()
        .iter()
        .map(|id| body.binding(*id).expect("parameter binding"))
        .collect::<Vec<_>>();
    assert_eq!(
        params.iter().map(|param| param.name()).collect::<Vec<_>>(),
        [Some("self"), Some("input"), Some("message")]
    );
    assert_eq!(params[0].kind(), BindingKind::SelfParameter);
    assert!(params[0].is_mutable(), "`&mut self` must stay mutable");
    assert!(
        params[1..]
            .iter()
            .all(|param| param.kind() == BindingKind::Parameter && param.type_ref().is_some())
    );

    let Expr::Block(root) = &body[body.root_expr()] else {
        panic!("callable root must lower to a block")
    };
    assert!(
        root.tail().is_some(),
        "final expression must remain the block tail"
    );

    let bindings = body
        .bindings()
        .map(|(_, binding)| binding)
        .collect::<Vec<_>>();
    assert!(bindings.iter().any(|binding| {
        binding.name() == Some("total")
            && binding.kind() == BindingKind::Let
            && binding.is_mutable()
            && binding.type_ref().is_some()
    }));
    assert!(
        bindings.iter().any(|binding| {
            binding.name() == Some("step") && binding.kind() == BindingKind::For
        })
    );
    assert!(bindings.iter().any(|binding| {
        binding.name() == Some("item") && binding.kind() == BindingKind::ClosureParameter
    }));
    assert!(bindings.iter().any(|binding| {
        binding.name() == Some("candidate") && binding.kind() == BindingKind::Pattern
    }));

    let expressions = body.exprs().map(|(_, expr)| expr).collect::<Vec<_>>();
    assert!(
        expressions
            .iter()
            .any(|expr| matches!(expr, Expr::Block(_)))
    );
    assert!(
        expressions
            .iter()
            .any(|expr| matches!(expr, Expr::Literal(_)))
    );
    assert!(expressions.iter().any(|expr| matches!(expr, Expr::Path(_))));
    assert!(
        expressions
            .iter()
            .any(|expr| matches!(expr, Expr::Unary { .. }))
    );
    assert!(
        expressions
            .iter()
            .any(|expr| matches!(expr, Expr::Binary { .. }))
    );
    assert!(
        expressions
            .iter()
            .any(|expr| matches!(expr, Expr::Range { .. }))
    );
    assert!(
        expressions
            .iter()
            .any(|expr| matches!(expr, Expr::Closure { .. }))
    );
    assert!(
        expressions
            .iter()
            .any(|expr| matches!(expr, Expr::Assign { .. }))
    );
    assert!(
        expressions
            .iter()
            .any(|expr| matches!(expr, Expr::Call { .. }))
    );
    assert!(
        expressions
            .iter()
            .any(|expr| matches!(expr, Expr::MethodCall { .. }))
    );
    assert!(
        expressions
            .iter()
            .any(|expr| matches!(expr, Expr::Field { .. }))
    );
    assert!(
        expressions
            .iter()
            .any(|expr| matches!(expr, Expr::Index { .. }))
    );
    assert!(
        expressions
            .iter()
            .any(|expr| matches!(expr, Expr::Paren { .. }))
    );
    assert!(
        expressions
            .iter()
            .any(|expr| matches!(expr, Expr::If { .. }))
    );
    assert!(
        expressions
            .iter()
            .any(|expr| matches!(expr, Expr::Match { .. }))
    );
    assert!(
        expressions
            .iter()
            .any(|expr| matches!(expr, Expr::StructLiteral { .. }))
    );
    assert!(expressions.iter().any(|expr| {
        let Expr::StructLiteral { path, .. } = expr else {
            return false;
        };
        path.iter()
            .map(|id| {
                let name_ref = &body[*id];
                (name_ref.kind(), name_ref.name())
            })
            .eq([
                (NameRefKind::StructPath, Some("self")),
                (NameRefKind::StructPath, Some("Snapshot")),
            ])
    }));
    assert!(
        expressions
            .iter()
            .any(|expr| matches!(expr, Expr::MacroCall { .. }))
    );

    let statements = expressions
        .iter()
        .filter_map(|expr| match expr {
            Expr::Block(block) => Some(block.statements()),
            _ => None,
        })
        .flatten()
        .collect::<Vec<_>>();
    assert!(
        statements
            .iter()
            .any(|stmt| matches!(stmt, Statement::Let { .. }))
    );
    assert!(
        statements
            .iter()
            .any(|stmt| matches!(stmt, Statement::Expr { .. }))
    );
    assert!(
        statements
            .iter()
            .any(|stmt| matches!(stmt, Statement::Return { .. }))
    );
    assert!(statements.iter().any(|stmt| {
        matches!(
            stmt,
            Statement::While {
                condition: Condition::Expr(_),
                ..
            }
        )
    }));
    assert!(
        statements
            .iter()
            .any(|stmt| matches!(stmt, Statement::Loop { .. }))
    );
    assert!(
        statements
            .iter()
            .any(|stmt| matches!(stmt, Statement::For { .. }))
    );
    assert!(
        statements
            .iter()
            .any(|stmt| matches!(stmt, Statement::Break))
    );
    assert!(
        statements
            .iter()
            .any(|stmt| matches!(stmt, Statement::Continue))
    );

    let patterns = body
        .patterns()
        .map(|(_, pattern)| pattern)
        .collect::<Vec<_>>();
    assert!(
        patterns
            .iter()
            .any(|pattern| matches!(pattern, Pat::Wildcard))
    );
    assert!(
        patterns
            .iter()
            .any(|pattern| matches!(pattern, Pat::Binding { .. }))
    );
    assert!(
        patterns
            .iter()
            .any(|pattern| matches!(pattern, Pat::Literal(_)))
    );
    assert!(
        patterns
            .iter()
            .any(|pattern| matches!(pattern, Pat::Range { .. }))
    );
    assert!(
        patterns
            .iter()
            .any(|pattern| matches!(pattern, Pat::Path(_)))
    );
    assert!(
        patterns
            .iter()
            .any(|pattern| matches!(pattern, Pat::TupleVariant { .. }))
    );
    assert!(patterns.iter().any(|pattern| {
        matches!(
            pattern,
            Pat::StructVariant {
                fields,
                has_rest: true,
                ..
            } if fields.iter().any(|field| field.is_shorthand())
        )
    }));
    assert!(expressions.iter().any(|expr| {
        matches!(
            expr,
            Expr::Match { arms, .. } if arms.iter().any(|arm| arm.guard().is_some())
        )
    }));
}

#[test]
fn body_lowering_recovers_malformed_children_as_explicit_missing_nodes() {
    let source = concat!(
        "fn broken(value: i64) -> i64 { ",
        "let missing = ; ",
        "let grouped = (); ",
        "let indexed = value[]; ",
        "value + }",
    );
    let (host, file_id) = single_file_host(source);
    let analysis = host.analysis();
    assert!(!analysis.parse(file_id).errors().is_empty());

    let owner = body_owner(&analysis, file_id, "broken", DefKind::Function);
    let (body, source_map) = body_and_map(&analysis, owner);
    let Expr::Block(root) = &body[body.root_expr()] else {
        panic!("broken function still has a recovered block")
    };
    let initializer_for = |name| {
        root.statements()
            .iter()
            .find_map(|statement| match statement {
                Statement::Let {
                    binding,
                    initializer,
                } if body[*binding].name() == Some(name) => Some(*initializer),
                _ => None,
            })
            .unwrap_or_else(|| panic!("`let {name}` initializer"))
    };

    let initializer = initializer_for("missing");
    assert!(matches!(body[initializer], Expr::Missing));
    let initializer_range = source_map
        .expr_range(initializer)
        .expect("missing initializer source");
    let semicolon_offset = source.find(';').expect("let closing semicolon") as u32;
    assert_eq!(initializer_range.file_id, file_id);
    assert_eq!(initializer_range.range.start(), semicolon_offset);
    assert_eq!(initializer_range.range.end(), semicolon_offset);

    let grouped = initializer_for("grouped");
    let Expr::Paren {
        expr: grouped_inner,
    } = &body[grouped]
    else {
        panic!("`()` must remain a recovered parenthesized expression")
    };
    assert!(matches!(body[*grouped_inner], Expr::Missing));
    let grouped_range = source_map
        .expr_range(*grouped_inner)
        .expect("empty paren child source");
    let close_paren = (source.find("()").expect("empty parentheses") + 1) as u32;
    assert_eq!(grouped_range.file_id, file_id);
    assert_eq!(grouped_range.range.start(), close_paren);
    assert_eq!(grouped_range.range.end(), close_paren);

    let indexed = initializer_for("indexed");
    let Expr::Index {
        index: indexed_inner,
        ..
    } = &body[indexed]
    else {
        panic!("`value[]` must remain a recovered index expression")
    };
    assert!(matches!(body[*indexed_inner], Expr::Missing));
    let indexed_range = source_map
        .expr_range(*indexed_inner)
        .expect("empty index child source");
    let close_bracket = (source.find("[]").expect("empty index brackets") + 1) as u32;
    assert_eq!(indexed_range.file_id, file_id);
    assert_eq!(indexed_range.range.start(), close_bracket);
    assert_eq!(indexed_range.range.end(), close_bracket);

    let missing = body
        .exprs()
        .filter_map(|(id, expr)| matches!(expr, Expr::Missing).then_some(id))
        .collect::<Vec<_>>();
    assert!(
        !missing.is_empty(),
        "recovery must preserve absent required expressions as `Expr::Missing`"
    );
    assert!(missing.iter().all(|id| {
        source_map
            .expr_range(*id)
            .is_some_and(|range| range.file_id == file_id && range.range.is_empty())
    }));
}

#[test]
fn body_lowering_trait_defaults_own_bodies_but_signatures_do_not() {
    let source = r#"
trait Compute {
    fn required(&self, value: i64) -> i64;

    fn defaulted(&self, value: i64) -> i64 {
        value + 1
    }
}
"#;
    let (host, file_id) = single_file_host(source);
    let analysis = host.analysis();
    assert!(analysis.parse(file_id).errors().is_empty());

    let required = body_owner(&analysis, file_id, "required", DefKind::Method);
    let defaulted = body_owner(&analysis, file_id, "defaulted", DefKind::Method);
    assert!(analysis.body(required).is_none());
    assert!(analysis.body_source_map(required).is_none());

    let (body, map) = body_and_map(&analysis, defaulted);
    assert_eq!(body.owner(), defaulted);
    assert_eq!(map.body_id(), body.id());
    assert!(matches!(&body[body.root_expr()], Expr::Block(_)));
    assert_eq!(
        body.params()
            .iter()
            .map(|id| body[*id].kind())
            .collect::<Vec<_>>(),
        [BindingKind::SelfParameter, BindingKind::Parameter]
    );
}

#[test]
fn body_lowering_preserves_let_conditions_and_signed_string_pattern_ranges() {
    let source = r#"
enum Maybe { Some(i64), None }

fn inspect(value: Maybe, number: i64, text: String) -> i64 {
    let mut total = 0;
    if let Maybe::Some(found) = value {
        total = found;
    }
    while let Maybe::Some(next) = value {
        total = next;
        break;
    }
    let number_class = match number {
        -10..=-1 => 1,
        _ => 0,
    };
    let text_class = match text {
        "a".."z" => 1,
        _ => 0,
    };
    total + number_class + text_class
}
"#;
    let (host, file_id) = single_file_host(source);
    let analysis = host.analysis();
    assert!(
        analysis.parse(file_id).errors().is_empty(),
        "condition/range fixture must parse: {:?}",
        analysis.parse(file_id).errors()
    );
    let owner = body_owner(&analysis, file_id, "inspect", DefKind::Function);
    let body = analysis.body(owner).expect("inspect body");

    assert!(body.exprs().any(|(_, expr)| {
        matches!(
            expr,
            Expr::If {
                condition: Condition::Let { .. },
                ..
            }
        )
    }));
    assert!(body.exprs().any(|(_, expr)| {
        let Expr::Block(block) = expr else {
            return false;
        };
        block.statements().iter().any(|statement| {
            matches!(
                statement,
                Statement::While {
                    condition: Condition::Let { .. },
                    ..
                }
            )
        })
    }));

    let ranges = body
        .patterns()
        .filter_map(|(_, pattern)| match pattern {
            Pat::Range {
                start,
                end,
                inclusive,
            } => Some((start.text(), end.text(), *inclusive)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(ranges.contains(&("-10", "-1", true)));
    assert!(ranges.contains(&("\"a\"", "\"z\"", false)));
}

#[test]
fn body_lowering_malformed_call_arguments_are_missing_inside_the_owner_range() {
    let source = "fn malformed(value: i64) -> i64 let kept = value; consume(value, , value }";
    let (host, file_id) = single_file_host(source);
    let analysis = host.analysis();
    let errors = analysis.parse(file_id);
    assert!(
        errors
            .errors()
            .iter()
            .any(|error| error.message.contains("expected expression"))
    );
    assert!(
        errors
            .errors()
            .iter()
            .any(|error| error.message.contains("expected `)`"))
    );
    assert!(
        errors
            .errors()
            .iter()
            .any(|error| error.message.contains("expected `{`"))
    );

    let owner = body_owner(&analysis, file_id, "malformed", DefKind::Function);
    let (body, map) = body_and_map(&analysis, owner);
    let Expr::Block(root) = &body[body.root_expr()] else {
        panic!("a missing opening brace must retain the recovered partial block")
    };
    assert!(
        root.statements()
            .iter()
            .any(|statement| matches!(statement, Statement::Let { .. }))
    );
    assert!(body.bindings().any(|(_, binding)| {
        binding.name() == Some("kept") && binding.kind() == BindingKind::Let
    }));
    let missing_args = body
        .exprs()
        .filter_map(|(_, expr)| match expr {
            Expr::Call { args, .. } => Some(args),
            _ => None,
        })
        .flatten()
        .copied()
        .filter(|id| matches!(body[*id], Expr::Missing))
        .collect::<Vec<_>>();
    assert!(
        !missing_args.is_empty(),
        "an erroneous argument must not silently disappear from the call"
    );
    assert!(missing_args.iter().all(|id| {
        map.expr_range(*id).is_some_and(|range| {
            range.file_id == file_id
                && !range.range.is_empty()
                && map.body_range().range.contains_range(range.range)
        })
    }));
}

#[test]
fn body_source_map_is_bidirectional_and_preserves_shorthand_collisions() {
    let source = r#"
struct Snapshot { value: i64 }
fn mapped(value: i64) -> i64 {
    let snapshot = Snapshot { value };
    match snapshot {
        Snapshot { value } => value,
    }
}
"#;
    let (host, file_id) = single_file_host(source);
    let analysis = host.analysis();
    assert!(analysis.parse(file_id).errors().is_empty());
    let owner = body_owner(&analysis, file_id, "mapped", DefKind::Function);
    let (body, map) = body_and_map(&analysis, owner);

    let body_source = BodySourceId::Body(body.id());
    assert_eq!(map.source(body_source), Some(map.body_range()));
    assert!(map.ids_for_range(map.body_range()).contains(&body_source));

    for (id, _) in body.exprs() {
        let source = map.expr_range(id).expect("every expression has source");
        let source_id = BodySourceId::Expr(id);
        assert_eq!(map.source(source_id), Some(source));
        assert!(map.ids_for_range(source).contains(&source_id));
        assert!(
            map.ids_at(file_id, source.range.start())
                .contains(&source_id)
        );
    }
    for (id, _) in body.patterns() {
        let source = map.pat_range(id).expect("every pattern has source");
        let source_id = BodySourceId::Pat(id);
        assert_eq!(map.source(source_id), Some(source));
        assert!(map.ids_for_range(source).contains(&source_id));
        assert!(
            map.ids_at(file_id, source.range.start())
                .contains(&source_id)
        );
    }
    for (id, _) in body.bindings() {
        let source = map.binding_range(id).expect("every binding has source");
        let source_id = BodySourceId::Binding(id);
        assert_eq!(map.source(source_id), Some(source));
        assert!(map.ids_for_range(source).contains(&source_id));
        assert!(
            map.ids_at(file_id, source.range.start())
                .contains(&source_id)
        );
    }
    for (id, _) in body.name_refs() {
        let source = map
            .name_ref_range(id)
            .expect("every name reference has source");
        let source_id = BodySourceId::NameRef(id);
        assert_eq!(map.source(source_id), Some(source));
        assert!(map.ids_for_range(source).contains(&source_id));
        assert!(
            map.ids_at(file_id, source.range.start())
                .contains(&source_id)
        );
    }

    let (field_id, _) = body
        .name_refs()
        .find(|(_, name)| name.kind() == NameRefKind::StructField && name.name() == Some("value"))
        .expect("struct literal shorthand field");
    let shorthand_range = map.name_ref_range(field_id).expect("field range");
    let shorthand_ids = map.ids_for_range(shorthand_range);
    assert!(shorthand_ids.contains(&BodySourceId::NameRef(field_id)));
    assert!(
        shorthand_ids
            .iter()
            .any(|id| matches!(id, BodySourceId::Expr(_))),
        "the synthesized shorthand value expression shares the field token range"
    );
    assert!(
        shorthand_ids
            .iter()
            .filter(|id| matches!(id, BodySourceId::NameRef(_)))
            .count()
            >= 2,
        "field label and synthesized path name reference must both be discoverable"
    );
}

#[test]
fn body_source_map_cache_tracks_edits_without_leaking_stale_results() {
    const ORIGINAL: &str = concat!(
        "fn target(value: i64) -> i64 { value + 1 }\n",
        "fn stable() -> i64 { 7 }\n",
    );
    const SHIFTED: &str = concat!(
        "// shifts every target range\n\n",
        "fn target(value: i64) -> i64 { value + 1 }\n",
        "fn stable() -> i64 { 7 }\n",
    );
    const BODY_CHANGED: &str = concat!(
        "// shifts every target range\n\n",
        "fn target(value: i64) -> i64 { value + 2 }\n",
        "fn stable() -> i64 { 7 }\n",
    );

    let root_id = SourceRootId::new(0);
    let main_file = FileId::new(0);
    let unrelated_file = FileId::new(1);
    let mut load = Change::new();
    load.set_source_root(root_id, SourceRootKind::Workspace);
    load.set_file_with_path(main_file, root_id, FileKind::Source, "main.rua", ORIGINAL);
    load.set_file_with_path(
        unrelated_file,
        root_id,
        FileKind::Source,
        "unrelated.rua",
        "fn unrelated() -> i64 { 1 }",
    );
    let mut host = AnalysisHost::new();
    host.apply_change(load);

    let original = host.analysis();
    let owner = body_owner(&original, main_file, "target", DefKind::Function);
    let (original_body, original_map) = body_and_map(&original, owner);
    let (hot_body, hot_map) = body_and_map(&original, owner);
    assert!(Arc::ptr_eq(&original_body, &hot_body));
    assert!(Arc::ptr_eq(&original_map, &hot_map));

    let mut shift = Change::new();
    shift.set_file_text(main_file, SHIFTED);
    host.apply_change(shift);
    let shifted = host.analysis();
    assert_eq!(
        body_owner(&shifted, main_file, "target", DefKind::Function),
        owner
    );
    let (shifted_body, shifted_map) = body_and_map(&shifted, owner);
    assert!(
        Arc::ptr_eq(&original_body, &shifted_body),
        "trivia/range-only edits must reuse semantic HIR"
    );
    assert!(
        !Arc::ptr_eq(&original_map, &shifted_map),
        "source coordinates must be rebuilt for the current revision"
    );
    assert_ne!(original_map.body_range(), shifted_map.body_range());
    assert_eq!(
        shifted_map.body_range().range.start() as usize,
        SHIFTED.find("fn target").expect("target source offset")
    );

    let mut body_edit = Change::new();
    body_edit.set_file_text(main_file, BODY_CHANGED);
    host.apply_change(body_edit);
    let changed = host.analysis();
    let (changed_body, changed_map) = body_and_map(&changed, owner);
    assert!(
        !Arc::ptr_eq(&shifted_body, &changed_body),
        "semantic body edits must replace the body value"
    );
    assert!(!Arc::ptr_eq(&shifted_map, &changed_map));

    let mut unrelated_edit = Change::new();
    unrelated_edit.set_file_text(unrelated_file, "fn unrelated() -> i64 { 2 }");
    host.apply_change(unrelated_edit);
    let unrelated = host.analysis();
    let (unrelated_body, unrelated_map) = body_and_map(&unrelated, owner);
    assert!(Arc::ptr_eq(&changed_body, &unrelated_body));
    assert!(Arc::ptr_eq(&changed_map, &unrelated_map));

    let mut delete = Change::new();
    delete.set_file_text(main_file, "fn replacement() -> i64 { 0 }\n");
    host.apply_change(delete);
    let deleted = host.analysis();
    assert!(deleted.body(owner).is_none());
    assert!(deleted.body_source_map(owner).is_none());

    assert!(
        original.body(owner).is_some(),
        "an older immutable snapshot keeps its valid body"
    );
}
