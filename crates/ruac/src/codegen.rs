//! Lua 5.5 source backend.
//!
//! Expression-to-statement lowering uses a "destination" model (docs §4.1):
//! every expression is generated *to* a `Dest` (discard / assign / return).
//! Control-flow expressions push the same `Dest` into their branches; only
//! value-producing control-flow in operand position hoists a temporary.
//!
//! Data mapping (docs §4.2 / §4.3):
//!   struct        -> table + metatable (methods in `__index`)
//!   enum variant  -> { tag = "Name", ... } + metatable
//!   Option        -> pure nil: Some(v) => v, None => nil
//!   Result        -> first-class tagged runtime value

use crate::annotations::{AnnotationRetention, AnnotationTarget, AnnotationValue};
use crate::ast::*;
use crate::backend_layout::{
    BackendLayout, module_class_name, module_table_identifier, single_public_type, user_identifier,
};
use crate::lua_ir::{
    BinaryOp as LuaBinaryOp, Expr as LuaExpr, FunctionTarget, InlineStatement, TableField,
    UnaryOp as LuaUnaryOp,
};
use crate::typeck::{IterAdapterKind, IterConsumerKind, IterPlan, IterSourceKind, TypeInfo};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

#[derive(Clone)]
enum Dest {
    Discard,
    Var(LuaExpr),
    Return,
}

fn result_is_ok(value: LuaExpr) -> LuaExpr {
    value.index(LuaExpr::integer("1"))
}

fn result_is_err(value: LuaExpr) -> LuaExpr {
    LuaExpr::unary(LuaUnaryOp::Not, result_is_ok(value))
}

fn result_payload(value: LuaExpr) -> LuaExpr {
    value.index(LuaExpr::integer("2"))
}

struct IterCall<'a> {
    kind: IterAdapterKind,
    args: &'a [Expr],
}

struct IterChain<'a> {
    source: &'a Expr,
    source_args: &'a [Expr],
    adapters: Vec<IterCall<'a>>,
    consumer_args: &'a [Expr],
}

#[derive(Clone, Debug)]
struct RuntimeImport {
    alias: String,
    abi: Option<u32>,
    exports: BTreeMap<String, String>,
}

type PatternBinding = (String, crate::token::SourceRange, LuaExpr);

#[derive(Default)]
struct IterAdapterState {
    counter: Option<String>,
    limit: Option<String>,
}

enum IterLoopSource {
    Range {
        start: String,
        end: String,
        inclusive: bool,
    },
    Vec {
        holder: LuaExpr,
    },
}

pub fn generate(
    program: &crate::typed_ir::TypedProgram,
    rules: &crate::builtins::CodegenRules,
) -> String {
    generate_with_source_map(program, rules).source
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LuaSourceMapping {
    pub generated_start: usize,
    pub generated_end: usize,
    pub source: crate::token::SourceRange,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedLua {
    pub source: String,
    pub source_map: Vec<LuaSourceMapping>,
    pub annotations: crate::annotations::AnnotationIndex,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedLuaModule {
    /// Lua `require` name. The root uses its output file stem.
    pub module_name: String,
    /// Portable path relative to the CLI `--out-dir`.
    pub output_path: String,
    pub source: String,
    pub source_map: Vec<LuaSourceMapping>,
    pub is_root: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedLuaModules {
    pub root_output_path: String,
    pub modules: Vec<GeneratedLuaModule>,
    pub annotations: crate::annotations::AnnotationIndex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodegenMode {
    Bundle,
    Modules,
}

pub fn generate_with_source_map(
    program: &crate::typed_ir::TypedProgram,
    rules: &crate::builtins::CodegenRules,
) -> GeneratedLua {
    generate_with_source_map_and_lua_path(program, rules, &[])
        .expect("empty Lua search path is valid")
}

pub fn generate_with_source_map_and_lua_path(
    program: &crate::typed_ir::TypedProgram,
    rules: &crate::builtins::CodegenRules,
    lua_path: &[PathBuf],
) -> Result<GeneratedLua, String> {
    let hir = program.hir();
    let mut metatable_types = HashSet::new();
    collect_metatable_types(
        &program.syntax().items,
        hir.root,
        program,
        &mut metatable_types,
    );
    let mut local_uses = HashMap::new();
    for target in hir.expression_targets.values() {
        if let crate::hir::ResolvedTarget::Local(local) = target {
            *local_uses.entry(*local).or_insert(0) += 1;
        }
    }
    let mut cg = Codegen {
        lua: crate::lua_ir::Builder::new(),
        runtime_imports: BTreeMap::new(),
        declaration_imports: HashMap::new(),
        uses_table_create: false,
        closure_return_targets: Vec::new(),
        loop_break_targets: Vec::new(),
        local_substitutions: Vec::new(),
        local_uses,
        nonnegative_integer_locals: Vec::new(),
        program,
        info: program.types(),
        hir,
        layout: BackendLayout::new(program),
        current_module: hir.root,
        mode: CodegenMode::Bundle,
        module_dependencies: BTreeSet::new(),
        flattened_module_type: None,
        flattened_module_class: None,
        builtin_rules: rules,
        metatable_types,
    };
    cg.gen_program(program.syntax());

    let mut prefix = String::from("-- Generated by ruac (Rua -> Lua 5.5). Do not edit by hand.\n");
    append_lua_search_path(&mut prefix, lua_path)?;
    for (module, import) in &cg.runtime_imports {
        prefix.push_str(&format!(
            "local {} = require({})\n",
            import.alias,
            crate::lua_ir::lua_string(module)
        ));
        if let Some(abi) = import.abi {
            prefix.push_str(&format!(
                "assert({0}.ABI_VERSION == {abi}, \"incompatible {module} ABI\")\n",
                import.alias
            ));
        }
        for (export, alias) in &import.exports {
            prefix.push_str(&format!(
                "local {alias} = {}\n",
                crate::lua_ir::lua_member(&import.alias, export)
            ));
        }
    }
    if cg.uses_table_create {
        prefix.push_str("local tbcreate = table.create\n");
    }
    let printed = crate::lua_ir::print_with_source_map(&cg.lua.finish());
    if !printed.source.is_empty() && !prefix.ends_with("\n\n") && !printed.source.starts_with('\n')
    {
        prefix.push('\n');
    }
    let generated_offset = prefix.len();
    let source_map = printed
        .mappings
        .into_iter()
        .map(|mapping| LuaSourceMapping {
            generated_start: generated_offset + mapping.generated_start,
            generated_end: generated_offset + mapping.generated_end,
            source: mapping.source,
        })
        .collect();
    prefix.push_str(&printed.source);
    Ok(GeneratedLua {
        source: prefix,
        source_map,
        annotations: program.annotations().clone(),
    })
}

struct ModuleUnit<'a> {
    module: crate::hir::ModuleId,
    items: &'a [Item],
    chunk: &'a Block,
    source_order: &'a [ChunkEntry],
    is_root: bool,
}

/// Emit one ordinary Lua file per resolved Rua module. Cross-module identities
/// become top-level `require("a.b.c")` bindings and every file immediately
/// defines, initializes, and returns its own export table.
pub fn generate_modules_with_source_maps(
    program: &crate::typed_ir::TypedProgram,
    rules: &crate::builtins::CodegenRules,
    root_output_path: &str,
    lua_path: &[PathBuf],
) -> Result<GeneratedLuaModules, String> {
    if program.syntax().is_decl {
        return Err("modules output requires an executable `.rua` root".to_string());
    }
    if root_output_path.is_empty()
        || root_output_path.contains('/')
        || root_output_path.contains('\\')
        || !root_output_path.ends_with(".lua")
    {
        return Err("module root output must be one relative `.lua` filename".to_string());
    }
    append_lua_search_path(&mut String::new(), lua_path)?;

    let mut units = vec![ModuleUnit {
        module: program.hir().root,
        items: &program.syntax().items,
        chunk: &program.syntax().chunk,
        source_order: &program.syntax().source_order,
        is_root: true,
    }];
    collect_module_units(
        &program.syntax().items,
        program.hir().root,
        program,
        &mut units,
    );

    let mut outputs = BTreeSet::new();
    outputs.insert(root_output_path.to_string());
    let root_module_name = root_output_path
        .strip_suffix(".lua")
        .expect("validated root output has a .lua suffix");
    if root_module_name.is_empty() {
        return Err("module root output must have a non-empty file stem".to_string());
    }
    let mut names = Vec::with_capacity(units.len());
    names.push((program.hir().root, root_module_name.to_string()));
    for unit in units.iter().skip(1) {
        let segments = program.hir().module(unit.module).path.segments();
        let module_name = segments.join(".");
        let output_path = format!("{}.lua", segments.join("/"));
        if !outputs.insert(output_path.clone()) {
            return Err(format!("multiple Rua modules map to `{output_path}`"));
        }
        names.push((unit.module, module_name));
    }

    let generated = units
        .into_iter()
        .map(|unit| {
            let module_name = if unit.is_root {
                root_module_name.to_string()
            } else {
                program.hir().module(unit.module).path.segments().join(".")
            };
            let output_path = if unit.is_root {
                root_output_path.to_string()
            } else {
                format!(
                    "{}.lua",
                    program.hir().module(unit.module).path.segments().join("/")
                )
            };
            generate_module_unit(
                program,
                rules,
                unit,
                &module_name,
                output_path,
                &names,
                lua_path,
            )
        })
        .collect::<Vec<_>>();

    let dependency_graph = generated
        .iter()
        .map(|unit| (unit.module, unit.dependencies.clone()))
        .collect::<BTreeMap<_, _>>();
    if let Some(cycle) = find_module_dependency_cycle(&dependency_graph) {
        let display = cycle
            .iter()
            .map(|module| {
                names
                    .iter()
                    .find_map(|(candidate, name)| (candidate == module).then_some(name.as_str()))
                    .expect("generated module has a package name")
            })
            .collect::<Vec<_>>()
            .join(" -> ");
        return Err(format!(
            "modules output cannot lower cyclic Lua require dependency: {display}"
        ));
    }
    let mut modules = generated
        .into_iter()
        .map(|unit| unit.output)
        .collect::<Vec<_>>();
    let runtime_modules = module_annotation_outputs(program, &names)?;
    for module in runtime_modules {
        if !outputs.insert(module.output_path.clone()) {
            return Err(format!(
                "runtime annotation metadata conflicts with `{}`",
                module.output_path
            ));
        }
        modules.push(module);
    }

    Ok(GeneratedLuaModules {
        root_output_path: root_output_path.to_string(),
        modules,
        annotations: program.annotations().clone(),
    })
}

#[derive(Clone, Debug)]
struct RuntimeAnnotationLocator {
    module_name: String,
    fields: Vec<String>,
    kind: &'static str,
}

#[derive(Clone, Debug)]
struct RuntimeAnnotationEntry<'a> {
    annotation: String,
    locator: RuntimeAnnotationLocator,
    arguments: &'a [(String, AnnotationValue)],
    source_order: u32,
}

fn runtime_annotation_entries<'a>(
    program: &'a crate::typed_ir::TypedProgram,
    module_names: &BTreeMap<crate::hir::ModuleId, String>,
) -> Result<Vec<RuntimeAnnotationEntry<'a>>, String> {
    let hir = program.hir();
    let mut layouts = BTreeMap::new();
    let mut field_names = BTreeMap::new();
    collect_annotation_field_names(&program.syntax().items, hir.root, program, &mut field_names);
    let mut entries = Vec::new();
    for instance in program.annotations().instances() {
        let definition = program
            .annotations()
            .definition(instance.annotation)
            .expect("annotation instance has a schema");
        if definition.retention != AnnotationRetention::Runtime {
            continue;
        }
        let (module, base, extra, kind) = match instance.target {
            AnnotationTarget::Definition(target) => {
                let target_data = hir.definition(target);
                match target_data.kind {
                    crate::hir::DefKind::EnumVariant { owner, .. } => (
                        hir.definition(owner).module,
                        owner,
                        vec![
                            BackendLayout::for_module(program, hir.definition(owner).module)
                                .member_name(&target_data.name),
                        ],
                        "variant",
                    ),
                    crate::hir::DefKind::Method { owner } => {
                        (hir.definition(owner).module, target, Vec::new(), "method")
                    }
                    crate::hir::DefKind::Function => {
                        (target_data.module, target, Vec::new(), "function")
                    }
                    crate::hir::DefKind::Struct => {
                        (target_data.module, target, Vec::new(), "struct")
                    }
                    crate::hir::DefKind::Enum => (target_data.module, target, Vec::new(), "enum"),
                    other => {
                        return Err(format!(
                            "runtime annotation target `{}` has unsupported backend kind {other:?}",
                            target_data.name
                        ));
                    }
                }
            }
            AnnotationTarget::Field { owner, index } => {
                let module = hir.definition(owner).module;
                let name = field_names.get(&instance.target).ok_or_else(|| {
                    format!("missing runtime field locator for {owner:?}/{index}")
                })?;
                let member = BackendLayout::for_module(program, module).member_name(name);
                (module, owner, vec![member], "field")
            }
            AnnotationTarget::VariantField { variant, index } => {
                let crate::hir::DefKind::EnumVariant { owner, .. } = hir.definition(variant).kind
                else {
                    return Err("runtime variant-field target has no enum owner".to_string());
                };
                let module = hir.definition(owner).module;
                let name = field_names.get(&instance.target).ok_or_else(|| {
                    format!("missing runtime variant field locator for {variant:?}/{index}")
                })?;
                let layout = BackendLayout::for_module(program, module);
                let extra = vec![
                    layout.member_name(&hir.definition(variant).name),
                    layout.member_name(name),
                ];
                (module, owner, extra, "variant_field")
            }
        };
        let layout = layouts
            .entry(module)
            .or_insert_with(|| BackendLayout::for_module(program, module));
        let mut fields = layout
            .definition(base)
            .ok_or_else(|| format!("runtime annotation target {base:?} has no backend place"))?
            .fields()
            .to_vec();
        fields.extend(extra);
        let module_name = module_names.get(&module).cloned().unwrap_or_else(|| {
            if module == hir.root {
                "main".to_string()
            } else {
                hir.module(module).path.segments().join(".")
            }
        });
        entries.push(RuntimeAnnotationEntry {
            annotation: definition.qualified_name.clone(),
            locator: RuntimeAnnotationLocator {
                module_name,
                fields,
                kind,
            },
            arguments: &instance.arguments,
            source_order: instance.source_order,
        });
    }
    entries.sort_by_key(|entry| entry.source_order);
    Ok(entries)
}

fn collect_annotation_field_names(
    items: &[Item],
    module: crate::hir::ModuleId,
    program: &crate::typed_ir::TypedProgram,
    names: &mut BTreeMap<AnnotationTarget, String>,
) {
    for (item_index, item) in items.iter().enumerate() {
        match item {
            Item::Struct(structure) => {
                let owner = program.item_definition(module, item_index);
                for (index, field) in structure.fields.iter().enumerate() {
                    names.insert(
                        AnnotationTarget::Field {
                            owner,
                            index: index as u32,
                        },
                        field.name.clone(),
                    );
                }
            }
            Item::Enum(enumeration) => {
                let owner = program.item_definition(module, item_index);
                for (variant_index, variant) in enumeration.variants.iter().enumerate() {
                    let variant_id = program.hir().enum_variant_targets[&(owner, variant_index)];
                    if let VariantKind::Struct(fields) = &variant.kind {
                        for (index, field) in fields.iter().enumerate() {
                            names.insert(
                                AnnotationTarget::VariantField {
                                    variant: variant_id,
                                    index: index as u32,
                                },
                                field.name.clone(),
                            );
                        }
                    }
                }
            }
            Item::Mod(child) => {
                let child_module = program.child_module(module, item_index);
                collect_annotation_field_names(&child.items, child_module, program, names);
            }
            _ => {}
        }
    }
}

fn module_annotation_outputs(
    program: &crate::typed_ir::TypedProgram,
    names: &[(crate::hir::ModuleId, String)],
) -> Result<Vec<GeneratedLuaModule>, String> {
    let names = names.iter().cloned().collect::<BTreeMap<_, _>>();
    let entries = runtime_annotation_entries(program, &names)?;
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let all = entries.iter().collect::<Vec<_>>();
    Ok(vec![GeneratedLuaModule {
        module_name: "rua_annotations".to_string(),
        output_path: "rua_annotations.lua".to_string(),
        source: render_annotation_table(&all, program.hir()),
        source_map: Vec::new(),
        is_root: false,
    }])
}

fn render_annotation_table(
    entries: &[&RuntimeAnnotationEntry<'_>],
    hir: &crate::hir::ResolvedHir,
) -> String {
    let mut output = String::from(
        "-- Generated by ruac. Runtime annotation metadata ABI 1.\nlocal entries = {\n",
    );
    for entry in entries {
        output.push_str("    { annotation = ");
        output.push_str(&crate::lua_ir::lua_string(&entry.annotation));
        output.push_str(", target = { module = ");
        output.push_str(&crate::lua_ir::lua_string(&entry.locator.module_name));
        output.push_str(", kind = ");
        output.push_str(&crate::lua_ir::lua_string(entry.locator.kind));
        output.push_str(", path = {");
        for (index, field) in entry.locator.fields.iter().enumerate() {
            if index > 0 {
                output.push_str(", ");
            }
            output.push_str(&crate::lua_ir::lua_string(field));
        }
        output.push_str("} }, args = ");
        render_annotation_arguments(&mut output, entry.arguments, Some(hir));
        output.push_str(" },\n");
    }
    output.push_str("}\n");
    output.push_str(
        r#"local M = require("rua_std").annotations
M.set_entries(entries)
return M
"#,
    );
    output
}

fn render_annotation_arguments(
    output: &mut String,
    arguments: &[(String, AnnotationValue)],
    hir: Option<&crate::hir::ResolvedHir>,
) {
    output.push('{');
    for (index, (name, value)) in arguments.iter().enumerate() {
        if index > 0 {
            output.push_str(", ");
        }
        output.push('[');
        output.push_str(&crate::lua_ir::lua_string(name));
        output.push_str("] = ");
        render_annotation_value(output, value, hir);
    }
    output.push('}');
}

fn render_annotation_value(
    output: &mut String,
    value: &AnnotationValue,
    hir: Option<&crate::hir::ResolvedHir>,
) {
    match value {
        AnnotationValue::String(value) => output.push_str(&crate::lua_ir::lua_string(value)),
        AnnotationValue::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        AnnotationValue::Integer(value) => output.push_str(&value.to_string()),
        AnnotationValue::Float(value) => output.push_str(value),
        AnnotationValue::EnumVariant(definition) => {
            let value = hir.map_or_else(
                || format!("definition:{}", definition.index()),
                |hir| qualified_definition_name(hir, *definition),
            );
            output.push_str(&crate::lua_ir::lua_string(&value));
        }
        AnnotationValue::List(values) => {
            output.push('{');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push_str(", ");
                }
                render_annotation_value(output, value, hir);
            }
            output.push('}');
        }
    }
}

fn annotation_arguments_expr(
    arguments: &[(String, AnnotationValue)],
    hir: &crate::hir::ResolvedHir,
) -> LuaExpr {
    LuaExpr::Table(
        arguments
            .iter()
            .map(|(name, value)| {
                TableField::Indexed(LuaExpr::string(name), annotation_value_expr(value, hir))
            })
            .collect(),
    )
}

fn annotation_value_expr(value: &AnnotationValue, hir: &crate::hir::ResolvedHir) -> LuaExpr {
    match value {
        AnnotationValue::String(value) => LuaExpr::string(value),
        AnnotationValue::Bool(value) => LuaExpr::Bool(*value),
        AnnotationValue::Integer(value) => LuaExpr::integer(value.to_string()),
        AnnotationValue::Float(value) => LuaExpr::number(value.clone()),
        AnnotationValue::EnumVariant(definition) => {
            LuaExpr::string(&qualified_definition_name(hir, *definition))
        }
        AnnotationValue::List(values) => LuaExpr::Table(
            values
                .iter()
                .map(|value| TableField::Value(annotation_value_expr(value, hir)))
                .collect(),
        ),
    }
}

fn qualified_definition_name(
    hir: &crate::hir::ResolvedHir,
    definition: crate::hir::DefId,
) -> String {
    let data = hir.definition(definition);
    match data.kind {
        crate::hir::DefKind::EnumVariant { owner, .. }
        | crate::hir::DefKind::TraitMethod { owner }
        | crate::hir::DefKind::Method { owner } => {
            format!("{}::{}", qualified_definition_name(hir, owner), data.name)
        }
        _ => {
            let mut segments = hir.module(data.module).path.segments().to_vec();
            segments.push(data.name.clone());
            segments.join("::")
        }
    }
}

struct GeneratedModuleUnit {
    module: crate::hir::ModuleId,
    dependencies: Vec<crate::hir::ModuleId>,
    output: GeneratedLuaModule,
}

