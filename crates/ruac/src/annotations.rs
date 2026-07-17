//! Resolved user annotation schemas and instances.

use std::collections::{BTreeMap, BTreeSet};

use rua_core::{Attribute, DiagnosticCode, MetaItem, MetaValue};
use serde::Serialize;

use crate::ast::{Field, Item, Program, Type, VariantKind};
use crate::diag::Diag;
use crate::hir::{DefId, DefKind, ModuleId, Namespace, ResolvedHir, ResolvedTarget, TypeTarget};
use crate::token::SourceRange;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AnnotationTargetKind {
    Struct,
    Enum,
    Function,
    Method,
    Field,
    Variant,
    ExternFunction,
}

impl AnnotationTargetKind {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "struct" => Self::Struct,
            "enum" => Self::Enum,
            "function" => Self::Function,
            "method" => Self::Method,
            "field" => Self::Field,
            "variant" => Self::Variant,
            "extern_function" => Self::ExternFunction,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AnnotationRetention {
    Source,
    #[default]
    Build,
    Runtime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotationParameter {
    pub name: String,
    pub ty: AnnotationParameterType,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnnotationParameterType {
    String,
    Bool,
    Integer,
    Float,
    Enum(DefId),
    List(Box<AnnotationParameterType>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnnotationValue {
    String(String),
    Bool(bool),
    Integer(i64),
    Float(String),
    EnumVariant(DefId),
    List(Vec<AnnotationValue>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotationDefinition {
    pub id: DefId,
    pub qualified_name: String,
    pub parameters: Vec<AnnotationParameter>,
    pub targets: BTreeSet<AnnotationTargetKind>,
    pub retention: AnnotationRetention,
    pub repeatable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AnnotationTarget {
    Definition(DefId),
    Field { owner: DefId, index: u32 },
    VariantField { variant: DefId, index: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotationInstance {
    pub annotation: DefId,
    pub target: AnnotationTarget,
    pub arguments: Vec<(String, AnnotationValue)>,
    pub source_order: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnnotationIndex {
    definitions: BTreeMap<DefId, AnnotationDefinition>,
    instances: Vec<AnnotationInstance>,
    by_annotation: BTreeMap<DefId, Vec<usize>>,
    by_target: BTreeMap<AnnotationTarget, Vec<usize>>,
    target_names: BTreeMap<AnnotationTarget, String>,
}

impl AnnotationIndex {
    pub fn definitions(&self) -> impl ExactSizeIterator<Item = &AnnotationDefinition> {
        self.definitions.values()
    }

    pub fn instances(&self) -> &[AnnotationInstance] {
        &self.instances
    }

    pub fn definition(&self, id: DefId) -> Option<&AnnotationDefinition> {
        self.definitions.get(&id)
    }

    pub fn by_annotation(&self, id: DefId) -> impl Iterator<Item = &AnnotationInstance> {
        self.by_annotation
            .get(&id)
            .into_iter()
            .flatten()
            .map(|index| &self.instances[*index])
    }

    pub fn for_target(
        &self,
        target: AnnotationTarget,
    ) -> impl Iterator<Item = &AnnotationInstance> {
        self.by_target
            .get(&target)
            .into_iter()
            .flatten()
            .map(|index| &self.instances[*index])
    }

    pub fn target_name(&self, target: AnnotationTarget) -> Option<&str> {
        self.target_names.get(&target).map(String::as_str)
    }

    fn push(&mut self, instance: AnnotationInstance) {
        let index = self.instances.len();
        self.by_annotation
            .entry(instance.annotation)
            .or_default()
            .push(index);
        self.by_target
            .entry(instance.target)
            .or_default()
            .push(index);
        self.instances.push(instance);
    }
}

pub fn build(program: &Program, hir: &ResolvedHir) -> Result<AnnotationIndex, Vec<Diag>> {
    let mut builder = Builder {
        hir,
        index: AnnotationIndex::default(),
        diagnostics: Vec::new(),
        source_order: 0,
    };
    builder.collect_definitions(hir.root, &program.items);
    builder.collect_instances(hir.root, &program.items);
    if builder.diagnostics.is_empty() {
        Ok(builder.index)
    } else {
        Err(builder.diagnostics)
    }
}

struct Builder<'a> {
    hir: &'a ResolvedHir,
    index: AnnotationIndex,
    diagnostics: Vec<Diag>,
    source_order: u32,
}

impl Builder<'_> {
    fn collect_definitions(&mut self, module: ModuleId, items: &[Item]) {
        for (item_index, item) in items.iter().enumerate() {
            match item {
                Item::Annotation(annotation) => {
                    let Some(id) = self.item_definition(module, item_index) else {
                        continue;
                    };
                    let Some((targets, retention, repeatable)) =
                        self.parse_schema(&annotation.attributes, annotation.name_span)
                    else {
                        continue;
                    };
                    let mut parameter_names = BTreeSet::new();
                    let mut valid = true;
                    for parameter in &annotation.params {
                        if !parameter_names.insert(parameter.name.clone()) {
                            self.error(
                                DiagnosticCode::AnnotationInvalidSchema,
                                parameter.name_span,
                                format!("duplicate annotation parameter `{}`", parameter.name),
                            );
                            valid = false;
                        }
                        if self.annotation_parameter_type(&parameter.ty).is_none() {
                            self.error(
                                DiagnosticCode::AnnotationInvalidSchema,
                                parameter.name_span,
                                format!(
                                    "unsupported type for annotation parameter `{}`",
                                    parameter.name
                                ),
                            );
                            valid = false;
                        }
                    }
                    if valid {
                        self.index.definitions.insert(
                            id,
                            AnnotationDefinition {
                                id,
                                qualified_name: self.qualified_name(id),
                                parameters: annotation
                                    .params
                                    .iter()
                                    .map(|parameter| AnnotationParameter {
                                        name: parameter.name.clone(),
                                        ty: self
                                            .annotation_parameter_type(&parameter.ty)
                                            .expect("validated annotation parameter type"),
                                    })
                                    .collect(),
                                targets,
                                retention,
                                repeatable,
                            },
                        );
                    }
                }
                Item::Mod(child) => {
                    if let Some(child_module) = self.child_module(module, item_index) {
                        self.collect_definitions(child_module, &child.items);
                    }
                }
                _ => {}
            }
        }
    }

    fn parse_schema(
        &mut self,
        attributes: &[Attribute],
        span: SourceRange,
    ) -> Option<(BTreeSet<AnnotationTargetKind>, AnnotationRetention, bool)> {
        let mut targets = None;
        let mut retention = AnnotationRetention::Build;
        let mut repeatable = false;
        let mut valid = true;
        for attribute in attributes {
            match attribute.name.as_str() {
                "targets" => {
                    if targets.is_some() {
                        self.error(
                            DiagnosticCode::AnnotationInvalidSchema,
                            span,
                            "duplicate `targets` schema attribute".to_string(),
                        );
                        valid = false;
                        continue;
                    }
                    let mut parsed = BTreeSet::new();
                    for item in &attribute.items {
                        let MetaItem::Word(name) = item else {
                            self.error(
                                DiagnosticCode::AnnotationInvalidSchema,
                                span,
                                "`targets` only accepts target names".to_string(),
                            );
                            valid = false;
                            continue;
                        };
                        let Some(target) = AnnotationTargetKind::parse(name) else {
                            self.error(
                                DiagnosticCode::AnnotationInvalidSchema,
                                span,
                                format!("unknown annotation target `{name}`"),
                            );
                            valid = false;
                            continue;
                        };
                        parsed.insert(target);
                    }
                    targets = Some(parsed);
                }
                "retention" => {
                    let [MetaItem::Word(policy)] = attribute.items.as_slice() else {
                        self.error(
                            DiagnosticCode::AnnotationInvalidSchema,
                            span,
                            "`retention` requires one of source, build, or runtime".to_string(),
                        );
                        valid = false;
                        continue;
                    };
                    retention = match policy.as_str() {
                        "source" => AnnotationRetention::Source,
                        "build" => AnnotationRetention::Build,
                        "runtime" => AnnotationRetention::Runtime,
                        _ => {
                            self.error(
                                DiagnosticCode::AnnotationInvalidSchema,
                                span,
                                format!("unknown annotation retention `{policy}`"),
                            );
                            valid = false;
                            continue;
                        }
                    };
                }
                "repeatable" if attribute.items.is_empty() => repeatable = true,
                name => {
                    self.error(
                        DiagnosticCode::AnnotationInvalidSchema,
                        span,
                        format!("unsupported annotation schema attribute `{name}`"),
                    );
                    valid = false;
                }
            }
        }
        let Some(targets) = targets else {
            self.error(
                DiagnosticCode::AnnotationInvalidSchema,
                span,
                "annotation declaration requires `#[targets(...)]`".to_string(),
            );
            return None;
        };
        valid.then_some((targets, retention, repeatable))
    }

    fn collect_instances(&mut self, module: ModuleId, items: &[Item]) {
        for (item_index, item) in items.iter().enumerate() {
            match item {
                Item::Annotation(_) => {}
                Item::Fn(function) => {
                    if let Some(definition) = self.item_definition(module, item_index) {
                        self.attributes(
                            module,
                            &function.attributes,
                            AnnotationTargetKind::Function,
                            AnnotationTarget::Definition(definition),
                            function.name_span,
                            function.is_pub,
                        );
                    }
                }
                Item::Struct(structure) => {
                    if let Some(owner) = self.item_definition(module, item_index) {
                        self.attributes(
                            module,
                            &structure.attributes,
                            AnnotationTargetKind::Struct,
                            AnnotationTarget::Definition(owner),
                            first_field_span(&structure.fields),
                            structure.is_pub,
                        );
                        for (index, field) in structure.fields.iter().enumerate() {
                            let target = AnnotationTarget::Field {
                                owner,
                                index: index as u32,
                            };
                            self.index.target_names.insert(
                                target,
                                format!("{}::{}", self.qualified_name(owner), field.name),
                            );
                            self.attributes(
                                module,
                                &field.attributes,
                                AnnotationTargetKind::Field,
                                target,
                                field.name_span,
                                structure.is_pub,
                            );
                        }
                    }
                }
                Item::Enum(enumeration) => {
                    if let Some(owner) = self.item_definition(module, item_index) {
                        self.attributes(
                            module,
                            &enumeration.attributes,
                            AnnotationTargetKind::Enum,
                            AnnotationTarget::Definition(owner),
                            fallback_span(),
                            enumeration.is_pub,
                        );
                        for (variant_index, variant) in enumeration.variants.iter().enumerate() {
                            let Some(variant_id) = self
                                .hir
                                .enum_variant_targets
                                .get(&(owner, variant_index))
                                .copied()
                            else {
                                continue;
                            };
                            self.attributes(
                                module,
                                &variant.attributes,
                                AnnotationTargetKind::Variant,
                                AnnotationTarget::Definition(variant_id),
                                fallback_span(),
                                enumeration.is_pub,
                            );
                            if let VariantKind::Struct(fields) = &variant.kind {
                                for (field_index, field) in fields.iter().enumerate() {
                                    let target = AnnotationTarget::VariantField {
                                        variant: variant_id,
                                        index: field_index as u32,
                                    };
                                    self.index.target_names.insert(
                                        target,
                                        format!(
                                            "{}::{}",
                                            self.qualified_name(variant_id),
                                            field.name
                                        ),
                                    );
                                    self.attributes(
                                        module,
                                        &field.attributes,
                                        AnnotationTargetKind::Field,
                                        target,
                                        field.name_span,
                                        enumeration.is_pub,
                                    );
                                }
                            }
                        }
                    }
                }
                Item::Impl(implementation) => {
                    for (method_index, method) in implementation.methods.iter().enumerate() {
                        let Some(definition) = self
                            .hir
                            .impl_method_targets
                            .get(&(module, item_index, method_index))
                            .copied()
                        else {
                            continue;
                        };
                        self.attributes(
                            module,
                            &method.attributes,
                            AnnotationTargetKind::Method,
                            AnnotationTarget::Definition(definition),
                            method.name_span,
                            method.is_pub,
                        );
                    }
                    self.reject_block_attributes(&implementation.attributes, "impl");
                }
                Item::Trait(trait_decl) => {
                    if let Some(owner) = self.item_definition(module, item_index) {
                        for (method_index, method) in trait_decl.methods.iter().enumerate() {
                            let Some(definition) = self
                                .hir
                                .trait_method_targets
                                .get(&(owner, method_index))
                                .copied()
                            else {
                                continue;
                            };
                            self.attributes(
                                module,
                                &method.attributes,
                                AnnotationTargetKind::Method,
                                AnnotationTarget::Definition(definition),
                                method.name_span,
                                trait_decl.is_pub,
                            );
                        }
                    }
                    self.reject_block_attributes(&trait_decl.attributes, "trait");
                }
                Item::Extern(block) => {
                    for (function_index, function) in block.fns.iter().enumerate() {
                        let Some(definition) = self
                            .hir
                            .extern_function_targets
                            .get(&(module, item_index, function_index))
                            .copied()
                        else {
                            continue;
                        };
                        self.attributes(
                            module,
                            &function.attributes,
                            AnnotationTargetKind::ExternFunction,
                            AnnotationTarget::Definition(definition),
                            function.name_span,
                            true,
                        );
                    }
                    self.reject_block_attributes(&block.attributes, "extern block");
                }
                Item::Mod(child) => {
                    self.reject_block_attributes(&child.attributes, "module");
                    if let Some(child_module) = self.child_module(module, item_index) {
                        self.collect_instances(child_module, &child.items);
                    }
                }
                Item::Use(import) => self.reject_block_attributes(&import.attributes, "use"),
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn attributes(
        &mut self,
        module: ModuleId,
        attributes: &[Attribute],
        kind: AnnotationTargetKind,
        target: AnnotationTarget,
        span: SourceRange,
        is_public: bool,
    ) {
        if !self.index.target_names.contains_key(&target) {
            let name = match target {
                AnnotationTarget::Definition(definition) => self.qualified_name(definition),
                AnnotationTarget::Field { owner, index } => {
                    format!("{}::field[{index}]", self.qualified_name(owner))
                }
                AnnotationTarget::VariantField { variant, index } => {
                    format!("{}::field[{index}]", self.qualified_name(variant))
                }
            };
            self.index.target_names.insert(target, name);
        }
        let mut seen = BTreeSet::new();
        for attribute in attributes {
            let Some(annotation) = self.resolve_annotation(module, &attribute.name) else {
                self.error(
                    DiagnosticCode::AnnotationUnresolved,
                    span,
                    format!("unresolved annotation `{}`", attribute.name),
                );
                continue;
            };
            let Some(definition) = self.index.definitions.get(&annotation).cloned() else {
                self.error(
                    DiagnosticCode::AnnotationUnresolved,
                    span,
                    format!("`{}` does not name an annotation", attribute.name),
                );
                continue;
            };
            if !definition.targets.contains(&kind) {
                self.error(
                    DiagnosticCode::AnnotationInvalidTarget,
                    span,
                    format!(
                        "annotation `{}` cannot target {kind:?}",
                        definition.qualified_name
                    ),
                );
                continue;
            }
            if definition.retention == AnnotationRetention::Runtime && !is_public {
                self.error(
                    DiagnosticCode::AnnotationRuntimePrivate,
                    span,
                    format!(
                        "runtime annotation `{}` requires a public target",
                        definition.qualified_name
                    ),
                );
                continue;
            }
            if definition.retention == AnnotationRetention::Runtime
                && !self.runtime_target_supported(target)
            {
                self.error(
                    DiagnosticCode::AnnotationInvalidTarget,
                    span,
                    format!(
                        "runtime annotation `{}` has no stable Lua target locator",
                        definition.qualified_name
                    ),
                );
                continue;
            }
            if !definition.repeatable && !seen.insert(annotation) {
                self.error(
                    DiagnosticCode::AnnotationDuplicate,
                    span,
                    format!(
                        "annotation `{}` is not repeatable",
                        definition.qualified_name
                    ),
                );
                continue;
            }
            let Some(arguments) = self.validate_arguments(module, &definition, attribute, span)
            else {
                continue;
            };
            if definition.retention != AnnotationRetention::Source {
                let source_order = self.source_order;
                self.source_order += 1;
                self.index.push(AnnotationInstance {
                    annotation,
                    target,
                    arguments,
                    source_order,
                });
            }
        }
    }

    fn validate_arguments(
        &mut self,
        module: ModuleId,
        definition: &AnnotationDefinition,
        attribute: &Attribute,
        span: SourceRange,
    ) -> Option<Vec<(String, AnnotationValue)>> {
        let mut provided = BTreeMap::new();
        let mut valid = true;
        for item in &attribute.items {
            let MetaItem::NameValue { name, value } = item else {
                self.error(
                    DiagnosticCode::AnnotationInvalidArguments,
                    span,
                    format!(
                        "annotation `{}` requires named arguments",
                        definition.qualified_name
                    ),
                );
                valid = false;
                continue;
            };
            if provided.insert(name.clone(), value.clone()).is_some() {
                self.error(
                    DiagnosticCode::AnnotationInvalidArguments,
                    span,
                    format!("duplicate annotation argument `{name}`"),
                );
                valid = false;
            }
        }
        let mut arguments = Vec::new();
        for parameter in &definition.parameters {
            let Some(value) = provided.remove(&parameter.name) else {
                self.error(
                    DiagnosticCode::AnnotationInvalidArguments,
                    span,
                    format!("missing annotation argument `{}`", parameter.name),
                );
                valid = false;
                continue;
            };
            let Some(value) = self.resolve_value(module, &value, &parameter.ty) else {
                self.error(
                    DiagnosticCode::AnnotationInvalidArguments,
                    span,
                    format!(
                        "annotation argument `{}` has the wrong type",
                        parameter.name
                    ),
                );
                valid = false;
                continue;
            };
            arguments.push((parameter.name.clone(), value));
        }
        for name in provided.keys() {
            self.error(
                DiagnosticCode::AnnotationInvalidArguments,
                span,
                format!("unknown annotation argument `{name}`"),
            );
            valid = false;
        }
        valid.then_some(arguments)
    }

    fn resolve_annotation(&self, module: ModuleId, name: &str) -> Option<DefId> {
        let segments = name.split("::").map(str::to_string).collect::<Vec<_>>();
        let target = if segments.len() == 1 {
            self.hir
                .imports
                .iter()
                .rev()
                .find(|import| import.module == module && import.name == segments[0])
                .map(|import| import.target)
                .or_else(|| self.hir.resolve_path(module, Namespace::Type, &segments))
        } else {
            self.hir.resolve_path(module, Namespace::Type, &segments)
        }?;
        let ResolvedTarget::Item(definition) = target else {
            return None;
        };
        matches!(self.hir.definition(definition).kind, DefKind::Annotation).then_some(definition)
    }

    fn annotation_parameter_type(&self, ty: &Type) -> Option<AnnotationParameterType> {
        let Type::Path { id, name, args } = ty else {
            return None;
        };
        if matches!(name.as_str(), "Vec" | "List") && args.len() == 1 {
            return self
                .annotation_parameter_type(&args[0])
                .map(Box::new)
                .map(AnnotationParameterType::List);
        }
        if !args.is_empty() {
            return None;
        }
        Some(match name.as_str() {
            "String" => AnnotationParameterType::String,
            "bool" => AnnotationParameterType::Bool,
            "i64" => AnnotationParameterType::Integer,
            "f64" => AnnotationParameterType::Float,
            _ => {
                let TypeTarget::Item(definition) = self.hir.type_targets.get(id).copied()? else {
                    return None;
                };
                if !matches!(self.hir.definition(definition).kind, DefKind::Enum) {
                    return None;
                }
                AnnotationParameterType::Enum(definition)
            }
        })
    }

    fn resolve_value(
        &self,
        module: ModuleId,
        value: &MetaValue,
        ty: &AnnotationParameterType,
    ) -> Option<AnnotationValue> {
        match (ty, value) {
            (AnnotationParameterType::String, MetaValue::String(value)) => {
                Some(AnnotationValue::String(value.clone()))
            }
            (AnnotationParameterType::Bool, MetaValue::Bool(value)) => {
                Some(AnnotationValue::Bool(*value))
            }
            (AnnotationParameterType::Integer, MetaValue::Integer(value)) => {
                Some(AnnotationValue::Integer(*value))
            }
            (AnnotationParameterType::Float, MetaValue::Float(value)) => {
                Some(AnnotationValue::Float(value.clone()))
            }
            (AnnotationParameterType::Enum(owner), MetaValue::Path(path)) => {
                let segments = path.split("::").map(str::to_string).collect::<Vec<_>>();
                let target = if segments.len() == 1 {
                    self.hir
                        .imports
                        .iter()
                        .rev()
                        .find(|import| import.module == module && import.name == segments[0])
                        .map(|import| import.target)
                        .or_else(|| self.hir.resolve_path(module, Namespace::Value, &segments))
                } else {
                    self.hir.resolve_path(module, Namespace::Value, &segments)
                };
                let Some(ResolvedTarget::Item(definition)) = target else {
                    return None;
                };
                matches!(
                    self.hir.definition(definition).kind,
                    DefKind::EnumVariant { owner: variant_owner, .. } if variant_owner == *owner
                )
                .then_some(AnnotationValue::EnumVariant(definition))
            }
            (AnnotationParameterType::List(element), MetaValue::List(values)) => values
                .iter()
                .map(|value| self.resolve_value(module, value, element))
                .collect::<Option<Vec<_>>>()
                .map(AnnotationValue::List),
            _ => None,
        }
    }

    fn reject_block_attributes(&mut self, attributes: &[Attribute], target: &str) {
        for attribute in attributes {
            self.diagnostics.push(Diag::bare(
                DiagnosticCode::AnnotationInvalidTarget,
                format!("annotation `{}` cannot target {target}", attribute.name),
            ));
        }
    }

    fn runtime_target_supported(&self, target: AnnotationTarget) -> bool {
        match target {
            AnnotationTarget::Field { owner, .. } => {
                matches!(self.hir.definition(owner).kind, DefKind::Struct)
            }
            AnnotationTarget::VariantField { variant, .. } => matches!(
                self.hir.definition(variant).kind,
                DefKind::EnumVariant { .. }
            ),
            AnnotationTarget::Definition(definition) => {
                match self.hir.definition(definition).kind {
                    DefKind::Function
                    | DefKind::Struct
                    | DefKind::Enum
                    | DefKind::EnumVariant { .. } => true,
                    DefKind::Method { owner } => matches!(
                        self.hir.definition(owner).kind,
                        DefKind::Struct | DefKind::Enum
                    ),
                    DefKind::Annotation
                    | DefKind::Trait
                    | DefKind::TraitMethod { .. }
                    | DefKind::ExternFunction { .. } => false,
                }
            }
        }
    }

    fn item_definition(&self, module: ModuleId, item_index: usize) -> Option<DefId> {
        let ResolvedTarget::Item(definition) =
            self.hir.item_targets.get(&(module, item_index)).copied()?
        else {
            return None;
        };
        Some(definition)
    }

    fn child_module(&self, module: ModuleId, item_index: usize) -> Option<ModuleId> {
        let ResolvedTarget::Module(child) =
            self.hir.item_targets.get(&(module, item_index)).copied()?
        else {
            return None;
        };
        Some(child)
    }

    fn qualified_name(&self, definition: DefId) -> String {
        let definition = self.hir.definition(definition);
        match definition.kind {
            DefKind::EnumVariant { owner, .. }
            | DefKind::TraitMethod { owner }
            | DefKind::Method { owner } => {
                return format!("{}::{}", self.qualified_name(owner), definition.name);
            }
            _ => {}
        }
        let mut segments = self.hir.module(definition.module).path.segments().to_vec();
        segments.push(definition.name.clone());
        segments.join("::")
    }

    fn error(&mut self, code: DiagnosticCode, span: SourceRange, message: String) {
        self.diagnostics.push(Diag::new(
            code, span.file, span.start, span.len, span.line, message,
        ));
    }
}

fn first_field_span(fields: &[Field]) -> SourceRange {
    fields
        .first()
        .map_or_else(fallback_span, |field| field.name_span)
}

fn fallback_span() -> SourceRange {
    SourceRange::new(0, 0, 1)
}

#[derive(Serialize)]
struct MetadataDocument {
    version: u32,
    definitions: Vec<MetadataDefinition>,
    instances: Vec<MetadataInstance>,
}

#[derive(Serialize)]
struct MetadataDefinition {
    name: String,
    retention: &'static str,
    repeatable: bool,
    targets: Vec<&'static str>,
    parameters: Vec<MetadataParameter>,
}

#[derive(Serialize)]
struct MetadataParameter {
    name: String,
    #[serde(rename = "type")]
    ty: String,
}

#[derive(Serialize)]
struct MetadataInstance {
    annotation: String,
    target: String,
    source_order: u32,
    arguments: BTreeMap<String, toml::Value>,
}

pub(crate) fn render_toml(
    index: &AnnotationIndex,
    hir: &ResolvedHir,
    filter: Option<&str>,
) -> Result<String, String> {
    let filter = filter.map(|filter| filter.replace('.', "::"));
    let definitions = index
        .definitions()
        .filter(|definition| {
            filter
                .as_ref()
                .is_none_or(|filter| definition.qualified_name == *filter)
        })
        .map(|definition| MetadataDefinition {
            name: definition.qualified_name.clone(),
            retention: retention_name(definition.retention),
            repeatable: definition.repeatable,
            targets: definition
                .targets
                .iter()
                .copied()
                .map(target_kind_name)
                .collect(),
            parameters: definition
                .parameters
                .iter()
                .map(|parameter| MetadataParameter {
                    name: parameter.name.clone(),
                    ty: parameter_type_name(&parameter.ty, hir),
                })
                .collect(),
        })
        .collect();
    let instances = index
        .instances()
        .iter()
        .filter_map(|instance| {
            let definition = index.definition(instance.annotation)?;
            if filter
                .as_ref()
                .is_some_and(|filter| definition.qualified_name != *filter)
            {
                return None;
            }
            Some(MetadataInstance {
                annotation: definition.qualified_name.clone(),
                target: index
                    .target_name(instance.target)
                    .unwrap_or("<unknown-target>")
                    .to_string(),
                source_order: instance.source_order,
                arguments: instance
                    .arguments
                    .iter()
                    .map(|(name, value)| (name.clone(), metadata_value(value, hir)))
                    .collect(),
            })
        })
        .collect();
    toml::to_string_pretty(&MetadataDocument {
        version: 1,
        definitions,
        instances,
    })
    .map_err(|error| format!("serializing annotation metadata: {error}"))
}

fn retention_name(retention: AnnotationRetention) -> &'static str {
    match retention {
        AnnotationRetention::Source => "source",
        AnnotationRetention::Build => "build",
        AnnotationRetention::Runtime => "runtime",
    }
}

fn target_kind_name(target: AnnotationTargetKind) -> &'static str {
    match target {
        AnnotationTargetKind::Struct => "struct",
        AnnotationTargetKind::Enum => "enum",
        AnnotationTargetKind::Function => "function",
        AnnotationTargetKind::Method => "method",
        AnnotationTargetKind::Field => "field",
        AnnotationTargetKind::Variant => "variant",
        AnnotationTargetKind::ExternFunction => "extern_function",
    }
}

fn parameter_type_name(ty: &AnnotationParameterType, hir: &ResolvedHir) -> String {
    match ty {
        AnnotationParameterType::String => "String".to_string(),
        AnnotationParameterType::Bool => "bool".to_string(),
        AnnotationParameterType::Integer => "i64".to_string(),
        AnnotationParameterType::Float => "f64".to_string(),
        AnnotationParameterType::Enum(definition) => qualified_name(hir, *definition),
        AnnotationParameterType::List(element) => {
            format!("Vec<{}>", parameter_type_name(element, hir))
        }
    }
}

fn metadata_value(value: &AnnotationValue, hir: &ResolvedHir) -> toml::Value {
    match value {
        AnnotationValue::String(value) => toml::Value::String(value.clone()),
        AnnotationValue::Bool(value) => toml::Value::Boolean(*value),
        AnnotationValue::Integer(value) => toml::Value::Integer(*value),
        AnnotationValue::Float(value) => toml::Value::Float(
            value
                .parse()
                .expect("validated annotation float has an f64 representation"),
        ),
        AnnotationValue::EnumVariant(definition) => {
            toml::Value::String(qualified_name(hir, *definition))
        }
        AnnotationValue::List(values) => toml::Value::Array(
            values
                .iter()
                .map(|value| metadata_value(value, hir))
                .collect(),
        ),
    }
}

fn qualified_name(hir: &ResolvedHir, definition: DefId) -> String {
    let data = hir.definition(definition);
    match data.kind {
        DefKind::EnumVariant { owner, .. }
        | DefKind::TraitMethod { owner }
        | DefKind::Method { owner } => {
            format!("{}::{}", qualified_name(hir, owner), data.name)
        }
        _ => {
            let mut segments = hir.module(data.module).path.segments().to_vec();
            segments.push(data.name.clone());
            segments.join("::")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_identity_index_and_orders_arguments_by_schema() {
        let program = crate::parser::parse(
            r#"
            #[targets(function)]
            pub annotation Route(method: String, path: String);

            #[Route(path = "/users", method = "GET")]
            pub fn users() {}
            "#,
        )
        .unwrap();
        let hir = crate::hir::resolve(&program);
        let index = build(&program, &hir).unwrap();
        assert_eq!(index.definitions().len(), 1);
        assert_eq!(index.instances().len(), 1);
        assert_eq!(index.instances()[0].arguments[0].0, "method");
        assert_eq!(index.instances()[0].arguments[1].0, "path");
    }

    #[test]
    fn rejects_duplicate_non_repeatable_annotation() {
        let program = crate::parser::parse(
            r#"
            #[targets(function)]
            annotation Tag();
            #[Tag]
            #[Tag]
            fn tagged() {}
            "#,
        )
        .unwrap();
        let hir = crate::hir::resolve(&program);
        let diagnostics = build(&program, &hir).unwrap_err();
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::AnnotationDuplicate)
        );
    }

    #[test]
    fn validates_float_enum_and_list_arguments() {
        let program = crate::parser::parse(
            r#"
            enum Format { Json, MessagePack }
            #[targets(struct)]
            annotation Codec(weight: f64, formats: Vec<Format>);
            #[Codec(weight = 1.5, formats = [Format::Json, Format::MessagePack])]
            struct Payload {}
            "#,
        )
        .unwrap();
        let hir = crate::hir::resolve(&program);
        let index = build(&program, &hir).unwrap();
        assert_eq!(index.instances().len(), 1);
        assert!(matches!(
            &index.instances()[0].arguments[1].1,
            AnnotationValue::List(values) if values.len() == 2
        ));
    }

    #[test]
    fn annotation_declarations_do_not_emit_lua_definitions() {
        let lua = crate::compile_str(
            r#"
            #[targets(function)]
            annotation Trace(name: String);
            #[Trace(name = "users")]
            fn users() -> String { "ok" }
            users();
            "#,
        )
        .unwrap();
        assert!(lua.contains("local function users"));
        assert!(!lua.contains("Trace"));
        assert!(!lua.contains("annotation"));
    }
}
