//! Checked compiler IR boundary consumed by the Lua backend.
//!
//! The strict parser tree remains the compact storage for block/expression
//! structure. Semantic uses are accepted here only after resolver and type
//! checker annotations are complete; backend code cannot construct this value
//! from unresolved syntax.

use crate::ast::{ExprId, Item, PatternId, Program};
use crate::hir::{DefId, ImplTarget, ModuleId, ResolvedHir, ResolvedTarget};
use crate::typeck::TypeInfo;

#[derive(Debug)]
pub struct TypedProgram {
    syntax: Program,
    hir: ResolvedHir,
    types: TypeInfo,
}

impl TypedProgram {
    pub fn new(syntax: Program, hir: ResolvedHir, types: TypeInfo) -> Self {
        let program = Self { syntax, hir, types };
        program.validate_items(program.hir.root, &program.syntax.items);
        program
    }

    pub(crate) fn syntax(&self) -> &Program {
        &self.syntax
    }

    pub fn hir(&self) -> &ResolvedHir {
        &self.hir
    }

    pub fn types(&self) -> &TypeInfo {
        &self.types
    }

    pub fn item_target(&self, module: ModuleId, item_index: usize) -> ResolvedTarget {
        self.hir
            .item_targets
            .get(&(module, item_index))
            .copied()
            .filter(|target| *target != ResolvedTarget::Error)
            .unwrap_or_else(|| panic!("typed item {module:?}/{item_index} has no resolved target"))
    }

    pub fn item_definition(&self, module: ModuleId, item_index: usize) -> DefId {
        let ResolvedTarget::Item(definition) = self.item_target(module, item_index) else {
            panic!("typed item {module:?}/{item_index} is not a definition");
        };
        definition
    }

    pub fn child_module(&self, module: ModuleId, item_index: usize) -> ModuleId {
        let ResolvedTarget::Module(child) = self.item_target(module, item_index) else {
            panic!("typed item {module:?}/{item_index} is not a module");
        };
        child
    }

    pub fn implementation(&self, module: ModuleId, item_index: usize) -> ImplTarget {
        self.hir
            .impl_targets
            .get(&(module, item_index))
            .copied()
            .unwrap_or_else(|| panic!("typed impl {module:?}/{item_index} has no resolved target"))
    }

    pub fn expression_target(&self, expression: ExprId) -> ResolvedTarget {
        self.hir
            .expression_targets
            .get(&expression)
            .copied()
            .filter(|target| *target != ResolvedTarget::Error)
            .unwrap_or_else(|| panic!("typed expression {expression:?} has no resolved target"))
    }

    pub fn pattern_target(&self, pattern: PatternId) -> ResolvedTarget {
        self.hir
            .pattern_targets
            .get(&pattern)
            .copied()
            .filter(|target| *target != ResolvedTarget::Error)
            .unwrap_or_else(|| panic!("typed pattern {pattern:?} has no resolved target"))
    }

    pub fn implementation_method(
        &self,
        module: ModuleId,
        item_index: usize,
        method_index: usize,
    ) -> DefId {
        self.hir
            .impl_method_targets
            .get(&(module, item_index, method_index))
            .copied()
            .unwrap_or_else(|| {
                panic!("typed impl method {module:?}/{item_index}/{method_index} has no definition")
            })
    }

    pub fn trait_method(&self, owner: DefId, method_index: usize) -> DefId {
        self.hir
            .trait_method_targets
            .get(&(owner, method_index))
            .copied()
            .unwrap_or_else(|| {
                panic!("typed trait method {owner:?}/{method_index} has no definition")
            })
    }

    pub fn inherited_method(&self, owner: DefId, origin: DefId) -> DefId {
        self.hir
            .method_origins
            .iter()
            .find_map(|(definition, candidate_origin)| {
                (*candidate_origin == origin
                    && matches!(
                        self.hir.definition(*definition).kind,
                        crate::hir::DefKind::Method {
                            owner: candidate_owner
                        } if candidate_owner == owner
                    ))
                .then_some(*definition)
            })
            .unwrap_or_else(|| {
                panic!("trait method {origin:?} has no inherited definition for {owner:?}")
            })
    }

    pub fn extern_function(
        &self,
        module: ModuleId,
        item_index: usize,
        function_index: usize,
    ) -> DefId {
        self.hir
            .extern_function_targets
            .get(&(module, item_index, function_index))
            .copied()
            .unwrap_or_else(|| {
                panic!("typed extern {module:?}/{item_index}/{function_index} has no definition")
            })
    }

    fn validate_items(&self, module: ModuleId, items: &[Item]) {
        for (item_index, item) in items.iter().enumerate() {
            match item {
                Item::Fn(_) | Item::Struct(_) | Item::Enum(_) | Item::Trait(_) => {
                    self.item_definition(module, item_index);
                }
                Item::Mod(child) => {
                    let child_module = self.child_module(module, item_index);
                    self.validate_items(child_module, &child.items);
                }
                Item::Impl(implementation) => {
                    self.implementation(module, item_index);
                    for method_index in 0..implementation.methods.len() {
                        self.implementation_method(module, item_index, method_index);
                    }
                }
                Item::Extern(block) => {
                    for function_index in 0..block.fns.len() {
                        self.extern_function(module, item_index, function_index);
                    }
                }
                Item::Use(_) => {}
            }
            if let Item::Enum(enumeration) = item {
                let owner = self.item_definition(module, item_index);
                for variant_index in 0..enumeration.variants.len() {
                    assert!(
                        self.hir
                            .enum_variant_targets
                            .contains_key(&(owner, variant_index)),
                        "typed enum variant {owner:?}/{variant_index} has no definition"
                    );
                }
            }
            if let Item::Trait(trait_decl) = item {
                let owner = self.item_definition(module, item_index);
                for method_index in 0..trait_decl.methods.len() {
                    self.trait_method(owner, method_index);
                }
            }
        }
    }
}