fn find_module_dependency_cycle(
    graph: &BTreeMap<crate::hir::ModuleId, Vec<crate::hir::ModuleId>>,
) -> Option<Vec<crate::hir::ModuleId>> {
    fn visit(
        module: crate::hir::ModuleId,
        graph: &BTreeMap<crate::hir::ModuleId, Vec<crate::hir::ModuleId>>,
        states: &mut HashMap<crate::hir::ModuleId, u8>,
        stack: &mut Vec<crate::hir::ModuleId>,
    ) -> Option<Vec<crate::hir::ModuleId>> {
        states.insert(module, 1);
        stack.push(module);
        for dependency in graph.get(&module).into_iter().flatten().copied() {
            match states.get(&dependency).copied().unwrap_or(0) {
                0 => {
                    if let Some(cycle) = visit(dependency, graph, states, stack) {
                        return Some(cycle);
                    }
                }
                1 => {
                    let start = stack
                        .iter()
                        .position(|candidate| *candidate == dependency)
                        .expect("active dependency is on the DFS stack");
                    let mut cycle = stack[start..].to_vec();
                    cycle.push(dependency);
                    return Some(cycle);
                }
                _ => {}
            }
        }
        stack.pop();
        states.insert(module, 2);
        None
    }

    let mut states = HashMap::new();
    let mut stack = Vec::new();
    for module in graph.keys().copied() {
        if states.get(&module).copied().unwrap_or(0) == 0
            && let Some(cycle) = visit(module, graph, &mut states, &mut stack)
        {
            return Some(cycle);
        }
    }
    None
}

fn collect_module_units<'a>(
    items: &'a [Item],
    module: crate::hir::ModuleId,
    program: &crate::typed_ir::TypedProgram,
    out: &mut Vec<ModuleUnit<'a>>,
) {
    for (item_index, item) in items.iter().enumerate() {
        let Item::Mod(child) = item else {
            continue;
        };
        let child_module = program.child_module(module, item_index);
        if child.is_decl {
            continue;
        }
        if child.is_file {
            out.push(ModuleUnit {
                module: child_module,
                items: &child.items,
                chunk: &child.chunk,
                source_order: &child.source_order,
                is_root: false,
            });
        }
        collect_module_units(&child.items, child_module, program, out);
    }
}

fn generate_module_unit(
    program: &crate::typed_ir::TypedProgram,
    rules: &crate::builtins::CodegenRules,
    unit: ModuleUnit<'_>,
    module_name: &str,
    output_path: String,
    all_modules: &[(crate::hir::ModuleId, String)],
    lua_path: &[PathBuf],
) -> GeneratedModuleUnit {
    let hir = program.hir();
    let flattened_module_type = single_public_type(program, unit.module);
    let flattened_type_name =
        flattened_module_type.map(|definition| hir.definition(definition).name.as_str());
    let module_table = flattened_type_name
        .map(user_identifier)
        .unwrap_or_else(|| module_table_identifier(module_name));
    let module_class = flattened_type_name.map_or_else(
        || module_class_name(module_name),
        |type_name| flattened_type_class_name(module_name, type_name),
    );
    let mut metatable_types = HashSet::new();
    collect_metatable_types(
        &program.syntax().items,
        hir.root,
        program,
        &mut metatable_types,
    );
    let mut local_uses = HashMap::new();
    for target in hir.expression_targets.values() {
        if let crate::hir::ResolvedTarget::Local(local) = target {
            *local_uses.entry(*local).or_insert(0) += 1;
        }
    }
    let mut cg = Codegen {
        lua: crate::lua_ir::Builder::new(),
        runtime_imports: BTreeMap::new(),
        declaration_imports: HashMap::new(),
        uses_table_create: false,
        closure_return_targets: Vec::new(),
        loop_break_targets: Vec::new(),
        local_substitutions: Vec::new(),
        local_uses,
        nonnegative_integer_locals: Vec::new(),
        program,
        info: program.types(),
        hir,
        layout: BackendLayout::for_module_named(program, unit.module, &module_table),
        current_module: unit.module,
        mode: CodegenMode::Modules,
        module_dependencies: BTreeSet::new(),
        flattened_module_type,
        flattened_module_class: flattened_module_type.map(|_| module_class.clone()),
        builtin_rules: rules,
        metatable_types,
    };
    cg.gen_module_unit(unit.items, unit.chunk, unit.source_order, unit.module);

    let dependencies = cg.module_dependencies.iter().copied().collect::<Vec<_>>();
    let mut prefix =
        String::from("-- Generated by ruac (Rua -> Lua 5.5 modules). Do not edit by hand.\n\n");
    if unit.is_root {
        let mut search_path = String::new();
        append_lua_search_path(&mut search_path, lua_path)
            .expect("Lua search paths are validated before module generation");
        append_header_section(&mut prefix, &search_path);
    }

    if let Some(import) = cg.runtime_imports.get("rua_std") {
        let mut standard_library = String::new();
        append_runtime_require(&mut standard_library, "rua_std", import);
        append_header_section(&mut prefix, &standard_library);
    }

    let mut dependency_imports = String::new();
    for dependency in &dependencies {
        let alias = cg
            .layout
            .module(*dependency)
            .expect("module dependency has a backend alias");
        let package_name = all_modules
            .iter()
            .find_map(|(module, name)| (module == dependency).then_some(name))
            .expect("generated module dependency has a package name");
        dependency_imports.push_str(&format!(
            "local {alias} = require({})\n",
            crate::lua_ir::lua_string(package_name)
        ));
    }
    for (module, import) in &cg.runtime_imports {
        if module != "rua_std" {
            append_runtime_require(&mut dependency_imports, module, import);
        }
    }
    append_header_section(&mut prefix, &dependency_imports);

    let mut aliases = String::new();
    for import in cg.runtime_imports.values() {
        append_runtime_export_aliases(&mut aliases, import);
    }
    if cg.uses_table_create {
        aliases.push_str("local tbcreate = table.create\n");
    }
    append_header_section(&mut prefix, &aliases);

    if flattened_module_type.is_none() {
        prefix.push_str(&format!(
            "---@class {module_class}\nlocal {module_table} = {{}}\n"
        ));
    }

    let printed = crate::lua_ir::print_with_source_map(&cg.lua.finish());
    if !printed.source.is_empty() && !prefix.ends_with("\n\n") && !printed.source.starts_with('\n')
    {
        prefix.push('\n');
    }
    let generated_offset = prefix.len();
    let source_map = printed
        .mappings
        .into_iter()
        .map(|mapping| LuaSourceMapping {
            generated_start: generated_offset + mapping.generated_start,
            generated_end: generated_offset + mapping.generated_end,
            source: mapping.source,
        })
        .collect();
    prefix.push_str(&printed.source);

    if !prefix.ends_with("\n\n") {
        if !prefix.ends_with('\n') {
            prefix.push('\n');
        }
        prefix.push('\n');
    }
    prefix.push_str(&format!("return {module_table}\n"));

    GeneratedModuleUnit {
        module: unit.module,
        dependencies,
        output: GeneratedLuaModule {
            module_name: module_name.to_string(),
            output_path,
            source: prefix,
            source_map,
            is_root: unit.is_root,
        },
    }
}

fn append_lua_search_path(prefix: &mut String, paths: &[PathBuf]) -> Result<(), String> {
    let mut patterns = Vec::new();
    let mut seen = HashSet::new();
    for path in paths {
        let path = path
            .to_str()
            .ok_or_else(|| format!("Lua search directory is not UTF-8: {}", path.display()))?
            .replace('\\', "/");
        if path.is_empty() {
            return Err("Lua search directory cannot be empty".to_string());
        }
        if path.contains(';') {
            return Err(format!("Lua search directory cannot contain `;`: {}", path));
        }
        let pattern = format!("{}/?.lua", path.trim_end_matches('/'));
        if seen.insert(pattern.clone()) {
            patterns.push(pattern);
        }
    }
    if !patterns.is_empty() {
        let search_path = patterns.join(";");
        prefix.push_str(&format!(
            "package.path = {} .. \";\" .. package.path\n",
            crate::lua_ir::lua_string(&search_path)
        ));
    }
    Ok(())
}

fn append_header_section(prefix: &mut String, section: &str) {
    if section.is_empty() {
        return;
    }
    prefix.push_str(section);
    if !section.ends_with('\n') {
        prefix.push('\n');
    }
    prefix.push('\n');
}

fn append_runtime_require(prefix: &mut String, module: &str, import: &RuntimeImport) {
    prefix.push_str(&format!(
        "local {} = require({})\n",
        import.alias,
        crate::lua_ir::lua_string(module)
    ));
    if let Some(abi) = import.abi {
        prefix.push_str(&format!(
            "assert({0}.ABI_VERSION == {abi}, \"incompatible {module} ABI\")\n",
            import.alias
        ));
    }
}

fn append_runtime_export_aliases(prefix: &mut String, import: &RuntimeImport) {
    for (export, alias) in &import.exports {
        prefix.push_str(&format!(
            "local {alias} = {}\n",
            crate::lua_ir::lua_member(&import.alias, export)
        ));
    }
}

fn flattened_type_class_name(module_name: &str, type_name: &str) -> String {
    if module_table_identifier(module_name) == user_identifier(type_name) {
        module_class_name(module_name)
    } else {
        format!("{module_name}.{type_name}")
    }
}

struct Codegen<'a> {
    lua: crate::lua_ir::Builder,
    runtime_imports: BTreeMap<String, RuntimeImport>,
    declaration_imports: HashMap<crate::hir::ModuleId, String>,
    /// Set when a table is populated after construction and benefits from a
    /// Lua 5.5 `table.create` capacity hint.
    uses_table_create: bool,
    /// Inlined iterator closure returns assign a local and jump out of the
    /// closure body instead of returning from the enclosing Rua function.
    closure_return_targets: Vec<(String, String)>,
    /// Destination for the innermost loop's `break value`; `None` marks
    /// `while`/`for` or a loop whose value is discarded.
    loop_break_targets: Vec<Option<LuaExpr>>,
    /// Identity-keyed replacements active while an expression closure is fused
    /// into its caller.
    local_substitutions: Vec<HashMap<crate::hir::LocalId, LuaExpr>>,
    local_uses: HashMap<crate::hir::LocalId, usize>,
    nonnegative_integer_locals: Vec<crate::hir::LocalId>,
    program: &'a crate::typed_ir::TypedProgram,
    info: &'a TypeInfo,
    hir: &'a crate::hir::ResolvedHir,
    layout: BackendLayout,
    current_module: crate::hir::ModuleId,
    mode: CodegenMode,
    module_dependencies: BTreeSet<crate::hir::ModuleId>,
    flattened_module_type: Option<crate::hir::DefId>,
    flattened_module_class: Option<String>,
    builtin_rules: &'a crate::builtins::CodegenRules,
    metatable_types: HashSet<crate::hir::DefId>,
}

fn statement_source(statement: &Stmt) -> Option<crate::token::SourceRange> {
    match statement {
        Stmt::Let { name_span, .. } => Some(*name_span),
        Stmt::Expr(expression) => Some(expression.span),
        Stmt::Return(Some(expression)) => Some(expression.span),
        Stmt::While { cond, .. } => Some(cond.span),
        Stmt::For { var_span, .. } => Some(*var_span),
        Stmt::WhileLet { expr, .. } => Some(expr.span),
        Stmt::Break(Some(value)) => Some(value.span),
        Stmt::Return(None) | Stmt::Loop { .. } | Stmt::Break(None) | Stmt::Continue => None,
    }
}

