//! Native aggregate, trait, implementation, and built-in member metadata.
//!
//! The index consumes only [`DefMap`] declaration summaries. It keeps generic
//! templates separate from receiver instantiation so completion can enumerate
//! stable candidates while inference receives concrete signatures.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use super::{
    CallableSignature, CallableTy, DefId, DefKind, DefMap, GenericParamId, ItemSignature,
    ItemSourceKind, NamedTy, ReceiverKind, ResolveStrategy, Substitution, Ty, TypeLoweringContext,
    TypeRef, VariantKind,
};

/// Analysis-owned identifiers for members supplied by the language runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BuiltinMemberId {
    VecNew,
    VecGet,
    VecLen,
    VecPop,
    VecPush,
    VecSet,
    HashMapNew,
    HashMapContainsKey,
    HashMapGet,
    HashMapInsert,
    HashMapLen,
    HashMapRemove,
    StringChars,
    StringClone,
    StringContains,
    StringEndsWith,
    StringIsEmpty,
    StringLen,
    StringRepeat,
    StringReplace,
    StringSplit,
    StringStartsWith,
    StringToLowercase,
    StringToOwned,
    StringToString,
    StringToUppercase,
    StringTrim,
    StringTrimEnd,
    StringTrimStart,
    OptionSome,
    OptionNone,
    ResultOk,
    ResultErr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BuiltinType {
    Vec,
    HashMap,
    String,
    Option,
    Result,
}

impl BuiltinType {
    pub const fn of(ty: &Ty) -> Option<Self> {
        match ty {
            Ty::Vec(_) => Some(Self::Vec),
            Ty::HashMap(_, _) => Some(Self::HashMap),
            Ty::Primitive(super::PrimitiveTy::String) => Some(Self::String),
            Ty::Option(_) => Some(Self::Option),
            Ty::Result(_, _) => Some(Self::Result),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemberTarget {
    Definition(DefId),
    Builtin(BuiltinMemberId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemberKind {
    Field,
    Method,
    AssociatedFunction,
    Variant,
}

/// Where a candidate entered the lookup set. This is presentation metadata;
/// ambiguity is decided by the full candidate set, not by enum ordering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemberOrigin {
    Aggregate,
    InherentImpl(DefId),
    TraitImpl {
        implementation: DefId,
        trait_id: DefId,
    },
    TraitDefault {
        implementation: DefId,
        trait_id: DefId,
    },
    TraitBound(DefId),
    Builtin,
}

/// A trait requirement after syntax has been lowered to stable identities.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TraitBound {
    trait_id: DefId,
    trait_ty: Ty,
}

pub(crate) type CallableRequirement = (Ty, TraitBound);

impl TraitBound {
    pub const fn trait_id(&self) -> DefId {
        self.trait_id
    }

    pub fn trait_ty(&self) -> &Ty {
        &self.trait_ty
    }
}

/// The compact, cacheable result stored at a resolved member expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberResolution {
    target: MemberTarget,
    kind: MemberKind,
    ty: Ty,
    receiver: Option<ReceiverKind>,
    substitution: Substitution,
    generic_params: Vec<GenericParamId>,
    requirements: Vec<CallableRequirement>,
}

impl MemberResolution {
    pub const fn target(&self) -> MemberTarget {
        self.target
    }

    pub const fn kind(&self) -> MemberKind {
        self.kind
    }

    pub fn ty(&self) -> &Ty {
        &self.ty
    }

    pub const fn receiver(&self) -> Option<ReceiverKind> {
        self.receiver
    }

    pub fn substitution(&self) -> &Substitution {
        &self.substitution
    }

    /// Generic parameters declared by the callable itself. Aggregate and impl
    /// parameters have already been handled by the receiver substitution.
    pub fn generic_params(&self) -> &[GenericParamId] {
        &self.generic_params
    }

    pub(crate) fn requirements(&self) -> &[CallableRequirement] {
        &self.requirements
    }

    pub fn callable(&self) -> Option<&CallableTy> {
        match &self.ty {
            Ty::Function(callable) | Ty::Closure(callable) => Some(callable),
            _ => None,
        }
    }
}

/// A named completion candidate plus the semantic result inference can retain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberCandidate {
    name: String,
    origin: MemberOrigin,
    resolution: MemberResolution,
}

impl MemberCandidate {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn origin(&self) -> MemberOrigin {
        self.origin
    }

    pub fn resolution(&self) -> &MemberResolution {
        &self.resolution
    }

    pub const fn target(&self) -> MemberTarget {
        self.resolution.target
    }

    pub const fn kind(&self) -> MemberKind {
        self.resolution.kind
    }

    pub fn ty(&self) -> &Ty {
        &self.resolution.ty
    }

    pub const fn receiver(&self) -> Option<ReceiverKind> {
        self.resolution.receiver
    }

    pub fn substitution(&self) -> &Substitution {
        &self.resolution.substitution
    }

    pub fn generic_params(&self) -> &[GenericParamId] {
        &self.resolution.generic_params
    }
}

#[derive(Clone, Debug)]
struct GenericParamDecl {
    id: GenericParamId,
    name: Option<String>,
}

#[derive(Clone, Debug)]
struct CallableTemplate {
    callable: CallableTy,
    receiver: Option<ReceiverKind>,
    receiver_ty: Option<Ty>,
    method_generics: Vec<GenericParamId>,
    requirements: Vec<CallableRequirement>,
}

#[derive(Clone, Debug)]
struct FieldTemplate {
    definition: DefId,
    name: String,
    ty: Ty,
}

#[derive(Clone, Debug)]
struct VariantTemplate {
    definition: DefId,
    name: String,
    enum_id: DefId,
    result_ty: Ty,
    payload: VariantPayload,
}

#[derive(Clone, Debug)]
enum VariantPayload {
    Unit,
    Tuple(Vec<Ty>),
    Struct,
}

#[derive(Clone, Debug)]
struct TraitData {
    methods: Vec<DefId>,
}

#[derive(Clone, Debug)]
pub struct ImplementationData {
    definition: DefId,
    target_ty: Ty,
    trait_ty: Option<Ty>,
    trait_id: Option<DefId>,
    methods: Vec<DefId>,
    requirements: Vec<(Ty, DefId)>,
}

impl ImplementationData {
    pub const fn definition(&self) -> DefId {
        self.definition
    }

    pub fn target_ty(&self) -> &Ty {
        &self.target_ty
    }

    pub const fn trait_definition(&self) -> Option<DefId> {
        self.trait_id
    }

    pub fn methods(&self) -> &[DefId] {
        &self.methods
    }
}

/// Immutable native member index for one DefMap/project snapshot.
#[derive(Clone, Debug)]
pub struct MemberIndex {
    def_map: Arc<DefMap>,
    generic_params: BTreeMap<DefId, Vec<GenericParamDecl>>,
    bounds: BTreeMap<GenericParamId, Vec<TraitBound>>,
    scoped_bounds: BTreeMap<DefId, BTreeMap<GenericParamId, Vec<TraitBound>>>,
    type_templates: BTreeMap<DefId, Ty>,
    callables: BTreeMap<DefId, CallableTemplate>,
    fields: BTreeMap<DefId, Vec<FieldTemplate>>,
    variant_fields: BTreeMap<DefId, Vec<FieldTemplate>>,
    variants: BTreeMap<DefId, Vec<VariantTemplate>>,
    traits: BTreeMap<DefId, TraitData>,
    implementations: Vec<ImplementationData>,
    builtin_callables: BTreeMap<BuiltinMemberId, CallableTy>,
    builtin_receivers: BTreeMap<BuiltinMemberId, Ty>,
}

impl MemberIndex {
    pub fn implementations(&self) -> impl Iterator<Item = &ImplementationData> {
        self.implementations.iter()
    }

