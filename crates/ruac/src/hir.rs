//! Compiler-owned semantic identities and resolved path side tables.

use std::collections::{BTreeMap, BTreeSet};

use rua_core::{
    BuiltinId, BuiltinTraitId, DiagnosticCode, FileId, ModulePath, StructuredDiagnostic, TextRange,
    builtin_macro, builtin_trait, builtin_type, builtin_value,
};

use crate::ast::*;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u32);

        impl $name {
            pub const fn new(raw: u32) -> Self {
                Self(raw)
            }

            pub const fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

id_type!(ModuleId);
id_type!(DefId);
id_type!(LocalId);
id_type!(ExternId);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Namespace {
    Value,
    Type,
    Module,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VariantShape {
    Unit,
    Tuple,
    Struct,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefKind {
    Function,
    Struct,
    Enum,
    EnumVariant { owner: DefId, shape: VariantShape },
    Trait,
    TraitMethod { owner: DefId },
    Method { owner: DefId },
    ExternFunction { extern_id: ExternId },
}

#[derive(Clone, Debug)]
pub struct DefData {
    pub id: DefId,
    pub module: ModuleId,
    pub name: String,
    pub kind: DefKind,
    pub is_public: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ModuleScope {
    pub values: BTreeMap<String, DefId>,
    pub types: BTreeMap<String, DefId>,
    pub modules: BTreeMap<String, ModuleId>,
}

#[derive(Clone, Debug)]
pub struct ModuleData {
    pub id: ModuleId,
    pub parent: Option<ModuleId>,
    pub path: ModulePath,
    pub file: FileId,
    pub is_file: bool,
    pub is_declaration: bool,
    pub is_public: bool,
    pub scope: ModuleScope,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolvedTarget {
    Local(LocalId),
    Item(DefId),
    Module(ModuleId),
    Builtin(BuiltinId),
    Extern(ExternId),
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TraitTarget {
    Item(DefId),
    Builtin(BuiltinTraitId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrimitiveType {
    I64,
    F64,
    Bool,
    Box,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeTarget {
    Primitive(PrimitiveType),
    Builtin(BuiltinId),
    Item(DefId),
    Generic(GenericParamId),
    SelfType,
    Infer,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImplTarget {
    pub owner: DefId,
    pub trait_target: Option<TraitTarget>,
}

#[derive(Clone, Debug)]
pub struct LocalData {
    pub id: LocalId,
    pub name: String,
    pub module: ModuleId,
    pub is_mutable: bool,
}

#[derive(Clone, Debug)]
pub struct ResolvedImport {
    pub module: ModuleId,
    pub name: String,
    pub target: ResolvedTarget,
}

#[derive(Clone, Debug)]
pub struct ResolvedHir {
    pub root: ModuleId,
    pub modules: Vec<ModuleData>,
    pub definitions: Vec<DefData>,
    pub locals: Vec<LocalData>,
    /// Declaration identity for each source item, keyed by its owning module and
    /// source-order item index. `Use` and `Impl` entries have no direct target.
    pub item_targets: BTreeMap<(ModuleId, usize), ResolvedTarget>,
    pub enum_variant_targets: BTreeMap<(DefId, usize), DefId>,
    pub trait_method_targets: BTreeMap<(DefId, usize), DefId>,
    pub impl_method_targets: BTreeMap<(ModuleId, usize, usize), DefId>,
    pub extern_function_targets: BTreeMap<(ModuleId, usize, usize), DefId>,
    pub expression_targets: BTreeMap<ExprId, ResolvedTarget>,
    pub pattern_targets: BTreeMap<PatternId, ResolvedTarget>,
    pub type_targets: BTreeMap<TypeId, TypeTarget>,
    pub trait_ref_targets: BTreeMap<TraitRefId, TraitTarget>,
    pub enum_variants: BTreeMap<(DefId, String), DefId>,
    pub associated_items: BTreeMap<(DefId, String), DefId>,
    pub trait_items: BTreeMap<(DefId, String), DefId>,
    /// Concrete type -> user traits implemented by that type.
    pub type_traits: BTreeMap<DefId, BTreeSet<TraitTarget>>,
    /// Synthetic type-owned inherited method -> source trait method.
    pub method_origins: BTreeMap<DefId, DefId>,
    /// Explicit impl method definition -> implemented user trait method.
    pub trait_method_implementations: BTreeMap<DefId, DefId>,
    pub trait_default_methods: BTreeSet<DefId>,
    pub method_traits: BTreeMap<DefId, TraitTarget>,
    pub impl_targets: BTreeMap<(ModuleId, usize), ImplTarget>,
    pub imports: Vec<ResolvedImport>,
    pub diagnostics: Vec<StructuredDiagnostic>,
}

impl ResolvedHir {
    pub fn definition(&self, id: DefId) -> &DefData {
        &self.definitions[id.index()]
    }

    pub fn module(&self, id: ModuleId) -> &ModuleData {
        &self.modules[id.index()]
    }

    pub fn type_is_builtin(&self, ty: &Type, builtin: BuiltinId) -> bool {
        matches!(
            ty,
            Type::Path { id, .. }
                if self.type_targets.get(id) == Some(&TypeTarget::Builtin(builtin))
        )
    }

    pub fn resolve_path(
        &self,
        from: ModuleId,
        namespace: Namespace,
        segments: &[String],
    ) -> Option<ResolvedTarget> {
        enum Cursor {
            Module(ModuleId),
            Item(DefId),
        }

        let (head, rest) = segments.split_first()?;
        if rest.is_empty() {
            return self.resolve_in_module(from, namespace, head);
        }

        let mut cursor = self.modules[from.index()]
            .scope
            .modules
            .get(head)
            .or_else(|| self.modules[self.root.index()].scope.modules.get(head))
            .copied()
            .map(Cursor::Module)
            .or_else(|| {
                let ResolvedTarget::Item(item) =
                    self.resolve_in_module(from, Namespace::Type, head)?
                else {
                    return None;
                };
                Some(Cursor::Item(item))
            })?;

        for (index, segment) in rest.iter().enumerate() {
            let is_last = index + 1 == rest.len();
            cursor = match cursor {
                Cursor::Module(module) if is_last => {
                    return self.resolve_in_module(module, namespace, segment);
                }
                Cursor::Module(module) => {
                    if let Some(child) = self.modules[module.index()].scope.modules.get(segment) {
                        Cursor::Module(*child)
                    } else {
                        let ResolvedTarget::Item(item) =
                            self.resolve_in_module(module, Namespace::Type, segment)?
                        else {
                            return None;
                        };
                        Cursor::Item(item)
                    }
                }
                Cursor::Item(owner) if is_last => {
                    return self
                        .enum_variants
                        .get(&(owner, segment.clone()))
                        .or_else(|| self.associated_items.get(&(owner, segment.clone())))
                        .or_else(|| self.trait_items.get(&(owner, segment.clone())))
                        .copied()
                        .map(ResolvedTarget::Item);
                }
                Cursor::Item(_) => return None,
            };
        }
        None
    }

    fn resolve_in_module(
        &self,
        module: ModuleId,
        namespace: Namespace,
        name: &str,
    ) -> Option<ResolvedTarget> {
        let scope = &self.modules[module.index()].scope;
        match namespace {
            Namespace::Value => scope.values.get(name).copied().map(|id| self.target(id)),
            Namespace::Type => scope.types.get(name).copied().map(ResolvedTarget::Item),
            Namespace::Module => scope.modules.get(name).copied().map(ResolvedTarget::Module),
        }
        .or_else(|| {
            (module != self.root).then(|| {
                let root = &self.modules[self.root.index()].scope;
                match namespace {
                    Namespace::Value => root.values.get(name).copied().map(|id| self.target(id)),
                    Namespace::Type => root.types.get(name).copied().map(ResolvedTarget::Item),
                    Namespace::Module => {
                        root.modules.get(name).copied().map(ResolvedTarget::Module)
                    }
                }
            })?
        })
        .or_else(|| {
            let prelude = self.modules[self.root.index()]
                .scope
                .modules
                .get("__rua_builtin")
                .copied()?;
            if prelude == module {
                return None;
            }
            let scope = &self.modules[prelude.index()].scope;
            match namespace {
                Namespace::Value => scope.values.get(name).copied().map(|id| self.target(id)),
                Namespace::Type => scope.types.get(name).copied().map(ResolvedTarget::Item),
                Namespace::Module => None,
            }
        })
    }

    fn target(&self, id: DefId) -> ResolvedTarget {
        match self.definition(id).kind {
            DefKind::ExternFunction { extern_id } => ResolvedTarget::Extern(extern_id),
            _ => ResolvedTarget::Item(id),
        }
    }

    fn canonical_value_target(&self, target: ResolvedTarget) -> ResolvedTarget {
        let ResolvedTarget::Item(definition_id) = target else {
            return target;
        };
        let definition = self.definition(definition_id);
        let owner = match definition.kind {
            DefKind::EnumVariant { owner, .. } | DefKind::Method { owner } => owner,
            _ => return target,
        };
        let Some(prelude) = self.modules[self.root.index()]
            .scope
            .modules
            .get("__rua_builtin")
            .copied()
        else {
            return target;
        };
        let owner = self.definition(owner);
        if owner.module != prelude {
            return target;
        }
        let builtin = match (owner.name.as_str(), definition.name.as_str()) {
            ("Option", "Some") => BuiltinId::VariantOptionSome,
            ("Option", "None") => BuiltinId::VariantOptionNone,
            ("Result", "Ok") => BuiltinId::VariantResultOk,
            ("Result", "Err") => BuiltinId::VariantResultErr,
            ("Vec", "new") => BuiltinId::AssociatedVecNew,
            ("HashMap", "new") => BuiltinId::AssociatedHashMapNew,
            _ => return target,
        };
        ResolvedTarget::Builtin(builtin)
    }

    fn private_barrier(&self, from: ModuleId, target: ResolvedTarget) -> Option<(String, String)> {
        let target_module = match target {
            ResolvedTarget::Item(definition) => {
                let definition = self.definition(definition);
                if !definition.is_public && !self.is_descendant_of(from, definition.module) {
                    return Some((
                        definition.name.clone(),
                        self.module_owner_label(definition.module),
                    ));
                }
                definition.module
            }
            ResolvedTarget::Module(module) => module,
            ResolvedTarget::Extern(_)
            | ResolvedTarget::Builtin(_)
            | ResolvedTarget::Local(_)
            | ResolvedTarget::Error => return None,
        };

        let mut modules = Vec::new();
        let mut cursor = Some(target_module);
        while let Some(module) = cursor {
            modules.push(module);
            cursor = self.module(module).parent;
        }
        for module in modules.into_iter().rev() {
            let data = self.module(module);
            let Some(parent) = data.parent else {
                continue;
            };
            if !data.is_public && !self.is_descendant_of(from, parent) {
                let name = data.path.segments().last().cloned().unwrap_or_default();
                return Some((name, self.module_owner_label(parent)));
            }
        }
        None
    }

    fn is_descendant_of(&self, mut module: ModuleId, ancestor: ModuleId) -> bool {
        loop {
            if module == ancestor {
                return true;
            }
            let Some(parent) = self.module(module).parent else {
                return false;
            };
            module = parent;
        }
    }

    fn module_owner_label(&self, module: ModuleId) -> String {
        let path = &self.module(module).path;
        if path.is_root() {
            "crate root".to_string()
        } else {
            format!("module `{path}`")
        }
    }
}

pub fn collect_declarations(program: &Program) -> ResolvedHir {
    let root = ModuleId::new(0);
    let mut hir = ResolvedHir {
        root,
        modules: vec![ModuleData {
            id: root,
            parent: None,
            path: ModulePath::default(),
            file: FileId::new(0),
            is_file: true,
            is_declaration: program.is_decl,
            is_public: true,
            scope: ModuleScope::default(),
        }],
        definitions: Vec::new(),
        locals: Vec::new(),
        item_targets: BTreeMap::new(),
        enum_variant_targets: BTreeMap::new(),
        trait_method_targets: BTreeMap::new(),
        impl_method_targets: BTreeMap::new(),
        extern_function_targets: BTreeMap::new(),
        expression_targets: BTreeMap::new(),
        pattern_targets: BTreeMap::new(),
        type_targets: BTreeMap::new(),
        trait_ref_targets: BTreeMap::new(),
        enum_variants: BTreeMap::new(),
        associated_items: BTreeMap::new(),
        trait_items: BTreeMap::new(),
        type_traits: BTreeMap::new(),
        method_origins: BTreeMap::new(),
        trait_method_implementations: BTreeMap::new(),
        trait_default_methods: BTreeSet::new(),
        method_traits: BTreeMap::new(),
        impl_targets: BTreeMap::new(),
        imports: Vec::new(),
        diagnostics: Vec::new(),
    };
    collect_items(&mut hir, root, &program.items);
    collect_trait_items(&mut hir, root, &program.items);
    collect_impl_items(&mut hir, root, &program.items);
    collect_inherited_methods(&mut hir);
    hir
}

/// Collect declarations first, then resolve every expression body without
/// changing source path text.
pub fn resolve(program: &Program) -> ResolvedHir {
    let mut hir = collect_declarations(program);
    let root = hir.root;
    resolve_module_bodies(&mut hir, root, &program.items, &program.chunk);
    hir
}

fn resolve_module_bodies(hir: &mut ResolvedHir, module: ModuleId, items: &[Item], chunk: &Block) {
    let aliases = collect_aliases(hir, module, items);
    resolve_module_types(hir, module, items, chunk, &aliases);
    for item in items {
        match item {
            Item::Fn(function) => {
                let mut resolver = BodyResolver::new(hir, module, &aliases);
                resolver.push();
                if function.has_self {
                    resolver.bind("self", function.receiver_mutable);
                }
                for parameter in &function.params {
                    resolver.bind(&parameter.name, false);
                }
                resolver.block(&function.body);
                resolver.pop();
            }
            Item::Impl(implementation) => {
                for method in &implementation.methods {
                    let mut resolver = BodyResolver::new(hir, module, &aliases);
                    resolver.push();
                    if method.has_self {
                        resolver.bind("self", method.receiver_mutable);
                    }
                    for parameter in &method.params {
                        resolver.bind(&parameter.name, false);
                    }
                    resolver.block(&method.body);
                    resolver.pop();
                }
            }
            Item::Trait(trait_decl) => {
                for method in &trait_decl.methods {
                    if let Some(body) = &method.default {
                        let mut resolver = BodyResolver::new(hir, module, &aliases);
                        resolver.push();
                        if method.has_self {
                            resolver.bind("self", method.receiver_mutable);
                        }
                        for parameter in &method.params {
                            resolver.bind(&parameter.name, false);
                        }
                        resolver.block(body);
                        resolver.pop();
                    }
                }
            }
            Item::Mod(child) => {
                if let Some(child_id) = hir.modules[module.index()]
                    .scope
                    .modules
                    .get(&child.name)
                    .copied()
                {
                    resolve_module_bodies(hir, child_id, &child.items, &child.chunk);
                }
            }
            Item::Struct(_) | Item::Enum(_) | Item::Extern(_) | Item::Use(_) => {}
        }
    }
    let mut resolver = BodyResolver::new(hir, module, &aliases);
    resolver.block(chunk);
}

fn resolve_module_types(
    hir: &mut ResolvedHir,
    module: ModuleId,
    items: &[Item],
    chunk: &Block,
    aliases: &BTreeMap<String, ResolvedTarget>,
) {
    for item in items {
        match item {
            Item::Fn(function) => {
                resolve_trait_refs(hir, module, aliases, &function.generics);
                let generics = generic_names(&function.generics, &[]);
                resolve_signature_types(
                    hir,
                    module,
                    aliases,
                    &generics,
                    &function.params,
                    function.ret.as_ref(),
                );
                resolve_block_types(hir, module, aliases, &generics, &function.body);
            }
            Item::Struct(structure) => {
                resolve_trait_refs(hir, module, aliases, &structure.generics);
                let generics = generic_names(&structure.generics, &[]);
                for field in &structure.fields {
                    resolve_type(hir, module, aliases, &generics, &field.ty);
                }
            }
            Item::Enum(enumeration) => {
                resolve_trait_refs(hir, module, aliases, &enumeration.generics);
                let generics = generic_names(&enumeration.generics, &[]);
                for variant in &enumeration.variants {
                    match &variant.kind {
                        VariantKind::Unit => {}
                        VariantKind::Tuple(types) => {
                            for ty in types {
                                resolve_type(hir, module, aliases, &generics, ty);
                            }
                        }
                        VariantKind::Struct(fields) => {
                            for field in fields {
                                resolve_type(hir, module, aliases, &generics, &field.ty);
                            }
                        }
                    }
                }
            }
            Item::Impl(implementation) => {
                resolve_trait_refs(hir, module, aliases, &implementation.generics);
                for method in &implementation.methods {
                    resolve_trait_refs(hir, module, aliases, &method.generics);
                    let generics = generic_names(&implementation.generics, &method.generics);
                    resolve_signature_types(
                        hir,
                        module,
                        aliases,
                        &generics,
                        &method.params,
                        method.ret.as_ref(),
                    );
                    resolve_block_types(hir, module, aliases, &generics, &method.body);
                }
            }
            Item::Trait(trait_decl) => {
                resolve_trait_refs(hir, module, aliases, &trait_decl.generics);
                for method in &trait_decl.methods {
                    resolve_trait_refs(hir, module, aliases, &method.generics);
                    let generics = generic_names(&trait_decl.generics, &method.generics);
                    resolve_signature_types(
                        hir,
                        module,
                        aliases,
                        &generics,
                        &method.params,
                        method.ret.as_ref(),
                    );
                    if let Some(body) = &method.default {
                        resolve_block_types(hir, module, aliases, &generics, body);
                    }
                }
            }
            Item::Extern(block) => {
                let generics = BTreeMap::new();
                for function in &block.fns {
                    resolve_signature_types(
                        hir,
                        module,
                        aliases,
                        &generics,
                        &function.params,
                        function.ret.as_ref(),
                    );
                }
            }
            Item::Mod(_) | Item::Use(_) => {}
        }
    }
    resolve_block_types(hir, module, aliases, &BTreeMap::new(), chunk);
}

fn generic_names(
    outer: &[GenericParam],
    inner: &[GenericParam],
) -> BTreeMap<String, GenericParamId> {
    outer
        .iter()
        .chain(inner)
        .map(|parameter| (parameter.name.clone(), parameter.id))
        .collect()
}

fn resolve_trait_refs(
    hir: &mut ResolvedHir,
    module: ModuleId,
    aliases: &BTreeMap<String, ResolvedTarget>,
    generics: &[GenericParam],
) {
    for parameter in generics {
        for bound in &parameter.bounds {
            let segments = bound
                .path
                .split("::")
                .map(str::to_string)
                .collect::<Vec<_>>();
            let resolved = if segments.len() == 1 {
                aliases
                    .get(&bound.path)
                    .copied()
                    .or_else(|| hir.resolve_path(module, Namespace::Type, &segments))
            } else {
                hir.resolve_path(module, Namespace::Type, &segments)
            };
            let target = match resolved {
                Some(ResolvedTarget::Item(definition))
                    if matches!(hir.definition(definition).kind, DefKind::Trait) =>
                {
                    Some(TraitTarget::Item(definition))
                }
                _ => builtin_trait(&bound.path).map(TraitTarget::Builtin),
            };
            if let Some(target) = target {
                hir.trait_ref_targets.insert(bound.id, target);
            }
        }
    }
}

fn resolve_signature_types(
    hir: &mut ResolvedHir,
    module: ModuleId,
    aliases: &BTreeMap<String, ResolvedTarget>,
    generics: &BTreeMap<String, GenericParamId>,
    parameters: &[Param],
    return_type: Option<&Type>,
) {
    for parameter in parameters {
        resolve_type(hir, module, aliases, generics, &parameter.ty);
    }
    if let Some(return_type) = return_type {
        resolve_type(hir, module, aliases, generics, return_type);
    }
}

fn resolve_type(
    hir: &mut ResolvedHir,
    module: ModuleId,
    aliases: &BTreeMap<String, ResolvedTarget>,
    generics: &BTreeMap<String, GenericParamId>,
    ty: &Type,
) {
    match ty {
        Type::Path { id, name, args } => {
            for argument in args {
                resolve_type(hir, module, aliases, generics, argument);
            }
            let target = if name == "_" {
                TypeTarget::Infer
            } else if name == "Self" {
                TypeTarget::SelfType
            } else if !name.contains("::")
                && let Some(parameter) = generics.get(name)
            {
                TypeTarget::Generic(*parameter)
            } else {
                let segments = name.split("::").map(str::to_string).collect::<Vec<_>>();
                let resolved = if segments.len() == 1 {
                    aliases
                        .get(name)
                        .copied()
                        .or_else(|| hir.resolve_path(module, Namespace::Type, &segments))
                } else {
                    hir.resolve_path(module, Namespace::Type, &segments)
                };
                match resolved {
                    Some(ResolvedTarget::Item(definition)) => {
                        canonical_type_target(hir, definition)
                    }
                    _ if primitive_type(name).is_some() => {
                        TypeTarget::Primitive(primitive_type(name).unwrap())
                    }
                    _ if builtin_type(name).is_some() => {
                        TypeTarget::Builtin(builtin_type(name).unwrap())
                    }
                    _ if name == "str" => TypeTarget::Builtin(BuiltinId::TypeString),
                    _ => {
                        unresolved(hir, None, name);
                        TypeTarget::Error
                    }
                }
            };
            hir.type_targets.insert(*id, target);
        }
        Type::Ref { inner, .. } => resolve_type(hir, module, aliases, generics, inner),
        Type::Function { params, ret } => {
            for parameter in params {
                resolve_type(hir, module, aliases, generics, parameter);
            }
            resolve_type(hir, module, aliases, generics, ret);
        }
        Type::Tuple(items) => {
            for item in items {
                resolve_type(hir, module, aliases, generics, item);
            }
        }
        Type::Unit => {}
    }
}

fn canonical_type_target(hir: &ResolvedHir, definition: DefId) -> TypeTarget {
    let data = hir.definition(definition);
    let prelude = hir.module(hir.root).scope.modules.get("__rua_builtin");
    if prelude == Some(&data.module)
        && let Some(builtin) = builtin_type(&data.name)
    {
        return TypeTarget::Builtin(builtin);
    }
    TypeTarget::Item(definition)
}

fn primitive_type(name: &str) -> Option<PrimitiveType> {
    Some(match name {
        "i8" | "i16" | "i32" | "i64" | "isize" | "u8" | "u16" | "u32" | "u64" | "usize" => {
            PrimitiveType::I64
        }
        "f32" | "f64" => PrimitiveType::F64,
        "bool" => PrimitiveType::Bool,
        "Box" => PrimitiveType::Box,
        _ => return None,
    })
}

fn resolve_block_types(
    hir: &mut ResolvedHir,
    module: ModuleId,
    aliases: &BTreeMap<String, ResolvedTarget>,
    generics: &BTreeMap<String, GenericParamId>,
    block: &Block,
) {
    for statement in &block.stmts {
        resolve_statement_types(hir, module, aliases, generics, statement);
    }
    if let Some(tail) = &block.tail {
        resolve_expression_types(hir, module, aliases, generics, tail);
    }
}

fn resolve_statement_types(
    hir: &mut ResolvedHir,
    module: ModuleId,
    aliases: &BTreeMap<String, ResolvedTarget>,
    generics: &BTreeMap<String, GenericParamId>,
    statement: &Stmt,
) {
    match statement {
        Stmt::Let { ty, init, .. } => {
            if let Some(ty) = ty {
                resolve_type(hir, module, aliases, generics, ty);
            }
            resolve_expression_types(hir, module, aliases, generics, init);
        }
        Stmt::Expr(expression) | Stmt::Return(Some(expression)) => {
            resolve_expression_types(hir, module, aliases, generics, expression);
        }
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        Stmt::While { cond, body } => {
            resolve_expression_types(hir, module, aliases, generics, cond);
            resolve_block_types(hir, module, aliases, generics, body);
        }
        Stmt::Loop { body } => resolve_block_types(hir, module, aliases, generics, body),
        Stmt::For { iter, body, .. } => {
            resolve_expression_types(hir, module, aliases, generics, iter);
            resolve_block_types(hir, module, aliases, generics, body);
        }
        Stmt::WhileLet {
            pat, expr, body, ..
        } => {
            resolve_pattern_types(hir, module, aliases, generics, pat);
            resolve_expression_types(hir, module, aliases, generics, expr);
            resolve_block_types(hir, module, aliases, generics, body);
        }
    }
}

fn resolve_expression_types(
    hir: &mut ResolvedHir,
    module: ModuleId,
    aliases: &BTreeMap<String, ResolvedTarget>,
    generics: &BTreeMap<String, GenericParamId>,
    expression: &Expr,
) {
    match &expression.kind {
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Bool(_)
        | ExprKind::Path(_) => {}
        ExprKind::Closure { params, ret, body } => {
            for parameter in params {
                if let Some(ty) = &parameter.ty {
                    resolve_type(hir, module, aliases, generics, ty);
                }
            }
            if let Some(ret) = ret {
                resolve_type(hir, module, aliases, generics, ret);
            }
            match body {
                ClosureBody::Expr(expression) => {
                    resolve_expression_types(hir, module, aliases, generics, expression)
                }
                ClosureBody::Block(block) => {
                    resolve_block_types(hir, module, aliases, generics, block)
                }
            }
        }
        ExprKind::Unary { expr, .. } | ExprKind::Try { expr } => {
            resolve_expression_types(hir, module, aliases, generics, expr)
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            resolve_expression_types(hir, module, aliases, generics, lhs);
            resolve_expression_types(hir, module, aliases, generics, rhs);
        }
        ExprKind::Call { callee, args } => {
            resolve_expression_types(hir, module, aliases, generics, callee);
            for argument in args {
                resolve_expression_types(hir, module, aliases, generics, argument);
            }
        }
        ExprKind::MethodCall {
            recv,
            type_args,
            args,
            ..
        } => {
            resolve_expression_types(hir, module, aliases, generics, recv);
            for ty in type_args {
                resolve_type(hir, module, aliases, generics, ty);
            }
            for argument in args {
                resolve_expression_types(hir, module, aliases, generics, argument);
            }
        }
        ExprKind::Field { base, .. } => {
            resolve_expression_types(hir, module, aliases, generics, base)
        }
        ExprKind::StructLit { fields, .. } => {
            for (_, value) in fields {
                resolve_expression_types(hir, module, aliases, generics, value);
            }
        }
        ExprKind::Match { scrut, arms } => {
            resolve_expression_types(hir, module, aliases, generics, scrut);
            for arm in arms {
                for pattern in &arm.pats {
                    resolve_pattern_types(hir, module, aliases, generics, pattern);
                }
                if let Some(guard) = &arm.guard {
                    resolve_expression_types(hir, module, aliases, generics, guard);
                }
                resolve_expression_types(hir, module, aliases, generics, &arm.body);
            }
        }
        ExprKind::Range { start, end, .. } => {
            resolve_expression_types(hir, module, aliases, generics, start);
            resolve_expression_types(hir, module, aliases, generics, end);
        }
        ExprKind::Index { base, index } => {
            resolve_expression_types(hir, module, aliases, generics, base);
            resolve_expression_types(hir, module, aliases, generics, index);
        }
        ExprKind::MacroCall { args, .. } => {
            for argument in args {
                resolve_expression_types(hir, module, aliases, generics, argument);
            }
        }
        ExprKind::If {
            cond,
            then_block,
            else_block,
        } => {
            resolve_expression_types(hir, module, aliases, generics, cond);
            resolve_block_types(hir, module, aliases, generics, then_block);
            resolve_else_types(hir, module, aliases, generics, else_block);
        }
        ExprKind::IfLet {
            pat,
            expr,
            then_block,
            else_block,
        } => {
            resolve_pattern_types(hir, module, aliases, generics, pat);
            resolve_expression_types(hir, module, aliases, generics, expr);
            resolve_block_types(hir, module, aliases, generics, then_block);
            resolve_else_types(hir, module, aliases, generics, else_block);
        }
        ExprKind::Block(block) => resolve_block_types(hir, module, aliases, generics, block),
        ExprKind::Assign { target, value } => {
            resolve_expression_types(hir, module, aliases, generics, target);
            resolve_expression_types(hir, module, aliases, generics, value);
        }
    }
}

fn resolve_pattern_types(
    hir: &mut ResolvedHir,
    module: ModuleId,
    aliases: &BTreeMap<String, ResolvedTarget>,
    generics: &BTreeMap<String, GenericParamId>,
    pattern: &Pattern,
) {
    match pattern {
        Pattern::Wildcard | Pattern::Binding(_, _) | Pattern::Path { .. } => {}
        Pattern::Literal(expression) => {
            resolve_expression_types(hir, module, aliases, generics, expression)
        }
        Pattern::Range { lo, hi, .. } => {
            resolve_expression_types(hir, module, aliases, generics, lo);
            resolve_expression_types(hir, module, aliases, generics, hi);
        }
        Pattern::TupleVariant { elems, .. } => {
            for element in elems {
                resolve_pattern_types(hir, module, aliases, generics, element);
            }
        }
        Pattern::StructVariant { fields, .. } => {
            for (_, field) in fields {
                resolve_pattern_types(hir, module, aliases, generics, field);
            }
        }
    }
}

fn resolve_else_types(
    hir: &mut ResolvedHir,
    module: ModuleId,
    aliases: &BTreeMap<String, ResolvedTarget>,
    generics: &BTreeMap<String, GenericParamId>,
    branch: &Option<Box<ElseBranch>>,
) {
    match branch.as_deref() {
        Some(ElseBranch::Block(block)) => {
            resolve_block_types(hir, module, aliases, generics, block)
        }
        Some(ElseBranch::If(expression)) => {
            resolve_expression_types(hir, module, aliases, generics, expression)
        }
        None => {}
    }
}

fn collect_aliases(
    hir: &mut ResolvedHir,
    module: ModuleId,
    items: &[Item],
) -> BTreeMap<String, ResolvedTarget> {
    let mut aliases = BTreeMap::new();
    for item in items {
        let Item::Use(use_decl) = item else { continue };
        for import in &use_decl.imports {
            let name = import
                .alias
                .clone()
                .unwrap_or_else(|| import.path.last().cloned().unwrap_or_default());
            let mut target = [Namespace::Value, Namespace::Type, Namespace::Module]
                .into_iter()
                .find_map(|namespace| hir.resolve_path(module, namespace, &import.path))
                .unwrap_or(ResolvedTarget::Error);
            if target == ResolvedTarget::Error {
                unresolved(hir, None, &import.path.join("::"));
            } else if let Some((private, owner)) = hir.private_barrier(module, target) {
                private_access(hir, None, &private, &owner);
                target = ResolvedTarget::Error;
            } else {
                target = hir.canonical_value_target(target);
            }
            aliases.insert(name.clone(), target);
            hir.imports.push(ResolvedImport {
                module,
                name,
                target,
            });
        }
    }
    aliases
}

struct BodyResolver<'a> {
    hir: &'a mut ResolvedHir,
    module: ModuleId,
    aliases: &'a BTreeMap<String, ResolvedTarget>,
    scopes: Vec<BTreeMap<String, LocalId>>,
}

impl<'a> BodyResolver<'a> {
    fn new(
        hir: &'a mut ResolvedHir,
        module: ModuleId,
        aliases: &'a BTreeMap<String, ResolvedTarget>,
    ) -> Self {
        Self {
            hir,
            module,
            aliases,
            scopes: Vec::new(),
        }
    }

    fn push(&mut self) {
        self.scopes.push(BTreeMap::new());
    }

    fn pop(&mut self) {
        self.scopes.pop();
    }

    fn bind(&mut self, name: &str, is_mutable: bool) -> LocalId {
        let id = LocalId::new(self.hir.locals.len() as u32);
        self.hir.locals.push(LocalData {
            id,
            name: name.to_string(),
            module: self.module,
            is_mutable,
        });
        self.scopes
            .last_mut()
            .expect("binding requires a body scope")
            .insert(name.to_string(), id);
        id
    }

    fn local(&self, name: &str) -> Option<LocalId> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }

    fn root_local_target(&self, expression: &Expr) -> Option<LocalId> {
        match &expression.kind {
            ExprKind::Path(_) => match self.hir.expression_targets.get(&expression.id) {
                Some(ResolvedTarget::Local(local)) => Some(*local),
                _ => None,
            },
            ExprKind::Field { base, .. } | ExprKind::Index { base, .. } => {
                self.root_local_target(base)
            }
            _ => None,
        }
    }

    fn block(&mut self, block: &Block) {
        self.push();
        for statement in &block.stmts {
            self.statement(statement);
        }
        if let Some(tail) = &block.tail {
            self.expression(tail);
        }
        self.pop();
    }

    fn statement(&mut self, statement: &Stmt) {
        match statement {
            Stmt::Let {
                name,
                mutable,
                init,
                ..
            } => {
                self.expression(init);
                self.bind(name, *mutable);
            }
            Stmt::Expr(expression) | Stmt::Return(Some(expression)) => self.expression(expression),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            Stmt::While { cond, body } => {
                self.expression(cond);
                self.block(body);
            }
            Stmt::Loop { body } => self.block(body),
            Stmt::For {
                var, iter, body, ..
            } => {
                self.expression(iter);
                self.push();
                self.bind(var, false);
                self.block(body);
                self.pop();
            }
            Stmt::WhileLet { pat, expr, body } => {
                self.expression(expr);
                self.push();
                self.pattern(pat, true);
                self.block(body);
                self.pop();
            }
        }
    }

    fn expression(&mut self, expression: &Expr) {
        match &expression.kind {
            ExprKind::Path(path) => {
                let target = self.value_path(path, Some(expression)).unwrap_or_else(|| {
                    self.report_unresolved_path(Some(expression), path);
                    ResolvedTarget::Error
                });
                self.hir.expression_targets.insert(expression.id, target);
            }
            ExprKind::StructLit { path, fields } => {
                let target = self
                    .resolve_path(Namespace::Type, path)
                    .or_else(|| self.resolve_path(Namespace::Value, path))
                    .map(|target| self.check_access(target, Some(expression)))
                    .unwrap_or_else(|| {
                        self.report_unresolved_path(Some(expression), path);
                        ResolvedTarget::Error
                    });
                self.hir.expression_targets.insert(expression.id, target);
                for (_, value) in fields {
                    self.expression(value);
                }
            }
            ExprKind::MacroCall { name, args } => {
                let target = builtin_macro(name)
                    .map(|builtin| ResolvedTarget::Builtin(builtin.id))
                    .unwrap_or(ResolvedTarget::Error);
                if target == ResolvedTarget::Error {
                    unresolved(self.hir, Some(expression), name);
                }
                self.hir.expression_targets.insert(expression.id, target);
                for argument in args {
                    self.expression(argument);
                }
            }
            ExprKind::Closure { params, body, .. } => {
                self.push();
                for parameter in params {
                    self.bind(&parameter.name, false);
                }
                match body {
                    ClosureBody::Expr(expression) => self.expression(expression),
                    ClosureBody::Block(block) => self.block(block),
                }
                self.pop();
            }
            ExprKind::Unary { expr, .. } | ExprKind::Try { expr } => self.expression(expr),
            ExprKind::Binary { lhs, rhs, .. } => {
                self.expression(lhs);
                self.expression(rhs);
            }
            ExprKind::Call { callee, args } => {
                self.expression(callee);
                for argument in args {
                    self.expression(argument);
                }
            }
            ExprKind::MethodCall { recv, args, .. } => {
                self.expression(recv);
                for argument in args {
                    self.expression(argument);
                }
            }
            ExprKind::Field { base, .. } => self.expression(base),
            ExprKind::Match { scrut, arms } => {
                self.expression(scrut);
                for arm in arms {
                    self.push();
                    for pattern in &arm.pats {
                        self.pattern(pattern, true);
                    }
                    if let Some(guard) = &arm.guard {
                        self.expression(guard);
                    }
                    self.expression(&arm.body);
                    self.pop();
                }
            }
            ExprKind::Range { start, end, .. } => {
                self.expression(start);
                self.expression(end);
            }
            ExprKind::Index { base, index } => {
                self.expression(base);
                self.expression(index);
            }
            ExprKind::If {
                cond,
                then_block,
                else_block,
            } => {
                self.expression(cond);
                self.block(then_block);
                self.else_branch(else_block.as_deref());
            }
            ExprKind::IfLet {
                pat,
                expr,
                then_block,
                else_block,
            } => {
                self.expression(expr);
                self.push();
                self.pattern(pat, true);
                self.block(then_block);
                self.pop();
                self.else_branch(else_block.as_deref());
            }
            ExprKind::Block(block) => self.block(block),
            ExprKind::Assign { target, value } => {
                self.expression(target);
                let immutable = self.root_local_target(target).and_then(|local| {
                    let local = &self.hir.locals[local.index()];
                    (!local.is_mutable).then(|| local.name.clone())
                });
                if let Some(name) = immutable {
                    immutable_assignment(self.hir, target, &name);
                }
                self.expression(value);
            }
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Bool(_) => {}
        }
    }

    fn else_branch(&mut self, branch: Option<&ElseBranch>) {
        match branch {
            Some(ElseBranch::Block(block)) => self.block(block),
            Some(ElseBranch::If(expression)) => self.expression(expression),
            None => {}
        }
    }

    fn pattern(&mut self, pattern: &Pattern, bind: bool) {
        match pattern {
            Pattern::Binding(name, _) if bind => {
                self.bind(name, false);
            }
            Pattern::Literal(expression) => self.expression(expression),
            Pattern::Range { lo, hi, .. } => {
                self.expression(lo);
                self.expression(hi);
            }
            Pattern::Path { id, path } => self.resolve_pattern_path(*id, path),
            Pattern::TupleVariant { id, path, elems } => {
                self.resolve_pattern_path(*id, path);
                for element in elems {
                    self.pattern(element, bind);
                }
            }
            Pattern::StructVariant {
                id, path, fields, ..
            } => {
                self.resolve_pattern_path(*id, path);
                for (_, field) in fields {
                    self.pattern(field, bind);
                }
            }
            Pattern::Wildcard | Pattern::Binding(_, _) => {}
        }
    }

    fn resolve_pattern_path(&mut self, id: PatternId, path: &[String]) {
        let target = if path.len() == 1 {
            builtin_value(&path[0]).map(ResolvedTarget::Builtin)
        } else {
            None
        }
        .or_else(|| self.resolve_path(Namespace::Value, path))
        .or_else(|| self.resolve_path(Namespace::Type, path))
        .map(|target| self.hir.canonical_value_target(target))
        .map(|target| self.check_access(target, None))
        .unwrap_or_else(|| {
            self.report_unresolved_path(None, path);
            ResolvedTarget::Error
        });
        self.hir.pattern_targets.insert(id, target);
    }

    fn value_path(&mut self, path: &[String], expression: Option<&Expr>) -> Option<ResolvedTarget> {
        if path.len() == 1 {
            if let Some(local) = self.local(&path[0]) {
                return Some(ResolvedTarget::Local(local));
            }
            if let Some(builtin) = builtin_value(&path[0]) {
                return Some(ResolvedTarget::Builtin(builtin));
            }
        }
        self.resolve_path(Namespace::Value, path)
            .map(|target| self.hir.canonical_value_target(target))
            .map(|target| self.check_access(target, expression))
    }

    fn resolve_path(&self, namespace: Namespace, path: &[String]) -> Option<ResolvedTarget> {
        let (head, rest) = path.split_first()?;
        if let Some(alias) = self.aliases.get(head).copied() {
            if rest.is_empty() {
                return Some(alias);
            }
            return match alias {
                ResolvedTarget::Module(module) => self.hir.resolve_path(module, namespace, rest),
                ResolvedTarget::Item(owner) if rest.len() == 1 => self
                    .hir
                    .enum_variants
                    .get(&(owner, rest[0].clone()))
                    .or_else(|| self.hir.associated_items.get(&(owner, rest[0].clone())))
                    .or_else(|| self.hir.trait_items.get(&(owner, rest[0].clone())))
                    .copied()
                    .map(ResolvedTarget::Item),
                _ => None,
            };
        }
        self.hir.resolve_path(self.module, namespace, path)
    }

    fn report_unresolved_path(&mut self, expression: Option<&Expr>, path: &[String]) {
        let member = path.last().map(String::as_str).unwrap_or("<unknown>");
        let owner = path.get(..path.len().saturating_sub(1)).and_then(|prefix| {
            (!prefix.is_empty())
                .then(|| {
                    self.resolve_path(Namespace::Type, prefix)
                        .or_else(|| self.resolve_path(Namespace::Module, prefix))
                })
                .flatten()
        });
        if let Some(owner) = owner {
            let (owner, kind) = match owner {
                ResolvedTarget::Item(definition) => {
                    let definition = self.hir.definition(definition);
                    let kind = if definition.kind == DefKind::Enum {
                        "enum"
                    } else {
                        "item"
                    };
                    (definition.name.clone(), kind)
                }
                ResolvedTarget::Module(module) => {
                    (self.hir.module(module).path.to_string(), "module")
                }
                _ => (path[..path.len() - 1].join("::"), "item"),
            };
            unknown_member(self.hir, expression, &owner, member, kind);
        } else {
            unresolved(self.hir, expression, &path.join("::"));
        }
    }

    fn check_access(
        &mut self,
        target: ResolvedTarget,
        expression: Option<&Expr>,
    ) -> ResolvedTarget {
        let Some((name, owner)) = self.hir.private_barrier(self.module, target) else {
            return target;
        };
        private_access(self.hir, expression, &name, &owner);
        ResolvedTarget::Error
    }
}

fn unresolved(hir: &mut ResolvedHir, expression: Option<&Expr>, name: &str) {
    let line = expression.map(|expression| expression.span.line);
    let (file, range) = expression.map_or((None, None), |expression| {
        (
            Some(FileId::new(expression.span.file)),
            Some(TextRange::at(
                expression.span.start as u32,
                expression.span.len as u32,
            )),
        )
    });
    let mut diagnostic = StructuredDiagnostic::new(DiagnosticCode::NameUnresolved, file, range)
        .with_argument("name", name);
    if let Some(line) = line {
        diagnostic = diagnostic.with_argument("line", line.to_string());
    }
    hir.diagnostics.push(diagnostic);
}

fn unknown_member(
    hir: &mut ResolvedHir,
    expression: Option<&Expr>,
    owner: &str,
    member: &str,
    kind: &str,
) {
    let line = expression.map(|expression| expression.span.line);
    let (file, range) = expression.map_or((None, None), |expression| {
        (
            Some(FileId::new(expression.span.file)),
            Some(TextRange::at(
                expression.span.start as u32,
                expression.span.len as u32,
            )),
        )
    });
    let mut diagnostic = StructuredDiagnostic::new(DiagnosticCode::NameUnknownMember, file, range)
        .with_argument("owner", owner)
        .with_argument("member", member)
        .with_argument("kind", kind);
    if let Some(line) = line {
        diagnostic = diagnostic.with_argument("line", line.to_string());
    }
    hir.diagnostics.push(diagnostic);
}

fn private_access(hir: &mut ResolvedHir, expression: Option<&Expr>, name: &str, owner: &str) {
    let line = expression.map(|expression| expression.span.line);
    let (file, range) = expression.map_or((None, None), |expression| {
        (
            Some(FileId::new(expression.span.file)),
            Some(TextRange::at(
                expression.span.start as u32,
                expression.span.len as u32,
            )),
        )
    });
    let mut diagnostic = StructuredDiagnostic::new(DiagnosticCode::NamePrivateAccess, file, range)
        .with_argument("name", name)
        .with_argument("owner", owner);
    if let Some(line) = line {
        diagnostic = diagnostic.with_argument("line", line.to_string());
    }
    hir.diagnostics.push(diagnostic);
}

fn immutable_assignment(hir: &mut ResolvedHir, target: &Expr, name: &str) {
    hir.diagnostics.push(
        StructuredDiagnostic::new(
            DiagnosticCode::TypeImmutableAssignment,
            Some(FileId::new(target.span.file)),
            Some(TextRange::at(
                target.span.start as u32,
                target.span.len as u32,
            )),
        )
        .with_argument("name", name)
        .with_argument("line", target.span.line.to_string()),
    );
}

fn collect_items(hir: &mut ResolvedHir, module: ModuleId, items: &[Item]) {
    for (item_index, item) in items.iter().enumerate() {
        match item {
            Item::Fn(function) => {
                let id = alloc_def(
                    hir,
                    module,
                    &function.name,
                    DefKind::Function,
                    function.is_pub,
                );
                insert(hir, module, Namespace::Value, &function.name, id);
                hir.item_targets
                    .insert((module, item_index), ResolvedTarget::Item(id));
            }
            Item::Struct(structure) => {
                let id = alloc_def(
                    hir,
                    module,
                    &structure.name,
                    DefKind::Struct,
                    structure.is_pub,
                );
                insert(hir, module, Namespace::Type, &structure.name, id);
                insert(hir, module, Namespace::Value, &structure.name, id);
                hir.item_targets
                    .insert((module, item_index), ResolvedTarget::Item(id));
            }
            Item::Enum(enumeration) => {
                let owner = alloc_def(
                    hir,
                    module,
                    &enumeration.name,
                    DefKind::Enum,
                    enumeration.is_pub,
                );
                insert(hir, module, Namespace::Type, &enumeration.name, owner);
                insert(hir, module, Namespace::Value, &enumeration.name, owner);
                hir.item_targets
                    .insert((module, item_index), ResolvedTarget::Item(owner));
                for (variant_index, variant) in enumeration.variants.iter().enumerate() {
                    let shape = match variant.kind {
                        VariantKind::Unit => VariantShape::Unit,
                        VariantKind::Tuple(_) => VariantShape::Tuple,
                        VariantKind::Struct(_) => VariantShape::Struct,
                    };
                    let variant_id = alloc_def(
                        hir,
                        module,
                        &variant.name,
                        DefKind::EnumVariant { owner, shape },
                        enumeration.is_pub,
                    );
                    if hir
                        .enum_variants
                        .insert((owner, variant.name.clone()), variant_id)
                        .is_some()
                    {
                        duplicate_variant(hir, owner, &variant.name);
                    }
                    hir.enum_variant_targets
                        .insert((owner, variant_index), variant_id);
                    if !matches!(variant.kind, VariantKind::Struct(_) | VariantKind::Tuple(_)) {
                        insert(hir, module, Namespace::Value, &variant.name, variant_id);
                    }
                }
            }
            Item::Trait(trait_decl) => {
                let id = alloc_def(
                    hir,
                    module,
                    &trait_decl.name,
                    DefKind::Trait,
                    trait_decl.is_pub,
                );
                insert(hir, module, Namespace::Type, &trait_decl.name, id);
                hir.item_targets
                    .insert((module, item_index), ResolvedTarget::Item(id));
            }
            Item::Extern(block) => {
                for (function_index, function) in block.fns.iter().enumerate() {
                    let extern_id = ExternId::new(
                        hir.definitions
                            .iter()
                            .filter(|definition| {
                                matches!(definition.kind, DefKind::ExternFunction { .. })
                            })
                            .count() as u32,
                    );
                    let id = alloc_def(
                        hir,
                        module,
                        &function.name,
                        DefKind::ExternFunction { extern_id },
                        true,
                    );
                    insert(hir, module, Namespace::Value, &function.name, id);
                    hir.extern_function_targets
                        .insert((module, item_index, function_index), id);
                }
            }
            Item::Mod(child) => {
                let child_id = ModuleId::new(hir.modules.len() as u32);
                let parent_path = hir.module(module).path.clone();
                let path = parent_path
                    .child(child.name.clone())
                    .expect("parser only accepts valid module identifiers");
                hir.modules.push(ModuleData {
                    id: child_id,
                    parent: Some(module),
                    path,
                    file: file_of_items(&child.items).unwrap_or(hir.module(module).file),
                    is_file: child.is_file,
                    is_declaration: child.is_decl,
                    is_public: child.is_pub,
                    scope: ModuleScope::default(),
                });
                if hir.modules[module.index()]
                    .scope
                    .modules
                    .insert(child.name.clone(), child_id)
                    .is_some()
                {
                    duplicate(hir, module, &child.name);
                }
                hir.item_targets
                    .insert((module, item_index), ResolvedTarget::Module(child_id));
                collect_items(hir, child_id, &child.items);
            }
            Item::Impl(_) | Item::Use(_) => {}
        }
    }
}

fn collect_trait_items(hir: &mut ResolvedHir, module: ModuleId, items: &[Item]) {
    for (item_index, item) in items.iter().enumerate() {
        match item {
            Item::Trait(trait_decl) => {
                let Some(ResolvedTarget::Item(owner)) =
                    hir.item_targets.get(&(module, item_index)).copied()
                else {
                    continue;
                };
                for (method_index, method) in trait_decl.methods.iter().enumerate() {
                    let definition = alloc_def(
                        hir,
                        module,
                        &method.name,
                        DefKind::TraitMethod { owner },
                        trait_decl.is_pub,
                    );
                    if hir
                        .trait_items
                        .insert((owner, method.name.clone()), definition)
                        .is_some()
                    {
                        duplicate(hir, module, &method.name);
                    }
                    if method.default.is_some() {
                        hir.trait_default_methods.insert(definition);
                    }
                    hir.trait_method_targets
                        .insert((owner, method_index), definition);
                }
            }
            Item::Mod(child) => {
                if let Some(&child_id) = hir.module(module).scope.modules.get(&child.name) {
                    collect_trait_items(hir, child_id, &child.items);
                }
            }
            Item::Fn(_)
            | Item::Struct(_)
            | Item::Enum(_)
            | Item::Impl(_)
            | Item::Extern(_)
            | Item::Use(_) => {}
        }
    }
}

fn collect_impl_items(hir: &mut ResolvedHir, module: ModuleId, items: &[Item]) {
    for (item_index, item) in items.iter().enumerate() {
        match item {
            Item::Impl(implementation) => {
                let segments = implementation
                    .type_name
                    .split("::")
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                let owner = match hir.resolve_path(module, Namespace::Type, &segments) {
                    Some(ResolvedTarget::Item(owner)) => owner,
                    _ => {
                        unresolved(hir, None, &implementation.type_name);
                        continue;
                    }
                };
                let trait_target = if let Some(trait_name) = &implementation.trait_name {
                    let segments = trait_name
                        .split("::")
                        .map(str::to_string)
                        .collect::<Vec<_>>();
                    let target = match hir.resolve_path(module, Namespace::Type, &segments) {
                        Some(ResolvedTarget::Item(trait_id))
                            if matches!(hir.definition(trait_id).kind, DefKind::Trait) =>
                        {
                            Some(TraitTarget::Item(trait_id))
                        }
                        _ => builtin_trait(trait_name).map(TraitTarget::Builtin),
                    };
                    if let Some(target) = target {
                        hir.type_traits.entry(owner).or_default().insert(target);
                        Some(target)
                    } else {
                        unresolved(hir, None, trait_name);
                        None
                    }
                } else {
                    None
                };
                hir.impl_targets.insert(
                    (module, item_index),
                    ImplTarget {
                        owner,
                        trait_target,
                    },
                );
                for (method_index, method) in implementation.methods.iter().enumerate() {
                    let definition = alloc_def(
                        hir,
                        module,
                        &method.name,
                        DefKind::Method { owner },
                        method.is_pub,
                    );
                    if hir
                        .associated_items
                        .insert((owner, method.name.clone()), definition)
                        .is_some()
                    {
                        duplicate(hir, module, &method.name);
                    }
                    if let Some(trait_target) = trait_target {
                        hir.method_traits.insert(definition, trait_target);
                        if let TraitTarget::Item(trait_id) = trait_target
                            && let Some(trait_method) = hir
                                .trait_items
                                .get(&(trait_id, method.name.clone()))
                                .copied()
                        {
                            hir.trait_method_implementations
                                .insert(definition, trait_method);
                        }
                    }
                    hir.impl_method_targets
                        .insert((module, item_index, method_index), definition);
                }
            }
            Item::Mod(child) => {
                if let Some(&child_id) = hir.module(module).scope.modules.get(&child.name) {
                    collect_impl_items(hir, child_id, &child.items);
                }
            }
            Item::Fn(_)
            | Item::Struct(_)
            | Item::Enum(_)
            | Item::Trait(_)
            | Item::Extern(_)
            | Item::Use(_) => {}
        }
    }
}

fn collect_inherited_methods(hir: &mut ResolvedHir) {
    let implementations = hir
        .type_traits
        .iter()
        .flat_map(|(owner, traits)| {
            traits.iter().filter_map(move |target| match target {
                TraitTarget::Item(trait_id) => Some((*owner, *trait_id)),
                TraitTarget::Builtin(_) => None,
            })
        })
        .collect::<Vec<_>>();
    for (owner, trait_id) in implementations {
        let defaults = hir
            .trait_items
            .iter()
            .filter(|((candidate, _), method)| {
                *candidate == trait_id && hir.trait_default_methods.contains(method)
            })
            .map(|((_, name), method)| (name.clone(), *method))
            .collect::<Vec<_>>();
        for (name, origin) in defaults {
            if hir.associated_items.contains_key(&(owner, name.clone())) {
                continue;
            }
            let definition = alloc_def(
                hir,
                hir.definition(owner).module,
                &name,
                DefKind::Method { owner },
                hir.definition(origin).is_public,
            );
            hir.associated_items.insert((owner, name), definition);
            hir.method_origins.insert(definition, origin);
            hir.method_traits
                .insert(definition, TraitTarget::Item(trait_id));
        }
    }
}

fn alloc_def(
    hir: &mut ResolvedHir,
    module: ModuleId,
    name: &str,
    kind: DefKind,
    is_public: bool,
) -> DefId {
    let id = DefId::new(hir.definitions.len() as u32);
    hir.definitions.push(DefData {
        id,
        module,
        name: name.to_string(),
        kind,
        is_public,
    });
    id
}

fn insert(hir: &mut ResolvedHir, module: ModuleId, namespace: Namespace, name: &str, id: DefId) {
    let scope = &mut hir.modules[module.index()].scope;
    let previous = match namespace {
        Namespace::Value => scope.values.insert(name.to_string(), id),
        Namespace::Type => scope.types.insert(name.to_string(), id),
        Namespace::Module => unreachable!(),
    };
    if let Some(previous) = previous {
        let same_enum = matches!(
            (
                hir.definition(previous).kind,
                hir.definition(id).kind,
            ),
            (
                DefKind::EnumVariant {
                    owner: left,
                    shape: _,
                },
                DefKind::EnumVariant {
                    owner: right,
                    shape: _,
                },
            ) if left == right
        );
        if same_enum {
            return;
        }
        duplicate(hir, module, name);
    }
}

fn duplicate_variant(hir: &mut ResolvedHir, owner: DefId, name: &str) {
    let owner = hir.definition(owner).name.clone();
    hir.diagnostics.push(
        StructuredDiagnostic::new(DiagnosticCode::NameDuplicateDefinition, None, None)
            .with_argument("name", name)
            .with_argument("owner", owner)
            .with_argument("kind", "variant"),
    );
}

fn duplicate(hir: &mut ResolvedHir, module: ModuleId, name: &str) {
    let owner = hir.module(module).path.to_string();
    let already_reported = hir.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == DiagnosticCode::NameDuplicateDefinition
            && diagnostic
                .arguments
                .iter()
                .any(|argument| argument.name == "name" && argument.value == name)
            && diagnostic
                .arguments
                .iter()
                .any(|argument| argument.name == "owner" && argument.value == owner)
    });
    if already_reported {
        return;
    }
    hir.diagnostics.push(
        StructuredDiagnostic::new(DiagnosticCode::NameDuplicateDefinition, None, None)
            .with_argument("name", name)
            .with_argument("owner", owner),
    );
}

fn file_of_items(items: &[Item]) -> Option<FileId> {
    items.iter().find_map(|item| match item {
        Item::Fn(function) => Some(FileId::new(function.name_span.file)),
        Item::Struct(structure) => structure
            .fields
            .first()
            .map(|field| FileId::new(field.name_span.file)),
        Item::Trait(trait_decl) => trait_decl
            .methods
            .first()
            .map(|method| FileId::new(method.name_span.file)),
        Item::Impl(implementation) => implementation
            .methods
            .first()
            .map(|method| FileId::new(method.name_span.file)),
        Item::Mod(module) => file_of_items(&module.items),
        Item::Enum(_) | Item::Extern(_) | Item::Use(_) => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_collection_is_module_scoped_and_deterministic() {
        let program = crate::parser::parse(
            "mod left { struct Point {} fn make() {} } mod right { struct Point {} }",
        )
        .unwrap();
        let hir = collect_declarations(&program);
        assert!(hir.diagnostics.is_empty());
        assert_eq!(hir.modules.len(), 3);
        let left = hir.module(hir.module(hir.root).scope.modules["left"]);
        let right = hir.module(hir.module(hir.root).scope.modules["right"]);
        assert_ne!(left.scope.types["Point"], right.scope.types["Point"]);
        assert_eq!(left.path.to_string(), "left");
    }

    #[test]
    fn qualified_and_variant_paths_resolve_to_identity() {
        let program = crate::parser::parse(
            "mod api { enum Status { Ready } fn helper() {} fn call() { helper(); } }",
        )
        .unwrap();
        let hir = collect_declarations(&program);
        let api = hir.module(hir.root).scope.modules["api"];
        let helper = hir.resolve_path(api, Namespace::Value, &["helper".into()]);
        let qualified =
            hir.resolve_path(hir.root, Namespace::Value, &["api".into(), "helper".into()]);
        assert_eq!(helper, qualified);
        assert!(matches!(helper, Some(ResolvedTarget::Item(_))));
        assert!(matches!(
            hir.resolve_path(api, Namespace::Value, &["Status".into(), "Ready".into()]),
            Some(ResolvedTarget::Item(_))
        ));
    }

    #[test]
    fn associated_function_path_resolves_to_method_identity() {
        let program = crate::parser::parse(
            "mod api { pub struct Value {} impl Value { pub fn make() -> Value { Value {} } } } fn run() { api::Value::make(); }",
        )
        .unwrap();
        let hir = resolve(&program);
        assert!(hir.diagnostics.is_empty(), "{:?}", hir.diagnostics);
        let api = hir.module(hir.root).scope.modules["api"];
        let value = hir.module(api).scope.types["Value"];
        let method = hir.associated_items[&(value, "make".to_string())];
        assert!(matches!(
            hir.definition(method).kind,
            DefKind::Method { owner } if owner == value
        ));

        let Item::Fn(run) = &program.items[1] else {
            panic!("expected run function");
        };
        let Stmt::Expr(call) = &run.body.stmts[0] else {
            panic!("expected call statement");
        };
        let ExprKind::Call { callee, .. } = &call.kind else {
            panic!("expected call");
        };
        assert_eq!(
            hir.expression_targets[&callee.id],
            ResolvedTarget::Item(method)
        );
    }

    #[test]
    fn body_resolution_preserves_alias_text_and_records_target() {
        let program =
            crate::parser::parse("mod api { pub fn f() {} } use api::f as go; fn run() { go(); }")
                .unwrap();
        let hir = resolve(&program);
        assert!(hir.diagnostics.is_empty());
        let api = hir.module(hir.root).scope.modules["api"];
        let expected = hir
            .resolve_path(api, Namespace::Value, &["f".into()])
            .unwrap();
        let Item::Fn(run) = &program.items[2] else {
            panic!("expected run function");
        };
        let Stmt::Expr(call) = &run.body.stmts[0] else {
            panic!("expected call statement");
        };
        let ExprKind::Call { callee, .. } = &call.kind else {
            panic!("expected call");
        };
        assert!(matches!(&callee.kind, ExprKind::Path(path) if path == &["go"]));
        assert_eq!(hir.expression_targets[&callee.id], expected);
        assert_eq!(hir.imports[0].target, expected);
    }

    #[test]
    fn module_enum_variant_alias_preserves_pattern_text_and_target() {
        let program = crate::parser::parse(
            "mod api { pub enum Status { Ready, Code(i64) } } use api::Status::Code as C; fn run(value: api::Status) { match value { C(code) => code } }",
        )
        .unwrap();
        let hir = resolve(&program);
        assert!(hir.diagnostics.is_empty(), "{:?}", hir.diagnostics);
        let Item::Fn(run) = &program.items[2] else {
            panic!("expected run function");
        };
        let match_expression = run.body.tail.as_ref().expect("match tail expression");
        let ExprKind::Match { arms, .. } = &match_expression.kind else {
            panic!("expected match expression");
        };
        let Pattern::TupleVariant { id, path, .. } = &arms[0].pats[0] else {
            panic!("expected tuple variant pattern");
        };
        assert_eq!(path, &["C"]);
        let target = hir.pattern_targets[id];
        let definition = hir.definition(match target {
            ResolvedTarget::Item(definition) => definition,
            other => panic!("unexpected target: {other:?}"),
        });
        assert_eq!(definition.name, "Code");
    }

    #[test]
    fn lexical_local_shadows_item_after_declaration() {
        let program = crate::parser::parse(
            "fn value() -> i64 { 1 } fn run() { value(); let value = 2; value; }",
        )
        .unwrap();
        let hir = resolve(&program);
        assert!(hir.diagnostics.is_empty());
        let Item::Fn(run) = &program.items[1] else {
            panic!("expected run function");
        };
        let Stmt::Expr(call) = &run.body.stmts[0] else {
            panic!("expected call");
        };
        let ExprKind::Call { callee, .. } = &call.kind else {
            panic!("expected call");
        };
        let Stmt::Expr(local_use) = &run.body.stmts[2] else {
            panic!("expected local use");
        };
        assert!(matches!(
            hir.expression_targets[&callee.id],
            ResolvedTarget::Item(_)
        ));
        assert!(matches!(
            hir.expression_targets[&local_use.id],
            ResolvedTarget::Local(_)
        ));
    }

    #[test]
    fn qualified_sysroot_values_resolve_to_builtin_identity() {
        let mut program = crate::parser::parse(
            "fn run() { Option::Some(1); Result::Err(\"no\"); Vec::new(); HashMap::new(); }",
        )
        .unwrap();
        crate::load_builtins(&mut program, None).unwrap();
        let hir = resolve(&program);
        assert!(hir.diagnostics.is_empty(), "{:?}", hir.diagnostics);
        let Item::Fn(run) = &program.items[1] else {
            panic!("expected run function after builtin module");
        };
        let expected = [
            BuiltinId::VariantOptionSome,
            BuiltinId::VariantResultErr,
            BuiltinId::AssociatedVecNew,
            BuiltinId::AssociatedHashMapNew,
        ];
        for (statement, expected) in run.body.stmts.iter().zip(expected) {
            let Stmt::Expr(call) = statement else {
                panic!("expected call statement");
            };
            let ExprKind::Call { callee, .. } = &call.kind else {
                panic!("expected call expression");
            };
            assert_eq!(
                hir.expression_targets[&callee.id],
                ResolvedTarget::Builtin(expected)
            );
        }
    }

    #[test]
    fn type_and_trait_uses_resolve_to_exact_identity() {
        let program = crate::parser::parse(
            "mod a { pub trait Read {} pub struct Item {} } mod b { pub trait Read {} pub struct Item {} } fn use_it<T: a::Read>(left: a::Item, right: b::Item) {}",
        )
        .unwrap();
        let hir = resolve(&program);
        assert!(hir.diagnostics.is_empty(), "{:?}", hir.diagnostics);
        let a = hir.module(hir.root).scope.modules["a"];
        let b = hir.module(hir.root).scope.modules["b"];
        let Item::Fn(function) = &program.items[2] else {
            panic!("expected function");
        };
        let Type::Path { id: left, .. } = &function.params[0].ty else {
            panic!("expected path type");
        };
        let Type::Path { id: right, .. } = &function.params[1].ty else {
            panic!("expected path type");
        };
        assert_eq!(
            hir.type_targets[left],
            TypeTarget::Item(hir.module(a).scope.types["Item"])
        );
        assert_eq!(
            hir.type_targets[right],
            TypeTarget::Item(hir.module(b).scope.types["Item"])
        );
        assert_ne!(hir.type_targets[left], hir.type_targets[right]);
        assert_eq!(
            hir.trait_ref_targets[&function.generics[0].bounds[0].id],
            TraitTarget::Item(hir.module(a).scope.types["Read"])
        );
    }

    #[test]
    fn builtin_injection_does_not_replace_user_expression_identity() {
        let mut program = crate::parser::parse(
            "mod api { fn add(v: i64) -> i64 { v } fn call() -> i64 { add(1) } }",
        )
        .unwrap();
        crate::load_builtins(&mut program, None).unwrap();
        let hir = resolve(&program);
        let Item::Mod(api) = &program.items[1] else {
            panic!("expected api module");
        };
        let Item::Fn(call) = &api.items[1] else {
            panic!("expected call function");
        };
        let tail = call.body.tail.as_ref().unwrap();
        let ExprKind::Call { callee, .. } = &tail.kind else {
            panic!("expected call");
        };
        assert!(matches!(
            hir.expression_targets.get(&callee.id),
            Some(ResolvedTarget::Item(_))
        ));
        let Item::Fn(add) = &api.items[0] else {
            panic!("expected add function");
        };
        let Type::Path { id, .. } = &add.params[0].ty else {
            panic!("expected parameter type");
        };
        assert_eq!(
            hir.type_targets[id],
            TypeTarget::Primitive(PrimitiveType::I64)
        );
    }
}