impl Codegen<'_> {
    fn type_to_emmylua(&self, ty: &Type) -> String {
        match ty {
            Type::Path { id, name, args } => {
                if self.is_modules()
                    && let Some(crate::hir::TypeTarget::Item(definition)) =
                        self.hir.type_targets.get(id).copied()
                    && single_public_type(self.program, self.hir.definition(definition).module)
                        == Some(definition)
                {
                    let definition = self.hir.definition(definition);
                    let module_name = self.hir.module(definition.module).path.segments().join(".");
                    return flattened_type_class_name(&module_name, &definition.name);
                }
                let base = match name.as_str() {
                    "i64" | "i8" | "i16" | "i32" | "isize" | "u8" | "u16" | "u32" | "u64"
                    | "usize" => "integer",
                    "f64" | "f32" => "number",
                    "bool" => "boolean",
                    "String" | "str" => "string",
                    "Vec" => {
                        if let Some(a) = args.first() {
                            return format!("{}[]", self.type_to_emmylua(a));
                        }
                        "table"
                    }
                    "Option" => {
                        if let Some(a) = args.first() {
                            return format!("{}|nil", self.type_to_emmylua(a));
                        }
                        "any|nil"
                    }
                    "Result" => "table",
                    "HashMap" => "table",
                    _ => name.as_str(),
                };
                base.to_string()
            }
            Type::Ref { inner, .. } => self.type_to_emmylua(inner),
            Type::Function { params, ret } => {
                let params = params
                    .iter()
                    .map(|parameter| self.type_to_emmylua(parameter))
                    .collect::<Vec<_>>();
                format!("fun({}): {}", params.join(", "), self.type_to_emmylua(ret))
            }
            Type::Tuple(items) => format!(
                "[{}]",
                items
                    .iter()
                    .map(|item| self.type_to_emmylua(item))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Type::Never => "never".into(),
            Type::Unit => "nil".into(),
        }
    }

    fn is_modules(&self) -> bool {
        self.mode == CodegenMode::Modules
    }

    fn register_declaration_import(&mut self, module: crate::hir::ModuleId) -> String {
        let module = self
            .declaration_runtime_boundary(module)
            .expect("declaration import belongs to a file-backed declaration module");
        if let Some(alias) = self.declaration_imports.get(&module) {
            return alias.clone();
        }
        let data = self.hir.module(module);
        let runtime = data.path.segments().join(".");
        let alias = self
            .layout
            .module(module)
            .expect("declaration module has a backend place")
            .to_string();
        self.runtime_imports
            .entry(runtime)
            .or_insert_with(|| RuntimeImport {
                alias: alias.clone(),
                abi: None,
                exports: BTreeMap::new(),
            });
        self.declaration_imports.insert(module, alias.clone());
        alias
    }

    fn declaration_runtime_boundary(
        &self,
        mut module: crate::hir::ModuleId,
    ) -> Option<crate::hir::ModuleId> {
        loop {
            let data = self.hir.module(module);
            if data.is_declaration && data.is_file {
                return Some(module);
            }
            module = data.parent?;
        }
    }

    fn register_runtime_import(
        &mut self,
        module: &str,
        export: Option<&str>,
        preferred: &str,
        abi: Option<u32>,
    ) -> String {
        if !self.runtime_imports.contains_key(module) {
            let package_name = export
                .and_then(|_| module.rsplit('.').next())
                .unwrap_or(preferred);
            let alias = self.layout.runtime_module_alias(package_name);
            self.runtime_imports.insert(
                module.to_string(),
                RuntimeImport {
                    alias,
                    abi,
                    exports: BTreeMap::new(),
                },
            );
        }

        let import = self
            .runtime_imports
            .get_mut(module)
            .expect("runtime import was inserted");
        match (import.abi, abi) {
            (Some(current), Some(required)) => assert_eq!(
                current, required,
                "runtime package `{module}` has conflicting ABI requirements"
            ),
            (None, Some(required)) => import.abi = Some(required),
            _ => {}
        }

        let Some(export) = export else {
            return import.alias.clone();
        };
        if let Some(alias) = import.exports.get(export) {
            return alias.clone();
        }
        let alias = self.layout.runtime_module_alias(preferred);
        import.exports.insert(export.to_string(), alias.clone());
        alias
    }

    fn runtime_module(&mut self, runtime: &crate::builtins::RuntimeModule) -> LuaExpr {
        let alias = self.register_runtime_import(
            &runtime.runtime,
            runtime.export.as_deref(),
            &runtime.alias,
            runtime.abi,
        );
        LuaExpr::name(alias)
    }

    fn standard_module(&mut self, name: &str) -> LuaExpr {
        let runtime = self
            .hir
            .standard_runtime_named(name)
            .unwrap_or_else(|| panic!("std.toml is missing runtime module `{name}`"))
            .clone();
        self.runtime_module(&runtime)
    }

    fn runtime_helper(&mut self, name: &str) -> LuaExpr {
        let runtime = self
            .hir
            .runtime_helper(name)
            .unwrap_or_else(|| panic!("std.toml is missing runtime helper `{name}`"))
            .clone();
        self.runtime_module(&runtime)
    }

    fn runtime_call(
        &mut self,
        runtime: &crate::builtins::RuntimeModule,
        member: &str,
        arguments: Vec<LuaExpr>,
    ) -> LuaExpr {
        self.runtime_module(runtime).member(member).call(arguments)
    }

    fn standard_call(&mut self, module: &str, member: &str, arguments: Vec<LuaExpr>) -> LuaExpr {
        self.standard_module(module).member(member).call(arguments)
    }

    fn helper_call(&mut self, helper: &str, member: &str, arguments: Vec<LuaExpr>) -> LuaExpr {
        self.runtime_helper(helper).member(member).call(arguments)
    }

    fn emit_struct_annotation(&mut self, s: &StructDecl) {
        self.annotation(format!("---@class {}", s.name));
        if !s.generics.is_empty() {
            let gens: Vec<&str> = s.generics.iter().map(|g| g.name.as_str()).collect();
            self.annotation(format!("---@generic {}", gens.join(", ")));
        }
    }

    fn emit_fn_annotation(&mut self, f: &FnDecl) {
        if !f.generics.is_empty() {
            let gens: Vec<&str> = f.generics.iter().map(|g| g.name.as_str()).collect();
            self.annotation(format!("---@generic {}", gens.join(", ")));
        }
        // Parameter types are explicit in Rua source; only emit the return
        // annotation so LuaLS can infer the result type at call sites.
        if let Some(ret) = &f.ret {
            self.annotation(format!("---@return {}", self.type_to_emmylua(ret)));
        }
    }

    fn block_has_continue(block: &Block) -> bool {
        for s in &block.stmts {
            if matches!(s, Stmt::Continue) {
                return true;
            }
            match s {
                Stmt::While { body, .. }
                | Stmt::Loop { body }
                | Stmt::WhileLet { body, .. }
                | Stmt::For { body, .. } => {
                    if Self::block_has_continue(body) {
                        return true;
                    }
                }
                Stmt::Expr(e) if Self::expr_has_continue(e) => {
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    fn expr_has_continue(expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Block(b) | ExprKind::Loop(b) => Self::block_has_continue(b),
            ExprKind::If {
                then_block,
                else_block,
                ..
            } => {
                Self::block_has_continue(then_block)
                    || else_block.as_ref().is_some_and(|eb| match eb.as_ref() {
                        ElseBranch::Block(b) => Self::block_has_continue(b),
                        ElseBranch::If(e) => Self::expr_has_continue(e),
                    })
            }
            ExprKind::Match { arms, .. } => arms.iter().any(|a| Self::expr_has_continue(&a.body)),
            _ => false,
        }
    }

    fn annotation(&mut self, source: impl Into<String>) {
        self.lua.annotation(source);
    }

    fn local(&mut self, name: impl Into<String>, value: Option<LuaExpr>) {
        self.lua
            .local(vec![name.into()], value.into_iter().collect());
    }

    fn locals(&mut self, names: Vec<String>) {
        self.lua.local(names, Vec::new());
    }

    fn assign(&mut self, target: LuaExpr, value: LuaExpr) {
        self.lua.assign(target, value);
    }

    fn return_value(&mut self, value: LuaExpr) {
        self.lua.return_values(vec![value]);
    }

    fn return_none(&mut self) {
        self.lua.return_values(Vec::new());
    }

    fn expression_statement(&mut self, expression: LuaExpr) {
        self.lua.expression(expression);
    }

    fn blank(&mut self) {
        self.lua.blank();
    }

    fn fresh_tmp(&mut self) -> String {
        self.layout.fresh_temporary()
    }

    fn local_name(&self, name: &str) -> String {
        self.layout.local_name(self.current_module, name)
    }

    fn table_create(&mut self, sequence: LuaExpr, records: usize) -> LuaExpr {
        self.uses_table_create = true;
        LuaExpr::name("tbcreate").call(vec![sequence, LuaExpr::integer(records.to_string())])
    }

    fn empty_table(&mut self, records: usize) -> LuaExpr {
        if records == 0 {
            LuaExpr::Table(Vec::new())
        } else {
            self.table_create(LuaExpr::integer("0"), records)
        }
    }

    fn type_table_record_capacity(&self, owner: crate::hir::DefId) -> usize {
        let mut fields = HashSet::new();
        if self.metatable_types.contains(&owner) {
            fields.insert("__index".to_string());
        }
        for definition in &self.hir.definitions {
            if !matches!(definition.kind, crate::hir::DefKind::Method { owner: method_owner } if method_owner == owner)
            {
                continue;
            }
            fields.insert(self.layout.member_name(&definition.name));
            if let Some(alias) = self
                .hir
                .method_traits
                .get(&definition.id)
                .copied()
                .and_then(|target| op_alias(target, &definition.name))
            {
                fields.insert(alias.to_string());
            }
        }
        fields.len()
    }

    fn type_needs_runtime_table(&self, owner: crate::hir::DefId) -> bool {
        self.hir.definition(owner).is_public
            || self.metatable_types.contains(&owner)
            || self.hir.definitions.iter().any(|definition| {
                matches!(
                    definition.kind,
                    crate::hir::DefKind::Method { owner: method_owner }
                        if method_owner == owner
                )
            })
    }

    fn module_table_record_capacity(
        &self,
        declaration: &ModDecl,
        module: crate::hir::ModuleId,
    ) -> usize {
        let mut fields = HashSet::new();
        let mut insert_item = |name: &str, is_public: bool| {
            let encoded = self.layout.member_name(name);
            fields.insert(encoded.clone());
            if is_public && encoded != name {
                fields.insert(name.to_string());
            }
        };
        for (item_index, item) in declaration.items.iter().enumerate() {
            match item {
                Item::Fn(function) => insert_item(&function.name, function.is_pub),
                Item::Struct(structure)
                    if self.type_needs_runtime_table(
                        self.program.item_definition(module, item_index),
                    ) =>
                {
                    insert_item(&structure.name, structure.is_pub);
                }
                Item::Enum(enumeration)
                    if self.type_needs_runtime_table(
                        self.program.item_definition(module, item_index),
                    ) =>
                {
                    insert_item(&enumeration.name, enumeration.is_pub);
                }
                Item::Mod(child) => {
                    let child_module = self.program.child_module(module, item_index);
                    let needed = if child.is_decl {
                        self.declaration_imports.contains_key(&child_module)
                    } else {
                        child.is_pub || self.module_table_record_capacity(child, child_module) > 0
                    };
                    if needed {
                        insert_item(&child.name, child.is_pub);
                    }
                }
                Item::Extern(block) => {
                    for function in &block.fns {
                        insert_item(&function.name, false);
                    }
                }
                _ => {}
            }
        }
        fields.len()
    }

    fn binding_is_unused(&self, source: crate::token::SourceRange) -> bool {
        self.hir
            .binding_target(source)
            .is_some_and(|local| self.local_uses.get(&local).copied().unwrap_or(0) == 0)
    }

    fn bind_local_substitution(&mut self, source: crate::token::SourceRange, value: LuaExpr) {
        let local = self
            .hir
            .binding_target(source)
            .expect("resolved binding has a local identity");
        self.local_substitutions
            .last_mut()
            .expect("local substitution requires a block scope")
            .insert(local, value);
    }

    fn emit_pattern_bindings(&mut self, bindings: &[PatternBinding]) {
        for (name, source, value) in bindings {
            if matches!(value, LuaExpr::Name(_)) {
                self.bind_local_substitution(*source, value.clone());
            } else {
                self.local(self.local_name(name), Some(value.clone()));
            }
        }
    }

    fn is_immutable_local_path(&self, expression: &Expr) -> bool {
        matches!(expression.kind, ExprKind::Path(_))
            && matches!(
                self.program.expression_target(expression.id),
                crate::hir::ResolvedTarget::Local(local)
                    if !self.hir.locals[local.index()].is_mutable
            )
    }

    fn expression_is_known_nonnegative(&self, expression: &Expr) -> bool {
        match &expression.kind {
            ExprKind::Int(source) => rua_int_value(source).is_some_and(|value| value >= 0),
            ExprKind::Path(_) => matches!(
                self.program.expression_target(expression.id),
                crate::hir::ResolvedTarget::Local(local)
                    if self.nonnegative_integer_locals.contains(&local)
            ),
            _ => false,
        }
    }

    fn closure_invocation_is_removable(&self, closure: &Expr) -> bool {
        let ExprKind::Closure { body, .. } = &closure.kind else {
            return false;
        };
        match body {
            ClosureBody::Expr(expression) => {
                self.expression_is_removable_with_returns(expression, true)
            }
            ClosureBody::Block(block) => self.block_is_removable(block, true),
        }
    }

    fn block_is_removable(&self, block: &Block, closure_returns: bool) -> bool {
        block.stmts.iter().all(|statement| match statement {
            Stmt::Let { init, .. } | Stmt::Expr(init) => {
                self.expression_is_removable_with_returns(init, closure_returns)
            }
            Stmt::Return(value) if closure_returns => value.as_ref().is_none_or(|value| {
                self.expression_is_removable_with_returns(value, closure_returns)
            }),
            Stmt::Return(_)
            | Stmt::While { .. }
            | Stmt::Loop { .. }
            | Stmt::For { .. }
            | Stmt::WhileLet { .. }
            | Stmt::Break(_)
            | Stmt::Continue => false,
        }) && block
            .tail
            .as_ref()
            .is_none_or(|tail| self.expression_is_removable_with_returns(tail, closure_returns))
    }

    fn iterator_is_removable(&self, expression: &Expr, plan: &IterPlan) -> bool {
        let Some(chain) = extract_iter_chain(expression, plan, true) else {
            return false;
        };
        if !self.expression_is_removable(chain.source) {
            return false;
        }
        if !chain
            .source_args
            .iter()
            .all(|argument| self.expression_is_removable(argument))
        {
            return false;
        }
        for adapter in &chain.adapters {
            match adapter.kind {
                IterAdapterKind::Map | IterAdapterKind::Filter | IterAdapterKind::FilterMap => {
                    if !adapter
                        .args
                        .first()
                        .is_some_and(|closure| self.closure_invocation_is_removable(closure))
                    {
                        return false;
                    }
                }
                IterAdapterKind::Skip | IterAdapterKind::Take => {
                    if !adapter
                        .args
                        .first()
                        .is_some_and(|limit| self.expression_is_removable(limit))
                    {
                        return false;
                    }
                }
                IterAdapterKind::Enumerate => {}
            }
        }
        match plan.consumer {
            IterConsumerKind::For => false,
            IterConsumerKind::CollectVec | IterConsumerKind::Count => true,
            IterConsumerKind::Next => true,
            IterConsumerKind::Fold => {
                chain.consumer_args.len() == 2
                    && self.expression_is_removable(&chain.consumer_args[0])
                    && self.closure_invocation_is_removable(&chain.consumer_args[1])
            }
            IterConsumerKind::Any | IterConsumerKind::All | IterConsumerKind::Find => chain
                .consumer_args
                .first()
                .is_some_and(|closure| self.closure_invocation_is_removable(closure)),
        }
    }

    fn expression_is_removable(&self, expression: &Expr) -> bool {
        self.expression_is_removable_with_returns(expression, false)
    }

    fn expression_is_removable_with_returns(
        &self,
        expression: &Expr,
        closure_returns: bool,
    ) -> bool {
        if let Some(plan) = self.info.iter_plan(expression.id) {
            return self.iterator_is_removable(expression, plan);
        }
        match &expression.kind {
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Str(_)
            | ExprKind::Bool(_)
            | ExprKind::Path(_)
            | ExprKind::Closure { .. } => true,
            ExprKind::VecLit(elements) => elements
                .iter()
                .all(|element| self.expression_is_removable_with_returns(element, closure_returns)),
            ExprKind::Unary { expr, .. } => {
                self.info.is_pure_operator(expression.id)
                    && self.expression_is_removable_with_returns(expr, closure_returns)
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let cannot_trap = !matches!(op, BinOp::Div | BinOp::Rem)
                    || matches!(&rhs.kind, ExprKind::Int(source) if rua_int_value(source).is_some_and(|value| value != 0));
                cannot_trap
                    && self.info.is_pure_operator(expression.id)
                    && self.expression_is_removable_with_returns(lhs, closure_returns)
                    && self.expression_is_removable_with_returns(rhs, closure_returns)
            }
            ExprKind::Call { callee, args } => {
                let enum_variant = matches!(callee.kind, ExprKind::Path(_))
                    && matches!(
                        self.program.expression_target(callee.id),
                        crate::hir::ResolvedTarget::Item(definition)
                            if matches!(
                                self.hir.definition(definition).kind,
                                crate::hir::DefKind::EnumVariant { .. }
                            )
                    );
                let builtin = if matches!(callee.kind, ExprKind::Path(_)) {
                    match self.program.expression_target(callee.id) {
                        crate::hir::ResolvedTarget::Builtin(builtin) => matches!(
                            self.builtin_rules.get(builtin),
                            Some(
                                crate::builtins::CodegenRule::InlineArg
                                    | crate::builtins::CodegenRule::TableCtor { .. }
                                    | crate::builtins::CodegenRule::TaggedResult { .. }
                            )
                        ),
                        _ => false,
                    }
                } else {
                    false
                };
                (builtin
                    || enum_variant
                    || matches!(callee.kind, ExprKind::Closure { .. })
                        && self.closure_invocation_is_removable(callee))
                    && args.iter().all(|argument| {
                        self.expression_is_removable_with_returns(argument, closure_returns)
                    })
            }
            ExprKind::MethodCall { recv, args, .. } if self.info.is_option_map(expression.id) => {
                self.expression_is_removable_with_returns(recv, closure_returns)
                    && args
                        .first()
                        .is_some_and(|closure| self.closure_invocation_is_removable(closure))
            }
            ExprKind::MethodCall {
                recv, method, args, ..
            } if self.info.is_str_method(expression.id)
                && matches!(
                    method.as_str(),
                    "len" | "is_empty" | "to_string" | "to_owned" | "clone"
                ) =>
            {
                self.expression_is_removable_with_returns(recv, closure_returns)
                    && args.iter().all(|argument| {
                        self.expression_is_removable_with_returns(argument, closure_returns)
                    })
            }
            ExprKind::StructLit { fields, .. } => fields.iter().all(|(_, value)| {
                self.expression_is_removable_with_returns(value, closure_returns)
            }),
            ExprKind::MapLit(entries) => entries.iter().all(|(key, value)| {
                self.expression_is_removable_with_returns(key, closure_returns)
                    && self.expression_is_removable_with_returns(value, closure_returns)
            }),
            ExprKind::Range { start, end, .. } => {
                self.expression_is_removable_with_returns(start, closure_returns)
                    && self.expression_is_removable_with_returns(end, closure_returns)
            }
            ExprKind::If {
                cond,
                then_block,
                else_block,
            } => {
                self.expression_is_removable_with_returns(cond, closure_returns)
                    && self.block_is_removable(then_block, closure_returns)
                    && else_block
                        .as_ref()
                        .is_none_or(|branch| match branch.as_ref() {
                            ElseBranch::Block(block) => {
                                self.block_is_removable(block, closure_returns)
                            }
                            ElseBranch::If(expression) => self
                                .expression_is_removable_with_returns(expression, closure_returns),
                        })
            }
            ExprKind::Block(block) => self.block_is_removable(block, closure_returns),
            ExprKind::MethodCall { .. }
            | ExprKind::Field { .. }
            | ExprKind::Try { .. }
            | ExprKind::Match { .. }
            | ExprKind::IfLet { .. }
            | ExprKind::Loop(_)
            | ExprKind::Index { .. }
            | ExprKind::Assign { .. } => false,
        }
    }

    fn unused_value_can_be_discarded(&self, expression: &Expr) -> bool {
        match &expression.kind {
            ExprKind::Call { callee, .. } => !matches!(
                self.program.expression_target(callee.id),
                crate::hir::ResolvedTarget::Item(definition)
                    if matches!(
                        self.hir.definition(definition).kind,
                        crate::hir::DefKind::EnumVariant { .. }
                    )
            ),
            ExprKind::MethodCall { .. }
            | ExprKind::If { .. }
            | ExprKind::IfLet { .. }
            | ExprKind::Match { .. }
            | ExprKind::Block(_)
            | ExprKind::Loop(_)
            | ExprKind::Assign { .. }
            | ExprKind::Try { .. } => true,
            _ => false,
        }
    }

    // --- program ----------------------------------------------------------

    fn gen_module_unit(
        &mut self,
        items: &[Item],
        chunk: &Block,
        source_order: &[ChunkEntry],
        module: crate::hir::ModuleId,
    ) {
        self.current_module = module;
        let mut traits = HashMap::new();
        collect_traits(
            &self.program.syntax().items,
            self.hir.root,
            self.program,
            &mut traits,
        );

        self.allocate_module_items(items, module);
        self.bind_externs_shallow(items, module);
        self.define_module_functions(items, module);
        self.gen_impls(items, &traits, module);
        self.publish_module_items(items, module);
        if !chunk.stmts.is_empty() || chunk.tail.is_some() {
            self.blank();
        }
        self.init_entries(source_order, items, chunk, module);
    }

    fn allocate_module_items(&mut self, items: &[Item], module: crate::hir::ModuleId) {
        let module_place = self
            .layout
            .module(module)
            .expect("modular unit has a module table")
            .clone();
        let mut emitted_allocation = false;
        let mut last_allocation_was_module = false;
        for (item_index, item) in items.iter().enumerate() {
            match item {
                Item::Struct(structure) => {
                    if emitted_allocation {
                        self.blank();
                    }
                    let definition = self.program.item_definition(module, item_index);
                    if self.flattened_module_type == Some(definition) {
                        self.annotation(format!(
                            "---@class {}",
                            self.flattened_module_class
                                .as_deref()
                                .expect("flattened module type has a class identity")
                        ));
                    } else {
                        self.emit_struct_annotation(structure);
                    }
                    for field in &structure.fields {
                        self.annotation(format!(
                            "---@field {} {}",
                            field.name,
                            self.type_to_emmylua(&field.ty)
                        ));
                    }
                    if self.type_needs_runtime_table(definition) {
                        let place = self.layout.definition(definition).unwrap().clone();
                        let table = self.empty_table(self.type_table_record_capacity(definition));
                        if self.flattened_module_type == Some(definition) {
                            self.local(place.to_string(), Some(table));
                        } else {
                            self.assign(place.expression(), table);
                        }
                        if self.metatable_types.contains(&definition) {
                            self.assign(place.field("__index").expression(), place.expression());
                        }
                    }
                    emitted_allocation = true;
                    last_allocation_was_module = false;
                }
                Item::Enum(enumeration) => {
                    if emitted_allocation {
                        self.blank();
                    }
                    let definition = self.program.item_definition(module, item_index);
                    let class = if self.flattened_module_type == Some(definition) {
                        self.flattened_module_class
                            .as_deref()
                            .expect("flattened module type has a class identity")
                    } else {
                        &enumeration.name
                    };
                    self.annotation(format!("---@class {class}"));
                    if self.type_needs_runtime_table(definition) {
                        let place = self.layout.definition(definition).unwrap().clone();
                        let table = self.empty_table(self.type_table_record_capacity(definition));
                        if self.flattened_module_type == Some(definition) {
                            self.local(place.to_string(), Some(table));
                        } else {
                            self.assign(place.expression(), table);
                        }
                        if self.metatable_types.contains(&definition) {
                            self.assign(place.field("__index").expression(), place.expression());
                        }
                    }
                    emitted_allocation = true;
                    last_allocation_was_module = false;
                }
                Item::Mod(child) => {
                    let child_module = self.program.child_module(module, item_index);
                    if self
                        .hir
                        .module(child_module)
                        .path
                        .segments()
                        .first()
                        .map(String::as_str)
                        == Some("__rua_builtin")
                    {
                        continue;
                    }
                    if child.is_decl || !child.is_file {
                        continue;
                    }
                    if emitted_allocation && !last_allocation_was_module {
                        self.blank();
                    }
                    self.record_module_dependency(child_module);
                    let value = self
                        .layout
                        .module(child_module)
                        .expect("runtime child module has a backend alias")
                        .expression();
                    let field = self.layout.member_name(&child.name);
                    self.assign(module_place.field(field).expression(), value);
                    emitted_allocation = true;
                    last_allocation_was_module = true;
                }
                _ => {}
            }
        }
    }

    fn bind_externs_shallow(&mut self, items: &[Item], module: crate::hir::ModuleId) {
        for (item_index, item) in items.iter().enumerate() {
            let Item::Extern(block) = item else {
                continue;
            };
            for (function_index, function) in block.fns.iter().enumerate() {
                let definition = self
                    .program
                    .extern_function(module, item_index, function_index);
                if block.abi == "lua-result" {
                    self.bind_result_extern(function, definition);
                } else {
                    self.bind_plain_extern(function, definition);
                }
            }
        }
    }

    fn define_module_functions(&mut self, items: &[Item], module: crate::hir::ModuleId) {
        for (item_index, item) in items.iter().enumerate() {
            let Item::Fn(function) = item else {
                continue;
            };
            let definition = self.program.item_definition(module, item_index);
            self.gen_free_fn(function, definition, true);
        }
    }

    fn publish_module_items(&mut self, items: &[Item], module: crate::hir::ModuleId) {
        let module_place = self.layout.module(module).unwrap().clone();
        for (item_index, item) in items.iter().enumerate() {
            let definition = match item {
                Item::Fn(_) | Item::Struct(_) | Item::Enum(_) => {
                    Some(self.program.item_definition(module, item_index))
                }
                _ => None,
            };
            if definition == self.flattened_module_type {
                continue;
            }
            let exported = match item {
                Item::Fn(function) if function.is_pub => self
                    .layout
                    .definition(definition.expect("function has a definition"))
                    .map(|place| (function.name.as_str(), place.clone())),
                Item::Struct(structure) if structure.is_pub => self
                    .layout
                    .definition(definition.expect("struct has a definition"))
                    .map(|place| (structure.name.as_str(), place.clone())),
                Item::Enum(enumeration) if enumeration.is_pub => self
                    .layout
                    .definition(definition.expect("enum has a definition"))
                    .map(|place| (enumeration.name.as_str(), place.clone())),
                Item::Mod(child) if child.is_pub && !child.is_decl && child.is_file => self
                    .layout
                    .module(self.program.child_module(module, item_index))
                    .map(|place| (child.name.as_str(), place.clone())),
                _ => None,
            };
            if let Some((name, place)) = exported {
                let encoded = self.layout.member_name(name);
                let canonical = module_place.field(&encoded);
                if place != canonical || encoded != name {
                    self.assign(
                        module_place.expression().index(LuaExpr::string(name)),
                        place.expression(),
                    );
                }
            }
        }
    }

    fn record_module_dependency(&mut self, module: crate::hir::ModuleId) {
        if self.is_modules()
            && module != self.current_module
            && !self.hir.module(module).is_declaration
        {
            self.module_dependencies.insert(module);
        }
    }

    fn gen_program(&mut self, prog: &Program) {
        if prog.is_decl {
            return;
        }
        // Class tables (with `__index`) for structs and enums so their values
        // can carry methods. Each is self-contained: @class annotation + local table.
        for (item_index, item) in prog.items.iter().enumerate() {
            match item {
                Item::Struct(s) => {
                    let definition = self.program.item_definition(self.hir.root, item_index);
                    let place = self.layout.definition(definition).unwrap().clone();
                    self.emit_struct_annotation(s);
                    for field in &s.fields {
                        self.annotation(format!(
                            "---@field {} {}",
                            field.name,
                            self.type_to_emmylua(&field.ty)
                        ));
                    }
                    if self.type_needs_runtime_table(definition) {
                        let capacity = self.type_table_record_capacity(definition);
                        let table = self.empty_table(capacity);
                        self.local(place.to_string(), Some(table));
                        if self.metatable_types.contains(&definition) {
                            self.assign(place.field("__index").expression(), place.expression());
                        }
                    }
                }
                Item::Enum(e) => {
                    let definition = self.program.item_definition(self.hir.root, item_index);
                    let place = self.layout.definition(definition).unwrap().clone();
                    self.annotation(format!("---@class {0}", e.name));
                    if self.type_needs_runtime_table(definition) {
                        let capacity = self.type_table_record_capacity(definition);
                        let table = self.empty_table(capacity);
                        self.local(place.to_string(), Some(table));
                        if self.metatable_types.contains(&definition) {
                            self.assign(place.field("__index").expression(), place.expression());
                        }
                    }
                }
                _ => {}
            }
        }

        // Trait table across all scopes (root + modules), keyed by simple name,
        // for resolving inherited default methods in `impl Trait for Type`.
        let mut traits = HashMap::new();
        collect_traits(&prog.items, self.hir.root, self.program, &mut traits);

        // Phase 1: allocate every module/type table.
        for (item_index, item) in prog.items.iter().enumerate() {
            if let Item::Mod(m) = item
                && !m.is_decl
            {
                let module = self.program.child_module(self.hir.root, item_index);
                self.declare_mod(m, module);
            }
        }
        // Nested extern places live under their owning module table, so all
        // runtime module places must exist before extern bindings are emitted.
        self.bind_externs(&prog.items, self.hir.root);

        // Root locals must be in lexical scope before module functions are
        // parsed. Acyclic dependencies are emitted callee-first as `local
        // function`; only genuine mutual-recursion groups need declarations.
        for group in root_function_schedule(self.program) {
            let mutually_recursive = group.len() > 1;
            if mutually_recursive {
                let names = group
                    .iter()
                    .map(|&item_index| {
                        let definition = self.program.item_definition(self.hir.root, item_index);
                        self.layout.definition(definition).unwrap().to_string()
                    })
                    .collect();
                self.locals(names);
            }
            for item_index in group {
                let Item::Fn(function) = &prog.items[item_index] else {
                    unreachable!("root function schedule only contains functions")
                };
                let definition = self.program.item_definition(self.hir.root, item_index);
                self.gen_free_fn(function, definition, mutually_recursive);
            }
        }

        // Phase 2: define module callables against their preallocated tables.
        for (item_index, item) in prog.items.iter().enumerate() {
            if let Item::Mod(m) = item
                && !m.is_decl
            {
                let module = self.program.child_module(self.hir.root, item_index);
                self.define_mod(m, &traits, module);
                self.publish_mod(m, module);
            }
        }
        self.gen_impls(&prog.items, &traits, self.hir.root);
        self.gen_bundle_annotation_registry();
        // Phase 3: execute initializers in observable source order.
        self.init_entries(&prog.source_order, &prog.items, &prog.chunk, self.hir.root);

        // Export every public runtime item after chunk initialization.
        let exports: Vec<(String, LuaExpr)> = prog
            .items
            .iter()
            .enumerate()
            .filter_map(|(item_index, item)| match item {
                Item::Fn(f) if f.is_pub => self
                    .layout
                    .definition(self.program.item_definition(self.hir.root, item_index))
                    .map(|place| (f.name.clone(), place.expression())),
                Item::Struct(s) if s.is_pub => self
                    .layout
                    .definition(self.program.item_definition(self.hir.root, item_index))
                    .map(|place| (s.name.clone(), place.expression())),
                Item::Enum(e) if e.is_pub => self
                    .layout
                    .definition(self.program.item_definition(self.hir.root, item_index))
                    .map(|place| (e.name.clone(), place.expression())),
                Item::Mod(m) if m.is_pub && !m.is_decl => self
                    .layout
                    .module(self.program.child_module(self.hir.root, item_index))
                    .map(|place| (m.name.clone(), place.expression())),
                _ => None,
            })
            .collect();
        if !exports.is_empty() {
            self.blank();
            self.lua.return_table(exports);
        }
    }

    fn gen_bundle_annotation_registry(&mut self) {
        let runtime_instances = self
            .program
            .annotations()
            .instances()
            .iter()
            .filter(|instance| {
                self.program
                    .annotations()
                    .definition(instance.annotation)
                    .is_some_and(|definition| definition.retention == AnnotationRetention::Runtime)
            })
            .collect::<Vec<_>>();
        if runtime_instances.is_empty() {
            return;
        }

        let mut field_names = BTreeMap::new();
        collect_annotation_field_names(
            &self.program.syntax().items,
            self.hir.root,
            self.program,
            &mut field_names,
        );
        let entries = runtime_instances
            .into_iter()
            .map(|instance| {
                let definition = self
                    .program
                    .annotations()
                    .definition(instance.annotation)
                    .expect("annotation instance has a schema");
                let target = match instance.target {
                    AnnotationTarget::Definition(target) => {
                        let data = self.hir.definition(target);
                        if let crate::hir::DefKind::EnumVariant { owner, .. } = data.kind {
                            self.layout
                                .definition(owner)
                                .expect("runtime enum has a backend place")
                                .field(self.layout.member_name(&data.name))
                                .expression()
                        } else {
                            self.layout
                                .definition(target)
                                .expect("runtime definition has a backend place")
                                .expression()
                        }
                    }
                    AnnotationTarget::Field { owner, .. } => LuaExpr::named_table(vec![
                        (
                            "owner".to_string(),
                            self.layout
                                .definition(owner)
                                .expect("runtime field owner has a backend place")
                                .expression(),
                        ),
                        (
                            "member".to_string(),
                            LuaExpr::string(
                                &self.layout.member_name(&field_names[&instance.target]),
                            ),
                        ),
                        ("kind".to_string(), LuaExpr::string("field")),
                    ]),
                    AnnotationTarget::VariantField { variant, .. } => {
                        let crate::hir::DefKind::EnumVariant { owner, .. } =
                            self.hir.definition(variant).kind
                        else {
                            unreachable!("variant field has an enum variant identity")
                        };
                        LuaExpr::named_table(vec![
                            (
                                "owner".to_string(),
                                self.layout
                                    .definition(owner)
                                    .expect("runtime enum owner has a backend place")
                                    .field(
                                        self.layout.member_name(&self.hir.definition(variant).name),
                                    )
                                    .expression(),
                            ),
                            (
                                "member".to_string(),
                                LuaExpr::string(
                                    &self.layout.member_name(&field_names[&instance.target]),
                                ),
                            ),
                            ("kind".to_string(), LuaExpr::string("variant_field")),
                        ])
                    }
                };
                LuaExpr::named_table(vec![
                    (
                        "annotation".to_string(),
                        LuaExpr::string(&definition.qualified_name),
                    ),
                    ("target".to_string(), target),
                    (
                        "args".to_string(),
                        annotation_arguments_expr(&instance.arguments, self.hir),
                    ),
                ])
            })
            .map(TableField::Value)
            .collect();
        self.local("__rua_annotation_entries", Some(LuaExpr::Table(entries)));
        self.local(
            "__rua_annotations",
            Some(
                LuaExpr::name("require")
                    .call(vec![LuaExpr::string("rua_std")])
                    .field("annotations"),
            ),
        );
        self.expression_statement(
            LuaExpr::name("__rua_annotations")
                .field("set_entries")
                .call(vec![LuaExpr::name("__rua_annotation_entries")]),
        );
        self.lua.begin_function(
            FunctionTarget::path(vec!["__rua_annotations".to_string(), "load".to_string()]),
            vec!["entry".to_string()],
        );
        self.return_value(LuaExpr::name("entry").field("target"));
        self.lua.end_block();
        self.assign(
            LuaExpr::name("package")
                .field("loaded")
                .index(LuaExpr::string("rua_annotations")),
            LuaExpr::name("__rua_annotations"),
        );
        self.blank();
    }

    fn bind_externs(&mut self, items: &[Item], module: crate::hir::ModuleId) {
        let previous_module = self.current_module;
        self.current_module = module;
        for (item_index, item) in items.iter().enumerate() {
            match item {
                Item::Extern(block) => {
                    for (function_index, function) in block.fns.iter().enumerate() {
                        let definition =
                            self.program
                                .extern_function(module, item_index, function_index);
                        if block.abi == "lua-result" {
                            self.bind_result_extern(function, definition);
                        } else {
                            self.bind_plain_extern(function, definition);
                        }
                    }
                }
                Item::Mod(child) if !child.is_decl => {
                    let child_module = self.program.child_module(module, item_index);
                    self.bind_externs(&child.items, child_module);
                }
                _ => {}
            }
        }
        self.current_module = previous_module;
    }

    fn bind_plain_extern(&mut self, function: &ExternFn, definition: crate::hir::DefId) {
        let place = self.layout.definition(definition).unwrap().clone();
        let ambient = LuaExpr::name("_G").index(LuaExpr::string(&function.name));
        let missing = LuaExpr::string(&format!("missing Lua extern `{}`", function.name));
        let value = LuaExpr::name("assert").call(vec![ambient, missing]);
        if !self.is_modules() && self.hir.definition(definition).module == self.hir.root {
            self.local(place.to_string(), Some(value));
        } else {
            self.assign(place.expression(), value);
        }
    }

    fn bind_result_extern(&mut self, function: &ExternFn, definition: crate::hir::DefId) {
        self.lua.push_anchor(function.name_span);
        let place = self.layout.definition(definition).unwrap().clone();
        let target =
            if !self.is_modules() && self.hir.definition(definition).module == self.hir.root {
                FunctionTarget::local(place.to_string())
            } else {
                place.function_target()
            };
        let host = self.fresh_tmp();
        let ambient = LuaExpr::name("_G").index(LuaExpr::string(&function.name));
        let missing = LuaExpr::string(&format!("missing Lua extern `{}`", function.name));
        self.local(
            &host,
            Some(LuaExpr::name("assert").call(vec![ambient, missing])),
        );

        let params = function
            .params
            .iter()
            .map(|parameter| self.local_name(&parameter.name))
            .collect::<Vec<_>>();
        self.lua.begin_function(target, params.clone());
        let mut host_args = Vec::new();
        for (parameter, parameter_name) in function.params.iter().zip(params) {
            if self
                .hir
                .type_is_builtin(&parameter.ty, rua_core::BuiltinId::TypeResult)
            {
                let ok = self.fresh_tmp();
                let error = self.fresh_tmp();
                self.lua.local(vec![ok.clone(), error.clone()], Vec::new());
                self.lua
                    .begin_if(result_is_ok(LuaExpr::name(&parameter_name)));
                self.assign(
                    LuaExpr::name(&ok),
                    result_payload(LuaExpr::name(&parameter_name)),
                );
                self.lua.begin_else();
                self.assign(
                    LuaExpr::name(&error),
                    result_payload(LuaExpr::name(&parameter_name)),
                );
                self.lua.end_block();
                host_args.extend([LuaExpr::name(ok), LuaExpr::name(error)]);
            } else {
                host_args.push(LuaExpr::name(parameter_name));
            }
        }

        let value = self.fresh_tmp();
        let error = self.fresh_tmp();
        self.lua.local(
            vec![value.clone(), error.clone()],
            vec![LuaExpr::name(host).call(host_args)],
        );
        self.lua
            .begin_if(LuaExpr::name(&error).binary(LuaBinaryOp::Ne, LuaExpr::Nil));
        let result = self.standard_call("std::result", "err", vec![LuaExpr::name(&error)]);
        self.lua.return_values(vec![result]);
        self.lua.end_block();
        let result = self.standard_call("std::result", "ok", vec![LuaExpr::name(&value)]);
        self.lua.return_values(vec![result]);
        self.lua.end_block();
        self.lua.pop_anchor();
    }

    fn declare_mod(&mut self, m: &ModDecl, module: crate::hir::ModuleId) {
        let place = self
            .layout
            .module(module)
            .expect("runtime module has a backend place")
            .clone();
        self.blank();
        self.annotation(format!("---@class {place}"));
        let capacity = self.module_table_record_capacity(m, module);
        if !m.is_pub && capacity == 0 {
            return;
        }
        let table = self.empty_table(capacity);
        if self.hir.module(module).parent == Some(self.hir.root) {
            self.local(place.to_string(), Some(table));
        } else {
            self.assign(place.expression(), table);
        }

        // Class tables for module-local structs/enums.
        for (item_index, item) in m.items.iter().enumerate() {
            match item {
                Item::Struct(_) => {
                    let definition = self.program.item_definition(module, item_index);
                    let type_place = self.layout.definition(definition).unwrap().clone();
                    if self.type_needs_runtime_table(definition) {
                        let capacity = self.type_table_record_capacity(definition);
                        let table = self.empty_table(capacity);
                        self.assign(type_place.expression(), table);
                        if self.metatable_types.contains(&definition) {
                            self.assign(
                                type_place.field("__index").expression(),
                                type_place.expression(),
                            );
                        }
                    }
                }
                Item::Enum(_) => {
                    let definition = self.program.item_definition(module, item_index);
                    let type_place = self.layout.definition(definition).unwrap().clone();
                    if self.type_needs_runtime_table(definition) {
                        let capacity = self.type_table_record_capacity(definition);
                        let table = self.empty_table(capacity);
                        self.assign(type_place.expression(), table);
                        if self.metatable_types.contains(&definition) {
                            self.assign(
                                type_place.field("__index").expression(),
                                type_place.expression(),
                            );
                        }
                    }
                }
                _ => {}
            }
        }

        for (item_index, item) in m.items.iter().enumerate() {
            if let Item::Mod(child) = item {
                let child_module = self.program.child_module(module, item_index);
                if child.is_decl {
                    continue;
                } else {
                    self.declare_mod(child, child_module);
                }
            }
        }
    }

    fn define_mod(
        &mut self,
        m: &ModDecl,
        traits: &HashMap<crate::hir::DefId, &TraitDecl>,
        module: crate::hir::ModuleId,
    ) {
        let previous_module = self.current_module;
        self.current_module = module;
        for (item_index, item) in m.items.iter().enumerate() {
            match item {
                Item::Fn(f) => {
                    let definition = self.program.item_definition(module, item_index);
                    let place = self.layout.definition(definition).unwrap().clone();
                    self.lua.push_anchor(f.name_span);
                    self.blank();
                    self.emit_fn_annotation(f);
                    let params = f
                        .params
                        .iter()
                        .map(|parameter| self.local_name(&parameter.name))
                        .collect();
                    self.lua.begin_function(place.function_target(), params);
                    self.gen_block_to(&f.body, &Dest::Return);
                    self.lua.end_block();
                    self.lua.pop_anchor();
                }
                Item::Mod(md) if !md.is_decl => {
                    let child = self.program.child_module(module, item_index);
                    self.define_mod(md, traits, child);
                }
                _ => {}
            }
        }
        self.gen_impls(&m.items, traits, module);
        self.current_module = previous_module;
    }

    fn init_entries(
        &mut self,
        order: &[ChunkEntry],
        items: &[Item],
        chunk: &Block,
        module: crate::hir::ModuleId,
    ) {
        let previous_module = self.current_module;
        self.current_module = module;
        self.local_substitutions.push(HashMap::new());
        for entry in order {
            match *entry {
                ChunkEntry::Statement(index) => {
                    if chunk.statement_blank_before[index] {
                        self.blank();
                    }
                    self.gen_stmt(&chunk.stmts[index]);
                }
                ChunkEntry::Item(index) => {
                    if let Item::Mod(child_decl) = &items[index]
                        && !child_decl.is_decl
                    {
                        let child = self.program.child_module(module, index);
                        if self.is_modules() {
                            if child_decl.is_file {
                                self.record_module_dependency(child);
                            }
                        } else {
                            self.init_entries(
                                &child_decl.source_order,
                                &child_decl.items,
                                &child_decl.chunk,
                                child,
                            );
                        }
                    }
                }
            }
        }
        self.local_substitutions.pop();
        self.current_module = previous_module;
    }

    fn publish_mod(&mut self, declaration: &ModDecl, module: crate::hir::ModuleId) {
        let module_place = self.layout.module(module).unwrap().clone();
        for (item_index, item) in declaration.items.iter().enumerate() {
            let exported = match item {
                Item::Fn(function) if function.is_pub => self
                    .layout
                    .definition(self.program.item_definition(module, item_index))
                    .map(|place| (function.name.as_str(), place.clone())),
                Item::Struct(structure) if structure.is_pub => self
                    .layout
                    .definition(self.program.item_definition(module, item_index))
                    .map(|place| (structure.name.as_str(), place.clone())),
                Item::Enum(enumeration) if enumeration.is_pub => self
                    .layout
                    .definition(self.program.item_definition(module, item_index))
                    .map(|place| (enumeration.name.as_str(), place.clone())),
                Item::Mod(child) if child.is_pub && !child.is_decl => self
                    .layout
                    .module(self.program.child_module(module, item_index))
                    .map(|place| (child.name.as_str(), place.clone())),
                _ => None,
            };
            if let Some((name, place)) = exported {
                let encoded = self.layout.member_name(name);
                let canonical = module_place.field(&encoded);
                if place != canonical || encoded != name {
                    self.assign(
                        module_place.expression().index(LuaExpr::string(name)),
                        place.expression(),
                    );
                }
            }
            if let Item::Mod(child) = item
                && !child.is_decl
            {
                let child_module = self.program.child_module(module, item_index);
                self.publish_mod(child, child_module);
            }
        }
    }

    /// Emit all `impl` blocks in `items` (methods, operator aliases, inherited
    /// trait defaults). The type's class table is a same-scope local, so
    /// `function Type.method(...)` binds correctly at root or inside a module.
    fn gen_impls(
        &mut self,
        items: &[Item],
        traits: &HashMap<crate::hir::DefId, &TraitDecl>,
        module: crate::hir::ModuleId,
    ) {
        let previous_module = self.current_module;
        self.current_module = module;
        for (item_index, item) in items.iter().enumerate() {
            if let Item::Impl(im) = item {
                let implementation = self.program.implementation(module, item_index);
                let type_place = self
                    .target_place(crate::hir::ResolvedTarget::Item(implementation.owner))
                    .expect("resolved impl owner has a backend place");
                let mut overridden = HashSet::new();
                for (method_index, m) in im.methods.iter().enumerate() {
                    let definition =
                        self.program
                            .implementation_method(module, item_index, method_index);
                    if let Some(origin) = self
                        .hir
                        .trait_method_implementations
                        .get(&definition)
                        .copied()
                    {
                        overridden.insert(origin);
                    }
                    self.gen_method(m, definition);
                    if let Some(trait_target) = self.hir.method_traits.get(&definition).copied()
                        && let Some(meta) = op_alias(trait_target, &m.name)
                    {
                        let source = self
                            .layout
                            .definition(definition)
                            .expect("typed method has a backend place")
                            .expression();
                        self.assign(type_place.field(meta).expression(), source);
                    }
                }
                if let Some(crate::hir::TraitTarget::Item(trait_id)) = implementation.trait_target
                    && let Some(td) = traits.get(&trait_id)
                {
                    for (method_index, tm) in td.methods.iter().enumerate() {
                        let origin = self.program.trait_method(trait_id, method_index);
                        if tm.default.is_some() && !overridden.contains(&origin) {
                            let definition =
                                self.program.inherited_method(implementation.owner, origin);
                            self.gen_trait_default(tm, definition);
                            let meta = self
                                .hir
                                .method_traits
                                .get(&definition)
                                .copied()
                                .and_then(|trait_target| op_alias(trait_target, &tm.name));
                            if let Some(meta) = meta {
                                let source = self
                                    .layout
                                    .definition(definition)
                                    .expect("inherited method has a backend place")
                                    .expression();
                                self.assign(type_place.field(meta).expression(), source);
                            }
                        }
                    }
                }
            }
        }
        self.current_module = previous_module;
    }

    fn gen_free_fn(&mut self, f: &FnDecl, definition: crate::hir::DefId, predeclared: bool) {
        self.lua.push_anchor(f.name_span);
        self.blank();
        self.emit_fn_annotation(f);
        let params = f
            .params
            .iter()
            .map(|parameter| self.local_name(&parameter.name))
            .collect();
        let place = self.layout.definition(definition).unwrap().clone();
        let target = if predeclared {
            place.function_target()
        } else {
            FunctionTarget::local(place.to_string())
        };
        self.lua.begin_function(target, params);
        self.gen_block_to(&f.body, &Dest::Return);
        self.lua.end_block();
        self.lua.pop_anchor();
    }

    fn gen_method(&mut self, m: &FnDecl, definition: crate::hir::DefId) {
        self.lua.push_anchor(m.name_span);
        self.blank();
        // Return annotation so LuaLS can infer result types.
        if let Some(ret) = &m.ret {
            self.annotation(format!("---@return {}", self.type_to_emmylua(ret)));
        }
        let params = m
            .params
            .iter()
            .map(|parameter| self.local_name(&parameter.name))
            .collect();
        let target = self.layout.callable_target(definition, m.has_self);
        self.lua.begin_function(target, params);
        self.gen_block_to(&m.body, &Dest::Return);
        self.lua.end_block();
        self.lua.pop_anchor();
    }

    fn gen_trait_default(&mut self, tm: &TraitMethod, definition: crate::hir::DefId) {
        self.lua.push_anchor(tm.name_span);
        self.blank();
        // Emit param/return annotations
        let skip_self = if tm.has_self { 1 } else { 0 };
        for p in tm.params.iter().skip(skip_self) {
            self.annotation(format!(
                "---@param {} {}",
                p.name,
                self.type_to_emmylua(&p.ty)
            ));
        }
        if let Some(ret) = &tm.ret {
            self.annotation(format!("---@return {}", self.type_to_emmylua(ret)));
        }
        let params = tm
            .params
            .iter()
            .map(|parameter| self.local_name(&parameter.name))
            .collect();
        let target = self.layout.callable_target(definition, tm.has_self);
        self.lua.begin_function(target, params);
        // `default` is guaranteed Some by the caller.
        if let Some(body) = &tm.default {
            self.gen_block_to(body, &Dest::Return);
        }
        self.lua.end_block();
        self.lua.pop_anchor();
    }

    // --- statements --------------------------------------------------------

    fn gen_stmt(&mut self, statement: &Stmt) {
        let anchor = statement_source(statement);
        if let Some(source) = anchor {
            self.lua.push_anchor(source);
        }
        self.gen_stmt_inner(statement);
        if anchor.is_some() {
            self.lua.pop_anchor();
        }
    }

    fn gen_stmt_inner(&mut self, s: &Stmt) {
        match s {
            Stmt::Let {
                name,
                name_span,
                init,
                ty,
                ..
            } => {
                if self.binding_is_unused(*name_span) {
                    if self.expression_is_removable(init) {
                        return;
                    }
                    if self.unused_value_can_be_discarded(init) {
                        self.gen_expr_to(init, &Dest::Discard);
                        return;
                    }
                }
                let local = self.local_name(name);
                // EmmyLua type annotation for explicitly typed bindings
                if let Some(ty_ann) = ty {
                    self.annotation(format!("---@type {}", self.type_to_emmylua(ty_ann)));
                }
                if let ExprKind::Closure { params, body, .. } = &init.kind {
                    self.gen_closure_local(&local, params, body);
                } else if let ExprKind::Try { expr } = &init.kind {
                    let v = self.gen_inline(expr);
                    if self.info.is_result_try(init.id) {
                        let result = self.fresh_tmp();
                        self.local(&result, Some(v));
                        self.lua.compact_if(
                            result_is_err(LuaExpr::name(&result)),
                            vec![InlineStatement::return_value(LuaExpr::name(&result))],
                        );
                        self.local(&local, Some(result_payload(LuaExpr::name(&result))));
                    } else {
                        if self.is_immutable_local_path(expr) {
                            self.lua.compact_if(
                                v.clone().binary(LuaBinaryOp::Eq, LuaExpr::Nil),
                                vec![InlineStatement::return_nil()],
                            );
                            self.bind_local_substitution(*name_span, v);
                            return;
                        }
                        self.local(&local, Some(v));
                        self.lua.compact_if(
                            LuaExpr::name(&local).binary(LuaBinaryOp::Eq, LuaExpr::Nil),
                            vec![InlineStatement::return_nil()],
                        );
                    }
                } else if let Some(plan) = self.info.iter_plan(init.id)
                    && let Some(chain) = extract_iter_chain(init, plan, true)
                {
                    self.lua.push_anchor(init.span);
                    self.gen_iter_loop(
                        &chain,
                        plan,
                        None,
                        &Dest::Var(LuaExpr::name(&local)),
                        Some(&local),
                    );
                    self.lua.pop_anchor();
                } else if needs_hoist(init) {
                    self.local(&local, None);
                    self.gen_expr_to(init, &Dest::Var(LuaExpr::name(local)));
                } else {
                    let v = self.gen_inline(init);
                    self.local(&local, Some(v));
                }
            }
            Stmt::Expr(e) => self.gen_expr_to(e, &Dest::Discard),
            Stmt::Return(opt) => {
                if let Some((target, label)) = self.closure_return_targets.last().cloned() {
                    match opt {
                        Some(e) => self.gen_expr_to(e, &Dest::Var(LuaExpr::name(&target))),
                        None => self.assign(LuaExpr::name(target), LuaExpr::Nil),
                    }
                    self.lua.goto(label);
                    return;
                }
                self.lua.begin_do();
                match opt {
                    Some(e) => self.gen_expr_to(e, &Dest::Return),
                    None => self.return_none(),
                }
                self.lua.end_block();
            }
            Stmt::While { cond, body } => {
                let c = self.gen_inline(cond);
                self.lua.begin_while(c);
                self.loop_break_targets.push(None);
                self.gen_block_to(body, &Dest::Discard);
                self.loop_break_targets.pop();
                if Self::block_has_continue(body) {
                    self.lua.label("continue");
                }
                self.lua.end_block();
            }
            Stmt::Loop { body } => {
                self.gen_loop_to(body, &Dest::Discard);
            }
            Stmt::For {
                var,
                var_span,
                iter,
                body,
            } => {
                let removable_empty_range = body.stmts.is_empty()
                    && body.tail.is_none()
                    && matches!(
                            &iter.kind,
                            ExprKind::Range { start, end, .. }
                                if self.expression_is_removable(start)
                                    && self.expression_is_removable(end)
                    );
                if !removable_empty_range {
                    self.loop_break_targets.push(None);
                    self.gen_for(var, *var_span, iter, body);
                    self.loop_break_targets.pop();
                }
            }
            Stmt::WhileLet { pat, expr, body } => {
                self.lua.begin_while(LuaExpr::Bool(true));
                self.loop_break_targets.push(None);
                let s = self.gen_inline(expr);
                let subject = if self.is_immutable_local_path(expr) {
                    s
                } else {
                    let temporary = self.fresh_tmp();
                    self.local(&temporary, Some(s));
                    LuaExpr::name(temporary)
                };
                let mut tests = Vec::new();
                let mut binds = Vec::new();
                self.pat_test(pat, subject, None, &mut tests, &mut binds);
                self.lua.begin_if(and_all(tests));
                self.emit_pattern_bindings(&binds);
                self.gen_block_to(body, &Dest::Discard);
                self.lua.begin_else();
                self.lua.break_statement();
                self.lua.end_block();
                if Self::block_has_continue(body) {
                    self.lua.label("continue");
                }
                self.loop_break_targets.pop();
                self.lua.end_block();
            }
            Stmt::Break(value) => {
                match (self.loop_break_targets.last().cloned().flatten(), value) {
                    (Some(target), Some(value)) => self.gen_expr_to(value, &Dest::Var(target)),
                    (Some(target), None) => self.assign(target, LuaExpr::Nil),
                    (None, Some(value)) => self.gen_expr_to(value, &Dest::Discard),
                    (None, None) => {}
                }
                self.lua.break_statement();
            }
            Stmt::Continue => self.lua.goto("continue"),
        }
    }

    fn gen_for(
        &mut self,
        var: &str,
        var_span: crate::token::SourceRange,
        iter: &Expr,
        body: &Block,
    ) {
        let variable = self.local_name(var);
        if let Some(plan) = self.info.iter_plan(iter.id)
            && (!plan.adapters.is_empty()
                || matches!(
                    plan.source.kind,
                    IterSourceKind::VecIter
                        | IterSourceKind::VecIntoIter
                        | IterSourceKind::StringChars
                        | IterSourceKind::StringSplit
                ))
            && let Some(chain) = extract_iter_chain(iter, plan, false)
        {
            self.gen_iter_loop(&chain, plan, Some((&variable, body)), &Dest::Discard, None);
            return;
        }
        if let Some(plan) = self.info.iter_plan(iter.id)
            && plan.source.kind != IterSourceKind::Vec
            && !matches!(iter.kind, ExprKind::Range { .. })
        {
            let holder = self.fresh_tmp();
            let iterator = self.gen_inline(iter);
            self.local(&holder, Some(iterator));
            self.lua.begin_while(LuaExpr::Bool(true));
            self.local(
                &variable,
                Some(LuaExpr::name(&holder).method_call("next", Vec::new())),
            );
            self.lua.compact_if(
                LuaExpr::name(&variable).binary(LuaBinaryOp::Eq, LuaExpr::Nil),
                vec![InlineStatement::Break],
            );
            self.gen_block_to(body, &Dest::Discard);
            if Self::block_has_continue(body) {
                self.lua.label("continue");
            }
            self.lua.end_block();
            return;
        }
        if let ExprKind::Range {
            start,
            end,
            inclusive,
        } = &iter.kind
        {
            let s = self.gen_inline(start);
            let e = self.gen_inline(end);
            let stop = if *inclusive {
                e
            } else if let ExprKind::Int(n) = &end.kind {
                // Compile-time constant: `0..5` → `0, 4`
                n.parse::<i64>()
                    .ok()
                    .map(|v| LuaExpr::integer((v - 1).to_string()))
                    .unwrap_or_else(|| e.clone().binary(LuaBinaryOp::Sub, LuaExpr::integer("1")))
            } else {
                e.binary(LuaBinaryOp::Sub, LuaExpr::integer("1"))
            };
            self.lua.begin_numeric_for(&variable, s, stop);
            let nonnegative = matches!(&start.kind, ExprKind::Int(source) if rua_int_value(source).is_some_and(|value| value >= 0));
            if nonnegative {
                self.nonnegative_integer_locals.push(
                    self.hir
                        .binding_target(var_span)
                        .expect("resolved for binding has a local identity"),
                );
            }
            self.gen_block_to(body, &Dest::Discard);
            if nonnegative {
                self.nonnegative_integer_locals.pop();
            }
            if Self::block_has_continue(body) {
                self.lua.label("continue");
            }
            self.lua.end_block();
        } else {
            // General iterable: a Vec `{ [1..n], n = len }`.
            let it = self.gen_inline(iter);
            let holder = self.fresh_tmp();
            self.local(&holder, Some(it));
            let idx = self.fresh_tmp();
            self.lua.begin_numeric_for(
                &idx,
                LuaExpr::integer("1"),
                LuaExpr::name(&holder).field("n"),
            );
            self.local(
                &variable,
                Some(LuaExpr::name(&holder).index(LuaExpr::name(&idx))),
            );
            self.gen_block_to(body, &Dest::Discard);
            if Self::block_has_continue(body) {
                self.lua.label("continue");
            }
            self.lua.end_block();
        }
    }

    fn gen_closure_local(&mut self, name: &str, params: &[ClosureParam], body: &ClosureBody) {
        let params = params
            .iter()
            .map(|param| self.local_name(&param.name))
            .collect::<Vec<_>>();
        self.lua.begin_function(FunctionTarget::local(name), params);
        match body {
            ClosureBody::Expr(expr) => self.gen_expr_to(expr, &Dest::Return),
            ClosureBody::Block(block) => self.gen_block_to(block, &Dest::Return),
        }
        self.lua.end_block();
    }

    fn gen_closure_value(&mut self, params: &[ClosureParam], body: &ClosureBody) -> LuaExpr {
        let local = self.fresh_tmp();
        self.gen_closure_local(&local, params, body);
        LuaExpr::name(local)
    }

    fn gen_inlined_closure(&mut self, closure: &Expr, inputs: &[LuaExpr]) -> LuaExpr {
        let ExprKind::Closure { params, body, .. } = &closure.kind else {
            return LuaExpr::Nil;
        };
        if let ClosureBody::Expr(expression) = body {
            let substitutions = params
                .iter()
                .zip(inputs)
                .map(|(parameter, input)| {
                    let local = self
                        .hir
                        .binding_target(parameter.name_span)
                        .expect("resolved closure parameter has a local identity");
                    (local, input.clone())
                })
                .collect();
            self.local_substitutions.push(substitutions);
            let result = self.gen_inline(expression);
            self.local_substitutions.pop();
            return result;
        }
        let result = self.fresh_tmp();
        let done =
            matches!(body, ClosureBody::Block(_)).then(|| format!("{}_done", self.fresh_tmp()));
        self.local(&result, None);
        self.lua.begin_do();
        for (param, input) in params.iter().zip(inputs) {
            self.local(self.local_name(&param.name), Some(input.clone()));
        }
        match body {
            ClosureBody::Expr(expr) => self.gen_expr_to(expr, &Dest::Var(LuaExpr::name(&result))),
            ClosureBody::Block(block) => {
                let done = done.as_ref().unwrap();
                self.closure_return_targets
                    .push((result.clone(), done.clone()));
                self.gen_block_to(block, &Dest::Var(LuaExpr::name(&result)));
                self.closure_return_targets.pop();
            }
        }
        self.lua.end_block();
        if let Some(done) = done {
            self.lua.label(done);
        }
        LuaExpr::name(result)
    }

    fn init_iter_result(
        &mut self,
        dest: &Dest,
        local_result: Option<&str>,
        value: LuaExpr,
    ) -> (LuaExpr, bool) {
        if let Some(local) = local_result {
            self.local(local, Some(value));
            return (LuaExpr::name(local), true);
        }
        if let Dest::Var(target) = dest
            && matches!(target, LuaExpr::Name(_))
        {
            self.assign(target.clone(), value);
            return (target.clone(), true);
        }
        let result = self.fresh_tmp();
        self.local(&result, Some(value));
        (LuaExpr::name(result), false)
    }

    fn gen_iter_loop(
        &mut self,
        chain: &IterChain<'_>,
        plan: &IterPlan,
        for_body: Option<(&str, &Block)>,
        dest: &Dest,
        local_result: Option<&str>,
    ) {
        let source = match plan.source.kind {
            IterSourceKind::ExclusiveRange | IterSourceKind::InclusiveRange => {
                let ExprKind::Range { start, end, .. } = &chain.source.kind else {
                    return;
                };
                let start_value = self.gen_inline(start);
                let start_local = self.fresh_tmp();
                self.local(&start_local, Some(start_value));
                let end_value = self.gen_inline(end);
                let end_local = self.fresh_tmp();
                self.local(&end_local, Some(end_value));
                IterLoopSource::Range {
                    start: start_local,
                    end: end_local,
                    inclusive: plan.source.kind == IterSourceKind::InclusiveRange,
                }
            }
            IterSourceKind::Vec | IterSourceKind::VecIter | IterSourceKind::VecIntoIter => {
                let value = self.gen_inline(chain.source);
                let holder = if self.is_immutable_local_path(chain.source) {
                    value
                } else {
                    let holder = self.fresh_tmp();
                    self.local(&holder, Some(value));
                    LuaExpr::name(holder)
                };
                IterLoopSource::Vec { holder }
            }
            IterSourceKind::StringChars | IterSourceKind::StringSplit => {
                let receiver = self.gen_inline(chain.source);
                let method = if plan.source.kind == IterSourceKind::StringChars {
                    "chars"
                } else {
                    "split"
                };
                let mut arguments = vec![receiver];
                arguments.extend(chain.source_args.iter().map(|arg| self.gen_inline(arg)));
                let value = self.standard_call("std::string", method, arguments);
                let holder = self.fresh_tmp();
                self.local(&holder, Some(value));
                IterLoopSource::Vec {
                    holder: LuaExpr::name(holder),
                }
            }
        };

        let mut states = Vec::with_capacity(chain.adapters.len());
        for adapter in &chain.adapters {
            let mut state = IterAdapterState::default();
            match adapter.kind {
                IterAdapterKind::Enumerate => {
                    let counter = self.fresh_tmp();
                    self.local(&counter, Some(LuaExpr::integer("0")));
                    state.counter = Some(counter);
                }
                IterAdapterKind::Skip | IterAdapterKind::Take => {
                    let limit_value = adapter
                        .args
                        .first()
                        .map(|arg| self.gen_inline(arg))
                        .unwrap_or_else(|| LuaExpr::integer("0"));
                    let limit = self.fresh_tmp();
                    self.local(&limit, Some(limit_value));
                    let counter = self.fresh_tmp();
                    self.local(&counter, Some(LuaExpr::integer("0")));
                    state.limit = Some(limit);
                    state.counter = Some(counter);
                }
                _ => {}
            }
            states.push(state);
        }

        let result = match plan.consumer {
            IterConsumerKind::For => None,
            IterConsumerKind::CollectVec => {
                let preserves_length = chain.adapters.iter().all(|adapter| {
                    matches!(
                        adapter.kind,
                        IterAdapterKind::Map | IterAdapterKind::Enumerate
                    )
                });
                let exact_capacity = if preserves_length {
                    match &source {
                        IterLoopSource::Vec { holder } => Some(holder.clone().field("n")),
                        IterLoopSource::Range { .. } => None,
                    }
                } else {
                    None
                };
                let preallocated = exact_capacity.is_some();
                let storage = if let Some(capacity) = exact_capacity {
                    // Rua Vec uses a one-based sequence plus an `n` record field.
                    self.table_create(capacity, 2)
                } else {
                    LuaExpr::named_table(vec![("n".into(), LuaExpr::integer("0"))])
                };
                let vector = self.standard_call("std::vec", "from_table", vec![storage]);
                let (result, sunk) = self.init_iter_result(dest, local_result, vector);
                if preallocated {
                    self.assign(result.clone().field("n"), LuaExpr::integer("0"));
                }
                Some((result, sunk))
            }
            IterConsumerKind::Fold => {
                let init = chain
                    .consumer_args
                    .first()
                    .map(|arg| self.gen_inline(arg))
                    .unwrap_or(LuaExpr::Nil);
                Some(self.init_iter_result(dest, local_result, init))
            }
            IterConsumerKind::Count => {
                Some(self.init_iter_result(dest, local_result, LuaExpr::integer("0")))
            }
            IterConsumerKind::Any | IterConsumerKind::All => {
                let initial = plan.consumer == IterConsumerKind::All;
                Some(self.init_iter_result(dest, local_result, LuaExpr::Bool(initial)))
            }
            IterConsumerKind::Find | IterConsumerKind::Next => {
                Some(self.init_iter_result(dest, local_result, LuaExpr::Nil))
            }
        };

        let item = self.fresh_tmp();
        match &source {
            IterLoopSource::Range {
                start,
                end,
                inclusive,
            } => {
                let stop = if *inclusive {
                    LuaExpr::name(end)
                } else {
                    LuaExpr::name(end).binary(LuaBinaryOp::Sub, LuaExpr::integer("1"))
                };
                let index = self.fresh_tmp();
                self.lua
                    .begin_numeric_for(&index, LuaExpr::name(start), stop);
                self.local(&item, Some(LuaExpr::name(index)));
            }
            IterLoopSource::Vec { holder } => {
                let index = self.fresh_tmp();
                self.lua.begin_numeric_for(
                    &index,
                    LuaExpr::integer("1"),
                    holder.clone().field("n"),
                );
                self.local(&item, Some(holder.clone().index(LuaExpr::name(index))));
            }
        }
        let mut open_guards = 0;

        for (adapter, state) in chain.adapters.iter().zip(&states) {
            match adapter.kind {
                IterAdapterKind::Map => {
                    if let Some(closure) = adapter.args.first() {
                        let mapped = self.gen_inlined_closure(
                            closure,
                            std::slice::from_ref(&LuaExpr::name(&item)),
                        );
                        self.assign(LuaExpr::name(&item), mapped);
                    }
                }
                IterAdapterKind::Filter => {
                    if let Some(closure) = adapter.args.first() {
                        let keep = self.gen_inlined_closure(
                            closure,
                            std::slice::from_ref(&LuaExpr::name(&item)),
                        );
                        self.lua.begin_if(keep);
                        open_guards += 1;
                    }
                }
                IterAdapterKind::FilterMap => {
                    if let Some(closure) = adapter.args.first() {
                        let mapped = self.gen_inlined_closure(
                            closure,
                            std::slice::from_ref(&LuaExpr::name(&item)),
                        );
                        self.lua
                            .begin_if(mapped.clone().binary(LuaBinaryOp::Ne, LuaExpr::Nil));
                        self.assign(LuaExpr::name(&item), mapped);
                        open_guards += 1;
                    }
                }
                IterAdapterKind::Enumerate => {
                    let counter = state.counter.as_deref().unwrap_or("0");
                    self.assign(
                        LuaExpr::name(&item),
                        LuaExpr::Table(vec![
                            TableField::Value(LuaExpr::name(counter)),
                            TableField::Value(LuaExpr::name(&item)),
                            TableField::Named("n".into(), LuaExpr::integer("2")),
                        ]),
                    );
                    self.assign(
                        LuaExpr::name(counter),
                        LuaExpr::name(counter).binary(LuaBinaryOp::Add, LuaExpr::integer("1")),
                    );
                }
                IterAdapterKind::Skip => {
                    let counter = state.counter.as_deref().unwrap_or("0");
                    let limit = state.limit.as_deref().unwrap_or("0");
                    self.lua.begin_if(
                        LuaExpr::name(counter).binary(LuaBinaryOp::Lt, LuaExpr::name(limit)),
                    );
                    self.assign(
                        LuaExpr::name(counter),
                        LuaExpr::name(counter).binary(LuaBinaryOp::Add, LuaExpr::integer("1")),
                    );
                    self.lua.begin_else();
                    open_guards += 1;
                }
                IterAdapterKind::Take => {
                    let counter = state.counter.as_deref().unwrap_or("0");
                    let limit = state.limit.as_deref().unwrap_or("0");
                    self.lua.compact_if(
                        LuaExpr::name(counter).binary(LuaBinaryOp::Ge, LuaExpr::name(limit)),
                        vec![InlineStatement::Break],
                    );
                    self.assign(
                        LuaExpr::name(counter),
                        LuaExpr::name(counter).binary(LuaBinaryOp::Add, LuaExpr::integer("1")),
                    );
                }
            }
        }

        match plan.consumer {
            IterConsumerKind::For => {
                if let Some((var, body)) = for_body {
                    self.local(var, Some(LuaExpr::name(&item)));
                    self.gen_block_to(body, &Dest::Discard);
                }
            }
            IterConsumerKind::CollectVec => {
                let result = &result.as_ref().unwrap().0;
                let length = result.clone().field("n");
                self.assign(
                    result.clone().index(
                        length
                            .clone()
                            .binary(LuaBinaryOp::Add, LuaExpr::integer("1")),
                    ),
                    LuaExpr::name(&item),
                );
                self.assign(
                    length.clone(),
                    length.binary(LuaBinaryOp::Add, LuaExpr::integer("1")),
                );
            }
            IterConsumerKind::Fold => {
                if let (Some(result), Some(closure)) = (
                    result.as_ref().map(|result| &result.0),
                    chain.consumer_args.get(1),
                ) {
                    let inputs = [result.clone(), LuaExpr::name(&item)];
                    let next = self.gen_inlined_closure(closure, &inputs);
                    self.assign(result.clone(), next);
                }
            }
            IterConsumerKind::Count => {
                let result = &result.as_ref().unwrap().0;
                self.assign(
                    result.clone(),
                    result
                        .clone()
                        .binary(LuaBinaryOp::Add, LuaExpr::integer("1")),
                );
            }
            IterConsumerKind::Any | IterConsumerKind::All | IterConsumerKind::Find => {
                if let (Some(result), Some(predicate)) = (
                    result.as_ref().map(|result| &result.0),
                    chain.consumer_args.first(),
                ) {
                    let matches = self.gen_inlined_closure(
                        predicate,
                        std::slice::from_ref(&LuaExpr::name(&item)),
                    );
                    match plan.consumer {
                        IterConsumerKind::Any => {
                            self.lua.compact_if(
                                matches,
                                vec![
                                    InlineStatement::assign(result.clone(), LuaExpr::Bool(true)),
                                    InlineStatement::Break,
                                ],
                            );
                        }
                        IterConsumerKind::All => {
                            self.lua.compact_if(
                                LuaExpr::unary(LuaUnaryOp::Not, matches),
                                vec![
                                    InlineStatement::assign(result.clone(), LuaExpr::Bool(false)),
                                    InlineStatement::Break,
                                ],
                            );
                        }
                        IterConsumerKind::Find => {
                            self.lua.compact_if(
                                matches,
                                vec![
                                    InlineStatement::assign(result.clone(), LuaExpr::name(&item)),
                                    InlineStatement::Break,
                                ],
                            );
                        }
                        _ => {}
                    }
                }
            }
            IterConsumerKind::Next => {
                let result = &result.as_ref().unwrap().0;
                self.assign(result.clone(), LuaExpr::name(&item));
                self.lua.break_statement();
            }
        }
        for _ in 0..open_guards {
            self.lua.end_block();
        }

        if plan.consumer == IterConsumerKind::For
            && let Some((_, body)) = for_body
            && Self::block_has_continue(body)
        {
            self.lua.label("continue");
        }
        for (adapter, state) in chain.adapters.iter().zip(&states) {
            if adapter.kind == IterAdapterKind::Take {
                let counter = state.counter.as_deref().unwrap_or("0");
                let limit = state.limit.as_deref().unwrap_or("0");
                self.lua.compact_if(
                    LuaExpr::name(counter).binary(LuaBinaryOp::Ge, LuaExpr::name(limit)),
                    vec![InlineStatement::Break],
                );
            }
        }
        self.lua.end_block();

        if let Some((result, sunk)) = result {
            match dest {
                Dest::Var(target) if !sunk => self.assign(target.clone(), result),
                Dest::Return => self.return_value(result),
                Dest::Discard => {}
                Dest::Var(_) => {}
            }
        }
    }

    // --- expression to destination ----------------------------------------

    fn gen_loop_to(&mut self, body: &Block, dest: &Dest) {
        let (break_target, return_target) = match dest {
            Dest::Discard => (None, None),
            Dest::Var(target) => (Some(target.clone()), None),
            Dest::Return => {
                let temporary = self.fresh_tmp();
                self.local(&temporary, None);
                let target = LuaExpr::name(&temporary);
                (Some(target.clone()), Some(target))
            }
        };

        self.lua.begin_while(LuaExpr::Bool(true));
        self.loop_break_targets.push(break_target);
        self.gen_block_to(body, &Dest::Discard);
        self.loop_break_targets.pop();
        if Self::block_has_continue(body) {
            self.lua.label("continue");
        }
        self.lua.end_block();

        if let Some(target) = return_target {
            self.return_value(target);
        }
    }

    /// Materialize the address components of a compound-assignment target
    /// before evaluating its right-hand side.
    fn gen_assignment_place(&mut self, target: &Expr) -> LuaExpr {
        match &target.kind {
            ExprKind::Field { base, name, .. } => {
                let base_value = self.gen_inline(base);
                let temporary = self.fresh_tmp();
                self.local(&temporary, Some(base_value));
                LuaExpr::name(temporary).field(self.layout.member_name(name))
            }
            ExprKind::Index { base, index } => {
                let base_value = self.gen_inline(base);
                let base_temporary = self.fresh_tmp();
                self.local(&base_temporary, Some(base_value));
                let index_value = self.gen_inline(index);
                let index_temporary = self.fresh_tmp();
                self.local(&index_temporary, Some(index_value));
                LuaExpr::name(base_temporary).index(LuaExpr::name(index_temporary))
            }
            _ => self.gen_inline(target),
        }
    }

    fn compound_value(
        &mut self,
        expression: ExprId,
        op: BinOp,
        lhs: LuaExpr,
        rhs: LuaExpr,
    ) -> LuaExpr {
        if op == BinOp::Div && self.info.is_int_div(expression) {
            return self.helper_call("number", "idiv", vec![lhs, rhs]);
        }
        if op == BinOp::Rem && self.info.is_int_rem(expression) {
            return self.helper_call("number", "irem", vec![lhs, rhs]);
        }
        if op == BinOp::Add && self.info.is_str_concat(expression) {
            return lhs.binary(LuaBinaryOp::Concat, rhs).parenthesized();
        }
        lhs.binary(binop_lua(op), rhs)
    }

    fn gen_block_to(&mut self, block: &Block, dest: &Dest) {
        self.local_substitutions.push(HashMap::new());
        for (index, statement) in block.stmts.iter().enumerate() {
            if block.statement_blank_before[index] {
                self.blank();
            }
            self.gen_stmt(statement);
        }
        match &block.tail {
            Some(e) => {
                if block.tail_blank_before {
                    self.blank();
                }
                self.gen_expr_to(e, dest);
            }
            None => {
                if let Dest::Var(d) = dest {
                    self.assign(d.clone(), LuaExpr::Nil);
                }
            }
        }
        self.local_substitutions.pop();
    }

    fn gen_expr_to(&mut self, expression: &Expr, dest: &Dest) {
        self.lua.push_anchor(expression.span);
        self.gen_expr_to_inner(expression, dest);
        self.lua.pop_anchor();
    }

    fn gen_expr_to_inner(&mut self, e: &Expr, dest: &Dest) {
        if let Some(plan) = self.info.iter_plan(e.id)
            && let Some(chain) = extract_iter_chain(e, plan, true)
        {
            self.gen_iter_loop(&chain, plan, None, dest, None);
            return;
        }
        match &e.kind {
            ExprKind::Loop(body) => self.gen_loop_to(body, dest),
            ExprKind::If {
                cond,
                then_block,
                else_block,
            } => {
                let c = self.gen_inline(cond);
                self.lua.begin_if(c);
                self.gen_block_to(then_block, dest);
                match else_block.as_deref() {
                    Some(ElseBranch::Block(b)) => {
                        self.lua.begin_else();
                        self.gen_block_to(b, dest);
                    }
                    Some(ElseBranch::If(inner)) => {
                        self.lua.begin_else();
                        self.gen_expr_to(inner, dest);
                    }
                    None => {
                        if let Dest::Var(d) = dest {
                            self.lua.begin_else();
                            self.assign(d.clone(), LuaExpr::Nil);
                        }
                    }
                }
                self.lua.end_block();
            }
            ExprKind::IfLet {
                pat,
                expr,
                then_block,
                else_block,
            } => {
                let s = self.gen_inline(expr);
                let subject = if self.is_immutable_local_path(expr) {
                    s
                } else {
                    let temporary = self.fresh_tmp();
                    self.local(&temporary, Some(s));
                    LuaExpr::name(temporary)
                };
                let mut tests = Vec::new();
                let mut binds = Vec::new();
                self.pat_test(pat, subject, None, &mut tests, &mut binds);
                self.lua.begin_if(and_all(tests));
                self.emit_pattern_bindings(&binds);
                self.gen_block_to(then_block, dest);
                match else_block.as_deref() {
                    Some(ElseBranch::Block(b)) => {
                        self.lua.begin_else();
                        self.gen_block_to(b, dest);
                    }
                    Some(ElseBranch::If(inner)) => {
                        self.lua.begin_else();
                        self.gen_expr_to(inner, dest);
                    }
                    None => {
                        if let Dest::Var(d) = dest {
                            self.lua.begin_else();
                            self.assign(d.clone(), LuaExpr::Nil);
                        }
                    }
                }
                self.lua.end_block();
            }
            ExprKind::Block(b) => self.gen_block_to(b, dest),
            ExprKind::Match { scrut, arms } => self.gen_match(scrut, arms, dest),
            ExprKind::Assign { op, target, value } => {
                if let Some(op) = op {
                    let target = self.gen_assignment_place(target);
                    let rhs = self.gen_inline(value);
                    let assigned = self.compound_value(e.id, *op, target.clone(), rhs);
                    self.assign(target, assigned);
                } else {
                    let target = self.gen_inline(target);
                    self.gen_expr_to(value, &Dest::Var(target));
                }
                match dest {
                    Dest::Var(d) => self.assign(d.clone(), LuaExpr::Nil),
                    Dest::Return => self.return_value(LuaExpr::Nil),
                    Dest::Discard => {}
                }
            }
            _ => {
                let v = self.gen_inline(e);
                match dest {
                    Dest::Discard => {
                        if v.is_statement_call() {
                            self.expression_statement(v);
                        }
                    }
                    Dest::Var(d) => self.assign(d.clone(), v),
                    Dest::Return => self.return_value(v),
                }
            }
        }
    }

    // --- match -------------------------------------------------------------

    fn gen_match(&mut self, scrut: &Expr, arms: &[MatchArm], dest: &Dest) {
        if matches!(dest, Dest::Return) && arms.iter().all(|arm| arm.guard.is_none()) {
            self.gen_return_match(scrut, arms);
            return;
        }

        let m = self.fresh_tmp();
        let s = self.gen_inline(scrut);
        self.local(&m, Some(s));
        let matched = self.fresh_tmp();
        self.local(&matched, Some(LuaExpr::Bool(false)));

        for arm in arms {
            let (tests, binds) = self.arm_tests(arm, LuaExpr::name(&m), None);
            let unmatched = LuaExpr::unary(LuaUnaryOp::Not, LuaExpr::name(&matched));
            let condition = if tests.is_empty() {
                unmatched
            } else {
                unmatched.binary(LuaBinaryOp::And, and_all(tests).parenthesized())
            };
            self.lua.begin_if(condition);

            self.emit_pattern_bindings(&binds);
            let guard = arm.guard.as_ref().map(|g| self.gen_inline(g));
            if let Some(g) = &guard {
                self.lua.begin_if(g.clone());
            }

            self.assign(LuaExpr::name(&matched), LuaExpr::Bool(true));
            self.gen_expr_to(&arm.body, dest);

            if guard.is_some() {
                self.lua.end_block();
            }
            self.lua.end_block();
        }

        self.lua.compact_if(
            LuaExpr::unary(LuaUnaryOp::Not, LuaExpr::name(matched)),
            vec![InlineStatement::expression(
                LuaExpr::name("error").call(vec![LuaExpr::string("non-exhaustive match")]),
            )],
        );
    }

    fn root_pattern_target(&self, pattern: &Pattern) -> Option<crate::hir::ResolvedTarget> {
        match pattern {
            Pattern::Path { id, .. }
            | Pattern::TupleVariant { id, .. }
            | Pattern::StructVariant { id, .. } => Some(self.pattern_target(*id)),
            _ => None,
        }
    }

    fn match_uses_root_tag(&self, arms: &[MatchArm]) -> bool {
        arms.iter().flat_map(|arm| &arm.pats).any(|pattern| {
            matches!(
                self.root_pattern_target(pattern),
                Some(crate::hir::ResolvedTarget::Builtin(
                    rua_core::BuiltinId::VariantResultOk | rua_core::BuiltinId::VariantResultErr
                ))
            ) || self
                .root_pattern_target(pattern)
                .is_some_and(|target| self.resolve_variant(Some(target)).is_some())
        })
    }

    fn match_uses_root_result_tag(&self, arms: &[MatchArm]) -> bool {
        arms.iter().flat_map(|arm| &arm.pats).any(|pattern| {
            matches!(
                self.root_pattern_target(pattern),
                Some(crate::hir::ResolvedTarget::Builtin(
                    rua_core::BuiltinId::VariantResultOk | rua_core::BuiltinId::VariantResultErr
                ))
            )
        })
    }

    fn match_is_exhaustive(&self, arms: &[MatchArm]) -> bool {
        let mut builtins = HashSet::new();
        let mut enum_owner = None;
        let mut enum_variants = HashSet::new();
        let mut booleans = HashSet::new();

        for pattern in arms.iter().flat_map(|arm| &arm.pats) {
            match pattern {
                Pattern::Wildcard | Pattern::Binding(_, _) => return true,
                Pattern::Literal(expression) => {
                    if let ExprKind::Bool(value) = expression.kind {
                        booleans.insert(value);
                    }
                }
                _ => match self.root_pattern_target(pattern) {
                    Some(crate::hir::ResolvedTarget::Builtin(builtin)) => {
                        builtins.insert(builtin);
                    }
                    Some(target) => {
                        if let Some((owner, variant, _)) = self.resolve_variant(Some(target)) {
                            if enum_owner.is_some_and(|existing| existing != owner) {
                                return false;
                            }
                            enum_owner = Some(owner);
                            enum_variants.insert(variant);
                        }
                    }
                    None => {}
                },
            }
        }

        let option = builtins.contains(&rua_core::BuiltinId::VariantOptionSome)
            && builtins.contains(&rua_core::BuiltinId::VariantOptionNone);
        let result = builtins.contains(&rua_core::BuiltinId::VariantResultOk)
            && builtins.contains(&rua_core::BuiltinId::VariantResultErr);
        if option || result || booleans.len() == 2 {
            return true;
        }
        let Some(owner) = enum_owner else {
            return false;
        };
        builtins.is_empty()
            && booleans.is_empty()
            && enum_variants.len()
                == self
                    .hir
                    .enum_variants
                    .keys()
                    .filter(|(variant_owner, _)| *variant_owner == owner)
                    .count()
    }

    fn gen_return_match(&mut self, scrut: &Expr, arms: &[MatchArm]) {
        let value = self.gen_inline(scrut);
        let subject = if matches!(&scrut.kind, ExprKind::Path(_)) {
            value
        } else {
            let temporary = self.fresh_tmp();
            self.local(&temporary, Some(value));
            LuaExpr::name(temporary)
        };

        let proven_exhaustive = self.match_is_exhaustive(arms);
        let conditional_arms = arms.len().saturating_sub(usize::from(proven_exhaustive));
        let cached_tag = (conditional_arms >= 2 && self.match_uses_root_tag(arms)).then(|| {
            let temporary = self.fresh_tmp();
            let tag = if self.match_uses_root_result_tag(arms) {
                result_is_ok(subject.clone())
            } else {
                subject.clone().field("tag")
            };
            self.local(&temporary, Some(tag));
            LuaExpr::name(temporary)
        });

        let mut open = false;
        let mut exhaustive = false;
        for (index, arm) in arms.iter().enumerate() {
            let (tests, binds) = self.arm_tests(arm, subject.clone(), cached_tag.as_ref());
            let final_exhaustive_arm = proven_exhaustive && index + 1 == arms.len();
            if tests.is_empty() || final_exhaustive_arm {
                if open {
                    self.lua.begin_else();
                }
                self.emit_pattern_bindings(&binds);
                self.gen_expr_to(&arm.body, &Dest::Return);
                exhaustive = true;
                break;
            }

            let condition = and_all(tests);
            if open {
                self.lua.begin_else_if(condition);
            } else {
                self.lua.begin_if(condition);
                open = true;
            }
            self.emit_pattern_bindings(&binds);
            self.gen_expr_to(&arm.body, &Dest::Return);
        }

        if !open {
            if !exhaustive {
                self.expression_statement(
                    LuaExpr::name("error").call(vec![LuaExpr::string("non-exhaustive match")]),
                );
            }
            return;
        }
        if !exhaustive {
            self.lua.begin_else();
            self.expression_statement(
                LuaExpr::name("error").call(vec![LuaExpr::string("non-exhaustive match")]),
            );
        }
        self.lua.end_block();
    }

    /// Structural tests + bindings for a match arm against subject variable `m`.
    fn arm_tests(
        &mut self,
        arm: &MatchArm,
        subject: LuaExpr,
        cached_tag: Option<&LuaExpr>,
    ) -> (Vec<LuaExpr>, Vec<PatternBinding>) {
        if arm.pats.len() == 1 {
            let mut tests = Vec::new();
            let mut binds = Vec::new();
            self.pat_test(&arm.pats[0], subject, cached_tag, &mut tests, &mut binds);
            binds.retain(|(name, _, _)| {
                arm.guard
                    .as_ref()
                    .is_some_and(|guard| expression_mentions_binding(guard, name))
                    || expression_mentions_binding(&arm.body, name)
            });
            (tests, binds)
        } else {
            // or-patterns: combine alternatives; bindings are not supported here.
            let mut alts = Vec::new();
            for p in &arm.pats {
                let mut tests = Vec::new();
                let mut binds = Vec::new();
                self.pat_test(p, subject.clone(), cached_tag, &mut tests, &mut binds);
                alts.push(and_all(tests).parenthesized());
            }
            (vec![or_all(alts)], Vec::new())
        }
    }

    fn pat_test(
        &mut self,
        pat: &Pattern,
        subject: LuaExpr,
        cached_tag: Option<&LuaExpr>,
        tests: &mut Vec<LuaExpr>,
        binds: &mut Vec<PatternBinding>,
    ) {
        match pat {
            Pattern::Wildcard => {}
            Pattern::Binding(name, source) => binds.push((name.clone(), *source, subject)),
            Pattern::Literal(lit) => {
                let v = self.gen_inline(lit);
                tests.push(subject.binary(LuaBinaryOp::Eq, v));
            }
            Pattern::Range { lo, hi, inclusive } => {
                let l = self.gen_inline(lo);
                let h = self.gen_inline(hi);
                let upper = if *inclusive {
                    LuaBinaryOp::Le
                } else {
                    LuaBinaryOp::Lt
                };
                tests.push(
                    subject
                        .clone()
                        .binary(LuaBinaryOp::Ge, l)
                        .binary(LuaBinaryOp::And, subject.binary(upper, h))
                        .parenthesized(),
                );
            }
            Pattern::Path { id, .. } => {
                let target = self.pattern_target(*id);
                match target {
                    crate::hir::ResolvedTarget::Builtin(rua_core::BuiltinId::VariantOptionNone) => {
                        tests.push(subject.binary(LuaBinaryOp::Eq, LuaExpr::Nil));
                    }
                    crate::hir::ResolvedTarget::Builtin(rua_core::BuiltinId::VariantOptionSome) => {
                        tests.push(subject.binary(LuaBinaryOp::Ne, LuaExpr::Nil))
                    }
                    crate::hir::ResolvedTarget::Builtin(rua_core::BuiltinId::VariantResultOk) => {
                        tests.push(cached_tag.cloned().unwrap_or_else(|| result_is_ok(subject)))
                    }
                    crate::hir::ResolvedTarget::Builtin(rua_core::BuiltinId::VariantResultErr) => {
                        let tag = cached_tag.cloned().unwrap_or_else(|| result_is_ok(subject));
                        tests.push(LuaExpr::unary(LuaUnaryOp::Not, tag))
                    }
                    _ => {
                        let (_, variant, _) = self
                            .resolve_variant(Some(target))
                            .expect("checked path pattern resolves to an enum variant");
                        tests.push(
                            cached_tag
                                .cloned()
                                .unwrap_or_else(|| subject.field("tag"))
                                .binary(
                                    LuaBinaryOp::Eq,
                                    LuaExpr::string(&self.hir.definition(variant).name),
                                ),
                        );
                    }
                }
            }
            Pattern::TupleVariant { id, elems, .. } => {
                let target = self.pattern_target(*id);
                match target {
                    crate::hir::ResolvedTarget::Builtin(rua_core::BuiltinId::VariantOptionSome) => {
                        // Some(x) is the bare value; None is nil.
                        tests.push(subject.clone().binary(LuaBinaryOp::Ne, LuaExpr::Nil));
                        if let Some(inner) = elems.first() {
                            self.pat_test(inner, subject, None, tests, binds);
                        }
                    }
                    crate::hir::ResolvedTarget::Builtin(rua_core::BuiltinId::VariantResultOk) => {
                        tests.push(
                            cached_tag
                                .cloned()
                                .unwrap_or_else(|| result_is_ok(subject.clone())),
                        );
                        if let Some(inner) = elems.first() {
                            self.pat_test(inner, result_payload(subject), None, tests, binds);
                        }
                    }
                    crate::hir::ResolvedTarget::Builtin(rua_core::BuiltinId::VariantResultErr) => {
                        let tag = cached_tag
                            .cloned()
                            .unwrap_or_else(|| result_is_ok(subject.clone()));
                        tests.push(LuaExpr::unary(LuaUnaryOp::Not, tag));
                        if let Some(inner) = elems.first() {
                            self.pat_test(inner, result_payload(subject), None, tests, binds);
                        }
                    }
                    _ => {
                        let (_, variant, crate::hir::VariantShape::Tuple) = self
                            .resolve_variant(Some(target))
                            .expect("checked tuple pattern resolves to a tuple enum variant")
                        else {
                            unreachable!("checked tuple pattern has tuple variant shape")
                        };
                        tests.push(
                            cached_tag
                                .cloned()
                                .unwrap_or_else(|| subject.clone().field("tag"))
                                .binary(
                                    LuaBinaryOp::Eq,
                                    LuaExpr::string(&self.hir.definition(variant).name),
                                ),
                        );
                        for (i, elem) in elems.iter().enumerate() {
                            let sub = subject.clone().index(LuaExpr::integer((i + 1).to_string()));
                            self.pat_test(elem, sub, None, tests, binds);
                        }
                    }
                }
            }
            Pattern::StructVariant {
                id,
                path: _,
                fields,
                ..
            } => {
                // If it resolves to an enum variant, test the tag; a plain struct
                // pattern needs no tag test.
                if let Some((_, variant, _)) =
                    self.resolve_variant(Some(self.program.pattern_target(*id)))
                {
                    tests.push(
                        cached_tag
                            .cloned()
                            .unwrap_or_else(|| subject.clone().field("tag"))
                            .binary(
                                LuaBinaryOp::Eq,
                                LuaExpr::string(&self.hir.definition(variant).name),
                            ),
                    );
                }
                for (fname, fpat) in fields {
                    let sub = subject.clone().field(self.layout.member_name(fname));
                    self.pat_test(fpat, sub, None, tests, binds);
                }
            }
        }
    }

    // --- inline (pure Lua expression, may hoist) --------------------------

    fn gen_inline(&mut self, expression: &Expr) -> LuaExpr {
        self.lua.push_anchor(expression.span);
        let value = self.gen_inline_inner(expression);
        self.lua.pop_anchor();
        value
    }

    fn gen_inline_inner(&mut self, e: &Expr) -> LuaExpr {
        if let Some(plan) = self.info.iter_plan(e.id)
            && let Some(chain) = extract_iter_chain(e, plan, true)
        {
            let tmp = self.fresh_tmp();
            self.local(&tmp, None);
            self.gen_iter_loop(&chain, plan, None, &Dest::Var(LuaExpr::name(&tmp)), None);
            return LuaExpr::name(tmp);
        }
        match &e.kind {
            ExprKind::Int(s) => LuaExpr::integer(lua_int_literal(s)),
            ExprKind::Float(s) => LuaExpr::number(s.replace('_', "")),
            ExprKind::Str(s) => LuaExpr::string_literal(s),
            ExprKind::Bool(value) => LuaExpr::Bool(*value),
            ExprKind::VecLit(elements) => self.gen_vec_literal(elements),
            ExprKind::Closure { params, body, .. } => self.gen_closure_value(params, body),
            ExprKind::Path(_) => self.gen_path(e),
            ExprKind::Unary { op, expr } => {
                let inner = self.gen_inline(expr);
                match op {
                    UnOp::Neg => LuaExpr::unary(LuaUnaryOp::Neg, inner),
                    UnOp::Not => LuaExpr::unary(LuaUnaryOp::Not, inner),
                }
            }
            ExprKind::Binary { op, lhs, rhs } => {
                if *op == BinOp::Coalesce {
                    let value = self.gen_inline(lhs);
                    let temporary = self.fresh_tmp();
                    self.local(&temporary, Some(value));
                    self.lua
                        .begin_if(LuaExpr::name(&temporary).binary(LuaBinaryOp::Eq, LuaExpr::Nil));
                    let fallback = self.gen_inline(rhs);
                    self.assign(LuaExpr::name(&temporary), fallback);
                    self.lua.end_block();
                    return LuaExpr::name(temporary);
                }
                if *op == BinOp::Contains {
                    let needle = self.gen_inline(lhs);
                    let container = self.gen_inline(rhs);
                    return match self
                        .info
                        .contains_kind(e.id)
                        .expect("type-checked `in` expression has a container kind")
                    {
                        crate::typeck::ContainsKind::Vec => {
                            container.method_call(self.layout.member_name("contains"), vec![needle])
                        }
                        crate::typeck::ContainsKind::Map => container
                            .method_call(self.layout.member_name("contains_key"), vec![needle]),
                        crate::typeck::ContainsKind::String => {
                            self.standard_call("std::string", "contains", vec![container, needle])
                        }
                        crate::typeck::ContainsKind::Iter => {
                            container.method_call(self.layout.member_name("contains"), vec![needle])
                        }
                    };
                }
                if let (ExprKind::Int(left), ExprKind::Int(right)) = (&lhs.kind, &rhs.kind)
                    && let (Some(left), Some(right)) = (rua_int_value(left), rua_int_value(right))
                    && right != 0
                {
                    let folded = if *op == BinOp::Div && self.info.is_int_div(e.id) {
                        left.checked_div(right)
                    } else if *op == BinOp::Rem && self.info.is_int_rem(e.id) {
                        left.checked_rem(right)
                    } else {
                        None
                    };
                    if let Some(value) = folded {
                        return LuaExpr::integer(value.to_string());
                    }
                }
                let l = self.gen_inline(lhs);
                let r = self.gen_inline(rhs);
                // `i64 / i64` and `i64 % i64` lower to configured number helpers, which
                // truncate toward zero to match Rust (Lua `//`/`%` floor, differing
                // when exactly one operand is negative: Rust `-7/2 == -3`,
                // `-7%2 == -1` vs Lua `-7//2 == -4`, `-7%2 == 1`).
                if *op == BinOp::Div && self.info.is_int_div(e.id) {
                    return self.helper_call("number", "idiv", vec![l, r]);
                }
                if *op == BinOp::Rem && self.info.is_int_rem(e.id) {
                    let positive_divisor = matches!(
                        &rhs.kind,
                        ExprKind::Int(source)
                            if rua_int_value(source).is_some_and(|value| value > 0)
                    );
                    if positive_divisor && self.expression_is_known_nonnegative(lhs) {
                        return l.binary(LuaBinaryOp::Rem, r);
                    }
                    return self.helper_call("number", "irem", vec![l, r]);
                }
                // `String + String` is Lua concatenation, not arithmetic.
                if *op == BinOp::Add && self.info.is_str_concat(e.id) {
                    return l.binary(LuaBinaryOp::Concat, r).parenthesized();
                }
                l.binary(binop_lua(*op), r)
            }
            ExprKind::Call { callee, args } => self.gen_call(callee, args),
            ExprKind::MethodCall {
                recv,
                method,
                optional,
                args,
                ..
            } => {
                let receiver = self.gen_inline(recv);
                let optional_target = if *optional {
                    let temporary = self.fresh_tmp();
                    self.local(&temporary, Some(receiver.clone()));
                    self.lua
                        .begin_if(LuaExpr::name(&temporary).binary(LuaBinaryOp::Ne, LuaExpr::Nil));
                    Some(temporary)
                } else {
                    None
                };
                let r = optional_target.as_ref().map_or(receiver, LuaExpr::name);
                let computed = (|| {
                    if let Some(definition) = self.info.standard_method(e.id) {
                        use crate::builtins::LanguageItem as L;
                        match self.hir.language_item(definition) {
                            Some(L::OptionMap | L::ResultMap) if args.len() == 1 => {
                                let is_option =
                                    self.hir.language_item(definition) == Some(L::OptionMap);
                                let callable = (!matches!(args[0].kind, ExprKind::Closure { .. }))
                                    .then(|| self.gen_inline(&args[0]));
                                let value = self.fresh_tmp();
                                self.local(&value, Some(r));
                                let condition = if is_option {
                                    LuaExpr::name(&value).binary(LuaBinaryOp::Ne, LuaExpr::Nil)
                                } else {
                                    result_is_ok(LuaExpr::name(&value))
                                };
                                self.lua.begin_if(condition);
                                let input = if is_option {
                                    LuaExpr::name(&value)
                                } else {
                                    result_payload(LuaExpr::name(&value))
                                };
                                let mapped = if matches!(args[0].kind, ExprKind::Closure { .. }) {
                                    self.gen_inlined_closure(&args[0], std::slice::from_ref(&input))
                                } else {
                                    callable.as_ref().unwrap().clone().call(vec![input])
                                };
                                let mapped = if is_option {
                                    mapped
                                } else {
                                    self.standard_call("std::result", "ok", vec![mapped])
                                };
                                self.assign(LuaExpr::name(&value), mapped);
                                self.lua.end_block();
                                return LuaExpr::name(value);
                            }
                            Some(L::OptionUnwrap) => {
                                let value = self.fresh_tmp();
                                self.local(&value, Some(r));
                                self.lua.compact_if(
                                    LuaExpr::name(&value).binary(LuaBinaryOp::Eq, LuaExpr::Nil),
                                    vec![InlineStatement::expression(LuaExpr::name("error").call(
                                        vec![
                                            LuaExpr::string("called Option::unwrap on None"),
                                            LuaExpr::integer("2"),
                                        ],
                                    ))],
                                );
                                return LuaExpr::name(value);
                            }
                            Some(L::OptionExpect) if args.len() == 1 => {
                                let value = self.fresh_tmp();
                                self.local(&value, Some(r));
                                let message = self.gen_inline(&args[0]);
                                self.lua.compact_if(
                                    LuaExpr::name(&value).binary(LuaBinaryOp::Eq, LuaExpr::Nil),
                                    vec![InlineStatement::expression(
                                        LuaExpr::name("error")
                                            .call(vec![message, LuaExpr::integer("2")]),
                                    )],
                                );
                                return LuaExpr::name(value);
                            }
                            Some(L::OptionUnwrapOr) if args.len() == 1 => {
                                let default = self.gen_inline(&args[0]);
                                let value = self.fresh_tmp();
                                self.local(&value, Some(r));
                                self.lua.begin_if(
                                    LuaExpr::name(&value).binary(LuaBinaryOp::Eq, LuaExpr::Nil),
                                );
                                self.assign(LuaExpr::name(&value), default);
                                self.lua.end_block();
                                return LuaExpr::name(value);
                            }
                            Some(L::OptionIsSome) => {
                                return r.binary(LuaBinaryOp::Ne, LuaExpr::Nil);
                            }
                            Some(L::OptionIsNone) => {
                                return r.binary(LuaBinaryOp::Eq, LuaExpr::Nil);
                            }
                            Some(
                                L::ResultUnwrap
                                | L::ResultExpect
                                | L::ResultUnwrapOr
                                | L::ResultIsOk
                                | L::ResultIsErr,
                            ) => {
                                let arguments = args
                                    .iter()
                                    .map(|argument| self.gen_inline(argument))
                                    .collect();
                                return r.method_call(self.layout.member_name(method), arguments);
                            }
                            _ => {}
                        }

                        if let Some(runtime) = self.hir.standard_runtime(definition).cloned() {
                            if runtime.dispatch == rua_resources::StdDispatch::Module {
                                if method == "len" {
                                    return LuaExpr::unary(LuaUnaryOp::Len, r);
                                }
                                if method == "is_empty" {
                                    return LuaExpr::unary(LuaUnaryOp::Len, r)
                                        .binary(LuaBinaryOp::Eq, LuaExpr::integer("0"));
                                }
                                if matches!(method.as_str(), "to_string" | "to_owned" | "clone") {
                                    return r;
                                }
                                let mut arguments = vec![r];
                                arguments
                                    .extend(args.iter().map(|argument| self.gen_inline(argument)));
                                return self.runtime_call(&runtime, method, arguments);
                            }
                            let arguments = args
                                .iter()
                                .map(|argument| self.gen_inline(argument))
                                .collect();
                            return r.method_call(self.layout.member_name(method), arguments);
                        }
                    }
                    let mut a = args
                        .iter()
                        .map(|argument| self.gen_inline(argument))
                        .collect::<Vec<_>>();
                    if let Some(dispatch) = self.info.user_method(e.id) {
                        use crate::typeck::UserMethodDispatch;
                        return match dispatch {
                            UserMethodDispatch::Static(definition) => {
                                let callable = self
                                    .target_place(crate::hir::ResolvedTarget::Item(definition))
                                    .expect("resolved user method has a backend place")
                                    .expression();
                                a.insert(0, r);
                                callable.call(a)
                            }
                            UserMethodDispatch::Dynamic => {
                                let receiver = if self.is_immutable_local_path(recv) {
                                    r
                                } else {
                                    let receiver = self.fresh_tmp();
                                    self.local(&receiver, Some(r));
                                    LuaExpr::name(receiver)
                                };
                                let callable = LuaExpr::name("getmetatable")
                                    .call(vec![receiver.clone()])
                                    .field(self.layout.member_name(method));
                                a.insert(0, receiver);
                                callable.call(a)
                            }
                        };
                    }
                    r.method_call(self.layout.member_name(method), a)
                })();
                if let Some(temporary) = optional_target {
                    self.assign(LuaExpr::name(&temporary), computed);
                    self.lua.end_block();
                    LuaExpr::name(temporary)
                } else {
                    computed
                }
            }
            ExprKind::Field {
                base,
                name,
                optional,
                ..
            } => {
                let base = self.gen_inline(base);
                if *optional {
                    let temporary = self.fresh_tmp();
                    self.local(&temporary, Some(base));
                    self.lua
                        .begin_if(LuaExpr::name(&temporary).binary(LuaBinaryOp::Ne, LuaExpr::Nil));
                    let field = LuaExpr::name(&temporary).field(self.layout.member_name(name));
                    self.assign(LuaExpr::name(&temporary), field);
                    self.lua.end_block();
                    LuaExpr::name(temporary)
                } else {
                    base.field(self.layout.member_name(name))
                }
            }
            ExprKind::Index { base, index } => {
                let b = self.gen_inline(base);
                let i = self.gen_inline(index);
                b.index(i)
            }
            ExprKind::Range {
                start,
                end,
                inclusive,
            } => {
                let start = self.gen_inline(start);
                let end = self.gen_inline(end);
                self.standard_call(
                    "std::iter",
                    "range",
                    vec![start, end, LuaExpr::Bool(*inclusive)],
                )
            }
            ExprKind::StructLit { fields, .. } => self.gen_struct_lit(e, fields),
            ExprKind::MapLit(entries) => self.gen_map_literal(entries),
            ExprKind::Try { expr } => {
                let inner = self.gen_inline(expr);
                let value = self.fresh_tmp();
                self.local(&value, Some(inner));
                if self.info.is_result_try(e.id) {
                    self.lua.compact_if(
                        result_is_err(LuaExpr::name(&value)),
                        vec![InlineStatement::return_value(LuaExpr::name(&value))],
                    );
                    result_payload(LuaExpr::name(value))
                } else {
                    self.lua.compact_if(
                        LuaExpr::name(&value).binary(LuaBinaryOp::Eq, LuaExpr::Nil),
                        vec![InlineStatement::return_nil()],
                    );
                    LuaExpr::name(value)
                }
            }
            // Control-flow in operand position: hoist into a temp.
            ExprKind::If { .. }
            | ExprKind::IfLet { .. }
            | ExprKind::Block(_)
            | ExprKind::Loop(_)
            | ExprKind::Match { .. }
            | ExprKind::Assign { .. } => {
                let tmp = self.fresh_tmp();
                self.local(&tmp, None);
                self.gen_expr_to(e, &Dest::Var(LuaExpr::name(&tmp)));
                LuaExpr::name(tmp)
            }
        }
    }

    fn gen_path(&mut self, expression: &Expr) -> LuaExpr {
        let target = self.program.expression_target(expression.id);
        if let crate::hir::ResolvedTarget::Local(local) = target
            && let Some(replacement) = self
                .local_substitutions
                .iter()
                .rev()
                .find_map(|scope| scope.get(&local))
        {
            return replacement.clone();
        }
        if let crate::hir::ResolvedTarget::Builtin(builtin) = target
            && matches!(
                self.builtin_rules.get(builtin),
                Some(crate::builtins::CodegenRule::Nil)
            )
        {
            return LuaExpr::Nil;
        }
        if let Some((owner, variant, shape)) = self.resolve_variant(Some(target))
            && let crate::hir::VariantShape::Unit = shape
        {
            let value = LuaExpr::named_table(vec![(
                "tag".into(),
                LuaExpr::string(&self.hir.definition(variant).name),
            )]);
            return self.attach_metatable(owner, value);
        }
        if let Some(place) = self.target_place(target) {
            return place.expression();
        }
        panic!("typed expression {:?} has no backend place", expression.id)
    }

    fn gen_map_literal(&mut self, entries: &[(Expr, Expr)]) -> LuaExpr {
        let fields = entries
            .iter()
            .map(|(key, value)| TableField::Indexed(self.gen_inline(key), self.gen_inline(value)))
            .collect();
        self.standard_call("std::hashmap", "from_table", vec![LuaExpr::Table(fields)])
    }

    fn target_place(
        &mut self,
        target: crate::hir::ResolvedTarget,
    ) -> Option<crate::backend_layout::Place> {
        if let Some(module) = self.target_module(target)
            && module != self.current_module
        {
            let data = self.hir.module(module);
            if data.is_declaration {
                if data.path.segments().first().map(String::as_str) != Some("__rua_builtin") {
                    self.register_declaration_import(module);
                }
            } else if self.is_modules() {
                self.record_module_dependency(module);
            }
        }
        self.layout.target(target).cloned()
    }

    fn target_module(&self, target: crate::hir::ResolvedTarget) -> Option<crate::hir::ModuleId> {
        match target {
            crate::hir::ResolvedTarget::Module(module) => Some(module),
            target @ (crate::hir::ResolvedTarget::Item(_)
            | crate::hir::ResolvedTarget::Extern(_)) => self
                .hir
                .definition_for_target(target)
                .map(|definition| self.hir.definition(definition).module),
            crate::hir::ResolvedTarget::Local(_)
            | crate::hir::ResolvedTarget::Builtin(_)
            | crate::hir::ResolvedTarget::Error => None,
        }
    }

    fn attach_metatable(&mut self, owner: crate::hir::DefId, value: LuaExpr) -> LuaExpr {
        if !self.metatable_types.contains(&owner) {
            return value;
        }
        let metatable = self
            .target_place(crate::hir::ResolvedTarget::Item(owner))
            .expect("type with impl has a backend place");
        setmetatable(value, metatable.expression())
    }

    fn resolve_variant(
        &self,
        target: Option<crate::hir::ResolvedTarget>,
    ) -> Option<(
        crate::hir::DefId,
        crate::hir::DefId,
        crate::hir::VariantShape,
    )> {
        let crate::hir::ResolvedTarget::Item(variant) = target? else {
            return None;
        };
        let crate::hir::DefKind::EnumVariant { owner, shape } = self.hir.definition(variant).kind
        else {
            return None;
        };
        Some((owner, variant, shape))
    }

    fn pattern_target(&self, id: PatternId) -> crate::hir::ResolvedTarget {
        self.program.pattern_target(id)
    }

    fn gen_call(&mut self, callee: &Expr, args: &[Expr]) -> LuaExpr {
        if matches!(callee.kind, ExprKind::Path(_)) {
            // Check codegen rules for builtin constructors.
            let rule = match self.program.expression_target(callee.id) {
                crate::hir::ResolvedTarget::Builtin(builtin) => self.builtin_rules.get(builtin),
                _ => None,
            };
            if let Some(rule) = rule {
                use crate::builtins::CodegenRule::*;
                match rule {
                    InlineArg if !args.is_empty() => return self.gen_inline(&args[0]),
                    TaggedResult { ok } if !args.is_empty() => {
                        let v = self.gen_inline(&args[0]);
                        let constructor = if *ok { "ok" } else { "err" };
                        return self.standard_call("std::result", constructor, vec![v]);
                    }
                    _ => {}
                }
            }
            if let Some(definition) = self
                .hir
                .definition_for_target(self.program.expression_target(callee.id))
                && let Some(runtime) = self.hir.standard_runtime(definition).cloned()
            {
                let arguments = args
                    .iter()
                    .map(|argument| self.gen_inline(argument))
                    .collect();
                return self.runtime_call(
                    &runtime,
                    &self.hir.definition(definition).name,
                    arguments,
                );
            }
            // Enum tuple-variant construction.
            let target = Some(self.program.expression_target(callee.id));
            if let Some((owner, variant, crate::hir::VariantShape::Tuple)) =
                self.resolve_variant(target)
            {
                let a: Vec<LuaExpr> = args.iter().map(|x| self.gen_inline(x)).collect();
                let mut fields = vec![TableField::Named(
                    "tag".into(),
                    LuaExpr::string(&self.hir.definition(variant).name),
                )];
                fields.extend(a.into_iter().map(TableField::Value));
                return self.attach_metatable(owner, LuaExpr::Table(fields));
            }
            // Associated function `Type::func(..)` or plain call.
            let a = args.iter().map(|x| self.gen_inline(x)).collect();
            return self.gen_path(callee).call(a);
        }
        let c = self.gen_inline(callee);
        let a = args.iter().map(|x| self.gen_inline(x)).collect();
        c.call(a)
    }

    fn gen_vec_literal(&mut self, elements: &[Expr]) -> LuaExpr {
        let length = elements.len();
        let values = elements.iter().map(|element| self.gen_inline(element));
        let mut fields = Vec::with_capacity(length + 1);
        fields.extend(values.map(TableField::Value));
        fields.push(TableField::Named(
            "n".into(),
            LuaExpr::integer(length.to_string()),
        ));
        self.standard_call("std::vec", "from_table", vec![LuaExpr::Table(fields)])
    }

    fn gen_struct_lit(&mut self, expression: &Expr, fields: &[(String, Expr)]) -> LuaExpr {
        let field_values: Vec<(String, LuaExpr)> = fields
            .iter()
            .map(|(name, expression)| (self.layout.member_name(name), self.gen_inline(expression)))
            .collect();

        // Struct variant of an enum?
        let target = Some(self.program.expression_target(expression.id));
        if let Some((owner, variant, crate::hir::VariantShape::Struct)) =
            self.resolve_variant(target)
        {
            let mut values = vec![(
                "tag".into(),
                LuaExpr::string(&self.hir.definition(variant).name),
            )];
            values.extend(field_values);
            return self.attach_metatable(owner, LuaExpr::named_table(values));
        }

        if let Some(crate::hir::ResolvedTarget::Item(definition)) = target
            && self.hir.definition(definition).kind == crate::hir::DefKind::Struct
        {
            return self.attach_metatable(definition, LuaExpr::named_table(field_values));
        }
        LuaExpr::named_table(field_values)
    }
}

fn extract_iter_chain<'a>(
    expr: &'a Expr,
    plan: &IterPlan,
    has_consumer: bool,
) -> Option<IterChain<'a>> {
    let (mut cursor, consumer_args): (&Expr, &[Expr]) = if has_consumer {
        let ExprKind::MethodCall { recv, args, .. } = &expr.kind else {
            return None;
        };
        (recv, args)
    } else {
        (expr, &[])
    };

    let mut adapters = Vec::with_capacity(plan.adapters.len());
    for adapter in plan.adapters.iter().rev() {
        let ExprKind::MethodCall { recv, args, .. } = &cursor.kind else {
            return None;
        };
        adapters.push(IterCall {
            kind: adapter.kind,
            args,
        });
        cursor = recv;
    }
    adapters.reverse();

    let mut source_args = &[][..];
    if matches!(
        plan.source.kind,
        IterSourceKind::VecIter | IterSourceKind::VecIntoIter
    ) {
        let ExprKind::MethodCall { recv, .. } = &cursor.kind else {
            return None;
        };
        cursor = recv;
    } else if matches!(
        plan.source.kind,
        IterSourceKind::StringChars | IterSourceKind::StringSplit
    ) {
        let ExprKind::MethodCall { recv, args, .. } = &cursor.kind else {
            return None;
        };
        cursor = recv;
        source_args = args;
    }

    Some(IterChain {
        source: cursor,
        source_args,
        adapters,
        consumer_args,
    })
}

/// Collect trait declarations across all scopes, keyed by resolved identity.
fn collect_traits<'p>(
    items: &'p [Item],
    module: crate::hir::ModuleId,
    program: &crate::typed_ir::TypedProgram,
    out: &mut HashMap<crate::hir::DefId, &'p TraitDecl>,
) {
    for (item_index, item) in items.iter().enumerate() {
        match item {
            Item::Trait(t) => {
                out.insert(program.item_definition(module, item_index), t);
            }
            Item::Mod(m) => {
                let child = program.child_module(module, item_index);
                collect_traits(&m.items, child, program, out);
            }
            _ => {}
        }
    }
}