    pub fn implementation(&self, definition: DefId) -> Option<&ImplementationData> {
        self.implementations
            .iter()
            .find(|implementation| implementation.definition == definition)
    }

    pub fn build(def_map: &DefMap) -> Self {
        Self::build_shared(Arc::new(def_map.clone()))
    }

    pub(crate) fn build_shared(def_map: Arc<DefMap>) -> Self {
        let mut index = Self {
            def_map,
            generic_params: BTreeMap::new(),
            bounds: BTreeMap::new(),
            scoped_bounds: BTreeMap::new(),
            type_templates: BTreeMap::new(),
            callables: BTreeMap::new(),
            fields: BTreeMap::new(),
            variant_fields: BTreeMap::new(),
            variants: BTreeMap::new(),
            traits: BTreeMap::new(),
            implementations: Vec::new(),
            builtin_callables: BTreeMap::new(),
            builtin_receivers: BTreeMap::new(),
        };
        index.collect_generic_params();
        index.collect_bounds();
        index.collect_type_templates();
        index.collect_callables();
        index.collect_aggregates_and_traits();
        index.collect_implementations();
        index.install_builtins();
        index
    }

    /// Lower a declaration type in the complete generic owner chain.
    pub fn lower_type(&self, owner: DefId, type_ref: &TypeRef) -> Ty {
        self.lower_type_syntax(owner, type_ref.syntax())
    }

    pub fn lower_type_syntax(&self, owner: DefId, syntax: Option<&str>) -> Ty {
        let Some(syntax) = syntax else {
            return Ty::Unknown;
        };
        let resolver = |path: &str| self.resolve_named_type(owner, path);
        self.lowering_context(owner)
            .with_named_resolver(&resolver)
            .lower_syntax(syntax)
    }

    /// Generic aggregate template, e.g. `Wrapper<T>` rather than `Wrapper<?>`.
    pub fn type_template(&self, definition: DefId) -> Option<&Ty> {
        self.type_templates.get(&definition)
    }

    /// Uninstantiated signature for a declared or built-in callable member.
    pub fn callable(&self, definition: DefId) -> Option<CallableTy> {
        self.callables
            .get(&definition)
            .map(|template| template.callable.clone())
    }

    pub(crate) fn callable_requirements(&self, definition: DefId) -> &[CallableRequirement] {
        self.callables
            .get(&definition)
            .map_or(&[], |callable| callable.requirements.as_slice())
    }

    pub fn builtin_callable(&self, builtin: BuiltinMemberId) -> Option<CallableTy> {
        self.builtin_callables.get(&builtin).cloned()
    }

    /// Target type on which a method is declared. References are intentionally
    /// transparent in the type model; receiver mutability lives on the method.
    pub fn receiver_type(&self, method: DefId) -> Option<Ty> {
        self.callables
            .get(&method)
            .and_then(|template| template.receiver_ty.clone())
    }

    pub fn builtin_receiver_type(&self, builtin: BuiltinMemberId) -> Option<Ty> {
        self.builtin_receivers.get(&builtin).cloned()
    }

    pub const fn builtin_type(ty: &Ty) -> Option<BuiltinType> {
        BuiltinType::of(ty)
    }

    pub fn bounds(&self, generic: GenericParamId) -> &[TraitBound] {
        self.bounds.get(&generic).map_or(&[], Vec::as_slice)
    }

    pub fn generic_params(&self, owner: DefId) -> impl Iterator<Item = GenericParamId> + '_ {
        self.generic_params
            .get(&owner)
            .into_iter()
            .flatten()
            .map(|param| param.id)
    }

    pub fn resolve_field(&self, receiver: &Ty, name: &str) -> Option<MemberResolution> {
        unique_named(self.field_candidates(receiver), name)
    }

    pub fn resolve_method(&self, receiver: &Ty, name: &str) -> Option<MemberResolution> {
        unique_named(self.method_candidates(receiver), name)
    }

    pub fn resolve_method_in(
        &self,
        receiver: &Ty,
        name: &str,
        scope: DefId,
    ) -> Option<MemberResolution> {
        unique_named(
            self.method_candidates_with_scope(receiver, Some(scope)),
            name,
        )
    }

    pub fn resolve_associated(&self, owner: DefId, name: &str) -> Option<MemberResolution> {
        let owner_ty = self.type_template(owner)?;
        self.resolve_associated_ty(owner_ty, name)
    }

    pub fn resolve_associated_ty(&self, owner_ty: &Ty, name: &str) -> Option<MemberResolution> {
        unique_named(self.associated_candidates(owner_ty), name)
    }

    pub fn resolve_variant_field(
        &self,
        variant: DefId,
        enum_ty: &Ty,
        name: &str,
    ) -> Option<MemberResolution> {
        let variant_data = self
            .variants
            .values()
            .flatten()
            .find(|candidate| candidate.definition == variant)?;
        if !matches!(enum_ty, Ty::Named(named) if named.definition() == variant_data.enum_id) {
            return None;
        }
        let substitution = self.match_receiver(&variant_data.result_ty, enum_ty)?;
        let fields = self.variant_fields.get(&variant)?;
        let mut matches = fields.iter().filter(|field| field.name == name);
        let field = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some(MemberResolution {
            target: MemberTarget::Definition(field.definition),
            kind: MemberKind::Field,
            ty: substitution.apply(&field.ty),
            receiver: None,
            substitution,
            generic_params: Vec::new(),
            requirements: Vec::new(),
        })
    }

    /// Fields and methods for `receiver`, sorted for deterministic completion.
    pub fn instance_candidates(&self, receiver: &Ty) -> Vec<MemberCandidate> {
        let mut candidates = self.field_candidates(receiver);
        candidates.extend(self.method_candidates(receiver));
        sort_candidates(&mut candidates);
        candidates
    }

    pub fn instance_candidates_in(&self, receiver: &Ty, scope: DefId) -> Vec<MemberCandidate> {
        let mut candidates = self.field_candidates(receiver);
        candidates.extend(self.method_candidates_in(receiver, scope));
        sort_candidates(&mut candidates);
        candidates
    }

