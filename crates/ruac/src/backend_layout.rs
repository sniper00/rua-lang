use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::hir::{DefId, DefKind, ExternId, LocalId, ModuleId, ResolvedHir, ResolvedTarget};
use crate::lua_ir::{Expr, FunctionTarget};
use crate::typed_ir::TypedProgram;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Place {
    root: String,
    fields: Vec<String>,
}

impl Place {
    fn name(name: String) -> Self {
        Self {
            root: name,
            fields: Vec::new(),
        }
    }

    pub fn field(&self, field: impl Into<String>) -> Self {
        let mut place = self.clone();
        place.fields.push(field.into());
        place
    }

    pub(crate) fn expression(&self) -> Expr {
        self.fields
            .iter()
            .fold(Expr::name(&self.root), |base, field| base.field(field))
    }

    pub(crate) fn function_target(&self) -> FunctionTarget {
        FunctionTarget::path(self.segments())
    }

    fn callable_target(&self, receiver: bool) -> FunctionTarget {
        if receiver {
            let mut owner = self.segments();
            let method = owner
                .pop()
                .expect("receiver callable has an owner and method segment");
            FunctionTarget::method(owner, method)
        } else {
            self.function_target()
        }
    }

    fn segments(&self) -> Vec<String> {
        let mut segments = Vec::with_capacity(1 + self.fields.len());
        segments.push(self.root.clone());
        segments.extend(self.fields.iter().cloned());
        segments
    }
}

impl fmt::Display for Place {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.root)?;
        for field in &self.fields {
            write!(formatter, ".{field}")?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct BackendLayout {
    modules: BTreeMap<ModuleId, Place>,
    definitions: BTreeMap<DefId, Place>,
    externs: BTreeMap<ExternId, Place>,
    locals: BTreeMap<LocalId, Place>,
    local_names: BTreeMap<(ModuleId, String), String>,
    next_temporary: usize,
}

impl BackendLayout {
    pub fn new(program: &TypedProgram) -> Self {
        let hir: &ResolvedHir = program.hir();
        let mut used = BTreeMap::<ModuleId, BTreeSet<String>>::new();
        let mut modules = BTreeMap::<ModuleId, Place>::new();
        for module in hir.modules.iter().filter(|module| module.id != hir.root) {
            let parent = module.parent.expect("non-root module has a parent");
            let source_name = module
                .path
                .segments()
                .last()
                .expect("non-root module has a path segment");
            let field = allocate_name(&mut used, parent, source_name, module.id.index());
            let place = if parent == hir.root {
                Place::name(field)
            } else {
                modules[&parent].field(field)
            };
            modules.insert(module.id, place);
        }

        let mut definitions = BTreeMap::<DefId, Place>::new();
        let mut externs = BTreeMap::new();
        for definition in &hir.definitions {
            let place = match definition.kind {
                DefKind::EnumVariant { owner, .. } => definitions.get(&owner).cloned(),
                DefKind::Method { owner } | DefKind::TraitMethod { owner } => definitions
                    .get(&owner)
                    .map(|owner| owner.field(user_identifier(&definition.name))),
                DefKind::ExternFunction { extern_id } => {
                    let field = allocate_name(
                        &mut used,
                        definition.module,
                        &definition.name,
                        definition.id.index(),
                    );
                    let place = if definition.module == hir.root {
                        Place::name(field)
                    } else {
                        modules[&definition.module].field(field)
                    };
                    externs.insert(extern_id, place.clone());
                    Some(place)
                }
                DefKind::Trait => None,
                DefKind::Function | DefKind::Struct | DefKind::Enum => {
                    let field = allocate_name(
                        &mut used,
                        definition.module,
                        &definition.name,
                        definition.id.index(),
                    );
                    Some(if definition.module == hir.root {
                        Place::name(field)
                    } else {
                        modules[&definition.module].field(field)
                    })
                }
            };
            if let Some(place) = place {
                definitions.insert(definition.id, place);
            }
        }

        let mut locals = BTreeMap::new();
        let mut local_names = BTreeMap::<(ModuleId, String), String>::new();
        for local in &hir.locals {
            let key = (local.module, local.name.clone());
            let place = if let Some(place) = local_names.get(&key) {
                place.clone()
            } else {
                let candidate = user_identifier(&local.name);
                let names = used.entry(local.module).or_default();
                let place = if names.contains(&candidate) {
                    let mut candidate = format!("{candidate}__local");
                    while names.contains(&candidate) {
                        candidate.push('_');
                    }
                    candidate
                } else {
                    candidate
                };
                names.insert(place.clone());
                local_names.insert(key, place.clone());
                place
            };
            locals.insert(local.id, Place::name(place));
        }
        Self {
            modules,
            definitions,
            externs,
            locals,
            local_names,
            next_temporary: 0,
        }
    }

    pub fn module(&self, module: ModuleId) -> Option<&Place> {
        self.modules.get(&module)
    }

    pub fn definition(&self, definition: DefId) -> Option<&Place> {
        self.definitions.get(&definition)
    }

    pub fn target(&self, target: ResolvedTarget) -> Option<&Place> {
        match target {
            ResolvedTarget::Item(definition) => self.definition(definition),
            ResolvedTarget::Module(module) => self.module(module),
            ResolvedTarget::Local(local) => self.locals.get(&local),
            ResolvedTarget::Extern(external) => self.externs.get(&external),
            ResolvedTarget::Builtin(_) | ResolvedTarget::Error => None,
        }
    }

    pub fn callable_target(&self, definition: DefId, receiver: bool) -> FunctionTarget {
        self.definition(definition)
            .unwrap_or_else(|| panic!("definition {definition:?} has no backend place"))
            .callable_target(receiver)
    }

    pub fn local_name(&self, module: ModuleId, name: &str) -> String {
        self.local_names
            .get(&(module, name.to_string()))
            .cloned()
            .unwrap_or_else(|| user_identifier(name))
    }

    pub fn member_name(&self, name: &str) -> String {
        user_identifier(name)
    }

    pub fn fresh_temporary(&mut self) -> String {
        self.next_temporary += 1;
        format!("__t{}", self.next_temporary)
    }
}

/// Injective ASCII encoding for every source identifier. Compiler-generated
/// names use other prefixes, so Lua keywords and user names cannot collide.
pub fn user_identifier(name: &str) -> String {
    if is_plain_identifier(name) {
        return name.to_string();
    }
    let mut encoded = String::with_capacity(11 + name.len() * 2);
    encoded.push_str("__rua_user_");
    for byte in name.as_bytes() {
        use std::fmt::Write;
        write!(encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

fn allocate_name(
    used: &mut BTreeMap<ModuleId, BTreeSet<String>>,
    scope: ModuleId,
    source_name: &str,
    identity: usize,
) -> String {
    let candidate = user_identifier(source_name);
    let names = used.entry(scope).or_default();
    if names.insert(candidate.clone()) {
        return candidate;
    }
    let unique = format!("{candidate}__{identity}");
    assert!(names.insert(unique.clone()), "backend identity is unique");
    unique
}

fn is_plain_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == b'_')
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return false;
    }
    !LUA_KEYWORDS.contains(&name)
        && !name.starts_with("__rua_")
        && !name.starts_with("__t")
        && name != "rt"
}

const LUA_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_identifier_is_lua_safe_and_injective() {
        assert_ne!(user_identifier("end"), "end");
        assert_eq!(user_identifier("ordinary_name"), "ordinary_name");
        assert_ne!(
            user_identifier("repeat"),
            user_identifier("__rua_user_726570656174")
        );
        assert!(
            user_identifier("变量")
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        );
    }
}