fn collect_metatable_types(
    items: &[Item],
    module: crate::hir::ModuleId,
    program: &crate::typed_ir::TypedProgram,
    out: &mut HashSet<crate::hir::DefId>,
) {
    for (item_index, item) in items.iter().enumerate() {
        match item {
            Item::Impl(_) => {
                let implementation = program.implementation(module, item_index);
                if implementation.trait_target.is_some()
                    || program.hir().definition(implementation.owner).is_public
                {
                    out.insert(implementation.owner);
                }
            }
            Item::Mod(child) if !child.is_decl => {
                collect_metatable_types(
                    &child.items,
                    program.child_module(module, item_index),
                    program,
                    out,
                );
            }
            _ => {}
        }
    }
}

fn block_mentions_binding(block: &Block, name: &str) -> bool {
    block
        .stmts
        .iter()
        .any(|statement| statement_mentions_binding(statement, name))
        || block
            .tail
            .as_deref()
            .is_some_and(|tail| expression_mentions_binding(tail, name))
}

fn statement_mentions_binding(statement: &Stmt, name: &str) -> bool {
    match statement {
        Stmt::Let { init, .. } | Stmt::Expr(init) => expression_mentions_binding(init, name),
        Stmt::Return(value) => value
            .as_ref()
            .is_some_and(|value| expression_mentions_binding(value, name)),
        Stmt::While { cond, body } => {
            expression_mentions_binding(cond, name) || block_mentions_binding(body, name)
        }
        Stmt::Loop { body } => block_mentions_binding(body, name),
        Stmt::For { iter, body, .. } => {
            expression_mentions_binding(iter, name) || block_mentions_binding(body, name)
        }
        Stmt::WhileLet {
            pat, expr, body, ..
        } => {
            pattern_mentions_binding(pat, name)
                || expression_mentions_binding(expr, name)
                || block_mentions_binding(body, name)
        }
        Stmt::Break(value) => value
            .as_ref()
            .is_some_and(|value| expression_mentions_binding(value, name)),
        Stmt::Continue => false,
    }
}