    pub fn field_candidates(&self, receiver: &Ty) -> Vec<MemberCandidate> {
        let Ty::Named(named) = receiver else {
            return Vec::new();
        };
        let Some(template) = self.type_templates.get(&named.definition()) else {
            return Vec::new();
        };
        let Some(substitution) = self.match_receiver(template, receiver) else {
            return Vec::new();
        };
        let mut candidates = self
            .fields
            .get(&named.definition())
            .into_iter()
            .flatten()
            .map(|field| MemberCandidate {
                name: field.name.clone(),
                origin: MemberOrigin::Aggregate,
                resolution: MemberResolution {
                    target: MemberTarget::Definition(field.definition),
                    kind: MemberKind::Field,
                    ty: substitution.apply(&field.ty),
                    receiver: None,
                    substitution: substitution.clone(),
                    generic_params: Vec::new(),
                    requirements: Vec::new(),
                },
            })
            .collect::<Vec<_>>();
        sort_candidates(&mut candidates);
        candidates
    }

    pub fn method_candidates(&self, receiver: &Ty) -> Vec<MemberCandidate> {
        self.method_candidates_with_scope(receiver, None)
    }

    pub fn method_candidates_in(&self, receiver: &Ty, scope: DefId) -> Vec<MemberCandidate> {
        self.method_candidates_with_scope(receiver, Some(scope))
    }

    fn method_candidates_with_scope(
        &self,
        receiver: &Ty,
        scope: Option<DefId>,
    ) -> Vec<MemberCandidate> {
        if receiver.is_unknown() {
            return Vec::new();
        }
        if let Ty::GenericParam(param) = receiver {
            return self.bound_method_candidates(param.id(), scope);
        }

        let mut candidates = self.builtin_method_candidates(receiver);
        candidates.extend(self.trait_definition_candidates(receiver, true));
        for implementation in &self.implementations {
            let Some(substitution) = self.match_implementation(implementation, receiver) else {
                continue;
            };
            if implementation.trait_id.is_some() {
                candidates.extend(self.trait_impl_candidates(implementation, &substitution, true));
            } else {
                candidates.extend(self.impl_declared_candidates(
                    implementation,
                    &substitution,
                    true,
                    MemberOrigin::InherentImpl(implementation.definition),
                ));
            }
        }
        sort_candidates(&mut candidates);
        candidates
    }

    pub fn associated_candidates(&self, owner_ty: &Ty) -> Vec<MemberCandidate> {
        if owner_ty.is_unknown() {
            return Vec::new();
        }
        let mut candidates = self.builtin_associated_candidates(owner_ty);
        if let Ty::Named(named) = owner_ty {
            let Some(template) = self.type_templates.get(&named.definition()) else {
                return candidates;
            };
            if let Some(substitution) = self.match_receiver(template, owner_ty) {
                candidates.extend(
                    self.variants
                        .get(&named.definition())
                        .into_iter()
                        .flatten()
                        .map(|variant| self.instantiate_variant(variant, &substitution)),
                );
            }
        }
        for implementation in &self.implementations {
            let Some(substitution) = self.match_associated_implementation(implementation, owner_ty)
            else {
                continue;
            };
            if implementation.trait_id.is_some() {
                candidates.extend(self.trait_impl_candidates(implementation, &substitution, false));
            } else {
                candidates.extend(self.impl_declared_candidates(
                    implementation,
                    &substitution,
                    false,
                    MemberOrigin::InherentImpl(implementation.definition),
                ));
            }
        }
        sort_candidates(&mut candidates);
        candidates
    }

    pub fn implements_trait(&self, ty: &Ty, trait_id: DefId) -> bool {
        self.implements_trait_inner(ty, trait_id, &mut BTreeSet::new())
    }

    fn collect_generic_params(&mut self) {
        for definition in self.def_map.definitions() {
            let params = match definition.signature() {
                ItemSignature::Callable(signature) => signature.generic_params(),
                ItemSignature::Aggregate(signature) => signature.generic_params(),
                ItemSignature::Impl(signature) => signature.generic_params(),
                ItemSignature::None | ItemSignature::Field(_) | ItemSignature::Variant(_) => &[],
            };
            if params.is_empty() {
                continue;
            }
            self.generic_params.insert(
                definition.id(),
                params
                    .iter()
                    .enumerate()
                    .map(|(index, param)| GenericParamDecl {
                        id: GenericParamId::new(definition.id(), index as u32),
                        name: param.name().map(str::to_string),
                    })
                    .collect(),
            );
        }
    }

    fn collect_bounds(&mut self) {
        let definitions = self
            .def_map
            .definitions()
            .map(|definition| definition.id())
            .collect::<Vec<_>>();
        for definition_id in definitions {
            let Some(signature) = self
                .def_map
                .definition(definition_id)
                .map(|definition| definition.signature().clone())
            else {
                continue;
            };
            let (params, where_predicates) = match &signature {
                ItemSignature::Callable(signature) => {
                    (signature.generic_params(), signature.where_predicates())
                }
                ItemSignature::Aggregate(signature) => {
                    (signature.generic_params(), signature.where_predicates())
                }
                ItemSignature::Impl(signature) => {
                    (signature.generic_params(), signature.where_predicates())
                }
                ItemSignature::None | ItemSignature::Field(_) | ItemSignature::Variant(_) => {
                    continue;
                }
            };
            let declared = self
                .generic_params
                .get(&definition_id)
                .cloned()
                .unwrap_or_default();
            for (param, syntax_param) in declared.iter().zip(params) {
                for bound in syntax_param.bounds() {
                    self.record_bound(definition_id, param.id, bound);
                }
            }
            for predicate in where_predicates {
                let Ty::GenericParam(param) = self.lower_type(definition_id, predicate.target())
                else {
                    continue;
                };
                // A callable may constrain a generic declared by its owner.
                // That predicate is local to the callable and must not turn
                // into an impl-wide requirement for sibling methods.
                if param.id().owner() != definition_id {
                    continue;
                }
                for bound in predicate.bounds() {
                    self.record_bound(definition_id, param.id(), bound);
                }
            }
        }
        for bounds in self.bounds.values_mut() {
            bounds.sort();
            bounds.dedup();
        }
    }

    fn record_bound(&mut self, owner: DefId, generic: GenericParamId, bound: &TypeRef) {
        let Some(bound) = self.lower_trait_bound(owner, bound) else {
            return;
        };
        self.bounds.entry(generic).or_default().push(bound);
    }

    fn lower_trait_bound(&self, owner: DefId, bound: &TypeRef) -> Option<TraitBound> {
        let trait_ty = self.lower_type(owner, bound);
        let Ty::Named(named) = &trait_ty else {
            return None;
        };
        let trait_id = named.definition();
        self.def_map
            .definition(trait_id)
            .is_some_and(|definition| definition.kind() == DefKind::Trait)
            .then_some(TraitBound { trait_id, trait_ty })
    }

    fn collect_type_templates(&mut self) {
        let aggregates = self
            .def_map
            .definitions()
            .filter(|definition| {
                matches!(
                    definition.kind(),
                    DefKind::Struct | DefKind::Enum | DefKind::Trait
                )
            })
            .map(|definition| (definition.id(), definition.name().to_string()))
            .collect::<Vec<_>>();
        for (definition, name) in aggregates {
            let args = self
                .generic_params
                .get(&definition)
                .into_iter()
                .flatten()
                .map(generic_ty)
                .collect();
            self.type_templates
                .insert(definition, Ty::Named(NamedTy::new(definition, name, args)));
        }
    }

