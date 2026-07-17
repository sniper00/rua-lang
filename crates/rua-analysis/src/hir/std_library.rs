//! Declaration-derived standard-library member index.

use std::{collections::BTreeMap, sync::OnceLock};

use rua_core::StdSymbolId;

use super::{
    CallableTy, DefId, GenericParamId, ItemKind, ItemSignature, ItemTree, ReceiverKind, Ty,
    TypeLoweringContext, VariantKind,
};
use crate::base::TextRange;

static STANDARD_LIBRARY: OnceLock<Result<StdLibraryIndex, String>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StdMemberKind {
    Method,
    AssociatedFunction,
    Variant,
}

#[derive(Clone, Debug)]
pub struct StdType {
    name: String,
    kind: ItemKind,
    generic_params: Vec<String>,
    source_path: String,
    name_range: TextRange,
    documentation: Option<String>,
}

impl StdType {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn kind(&self) -> ItemKind {
        self.kind
    }

    pub fn generic_params(&self) -> &[String] {
        &self.generic_params
    }

    pub fn source_path(&self) -> &str {
        &self.source_path
    }

    pub const fn name_range(&self) -> TextRange {
        self.name_range
    }

    pub fn documentation(&self) -> Option<&str> {
        self.documentation.as_deref()
    }
}

#[derive(Clone, Debug)]
pub struct StdMember {
    id: StdSymbolId,
    owner: String,
    owner_generics: Vec<String>,
    name: String,
    kind: StdMemberKind,
    receiver: Option<ReceiverKind>,
    method_generics: Vec<String>,
    params: Vec<String>,
    return_type: Option<String>,
    variant_kind: Option<VariantKind>,
    source_path: String,
    name_range: TextRange,
    documentation: Option<String>,
}

#[derive(Clone, Debug)]
pub struct StdFunction {
    name: String,
    generic_params: Vec<String>,
    params: Vec<(Option<String>, String)>,
    return_type: Option<String>,
    variadic: bool,
    source_path: String,
    name_range: TextRange,
    documentation: Option<String>,
}

impl StdFunction {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn params(&self) -> &[(Option<String>, String)] {
        &self.params
    }

    pub fn generic_params(&self) -> &[String] {
        &self.generic_params
    }

    pub fn return_type(&self) -> Option<&str> {
        self.return_type.as_deref()
    }

    pub const fn is_variadic(&self) -> bool {
        self.variadic
    }

    pub fn source_path(&self) -> &str {
        &self.source_path
    }

    pub const fn name_range(&self) -> TextRange {
        self.name_range
    }

    pub fn documentation(&self) -> Option<&str> {
        self.documentation.as_deref()
    }

    pub fn callable(&self) -> CallableTy {
        // Standard functions are not owned by a project DefMap. The synthetic
        // owner only scopes parameter identities inside this callable; each
        // call gets an independent substitution.
        let generic_owner = DefId::from_index(u32::MAX);
        let lowering = TypeLoweringContext::new().with_generic_params(
            self.generic_params.iter().enumerate().map(|(index, name)| {
                (
                    name.as_str(),
                    GenericParamId::new(generic_owner, index as u32),
                )
            }),
        );
        CallableTy::new(
            self.params
                .iter()
                .map(|(_, ty)| lowering.lower_syntax(ty))
                .collect(),
            self.return_type
                .as_deref()
                .map(|ty| lowering.lower_syntax(ty))
                .unwrap_or(Ty::UNIT),
        )
    }

    pub fn signature(&self) -> String {
        let mut params = self
            .params
            .iter()
            .map(|(name, ty)| match name {
                Some(name) => format!("{name}: {ty}"),
                None => ty.clone(),
            })
            .collect::<Vec<_>>();
        if self.variadic {
            params.push("...".to_string());
        }
        let return_type = self
            .return_type
            .as_deref()
            .map(|ty| format!(" -> {ty}"))
            .unwrap_or_default();
        let generics = if self.generic_params.is_empty() {
            String::new()
        } else {
            format!("<{}>", self.generic_params.join(", "))
        };
        format!(
            "fn {}{generics}({}){return_type}",
            self.name,
            params.join(", ")
        )
    }
}