fn expression_mentions_binding(expression: &Expr, name: &str) -> bool {
    match &expression.kind {
        ExprKind::Path(path) => path.len() == 1 && path[0] == name,
        ExprKind::Closure { body, .. } => match body {
            ClosureBody::Expr(expression) => expression_mentions_binding(expression, name),
            ClosureBody::Block(block) => block_mentions_binding(block, name),
        },
        ExprKind::Unary { expr, .. } | ExprKind::Try { expr } => {
            expression_mentions_binding(expr, name)
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            expression_mentions_binding(lhs, name) || expression_mentions_binding(rhs, name)
        }
        ExprKind::Call { callee, args } => {
            expression_mentions_binding(callee, name)
                || args
                    .iter()
                    .any(|argument| expression_mentions_binding(argument, name))
        }
        ExprKind::MethodCall { recv, args, .. } => {
            expression_mentions_binding(recv, name)
                || args
                    .iter()
                    .any(|argument| expression_mentions_binding(argument, name))
        }
        ExprKind::Field { base, .. } => expression_mentions_binding(base, name),
        ExprKind::StructLit { fields, .. } => fields
            .iter()
            .any(|(_, value)| expression_mentions_binding(value, name)),
        ExprKind::MapLit(entries) => entries.iter().any(|(key, value)| {
            expression_mentions_binding(key, name) || expression_mentions_binding(value, name)
        }),
        ExprKind::Match { scrut, arms } => {
            expression_mentions_binding(scrut, name)
                || arms.iter().any(|arm| {
                    arm.pats
                        .iter()
                        .any(|pattern| pattern_mentions_binding(pattern, name))
                        || arm
                            .guard
                            .as_ref()
                            .is_some_and(|guard| expression_mentions_binding(guard, name))
                        || expression_mentions_binding(&arm.body, name)
                })
        }
        ExprKind::Range { start, end, .. } => {
            expression_mentions_binding(start, name) || expression_mentions_binding(end, name)
        }
        ExprKind::Index { base, index } => {
            expression_mentions_binding(base, name) || expression_mentions_binding(index, name)
        }
        ExprKind::VecLit(elements) => elements
            .iter()
            .any(|element| expression_mentions_binding(element, name)),
        ExprKind::If {
            cond,
            then_block,
            else_block,
        } => {
            expression_mentions_binding(cond, name)
                || block_mentions_binding(then_block, name)
                || else_block
                    .as_deref()
                    .is_some_and(|branch| else_mentions_binding(branch, name))
        }
        ExprKind::IfLet {
            pat,
            expr,
            then_block,
            else_block,
        } => {
            pattern_mentions_binding(pat, name)
                || expression_mentions_binding(expr, name)
                || block_mentions_binding(then_block, name)
                || else_block
                    .as_deref()
                    .is_some_and(|branch| else_mentions_binding(branch, name))
        }
        ExprKind::Block(block) => block_mentions_binding(block, name),
        ExprKind::Loop(block) => block_mentions_binding(block, name),
        ExprKind::Assign { target, value, .. } => {
            expression_mentions_binding(target, name) || expression_mentions_binding(value, name)
        }
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Bool(_) => false,
    }
}