    fn collect_callables(&mut self) {
        let callables = self
            .def_map
            .definitions()
            .filter_map(|definition| match definition.signature() {
                ItemSignature::Callable(signature) => {
                    Some((definition.id(), definition.owner(), signature.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for (definition, owner, signature) in callables {
            let receiver_ty = signature
                .receiver()
                .and(owner)
                .and_then(|owner| self.owner_receiver_type(owner));
            let callable = self.lower_callable(definition, &signature);
            let method_generics = self
                .generic_params
                .get(&definition)
                .into_iter()
                .flatten()
                .map(|param| param.id)
                .collect::<Vec<_>>();
            let (requirements, scoped_requirements) =
                self.lower_callable_requirements(definition, &signature);
            for (target, bound) in &scoped_requirements {
                let Ty::GenericParam(param) = target else {
                    continue;
                };
                self.scoped_bounds
                    .entry(definition)
                    .or_default()
                    .entry(param.id())
                    .or_default()
                    .push(bound.clone());
            }
            self.callables.insert(
                definition,
                CallableTemplate {
                    callable,
                    receiver: signature.receiver(),
                    receiver_ty,
                    method_generics,
                    requirements,
                },
            );
        }
        for bounds in self.scoped_bounds.values_mut() {
            for bounds in bounds.values_mut() {
                bounds.sort();
                bounds.dedup();
            }
        }
    }

    fn lower_callable_requirements(
        &self,
        definition: DefId,
        signature: &CallableSignature,
    ) -> (Vec<CallableRequirement>, Vec<CallableRequirement>) {
        let mut requirements = self
            .generic_params
            .get(&definition)
            .into_iter()
            .flatten()
            .flat_map(|param| {
                self.bounds(param.id)
                    .iter()
                    .cloned()
                    .map(move |bound| (generic_ty(param), bound))
            })
            .collect::<Vec<_>>();
        let mut scoped_requirements = requirements.clone();
        for predicate in signature.where_predicates() {
            let target = self.lower_type(definition, predicate.target());
            let Ty::GenericParam(param) = &target else {
                continue;
            };
            for bound in predicate.bounds() {
                let Some(bound) = self.lower_trait_bound(definition, bound) else {
                    continue;
                };
                scoped_requirements.push((target.clone(), bound.clone()));
                if param.id().owner() == definition {
                    requirements.push((target.clone(), bound));
                }
            }
        }
        requirements.sort();
        requirements.dedup();
        scoped_requirements.sort();
        scoped_requirements.dedup();
        (requirements, scoped_requirements)
    }

    fn collect_aggregates_and_traits(&mut self) {
        let aggregates = self
            .def_map
            .definitions()
            .filter(|definition| {
                matches!(
                    definition.kind(),
                    DefKind::Struct | DefKind::Enum | DefKind::Trait
                )
            })
            .map(|definition| definition.id())
            .collect::<Vec<_>>();
        for aggregate in aggregates {
            let Some(definition) = self.def_map.definition(aggregate) else {
                continue;
            };
            match definition.kind() {
                DefKind::Struct => self.collect_struct_fields(aggregate),
                DefKind::Enum => self.collect_enum_variants(aggregate),
                DefKind::Trait => {
                    let methods = self
                        .def_map
                        .members(aggregate)
                        .filter(|member| member.kind() == DefKind::Method)
                        .map(|member| member.id())
                        .collect();
                    self.traits.insert(aggregate, TraitData { methods });
                }
                _ => {}
            }
        }
    }

    fn collect_struct_fields(&mut self, structure: DefId) {
        let fields = self
            .def_map
            .members(structure)
            .filter_map(|field| {
                let ItemSignature::Field(type_ref) = field.signature() else {
                    return None;
                };
                Some(FieldTemplate {
                    definition: field.id(),
                    name: field.name().to_string(),
                    ty: self.lower_type(field.id(), type_ref),
                })
            })
            .collect();
        self.fields.insert(structure, fields);
    }

    fn collect_enum_variants(&mut self, enumeration: DefId) {
        let Some(result_ty) = self.type_templates.get(&enumeration).cloned() else {
            return;
        };
        let declarations = self
            .def_map
            .members(enumeration)
            .map(|variant| {
                let fields = self
                    .def_map
                    .members(variant.id())
                    .cloned()
                    .collect::<Vec<_>>();
                (variant.clone(), fields)
            })
            .collect::<Vec<_>>();
        let variants = declarations
            .into_iter()
            .filter_map(|(variant, fields)| {
                let ItemSignature::Variant(signature) = variant.signature().clone() else {
                    return None;
                };
                let payload = match signature.kind() {
                    VariantKind::Unit => VariantPayload::Unit,
                    VariantKind::Tuple => VariantPayload::Tuple(
                        signature
                            .tuple_types()
                            .iter()
                            .map(|ty| self.lower_type(variant.id(), ty))
                            .collect(),
                    ),
                    VariantKind::Struct => VariantPayload::Struct,
                };
                if matches!(payload, VariantPayload::Struct) {
                    let fields = fields
                        .iter()
                        .filter_map(|field| {
                            let ItemSignature::Field(type_ref) = field.signature() else {
                                return None;
                            };
                            Some(FieldTemplate {
                                definition: field.id(),
                                name: field.name().to_string(),
                                ty: self.lower_type(field.id(), type_ref),
                            })
                        })
                        .collect();
                    self.variant_fields.insert(variant.id(), fields);
                }
                Some(VariantTemplate {
                    definition: variant.id(),
                    name: variant.name().to_string(),
                    enum_id: enumeration,
                    result_ty: result_ty.clone(),
                    payload,
                })
            })
            .collect();
        self.variants.insert(enumeration, variants);
    }

    fn collect_implementations(&mut self) {
        let implementations = self
            .def_map
            .definitions()
            .filter_map(|definition| match definition.signature() {
                ItemSignature::Impl(signature) => Some((definition.id(), signature.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();
        for (definition, signature) in implementations {
            let target_ty = self.lower_type(definition, signature.target_type());
            if target_ty.is_unknown() {
                continue;
            }
            let trait_ty = signature
                .trait_ref()
                .map(|trait_ref| self.lower_type(definition, trait_ref));
            let trait_id = trait_ty.as_ref().and_then(trait_definition);
            if signature.trait_ref().is_some() && trait_id.is_none() {
                continue;
            }
            let requirements = self
                .generic_params
                .get(&definition)
                .into_iter()
                .flatten()
                .flat_map(|param| {
                    self.bounds(param.id)
                        .iter()
                        .map(move |bound| (generic_ty(param), bound.trait_id))
                })
                .collect();
            let methods = self
                .def_map
                .members(definition)
                .filter(|method| method.kind() == DefKind::Method)
                .map(|method| method.id())
                .collect();
            self.implementations.push(ImplementationData {
                definition,
                target_ty,
                trait_ty,
                trait_id,
                methods,
                requirements,
            });
        }
        self.implementations
            .sort_by_key(|implementation| implementation.definition);
    }

    fn lower_callable(&self, definition: DefId, signature: &CallableSignature) -> CallableTy {
        CallableTy::new(
            signature
                .params()
                .iter()
                .map(|param| self.lower_type(definition, param.type_ref()))
                .collect(),
            self.lower_type(definition, signature.return_type()),
        )
        .with_target(definition)
    }

    fn owner_receiver_type(&self, owner: DefId) -> Option<Ty> {
        let definition = self.def_map.definition(owner)?;
        match definition.signature() {
            ItemSignature::Impl(signature) => Some(self.lower_type(owner, signature.target_type())),
            ItemSignature::Aggregate(_) if definition.kind() == DefKind::Trait => {
                self.type_templates.get(&owner).cloned()
            }
            _ => None,
        }
    }

    fn lowering_context(&self, owner: DefId) -> TypeLoweringContext<'_> {
        let mut chain = Vec::new();
        let mut current = Some(owner);
        while let Some(definition) = current.and_then(|id| self.def_map.definition(id)) {
            chain.push(definition.id());
            current = definition.owner();
        }
        let params = chain.into_iter().rev().flat_map(|owner| {
            self.generic_params
                .get(&owner)
                .into_iter()
                .flatten()
                .filter_map(|param| Some((param.name.clone()?, param.id)))
        });
        TypeLoweringContext::new().with_generic_params(params)
    }

    fn resolve_named_type(&self, owner: DefId, path: &str) -> Option<DefId> {
        let module = self.def_map.definition(owner)?.module_id();
        let segments = path.split("::").collect::<Vec<_>>();
        let definition =
            self.def_map
                .resolve_path(module, &segments, ResolveStrategy::LexicalUnique)?;
        matches!(
            definition.kind(),
            DefKind::Struct | DefKind::Enum | DefKind::Trait | DefKind::TypeAlias
        )
        .then_some(definition.id())
    }

    fn match_receiver(&self, template: &Ty, receiver: &Ty) -> Option<Substitution> {
        if template.is_unknown() || receiver.is_unknown() {
            return None;
        }
        let mut substitution = Substitution::new();
        bind_receiver_type(template, receiver, &mut substitution).then_some(substitution)
    }

    fn match_implementation(
        &self,
        implementation: &ImplementationData,
        receiver: &Ty,
    ) -> Option<Substitution> {
        let substitution = self.match_receiver(&implementation.target_ty, receiver)?;
        implementation
            .requirements
            .iter()
            .all(|(target, trait_id)| {
                let target = substitution.apply(target);
                !target.is_unknown() && self.implements_trait(&target, *trait_id)
            })
            .then_some(substitution)
    }

    fn match_associated_implementation(
        &self,
        implementation: &ImplementationData,
        owner_ty: &Ty,
    ) -> Option<Substitution> {
        if let Some(substitution) = self.match_implementation(implementation, owner_ty) {
            return Some(substitution);
        }
        let wildcard_owner = aggregate_template_owner(self, owner_ty)?;
        let mut substitution = Substitution::new();
        if !bind_associated_type(
            &implementation.target_ty,
            owner_ty,
            wildcard_owner,
            &mut substitution,
        ) {
            return None;
        }
        implementation
            .requirements
            .iter()
            .all(|(target, trait_id)| {
                let target = substitution.apply(target);
                !target.is_unknown() && self.implements_trait(&target, *trait_id)
            })
            .then_some(substitution)
    }

    fn trait_definition_candidates(&self, receiver: &Ty, instance: bool) -> Vec<MemberCandidate> {
        let Ty::Named(named) = receiver else {
            return Vec::new();
        };
        let Some(trait_data) = self.traits.get(&named.definition()) else {
            return Vec::new();
        };
        let Some(template) = self.type_templates.get(&named.definition()) else {
            return Vec::new();
        };
        let Some(substitution) = self.match_receiver(template, receiver) else {
            return Vec::new();
        };
        trait_data
            .methods
            .iter()
            .filter_map(|method| {
                let template = self.callables.get(method)?;
                if template.receiver.is_some() != instance {
                    return None;
                }
                Some(self.instantiate_callable(
                    *method,
                    template,
                    &substitution,
                    MemberOrigin::TraitBound(named.definition()),
                ))
            })
            .collect()
    }

    fn impl_declared_candidates(
        &self,
        implementation: &ImplementationData,
        substitution: &Substitution,
        instance: bool,
        origin: MemberOrigin,
    ) -> Vec<MemberCandidate> {
        implementation
            .methods
            .iter()
            .filter_map(|method| {
                let template = self.callables.get(method)?;
                (template.receiver.is_some() == instance)
                    .then(|| self.instantiate_callable(*method, template, substitution, origin))
            })
            .collect()
    }

    fn trait_impl_candidates(
        &self,
        implementation: &ImplementationData,
        receiver_substitution: &Substitution,
        instance: bool,
    ) -> Vec<MemberCandidate> {
        let Some(trait_id) = implementation.trait_id else {
            return Vec::new();
        };
        let mut substitution = receiver_substitution.clone();
        if let Some(trait_ty) = &implementation.trait_ty {
            let trait_ty = receiver_substitution.apply(trait_ty);
            self.bind_trait_arguments(trait_id, &trait_ty, &mut substitution);
        }

        let explicit_names = implementation
            .methods
            .iter()
            .filter_map(|method| self.def_map.definition(*method).map(|def| def.name()))
            .collect::<BTreeSet<_>>();
        let mut candidates = self.impl_declared_candidates(
            implementation,
            &substitution,
            instance,
            MemberOrigin::TraitImpl {
                implementation: implementation.definition,
                trait_id,
            },
        );
        let Some(trait_data) = self.traits.get(&trait_id) else {
            return candidates;
        };
        for method in &trait_data.methods {
            let Some(definition) = self.def_map.definition(*method) else {
                continue;
            };
            if explicit_names.contains(definition.name())
                || definition.source_kind().item_kind() != ItemSourceKind::TraitDefault
            {
                continue;
            }
            let Some(template) = self.callables.get(method) else {
                continue;
            };
            if template.receiver.is_some() != instance {
                continue;
            }
            candidates.push(self.instantiate_callable(
                *method,
                template,
                &substitution,
                MemberOrigin::TraitDefault {
                    implementation: implementation.definition,
                    trait_id,
                },
            ));
        }
        candidates
    }

    fn bound_method_candidates(
        &self,
        generic: GenericParamId,
        scope: Option<DefId>,
    ) -> Vec<MemberCandidate> {
        let mut candidates = Vec::new();
        let mut bounds = self.bounds(generic).to_vec();
        if let Some(scope) = scope
            && let Some(scoped) = self
                .scoped_bounds
                .get(&scope)
                .and_then(|bounds| bounds.get(&generic))
        {
            bounds.extend(scoped.iter().cloned());
        }
        bounds.sort();
        bounds.dedup();
        for bound in &bounds {
            let mut substitution = Substitution::new();
            self.bind_trait_arguments(bound.trait_id, &bound.trait_ty, &mut substitution);
            let Some(trait_data) = self.traits.get(&bound.trait_id) else {
                continue;
            };
            for method in &trait_data.methods {
                let Some(template) = self.callables.get(method) else {
                    continue;
                };
                if template.receiver.is_none() {
                    continue;
                }
                candidates.push(self.instantiate_callable(
                    *method,
                    template,
                    &substitution,
                    MemberOrigin::TraitBound(bound.trait_id),
                ));
            }
        }
        sort_candidates(&mut candidates);
        candidates
    }

    fn bind_trait_arguments(
        &self,
        trait_id: DefId,
        trait_ty: &Ty,
        substitution: &mut Substitution,
    ) {
        let (Some(Ty::Named(template)), Ty::Named(actual)) =
            (self.type_templates.get(&trait_id), trait_ty)
        else {
            return;
        };
        if template.definition() != actual.definition()
            || template.args().len() != actual.args().len()
        {
            return;
        }
        for (template, actual) in template.args().iter().zip(actual.args()) {
            let _ = bind_receiver_type(template, actual, substitution);
        }
    }

    fn instantiate_callable(
        &self,
        definition: DefId,
        template: &CallableTemplate,
        substitution: &Substitution,
        origin: MemberOrigin,
    ) -> MemberCandidate {
        let name = self
            .def_map
            .definition(definition)
            .map(|definition| definition.name().to_string())
            .unwrap_or_default();
        MemberCandidate {
            name,
            origin,
            resolution: MemberResolution {
                target: MemberTarget::Definition(definition),
                kind: if template.receiver.is_some() {
                    MemberKind::Method
                } else {
                    MemberKind::AssociatedFunction
                },
                ty: substitution.apply(&Ty::Function(template.callable.clone())),
                receiver: template.receiver,
                substitution: substitution.clone(),
                generic_params: template.method_generics.clone(),
                requirements: template
                    .requirements
                    .iter()
                    .map(|(target, bound)| {
                        (
                            substitution.apply(target),
                            TraitBound {
                                trait_id: bound.trait_id,
                                trait_ty: substitution.apply(&bound.trait_ty),
                            },
                        )
                    })
                    .collect(),
            },
        }
    }

    fn instantiate_variant(
        &self,
        variant: &VariantTemplate,
        substitution: &Substitution,
    ) -> MemberCandidate {
        let result_ty = substitution.apply(&variant.result_ty);
        let ty = match &variant.payload {
            VariantPayload::Unit | VariantPayload::Struct => result_ty,
            VariantPayload::Tuple(params) => Ty::Function(
                CallableTy::new(
                    params
                        .iter()
                        .map(|param| substitution.apply(param))
                        .collect(),
                    result_ty,
                )
                .with_target(variant.definition),
            ),
        };
        MemberCandidate {
            name: variant.name.clone(),
            origin: MemberOrigin::Aggregate,
            resolution: MemberResolution {
                target: MemberTarget::Definition(variant.definition),
                kind: MemberKind::Variant,
                ty,
                receiver: None,
                substitution: substitution.clone(),
                generic_params: Vec::new(),
                requirements: Vec::new(),
            },
        }
    }

    fn implements_trait_inner(
        &self,
        ty: &Ty,
        trait_id: DefId,
        visiting: &mut BTreeSet<(Ty, DefId)>,
    ) -> bool {
        if ty.is_unknown() || !visiting.insert((ty.clone(), trait_id)) {
            return false;
        }
        let result = match ty {
            Ty::GenericParam(param) => self.bounds(param.id()).iter().any(|bound| {
                bound.trait_id == trait_id
                    || self.implements_trait_inner(&bound.trait_ty, trait_id, visiting)
            }),
            _ => self.implementations.iter().any(|implementation| {
                implementation.trait_id == Some(trait_id)
                    && self
                        .match_receiver(&implementation.target_ty, ty)
                        .is_some_and(|substitution| {
                            implementation
                                .requirements
                                .iter()
                                .all(|(target, required)| {
                                    let target = substitution.apply(target);
                                    !target.is_unknown()
                                        && self.implements_trait_inner(&target, *required, visiting)
                                })
                        })
            }),
        };
        visiting.remove(&(ty.clone(), trait_id));
        result
    }

    fn install_builtins(&mut self) {
        for (id, receiver, callable) in builtin_templates() {
            self.builtin_receivers.insert(id, receiver);
            self.builtin_callables.insert(id, callable);
        }
    }

    fn builtin_method_candidates(&self, receiver: &Ty) -> Vec<MemberCandidate> {
        match receiver {
            Ty::Vec(item) => vec![
                builtin_method(
                    BuiltinMemberId::VecGet,
                    "get",
                    vec![Ty::I64],
                    Ty::Option(item.clone()),
                    ReceiverKind::SharedRef,
                ),
                builtin_method(
                    BuiltinMemberId::VecLen,
                    "len",
                    vec![],
                    Ty::I64,
                    ReceiverKind::SharedRef,
                ),
                builtin_method(
                    BuiltinMemberId::VecPop,
                    "pop",
                    vec![],
                    Ty::Option(item.clone()),
                    ReceiverKind::MutRef,
                ),
                builtin_method(
                    BuiltinMemberId::VecPush,
                    "push",
                    vec![(**item).clone()],
                    Ty::UNIT,
                    ReceiverKind::MutRef,
                ),
                builtin_method(
                    BuiltinMemberId::VecSet,
                    "set",
                    vec![Ty::I64, (**item).clone()],
                    Ty::UNIT,
                    ReceiverKind::MutRef,
                ),
            ],
            Ty::HashMap(key, value) => vec![
                builtin_method(
                    BuiltinMemberId::HashMapContainsKey,
                    "contains_key",
                    vec![(**key).clone()],
                    Ty::BOOL,
                    ReceiverKind::SharedRef,
                ),
                builtin_method(
                    BuiltinMemberId::HashMapGet,
                    "get",
                    vec![(**key).clone()],
                    Ty::Option(value.clone()),
                    ReceiverKind::SharedRef,
                ),
                builtin_method(
                    BuiltinMemberId::HashMapInsert,
                    "insert",
                    vec![(**key).clone(), (**value).clone()],
                    Ty::UNIT,
                    ReceiverKind::MutRef,
                ),
                builtin_method(
                    BuiltinMemberId::HashMapLen,
                    "len",
                    vec![],
                    Ty::I64,
                    ReceiverKind::SharedRef,
                ),
                builtin_method(
                    BuiltinMemberId::HashMapRemove,
                    "remove",
                    vec![(**key).clone()],
                    Ty::Option(value.clone()),
                    ReceiverKind::MutRef,
                ),
            ],
            Ty::Primitive(super::PrimitiveTy::String) => builtin_string_methods(),
            _ => Vec::new(),
        }
    }

    fn builtin_associated_candidates(&self, owner_ty: &Ty) -> Vec<MemberCandidate> {
        match owner_ty {
            Ty::Vec(item) => vec![builtin_associated(
                BuiltinMemberId::VecNew,
                "new",
                MemberKind::AssociatedFunction,
                CallableTy::new(vec![], Ty::Vec(item.clone())),
            )],
            Ty::HashMap(key, value) => vec![builtin_associated(
                BuiltinMemberId::HashMapNew,
                "new",
                MemberKind::AssociatedFunction,
                CallableTy::new(vec![], Ty::HashMap(key.clone(), value.clone())),
            )],
            Ty::Option(item) => vec![
                builtin_value(
                    BuiltinMemberId::OptionNone,
                    "None",
                    MemberKind::Variant,
                    Ty::Option(item.clone()),
                ),
                builtin_associated(
                    BuiltinMemberId::OptionSome,
                    "Some",
                    MemberKind::Variant,
                    CallableTy::new(vec![(**item).clone()], Ty::Option(item.clone())),
                ),
            ],
            Ty::Result(ok, error) => vec![
                builtin_associated(
                    BuiltinMemberId::ResultErr,
                    "Err",
                    MemberKind::Variant,
                    CallableTy::new(
                        vec![(**error).clone()],
                        Ty::Result(ok.clone(), error.clone()),
                    ),
                ),
                builtin_associated(
                    BuiltinMemberId::ResultOk,
                    "Ok",
                    MemberKind::Variant,
                    CallableTy::new(vec![(**ok).clone()], Ty::Result(ok.clone(), error.clone())),
                ),
            ],
            _ => Vec::new(),
        }
    }
}

fn generic_ty(param: &GenericParamDecl) -> Ty {
    Ty::GenericParam(super::GenericParamTy::new(
        param.id,
        param.name.clone().unwrap_or_else(|| "_".to_string()),
    ))
}

/// Receiver matching is identity-sensitive. Inference's general `unify`
/// deliberately treats numeric types as compatible, which is correct for
/// diagnostics but would make an `i64` impl spuriously apply to `f64`.
fn bind_receiver_type(template: &Ty, actual: &Ty, substitution: &mut Substitution) -> bool {
    if let Ty::GenericParam(param) = template {
        if actual.is_unknown() {
            return true;
        }
        if matches!(actual, Ty::GenericParam(actual) if actual.id() == param.id()) {
            return true;
        }
        if let Some(bound) = substitution.get(param.id()) {
            return bound == actual;
        }
        substitution.insert(param.id(), actual.clone());
        return true;
    }
    match (template, actual) {
        (Ty::Unknown, _) | (_, Ty::Unknown) => false,
        (Ty::Primitive(left), Ty::Primitive(right)) => left == right,
        (Ty::Named(left), Ty::Named(right)) => {
            left.definition() == right.definition()
                && bind_receiver_slices(left.args(), right.args(), substitution)
        }
        (Ty::Tuple(left), Ty::Tuple(right)) => bind_receiver_slices(left, right, substitution),
        (Ty::Function(left), Ty::Function(right))
        | (Ty::Function(left), Ty::Closure(right))
        | (Ty::Closure(left), Ty::Function(right))
        | (Ty::Closure(left), Ty::Closure(right)) => {
            bind_receiver_slices(left.params(), right.params(), substitution)
                && bind_receiver_type(left.return_ty(), right.return_ty(), substitution)
        }
        (Ty::Vec(left), Ty::Vec(right))
        | (Ty::Option(left), Ty::Option(right))
        | (Ty::Iterator(left), Ty::Iterator(right)) => {
            bind_receiver_type(left, right, substitution)
        }
        (Ty::HashMap(left_key, left_value), Ty::HashMap(right_key, right_value))
        | (Ty::Result(left_key, left_value), Ty::Result(right_key, right_value)) => {
            bind_receiver_type(left_key, right_key, substitution)
                && bind_receiver_type(left_value, right_value, substitution)
        }
        (Ty::Never, Ty::Never) => true,
        _ => false,
    }
}

fn bind_receiver_slices(templates: &[Ty], actuals: &[Ty], substitution: &mut Substitution) -> bool {
    templates.len() == actuals.len()
        && templates
            .iter()
            .zip(actuals)
            .all(|(template, actual)| bind_receiver_type(template, actual, substitution))
}

fn aggregate_template_owner(index: &MemberIndex, owner_ty: &Ty) -> Option<DefId> {
    let Ty::Named(named) = owner_ty else {
        return None;
    };
    (index.type_template(named.definition()) == Some(owner_ty)
        && named.args().iter().all(
            |arg| matches!(arg, Ty::GenericParam(param) if param.id().owner() == named.definition()),
        ))
    .then_some(named.definition())
}

fn bind_associated_type(
    template: &Ty,
    actual: &Ty,
    wildcard_owner: DefId,
    substitution: &mut Substitution,
) -> bool {
    if matches!(actual, Ty::GenericParam(param) if param.id().owner() == wildcard_owner) {
        return true;
    }
    match (template, actual) {
        (Ty::Named(left), Ty::Named(right)) => {
            left.definition() == right.definition()
                && left.args().len() == right.args().len()
                && left.args().iter().zip(right.args()).all(|(left, right)| {
                    bind_associated_type(left, right, wildcard_owner, substitution)
                })
        }
        (Ty::Tuple(left), Ty::Tuple(right)) => {
            left.len() == right.len()
                && left.iter().zip(right).all(|(left, right)| {
                    bind_associated_type(left, right, wildcard_owner, substitution)
                })
        }
        (Ty::Vec(left), Ty::Vec(right))
        | (Ty::Option(left), Ty::Option(right))
        | (Ty::Iterator(left), Ty::Iterator(right)) => {
            bind_associated_type(left, right, wildcard_owner, substitution)
        }
        (Ty::HashMap(left_key, left_value), Ty::HashMap(right_key, right_value))
        | (Ty::Result(left_key, left_value), Ty::Result(right_key, right_value)) => {
            bind_associated_type(left_key, right_key, wildcard_owner, substitution)
                && bind_associated_type(left_value, right_value, wildcard_owner, substitution)
        }
        _ => bind_receiver_type(template, actual, substitution),
    }
}

fn trait_definition(ty: &Ty) -> Option<DefId> {
    match ty {
        Ty::Named(named) => Some(named.definition()),
        _ => None,
    }
}

fn unique_named(candidates: Vec<MemberCandidate>, name: &str) -> Option<MemberResolution> {
    let mut matches = candidates
        .into_iter()
        .filter(|candidate| candidate.name == name);
    let candidate = matches.next()?;
    matches.next().is_none().then_some(candidate.resolution)
}

fn sort_candidates(candidates: &mut [MemberCandidate]) {
    candidates.sort_by(|left, right| {
        member_kind_rank(left.kind())
            .cmp(&member_kind_rank(right.kind()))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.target().cmp(&right.target()))
            .then_with(|| left.origin.cmp(&right.origin))
    });
}

const fn member_kind_rank(kind: MemberKind) -> u8 {
    match kind {
        MemberKind::Field => 0,
        MemberKind::Method => 1,
        MemberKind::AssociatedFunction => 2,
        MemberKind::Variant => 3,
    }
}

fn builtin_method(
    id: BuiltinMemberId,
    name: &str,
    params: Vec<Ty>,
    return_ty: Ty,
    receiver: ReceiverKind,
) -> MemberCandidate {
    MemberCandidate {
        name: name.to_string(),
        origin: MemberOrigin::Builtin,
        resolution: MemberResolution {
            target: MemberTarget::Builtin(id),
            kind: MemberKind::Method,
            ty: Ty::Function(CallableTy::new(params, return_ty)),
            receiver: Some(receiver),
            substitution: Substitution::new(),
            generic_params: Vec::new(),
            requirements: Vec::new(),
        },
    }
}

fn builtin_associated(
    id: BuiltinMemberId,
    name: &str,
    kind: MemberKind,
    callable: CallableTy,
) -> MemberCandidate {
    MemberCandidate {
        name: name.to_string(),
        origin: MemberOrigin::Builtin,
        resolution: MemberResolution {
            target: MemberTarget::Builtin(id),
            kind,
            ty: Ty::Function(callable),
            receiver: None,
            substitution: Substitution::new(),
            generic_params: Vec::new(),
            requirements: Vec::new(),
        },
    }
}

fn builtin_value(id: BuiltinMemberId, name: &str, kind: MemberKind, ty: Ty) -> MemberCandidate {
    MemberCandidate {
        name: name.to_string(),
        origin: MemberOrigin::Builtin,
        resolution: MemberResolution {
            target: MemberTarget::Builtin(id),
            kind,
            ty,
            receiver: None,
            substitution: Substitution::new(),
            generic_params: Vec::new(),
            requirements: Vec::new(),
        },
    }
}

fn builtin_string_methods() -> Vec<MemberCandidate> {
    use BuiltinMemberId as B;
    vec![
        builtin_method(
            B::StringChars,
            "chars",
            vec![],
            Ty::Vec(Box::new(Ty::STRING)),
            ReceiverKind::SharedRef,
        ),
        builtin_method(
            B::StringClone,
            "clone",
            vec![],
            Ty::STRING,
            ReceiverKind::SharedRef,
        ),
        builtin_method(
            B::StringContains,
            "contains",
            vec![Ty::STRING],
            Ty::BOOL,
            ReceiverKind::SharedRef,
        ),
        builtin_method(
            B::StringEndsWith,
            "ends_with",
            vec![Ty::STRING],
            Ty::BOOL,
            ReceiverKind::SharedRef,
        ),
        builtin_method(
            B::StringIsEmpty,
            "is_empty",
            vec![],
            Ty::BOOL,
            ReceiverKind::SharedRef,
        ),
        builtin_method(
            B::StringLen,
            "len",
            vec![],
            Ty::I64,
            ReceiverKind::SharedRef,
        ),
        builtin_method(
            B::StringRepeat,
            "repeat",
            vec![Ty::I64],
            Ty::STRING,
            ReceiverKind::SharedRef,
        ),
        builtin_method(
            B::StringReplace,
            "replace",
            vec![Ty::STRING, Ty::STRING],
            Ty::STRING,
            ReceiverKind::SharedRef,
        ),
        builtin_method(
            B::StringSplit,
            "split",
            vec![Ty::STRING],
            Ty::Vec(Box::new(Ty::STRING)),
            ReceiverKind::SharedRef,
        ),
        builtin_method(
            B::StringStartsWith,
            "starts_with",
            vec![Ty::STRING],
            Ty::BOOL,
            ReceiverKind::SharedRef,
        ),
        builtin_method(
            B::StringToLowercase,
            "to_lowercase",
            vec![],
            Ty::STRING,
            ReceiverKind::SharedRef,
        ),
        builtin_method(
            B::StringToOwned,
            "to_owned",
            vec![],
            Ty::STRING,
            ReceiverKind::SharedRef,
        ),
        builtin_method(
            B::StringToString,
            "to_string",
            vec![],
            Ty::STRING,
            ReceiverKind::SharedRef,
        ),
        builtin_method(
            B::StringToUppercase,
            "to_uppercase",
            vec![],
            Ty::STRING,
            ReceiverKind::SharedRef,
        ),
        builtin_method(
            B::StringTrim,
            "trim",
            vec![],
            Ty::STRING,
            ReceiverKind::SharedRef,
        ),
        builtin_method(
            B::StringTrimEnd,
            "trim_end",
            vec![],
            Ty::STRING,
            ReceiverKind::SharedRef,
        ),
        builtin_method(
            B::StringTrimStart,
            "trim_start",
            vec![],
            Ty::STRING,
            ReceiverKind::SharedRef,
        ),
    ]
}

fn builtin_templates() -> Vec<(BuiltinMemberId, Ty, CallableTy)> {
    use BuiltinMemberId as B;
    let unknown = Ty::Unknown;
    let vec_ty = Ty::Vec(Box::new(unknown.clone()));
    let map_ty = Ty::HashMap(Box::new(unknown.clone()), Box::new(unknown.clone()));
    let mut templates = vec![
        (
            B::VecNew,
            vec_ty.clone(),
            CallableTy::new(vec![], vec_ty.clone()),
        ),
        (
            B::VecGet,
            vec_ty.clone(),
            CallableTy::new(vec![Ty::I64], Ty::Option(Box::new(unknown.clone()))),
        ),
        (B::VecLen, vec_ty.clone(), CallableTy::new(vec![], Ty::I64)),
        (
            B::VecPop,
            vec_ty.clone(),
            CallableTy::new(vec![], Ty::Option(Box::new(unknown.clone()))),
        ),
        (
            B::VecPush,
            vec_ty.clone(),
            CallableTy::new(vec![unknown.clone()], Ty::UNIT),
        ),
        (
            B::VecSet,
            vec_ty,
            CallableTy::new(vec![Ty::I64, unknown.clone()], Ty::UNIT),
        ),
        (
            B::HashMapNew,
            map_ty.clone(),
            CallableTy::new(vec![], map_ty.clone()),
        ),
        (
            B::HashMapContainsKey,
            map_ty.clone(),
            CallableTy::new(vec![unknown.clone()], Ty::BOOL),
        ),
        (
            B::HashMapGet,
            map_ty.clone(),
            CallableTy::new(vec![unknown.clone()], Ty::Option(Box::new(unknown.clone()))),
        ),
        (
            B::HashMapInsert,
            map_ty.clone(),
            CallableTy::new(vec![unknown.clone(), unknown.clone()], Ty::UNIT),
        ),
        (
            B::HashMapLen,
            map_ty.clone(),
            CallableTy::new(vec![], Ty::I64),
        ),
        (
            B::HashMapRemove,
            map_ty,
            CallableTy::new(vec![unknown.clone()], Ty::Option(Box::new(unknown.clone()))),
        ),
        (
            B::OptionSome,
            Ty::Option(Box::new(unknown.clone())),
            CallableTy::new(vec![unknown.clone()], Ty::Option(Box::new(unknown.clone()))),
        ),
        (
            B::ResultOk,
            Ty::Result(Box::new(unknown.clone()), Box::new(unknown.clone())),
            CallableTy::new(
                vec![unknown.clone()],
                Ty::Result(Box::new(unknown.clone()), Box::new(unknown.clone())),
            ),
        ),
        (
            B::ResultErr,
            Ty::Result(Box::new(unknown.clone()), Box::new(unknown.clone())),
            CallableTy::new(
                vec![unknown.clone()],
                Ty::Result(Box::new(unknown.clone()), Box::new(unknown.clone())),
            ),
        ),
    ];
    for candidate in builtin_string_methods() {
        let MemberTarget::Builtin(id) = candidate.target() else {
            continue;
        };
        if let Some(callable) = candidate.resolution.callable().cloned() {
            templates.push((id, Ty::STRING, callable));
        }
    }
    templates
}