impl StdMember {
    pub const fn id(&self) -> StdSymbolId {
        self.id
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn kind(&self) -> StdMemberKind {
        self.kind
    }

    pub const fn receiver(&self) -> Option<ReceiverKind> {
        self.receiver
    }

    pub fn source_path(&self) -> &str {
        &self.source_path
    }

    pub const fn name_range(&self) -> TextRange {
        self.name_range
    }

    pub fn documentation(&self) -> Option<&str> {
        self.documentation.as_deref()
    }

    pub fn instantiate(&self, owner_ty: &Ty) -> Option<Ty> {
        let (_, owner_args) = std_owner(owner_ty)?;
        let aliases = self.owner_generics.iter().cloned().zip(owner_args).chain(
            self.method_generics
                .iter()
                .cloned()
                .map(|name| (name, Ty::Unknown)),
        );
        let lowering = TypeLoweringContext::new().with_type_aliases(aliases);
        match (self.kind, self.variant_kind) {
            (StdMemberKind::Variant, Some(VariantKind::Unit)) => Some(owner_ty.clone()),
            (StdMemberKind::Variant, _) => Some(Ty::Function(CallableTy::new(
                self.params
                    .iter()
                    .map(|parameter| lowering.lower_syntax(parameter))
                    .collect(),
                owner_ty.clone(),
            ))),
            (StdMemberKind::Method | StdMemberKind::AssociatedFunction, _) => {
                Some(Ty::Function(CallableTy::new(
                    self.params
                        .iter()
                        .map(|parameter| lowering.lower_syntax(parameter))
                        .collect(),
                    self.return_type
                        .as_deref()
                        .map(|return_type| lowering.lower_syntax(return_type))
                        .unwrap_or(Ty::UNIT),
                )))
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct StdLibraryIndex {
    types: Vec<StdType>,
    members: Vec<StdMember>,
    functions: Vec<StdFunction>,
    by_id: BTreeMap<StdSymbolId, usize>,
}

impl StdLibraryIndex {
    pub fn types(&self) -> impl ExactSizeIterator<Item = &StdType> {
        self.types.iter()
    }

    pub fn member(&self, id: StdSymbolId) -> Option<&StdMember> {
        self.by_id.get(&id).map(|index| &self.members[*index])
    }

    pub fn type_named(&self, name: &str) -> Option<&StdType> {
        self.types
            .binary_search_by(|standard_type| standard_type.name.as_str().cmp(name))
            .ok()
            .map(|index| &self.types[index])
    }

    pub fn functions(&self) -> impl ExactSizeIterator<Item = &StdFunction> {
        self.functions.iter()
    }

    pub fn function_named(&self, name: &str) -> Option<&StdFunction> {
        self.functions
            .binary_search_by(|function| function.name.as_str().cmp(name))
            .ok()
            .map(|index| &self.functions[index])
    }

    pub fn members_for<'a>(&'a self, owner_ty: &Ty) -> impl Iterator<Item = &'a StdMember> + 'a {
        let owner = std_owner(owner_ty).map(|(owner, _)| owner);
        self.members
            .iter()
            .filter(move |member| owner.is_some_and(|owner| member.owner == owner))
    }
}

pub fn standard_library() -> Result<&'static StdLibraryIndex, &'static str> {
    STANDARD_LIBRARY
        .get_or_init(build_standard_library)
        .as_ref()
        .map_err(String::as_str)
}

fn build_standard_library() -> Result<StdLibraryIndex, String> {
    let library = rua_resources::embedded_std().map_err(ToString::to_string)?;
    StdLibraryIndex::build(library)
}

impl StdLibraryIndex {
    pub fn build(library: &rua_resources::StdLibrary) -> Result<Self, String> {
        let mut types = Vec::new();
        let mut members = Vec::new();
        let mut functions = Vec::new();

        for source in library.declarations() {
            let parse = rua_syntax::parse(source.text());
            if !parse.errors().is_empty() {
                return Err(format!(
                    "standard declaration `{}` has syntax errors: {:?}",
                    source.path(),
                    parse.errors()
                ));
            }
            let tree = ItemTree::lower(parse.tree());
            let owners = tree
                .items()
                .iter()
                .filter_map(|item| match (item.kind(), item.signature()) {
                    (ItemKind::Struct | ItemKind::Enum, ItemSignature::Aggregate(signature)) => {
                        Some((
                            item.name().to_string(),
                            signature
                                .generic_params()
                                .iter()
                                .filter_map(|parameter| parameter.name().map(str::to_string))
                                .collect::<Vec<_>>(),
                        ))
                    }
                    _ => None,
                })
                .collect::<BTreeMap<_, _>>();

            for item in tree.items() {
                if matches!(item.kind(), ItemKind::Struct | ItemKind::Enum)
                    && let Some(generic_params) = owners.get(item.name())
                {
                    types.push(StdType {
                        name: item.name().to_string(),
                        kind: item.kind(),
                        generic_params: generic_params.clone(),
                        source_path: source.path().to_string(),
                        name_range: item.name_range(),
                        documentation: item.documentation().map(str::to_string),
                    });
                }
            }

            for item in tree.items() {
                match (item.kind(), item.signature()) {
                    (ItemKind::ExternFunction, ItemSignature::Callable(signature)) => {
                        functions.push(StdFunction {
                            name: item.name().to_string(),
                            generic_params: signature
                                .generic_params()
                                .iter()
                                .filter_map(|parameter| parameter.name().map(str::to_string))
                                .collect(),
                            params: signature
                                .params()
                                .iter()
                                .filter_map(|parameter| {
                                    Some((
                                        parameter.name().map(str::to_string),
                                        parameter.type_ref().syntax()?.to_string(),
                                    ))
                                })
                                .collect(),
                            return_type: signature.return_type().syntax().map(str::to_string),
                            variadic: signature.is_variadic(),
                            source_path: source.path().to_string(),
                            name_range: item.name_range(),
                            documentation: item.documentation().map(str::to_string),
                        });
                    }
                    (ItemKind::Enum, ItemSignature::Aggregate(_)) => {
                        let owner_generics = owners.get(item.name()).cloned().unwrap_or_default();
                        for variant in item.children() {
                            let ItemSignature::Variant(signature) = variant.signature() else {
                                continue;
                            };
                            members.push(std_member(
                                source.path(),
                                item.name(),
                                owner_generics.clone(),
                                variant.name(),
                                StdMemberKind::Variant,
                                None,
                                Vec::new(),
                                signature
                                    .tuple_types()
                                    .iter()
                                    .filter_map(|ty| ty.syntax().map(str::to_string))
                                    .collect(),
                                None,
                                Some(signature.kind()),
                                variant.name_range(),
                                variant.documentation(),
                            ));
                        }
                    }
                    (ItemKind::Impl, ItemSignature::Impl(signature)) => {
                        let Some(target) = signature.target_type().syntax() else {
                            continue;
                        };
                        let owner = target
                            .split(['<', ':'])
                            .find(|segment| !segment.is_empty())
                            .unwrap_or(target);
                        let owner_generics = owners.get(owner).cloned().unwrap_or_default();
                        for method in item.children() {
                            let ItemSignature::Callable(signature) = method.signature() else {
                                continue;
                            };
                            let kind = if signature.receiver().is_some() {
                                StdMemberKind::Method
                            } else {
                                StdMemberKind::AssociatedFunction
                            };
                            members.push(std_member(
                                source.path(),
                                owner,
                                owner_generics.clone(),
                                method.name(),
                                kind,
                                signature.receiver(),
                                signature
                                    .generic_params()
                                    .iter()
                                    .filter_map(|parameter| parameter.name().map(str::to_string))
                                    .collect(),
                                signature
                                    .params()
                                    .iter()
                                    .filter_map(|parameter| {
                                        parameter.type_ref().syntax().map(str::to_string)
                                    })
                                    .collect(),
                                signature.return_type().syntax().map(str::to_string),
                                None,
                                method.name_range(),
                                method.documentation(),
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }

        let mut by_id = BTreeMap::new();
        for (index, member) in members.iter().enumerate() {
            if let Some(previous) = by_id.insert(member.id, index) {
                return Err(format!(
                    "standard symbol hash collision between `{}::{}` and `{}::{}`",
                    members[previous].owner, members[previous].name, member.owner, member.name
                ));
            }
        }
        types.sort_by(|left, right| left.name.cmp(&right.name));
        functions.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(StdLibraryIndex {
            types,
            members,
            functions,
            by_id,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn std_member(
    source_path: &str,
    owner: &str,
    owner_generics: Vec<String>,
    name: &str,
    kind: StdMemberKind,
    receiver: Option<ReceiverKind>,
    method_generics: Vec<String>,
    params: Vec<String>,
    return_type: Option<String>,
    variant_kind: Option<VariantKind>,
    name_range: TextRange,
    documentation: Option<&str>,
) -> StdMember {
    StdMember {
        id: StdSymbolId::new(&format!("{source_path}::{owner}::{name}")),
        owner: owner.to_string(),
        owner_generics,
        name: name.to_string(),
        kind,
        receiver,
        method_generics,
        params,
        return_type,
        variant_kind,
        source_path: source_path.to_string(),
        name_range,
        documentation: documentation.map(str::to_string),
    }
}

fn std_owner(ty: &Ty) -> Option<(&'static str, Vec<Ty>)> {
    match ty {
        Ty::Vec(item) => Some(("Vec", vec![(**item).clone()])),
        Ty::HashMap(key, value) => Some(("HashMap", vec![(**key).clone(), (**value).clone()])),
        Ty::Primitive(super::PrimitiveTy::String) => Some(("String", Vec::new())),
        Ty::Option(item) => Some(("Option", vec![(**item).clone()])),
        Ty::Result(ok, error) => Some(("Result", vec![(**ok).clone(), (**error).clone()])),
        Ty::Iterator(item) => Some(("Iter", vec![(**item).clone()])),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_members_come_from_ruai_declarations() {
        let library = standard_library().expect("standard member index");
        let names = library
            .members_for(&Ty::Option(Box::new(Ty::I64)))
            .map(StdMember::name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "Some",
                "None",
                "map",
                "unwrap",
                "expect",
                "unwrap_or",
                "is_some",
                "is_none"
            ]
        );

        let names = library
            .members_for(&Ty::Result(Box::new(Ty::I64), Box::new(Ty::STRING)))
            .map(StdMember::name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "Ok",
                "Err",
                "map",
                "unwrap",
                "expect",
                "unwrap_or",
                "is_ok",
                "is_err"
            ]
        );

        let print = library.function_named("print").expect("standard print");
        assert!(print.is_variadic());
        assert_eq!(print.callable().return_ty(), &Ty::UNIT);
        assert_eq!(print.source_path(), "fmt.ruai");
        let panic = library.function_named("panic").expect("standard panic");
        assert_eq!(panic.callable().return_ty(), &Ty::Never);

        let assert_eq = library
            .function_named("assert_eq")
            .expect("standard generic assertion");
        assert_eq!(assert_eq.generic_params(), ["T"]);
        assert_eq!(
            assert_eq.signature(),
            "fn assert_eq<T>(left: T, right: T) -> ()"
        );
        let callable = assert_eq.callable();
        assert_eq!(callable.params().len(), 2);
        assert_eq!(callable.params()[0], callable.params()[1]);
        assert!(matches!(callable.params()[0], Ty::GenericParam(_)));
    }
}