fn else_mentions_binding(branch: &ElseBranch, name: &str) -> bool {
    match branch {
        ElseBranch::Block(block) => block_mentions_binding(block, name),
        ElseBranch::If(expression) => expression_mentions_binding(expression, name),
    }
}

fn pattern_mentions_binding(pattern: &Pattern, name: &str) -> bool {
    match pattern {
        Pattern::Literal(expression) => expression_mentions_binding(expression, name),
        Pattern::Range { lo, hi, .. } => {
            expression_mentions_binding(lo, name) || expression_mentions_binding(hi, name)
        }
        Pattern::TupleVariant { elems, .. } => elems
            .iter()
            .any(|pattern| pattern_mentions_binding(pattern, name)),
        Pattern::StructVariant { fields, .. } => fields
            .iter()
            .any(|(_, pattern)| pattern_mentions_binding(pattern, name)),
        Pattern::Wildcard | Pattern::Binding(..) | Pattern::Path { .. } => false,
    }
}

/// Return root function item indices in dependency-first strongly connected
/// groups. A group with more than one member is the only case that needs Lua
/// forward declarations; `local function` handles direct recursion itself.
fn root_function_schedule(program: &crate::typed_ir::TypedProgram) -> Vec<Vec<usize>> {
    let syntax = program.syntax();
    let hir = program.hir();
    let functions = syntax
        .items
        .iter()
        .enumerate()
        .filter_map(|(item_index, item)| match item {
            Item::Fn(function) => Some((
                item_index,
                program.item_definition(hir.root, item_index),
                function,
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    let function_by_definition = functions
        .iter()
        .enumerate()
        .map(|(function_index, (_, definition, _))| (*definition, function_index))
        .collect::<HashMap<_, _>>();

    let graph = functions
        .iter()
        .map(|(_, _, function)| {
            let mut dependencies = HashSet::new();
            collect_block_function_dependencies(
                &function.body,
                hir,
                &function_by_definition,
                &mut dependencies,
            );
            let mut dependencies = dependencies.into_iter().collect::<Vec<_>>();
            dependencies.sort_unstable();
            dependencies
        })
        .collect::<Vec<_>>();

    FunctionComponents::new(&graph)
        .run()
        .into_iter()
        .map(|component| {
            component
                .into_iter()
                .map(|function_index| functions[function_index].0)
                .collect()
        })
        .collect()
}

fn collect_block_function_dependencies(
    block: &Block,
    hir: &crate::hir::ResolvedHir,
    functions: &HashMap<crate::hir::DefId, usize>,
    dependencies: &mut HashSet<usize>,
) {
    for statement in &block.stmts {
        match statement {
            Stmt::Let { init, .. } | Stmt::Expr(init) => {
                collect_expr_function_dependencies(init, hir, functions, dependencies);
            }
            Stmt::Return(Some(value)) => {
                collect_expr_function_dependencies(value, hir, functions, dependencies);
            }
            Stmt::While { cond, body } => {
                collect_expr_function_dependencies(cond, hir, functions, dependencies);
                collect_block_function_dependencies(body, hir, functions, dependencies);
            }
            Stmt::Loop { body } => {
                collect_block_function_dependencies(body, hir, functions, dependencies);
            }
            Stmt::For { iter, body, .. } => {
                collect_expr_function_dependencies(iter, hir, functions, dependencies);
                collect_block_function_dependencies(body, hir, functions, dependencies);
            }
            Stmt::WhileLet {
                pat, expr, body, ..
            } => {
                collect_pattern_function_dependencies(pat, hir, functions, dependencies);
                collect_expr_function_dependencies(expr, hir, functions, dependencies);
                collect_block_function_dependencies(body, hir, functions, dependencies);
            }
            Stmt::Break(value) => {
                if let Some(value) = value {
                    collect_expr_function_dependencies(value, hir, functions, dependencies);
                }
            }
            Stmt::Return(None) | Stmt::Continue => {}
        }
    }
    if let Some(tail) = &block.tail {
        collect_expr_function_dependencies(tail, hir, functions, dependencies);
    }
}

fn collect_expr_function_dependencies(
    expression: &Expr,
    hir: &crate::hir::ResolvedHir,
    functions: &HashMap<crate::hir::DefId, usize>,
    dependencies: &mut HashSet<usize>,
) {
    if let Some(crate::hir::ResolvedTarget::Item(definition)) =
        hir.expression_targets.get(&expression.id)
        && let Some(function) = functions.get(definition)
    {
        dependencies.insert(*function);
    }

    match &expression.kind {
        ExprKind::Closure { body, .. } => match body {
            ClosureBody::Expr(expression) => {
                collect_expr_function_dependencies(expression, hir, functions, dependencies);
            }
            ClosureBody::Block(block) => {
                collect_block_function_dependencies(block, hir, functions, dependencies);
            }
        },
        ExprKind::Unary { expr, .. } | ExprKind::Try { expr } => {
            collect_expr_function_dependencies(expr, hir, functions, dependencies);
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            collect_expr_function_dependencies(lhs, hir, functions, dependencies);
            collect_expr_function_dependencies(rhs, hir, functions, dependencies);
        }
        ExprKind::Call { callee, args } => {
            collect_expr_function_dependencies(callee, hir, functions, dependencies);
            for argument in args {
                collect_expr_function_dependencies(argument, hir, functions, dependencies);
            }
        }
        ExprKind::MethodCall { recv, args, .. } => {
            collect_expr_function_dependencies(recv, hir, functions, dependencies);
            for argument in args {
                collect_expr_function_dependencies(argument, hir, functions, dependencies);
            }
        }
        ExprKind::Field { base, .. } => {
            collect_expr_function_dependencies(base, hir, functions, dependencies);
        }
        ExprKind::StructLit { fields, .. } => {
            for (_, value) in fields {
                collect_expr_function_dependencies(value, hir, functions, dependencies);
            }
        }
        ExprKind::MapLit(entries) => {
            for (key, value) in entries {
                collect_expr_function_dependencies(key, hir, functions, dependencies);
                collect_expr_function_dependencies(value, hir, functions, dependencies);
            }
        }
        ExprKind::Match { scrut, arms } => {
            collect_expr_function_dependencies(scrut, hir, functions, dependencies);
            for arm in arms {
                for pattern in &arm.pats {
                    collect_pattern_function_dependencies(pattern, hir, functions, dependencies);
                }
                if let Some(guard) = &arm.guard {
                    collect_expr_function_dependencies(guard, hir, functions, dependencies);
                }
                collect_expr_function_dependencies(&arm.body, hir, functions, dependencies);
            }
        }
        ExprKind::Range { start, end, .. } => {
            collect_expr_function_dependencies(start, hir, functions, dependencies);
            collect_expr_function_dependencies(end, hir, functions, dependencies);
        }
        ExprKind::Index { base, index } => {
            collect_expr_function_dependencies(base, hir, functions, dependencies);
            collect_expr_function_dependencies(index, hir, functions, dependencies);
        }
        ExprKind::VecLit(elements) => {
            for element in elements {
                collect_expr_function_dependencies(element, hir, functions, dependencies);
            }
        }
        ExprKind::If {
            cond,
            then_block,
            else_block,
        } => {
            collect_expr_function_dependencies(cond, hir, functions, dependencies);
            collect_block_function_dependencies(then_block, hir, functions, dependencies);
            if let Some(branch) = else_block {
                collect_else_function_dependencies(branch, hir, functions, dependencies);
            }
        }
        ExprKind::IfLet {
            pat,
            expr,
            then_block,
            else_block,
        } => {
            collect_pattern_function_dependencies(pat, hir, functions, dependencies);
            collect_expr_function_dependencies(expr, hir, functions, dependencies);
            collect_block_function_dependencies(then_block, hir, functions, dependencies);
            if let Some(branch) = else_block {
                collect_else_function_dependencies(branch, hir, functions, dependencies);
            }
        }
        ExprKind::Block(block) => {
            collect_block_function_dependencies(block, hir, functions, dependencies);
        }
        ExprKind::Loop(block) => {
            collect_block_function_dependencies(block, hir, functions, dependencies);
        }
        ExprKind::Assign { target, value, .. } => {
            collect_expr_function_dependencies(target, hir, functions, dependencies);
            collect_expr_function_dependencies(value, hir, functions, dependencies);
        }
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Bool(_)
        | ExprKind::Path(_) => {}
    }
}

fn collect_else_function_dependencies(
    branch: &ElseBranch,
    hir: &crate::hir::ResolvedHir,
    functions: &HashMap<crate::hir::DefId, usize>,
    dependencies: &mut HashSet<usize>,
) {
    match branch {
        ElseBranch::Block(block) => {
            collect_block_function_dependencies(block, hir, functions, dependencies);
        }
        ElseBranch::If(expression) => {
            collect_expr_function_dependencies(expression, hir, functions, dependencies);
        }
    }
}

fn collect_pattern_function_dependencies(
    pattern: &Pattern,
    hir: &crate::hir::ResolvedHir,
    functions: &HashMap<crate::hir::DefId, usize>,
    dependencies: &mut HashSet<usize>,
) {
    match pattern {
        Pattern::Literal(expression) => {
            collect_expr_function_dependencies(expression, hir, functions, dependencies);
        }
        Pattern::Range { lo, hi, .. } => {
            collect_expr_function_dependencies(lo, hir, functions, dependencies);
            collect_expr_function_dependencies(hi, hir, functions, dependencies);
        }
        Pattern::TupleVariant { elems, .. } => {
            for element in elems {
                collect_pattern_function_dependencies(element, hir, functions, dependencies);
            }
        }
        Pattern::StructVariant { fields, .. } => {
            for (_, field) in fields {
                collect_pattern_function_dependencies(field, hir, functions, dependencies);
            }
        }
        Pattern::Wildcard | Pattern::Binding(..) | Pattern::Path { .. } => {}
    }
}

struct FunctionComponents<'a> {
    graph: &'a [Vec<usize>],
    next_index: usize,
    indices: Vec<Option<usize>>,
    lowlinks: Vec<usize>,
    stack: Vec<usize>,
    on_stack: Vec<bool>,
    components: Vec<Vec<usize>>,
}

impl<'a> FunctionComponents<'a> {
    fn new(graph: &'a [Vec<usize>]) -> Self {
        Self {
            graph,
            next_index: 0,
            indices: vec![None; graph.len()],
            lowlinks: vec![0; graph.len()],
            stack: Vec::new(),
            on_stack: vec![false; graph.len()],
            components: Vec::new(),
        }
    }

    fn run(mut self) -> Vec<Vec<usize>> {
        for function in 0..self.graph.len() {
            if self.indices[function].is_none() {
                self.visit(function);
            }
        }
        self.components
    }

    fn visit(&mut self, function: usize) {
        let index = self.next_index;
        self.next_index += 1;
        self.indices[function] = Some(index);
        self.lowlinks[function] = index;
        self.stack.push(function);
        self.on_stack[function] = true;

        for dependency in self.graph[function].clone() {
            if self.indices[dependency].is_none() {
                self.visit(dependency);
                self.lowlinks[function] = self.lowlinks[function].min(self.lowlinks[dependency]);
            } else if self.on_stack[dependency] {
                self.lowlinks[function] =
                    self.lowlinks[function].min(self.indices[dependency].unwrap());
            }
        }

        if self.lowlinks[function] != index {
            return;
        }
        let mut component = Vec::new();
        loop {
            let member = self.stack.pop().expect("active function is on SCC stack");
            self.on_stack[member] = false;
            component.push(member);
            if member == function {
                break;
            }
        }
        component.sort_unstable();
        self.components.push(component);
    }
}

/// If `impl <trait> for T` defines the operator method `method`, return the Lua
/// metamethod name to alias it to (enabling `a + b`, `a == b`, etc.).
fn op_alias(target: crate::hir::TraitTarget, method: &str) -> Option<&'static str> {
    use rua_core::BuiltinTraitId;
    let crate::hir::TraitTarget::Builtin(trait_id) = target else {
        return None;
    };
    Some(match (trait_id, method) {
        (BuiltinTraitId::Add, "add") => "__add",
        (BuiltinTraitId::Sub, "sub") => "__sub",
        (BuiltinTraitId::Mul, "mul") => "__mul",
        (BuiltinTraitId::Div, "div") => "__div",
        (BuiltinTraitId::Rem, "rem") => "__mod",
        (BuiltinTraitId::Neg, "neg") => "__unm",
        (BuiltinTraitId::PartialEq | BuiltinTraitId::Eq, "eq") => "__eq",
        _ => return None,
    })
}

fn needs_hoist(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::If { .. } | ExprKind::IfLet { .. } | ExprKind::Block(_) | ExprKind::Match { .. }
    )
}

fn binop_lua(op: BinOp) -> LuaBinaryOp {
    match op {
        BinOp::Add => LuaBinaryOp::Add,
        BinOp::Sub => LuaBinaryOp::Sub,
        BinOp::Mul => LuaBinaryOp::Mul,
        BinOp::Div => LuaBinaryOp::Div,
        BinOp::Rem => LuaBinaryOp::Rem,
        BinOp::Eq => LuaBinaryOp::Eq,
        BinOp::Ne => LuaBinaryOp::Ne,
        BinOp::Lt => LuaBinaryOp::Lt,
        BinOp::Le => LuaBinaryOp::Le,
        BinOp::Gt => LuaBinaryOp::Gt,
        BinOp::Ge => LuaBinaryOp::Ge,
        BinOp::And => LuaBinaryOp::And,
        BinOp::Or => LuaBinaryOp::Or,
        BinOp::Coalesce => unreachable!("coalescing is lowered with explicit short-circuiting"),
        BinOp::Contains => unreachable!("membership is lowered through typed container methods"),
    }
}

fn setmetatable(table: LuaExpr, metatable: LuaExpr) -> LuaExpr {
    LuaExpr::name("setmetatable").call(vec![table, metatable])
}

fn and_all(expressions: Vec<LuaExpr>) -> LuaExpr {
    expressions
        .into_iter()
        .reduce(|left, right| left.binary(LuaBinaryOp::And, right))
        .unwrap_or(LuaExpr::Bool(true))
}

fn or_all(expressions: Vec<LuaExpr>) -> LuaExpr {
    expressions
        .into_iter()
        .reduce(|left, right| left.binary(LuaBinaryOp::Or, right))
        .unwrap_or(LuaExpr::Bool(false))
}

fn lua_int_literal(s: &str) -> String {
    let clean = s.replace('_', "");
    if let Some(bits) = clean
        .strip_prefix("0b")
        .or_else(|| clean.strip_prefix("0B"))
        && let Ok(v) = i64::from_str_radix(bits, 2)
    {
        return v.to_string();
    }
    clean
}

fn rua_int_value(source: &str) -> Option<i64> {
    let clean = source.replace('_', "");
    let (digits, radix) = if let Some(digits) = clean
        .strip_prefix("0b")
        .or_else(|| clean.strip_prefix("0B"))
    {
        (digits, 2)
    } else if let Some(digits) = clean
        .strip_prefix("0o")
        .or_else(|| clean.strip_prefix("0O"))
    {
        (digits, 8)
    } else if let Some(digits) = clean
        .strip_prefix("0x")
        .or_else(|| clean.strip_prefix("0X"))
    {
        (digits, 16)
    } else {
        (clean.as_str(), 10)
    };
    i64::from_str_radix(digits, radix).ok()
}
