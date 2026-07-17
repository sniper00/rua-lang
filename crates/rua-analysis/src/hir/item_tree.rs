//! Per-file declaration summaries used by name resolution and semantic queries.

use std::hash::{Hash, Hasher};

use rua_core::{CfgOptions, expand_cfg_attributes};
use rua_syntax::{
    AstNode, Named, SyntaxKind, SyntaxNode, SyntaxToken,
    ast::{
        AnnotationDecl, EnumDecl, EnumVariant, ExternFn, FnDecl, GenericParams, HasAttributes,
        ImplDecl, Item, Param, ReceiverKind as AstReceiverKind, SourceFile, StructDecl, TraitDecl,
        TraitMethod, Type, VariantKind as AstVariantKind, WhereClause,
    },
};

use crate::base::TextRange;
use crate::vfs::FileKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ItemKind {
    Annotation,
    Function,
    Struct,
    Field,
    Enum,
    Variant,
    Trait,
    Impl,
    Method,
    ExternFunction,
    Module,
    /// Reserved for `type Name = ...` once both language parsers accept it.
    TypeAlias,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Visibility {
    Private,
    Public,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ItemSourceKind {
    Definition,
    TraitSignature,
    TraitDefault,
    ImplMethod,
    Extern,
    SyntheticFileChunk,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReceiverKind {
    Value,
    SharedRef,
    MutRef,
}

impl From<AstReceiverKind> for ReceiverKind {
    fn from(value: AstReceiverKind) -> Self {
        match value {
            AstReceiverKind::Value => Self::Value,
            AstReceiverKind::SharedRef => Self::SharedRef,
            AstReceiverKind::MutRef => Self::MutRef,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VariantKind {
    Unit,
    Tuple,
    Struct,
}

impl From<AstVariantKind> for VariantKind {
    fn from(value: AstVariantKind) -> Self {
        match value {
            AstVariantKind::Unit => Self::Unit,
            AstVariantKind::Tuple => Self::Tuple,
            AstVariantKind::Struct => Self::Struct,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SignatureSyntax {
    display: String,
    token_key: String,
}

impl SignatureSyntax {
    fn from_tokens<'a>(tokens: impl IntoIterator<Item = &'a SyntaxToken>) -> Self {
        let mut display = String::new();
        let mut token_key = String::new();
        let mut previous = None;
        for token in tokens {
            if previous.is_some_and(|kind| word_like(kind) && word_like(token.kind())) {
                display.push(' ');
            }
            display.push_str(token.text());
            let text = token.text();
            token_key.push_str(&format!("{}:{}:{};", token.kind() as u16, text.len(), text));
            previous = Some(token.kind());
        }
        Self { display, token_key }
    }

    fn from_display(display: impl Into<String>) -> Self {
        let display = display.into();
        Self {
            token_key: format!("display:{}:{display}", display.len()),
            display,
        }
    }

    fn display(&self) -> &str {
        &self.display
    }
}

/// Normalized, trivia-free type syntax. Missing recovered types remain explicit.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TypeRef {
    syntax: Option<SignatureSyntax>,
}

impl TypeRef {
    pub fn syntax(&self) -> Option<&str> {
        self.syntax.as_ref().map(SignatureSyntax::display)
    }

    /// Check whether this type reference's syntax matches a bare trait
    /// name.  The syntax may be a plain name (`MyTrait`) or include
    /// generic arguments (`MyTrait<i64>`).  Accepts exact equality or
    /// a prefix match where the name is followed by `<`.  This prevents
    /// `Foo` from matching `FooBar` (unlike `contains`).
    pub fn name_matches(&self, name: &str) -> bool {
        self.syntax().is_some_and(|s| {
            s == name
                || (s.starts_with(name) && s.as_bytes().get(name.len()).is_some_and(|c| *c == b'<'))
        })
    }

    pub const fn is_missing(&self) -> bool {
        self.syntax.is_none()
    }

    fn missing() -> Self {
        Self { syntax: None }
    }

    fn from_signature(syntax: SignatureSyntax) -> Self {
        if syntax.display().is_empty() {
            Self::missing()
        } else {
            Self {
                syntax: Some(syntax),
            }
        }
    }

    fn from_display(syntax: impl Into<String>) -> Self {
        Self::from_signature(SignatureSyntax::from_display(syntax))
    }

    pub(crate) fn from_type(ty: Option<Type>) -> Self {
        ty.map(|ty| Self::from_signature(canonical_syntax(ty.syntax())))
            .unwrap_or_else(Self::missing)
    }

    fn unit_if_missing(ty: Option<Type>) -> Self {
        ty.map(|ty| Self::from_signature(canonical_syntax(ty.syntax())))
            .unwrap_or_else(|| Self::from_display("()"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GenericParamData {
    name: Option<String>,
    bounds: Vec<TypeRef>,
    declaration: SignatureSyntax,
}

impl GenericParamData {
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn declaration(&self) -> &str {
        self.declaration.display()
    }

    pub fn bounds(&self) -> &[TypeRef] {
        &self.bounds
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WherePredicateData {
    target: TypeRef,
    bounds: Vec<TypeRef>,
    declaration: SignatureSyntax,
}

impl WherePredicateData {
    pub fn declaration(&self) -> &str {
        self.declaration.display()
    }

    pub fn target(&self) -> &TypeRef {
        &self.target
    }

    pub fn bounds(&self) -> &[TypeRef] {
        &self.bounds
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ParameterData {
    name: Option<String>,
    type_ref: TypeRef,
}

impl ParameterData {
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn type_ref(&self) -> &TypeRef {
        &self.type_ref
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CallableSignature {
    generic_clause: Option<SignatureSyntax>,
    generic_params: Vec<GenericParamData>,
    receiver: Option<ReceiverKind>,
    params: Vec<ParameterData>,
    return_type: TypeRef,
    where_predicates: Vec<WherePredicateData>,
    variadic: bool,
    abi: Option<String>,
}

impl CallableSignature {
    pub fn generic_clause(&self) -> Option<&str> {
        self.generic_clause.as_ref().map(SignatureSyntax::display)
    }

    pub fn generic_params(&self) -> &[GenericParamData] {
        &self.generic_params
    }

    pub const fn receiver(&self) -> Option<ReceiverKind> {
        self.receiver
    }

    pub fn params(&self) -> &[ParameterData] {
        &self.params
    }

    pub fn return_type(&self) -> &TypeRef {
        &self.return_type
    }

    pub fn where_predicates(&self) -> &[WherePredicateData] {
        &self.where_predicates
    }

    pub const fn is_variadic(&self) -> bool {
        self.variadic
    }

    pub fn abi(&self) -> Option<&str> {
        self.abi.as_deref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AggregateSignature {
    generic_clause: Option<SignatureSyntax>,
    generic_params: Vec<GenericParamData>,
    where_predicates: Vec<WherePredicateData>,
}

impl AggregateSignature {
    pub fn generic_clause(&self) -> Option<&str> {
        self.generic_clause.as_ref().map(SignatureSyntax::display)
    }

    pub fn generic_params(&self) -> &[GenericParamData] {
        &self.generic_params
    }

    pub fn where_predicates(&self) -> &[WherePredicateData] {
        &self.where_predicates
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ImplSignature {
    generic_clause: Option<SignatureSyntax>,
    generic_params: Vec<GenericParamData>,
    trait_ref: Option<TypeRef>,
    target_type: TypeRef,
    where_predicates: Vec<WherePredicateData>,
}

impl ImplSignature {
    pub fn generic_clause(&self) -> Option<&str> {
        self.generic_clause.as_ref().map(SignatureSyntax::display)
    }

    pub fn generic_params(&self) -> &[GenericParamData] {
        &self.generic_params
    }

    pub fn trait_ref(&self) -> Option<&TypeRef> {
        self.trait_ref.as_ref()
    }

    pub fn target_type(&self) -> &TypeRef {
        &self.target_type
    }

    pub fn where_predicates(&self) -> &[WherePredicateData] {
        &self.where_predicates
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VariantSignature {
    kind: VariantKind,
    tuple_types: Vec<TypeRef>,
}

impl VariantSignature {
    pub const fn kind(&self) -> VariantKind {
        self.kind
    }

    pub fn tuple_types(&self) -> &[TypeRef] {
        &self.tuple_types
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ItemSignature {
    None,
    Callable(CallableSignature),
    Aggregate(AggregateSignature),
    Impl(ImplSignature),
    Field(TypeRef),
    Variant(VariantSignature),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SignatureFingerprint([u64; 2]);

impl SignatureFingerprint {
    pub const fn words(self) -> [u64; 2] {
        self.0
    }

    pub(crate) const fn with_file_kind(self, file_kind: FileKind) -> Self {
        let salt = match file_kind {
            FileKind::Source => [0x8f4d_0f65_6d3a_27b1, 0x1709_3c42_fab1_8e6d],
            FileKind::Declaration => [0xd02b_13aa_7c91_e845, 0x6e4f_a8d0_39bc_5217],
        };
        Self([self.0[0] ^ salt[0], self.0[1] ^ salt[1]])
    }

    fn for_item(input: ItemFingerprintInput<'_>) -> Self {
        let ItemFingerprintInput {
            name,
            kind,
            visibility,
            source_kind,
            signature,
            documentation,
            imports,
            children,
        } = input;
        let value = (
            name,
            kind,
            visibility,
            source_kind,
            signature,
            documentation,
            imports,
            children
                .iter()
                .map(|child| {
                    (
                        child.name.as_str(),
                        child.kind,
                        child.visibility,
                        child.source_kind,
                        child.signature_fingerprint,
                    )
                })
                .collect::<Vec<_>>(),
        );
        let mut first = StableHasher::new(0xcbf29ce484222325);
        value.hash(&mut first);
        let mut second = StableHasher::new(0x84222325cbf29ce4);
        value.hash(&mut second);
        Self([first.finish(), second.finish()])
    }

    fn for_tree(items: &[ItemTreeItem], imports: &[Import]) -> Self {
        let value = (
            imports,
            items
                .iter()
                .map(|item| item.signature_fingerprint)
                .collect::<Vec<_>>(),
        );
        let mut first = StableHasher::new(0xcbf29ce484222325);
        value.hash(&mut first);
        let mut second = StableHasher::new(0x84222325cbf29ce4);
        value.hash(&mut second);
        Self([first.finish(), second.finish()])
    }
}

struct ItemFingerprintInput<'a> {
    name: &'a str,
    kind: ItemKind,
    visibility: Visibility,
    source_kind: ItemSourceKind,
    signature: &'a ItemSignature,
    documentation: &'a Option<String>,
    imports: &'a [Import],
    children: &'a [ItemTreeItem],
}

struct StableHasher(u64);

impl StableHasher {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }
}

impl Hasher for StableHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Import {
    path: Vec<String>,
    alias: Option<String>,
}

impl Import {
    pub fn path(&self) -> &[String] {
        &self.path
    }

    pub fn alias(&self) -> Option<&str> {
        self.alias.as_deref()
    }

    pub fn binding_name(&self) -> Option<&str> {
        self.alias()
            .or_else(|| self.path.last().map(String::as_str))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemTreeItem {
    name: String,
    kind: ItemKind,
    range: TextRange,
    name_range: TextRange,
    visibility: Visibility,
    source_kind: ItemSourceKind,
    signature: ItemSignature,
    documentation: Option<String>,
    signature_fingerprint: SignatureFingerprint,
    children: Vec<ItemTreeItem>,
    imports: Vec<Import>,
}

struct ItemTreeItemData {
    kind: ItemKind,
    visibility: Visibility,
    source_kind: ItemSourceKind,
    signature: ItemSignature,
    documentation: Option<String>,
    children: Vec<ItemTreeItem>,
}

impl ItemTreeItem {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn kind(&self) -> ItemKind {
        self.kind
    }

    pub const fn range(&self) -> TextRange {
        self.range
    }

    pub const fn name_range(&self) -> TextRange {
        self.name_range
    }

    pub const fn visibility(&self) -> Visibility {
        self.visibility
    }

    pub const fn source_kind(&self) -> ItemSourceKind {
        self.source_kind
    }

    pub fn signature(&self) -> &ItemSignature {
        &self.signature
    }

    pub fn documentation(&self) -> Option<&str> {
        self.documentation.as_deref()
    }

    pub const fn signature_fingerprint(&self) -> SignatureFingerprint {
        self.signature_fingerprint
    }

    pub fn children(&self) -> &[ItemTreeItem] {
        &self.children
    }

    pub fn imports(&self) -> &[Import] {
        &self.imports
    }

    fn new(name: String, range: TextRange, name_range: TextRange, data: ItemTreeItemData) -> Self {
        let ItemTreeItemData {
            kind,
            visibility,
            source_kind,
            signature,
            documentation,
            children,
        } = data;
        let signature_fingerprint = SignatureFingerprint::for_item(ItemFingerprintInput {
            name: &name,
            kind,
            visibility,
            source_kind,
            signature: &signature,
            documentation: &documentation,
            imports: &[],
            children: &children,
        });
        Self {
            name,
            kind,
            range,
            name_range,
            visibility,
            source_kind,
            signature,
            documentation,
            signature_fingerprint,
            children,
            imports: Vec::new(),
        }
    }
}

/// Compact declaration-only representation of one file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ItemTree {
    items: Vec<ItemTreeItem>,
    imports: Vec<Import>,
    declaration_fingerprint: SignatureFingerprint,
}

impl ItemTree {
    pub fn lower(file: &SourceFile) -> Self {
        Self::lower_with_cfg(file, &CfgOptions::default())
    }

    pub fn lower_with_cfg(file: &SourceFile, cfg: &CfgOptions) -> Self {
        let (items, imports) = Self::lower_scope(file.items(), cfg);
        let declaration_fingerprint = SignatureFingerprint::for_tree(&items, &imports);
        Self {
            items,
            imports,
            declaration_fingerprint,
        }
    }

    pub fn items(&self) -> &[ItemTreeItem] {
        &self.items
    }

    pub fn imports(&self) -> &[Import] {
        &self.imports
    }

    pub const fn declaration_fingerprint(&self) -> SignatureFingerprint {
        self.declaration_fingerprint
    }

    fn lower_scope(
        items: impl Iterator<Item = Item>,
        cfg: &CfgOptions,
    ) -> (Vec<ItemTreeItem>, Vec<Import>) {
        let mut summaries = Vec::new();
        let mut imports = Vec::new();
        for item in items {
            if !attributes_active(item.attributes(), cfg) {
                continue;
            }
            match item {
                Item::Use(item) => imports.extend(item.imports().map(|import| {
                    Import {
                        path: import
                            .path
                            .into_iter()
                            .map(|token| token.text().to_string())
                            .collect(),
                        alias: import.alias.map(|token| token.text().to_string()),
                    }
                })),
                item => summaries.extend(Self::lower_non_module_item(item, cfg)),
            }
        }
        (summaries, imports)
    }

    fn lower_non_module_item(item: Item, cfg: &CfgOptions) -> Vec<ItemTreeItem> {
        match item {
            Item::Annotation(item) => Self::lower_annotation(&item).into_iter().collect(),
            Item::Fn(item) => {
                Self::lower_function(&item, ItemKind::Function, ItemSourceKind::Definition)
                    .into_iter()
                    .collect()
            }
            Item::Struct(item) => Self::lower_struct(&item, cfg).into_iter().collect(),
            Item::Enum(item) => Self::lower_enum(&item, cfg).into_iter().collect(),
            Item::Trait(item) => Self::lower_trait(&item, cfg).into_iter().collect(),
            Item::Impl(item) => Self::lower_impl(&item, cfg).into_iter().collect(),
            Item::Extern(block) => {
                let abi = normalized_extern_abi(block.abi());
                block
                    .fns()
                    .filter(|function| attributes_active(function.attributes(), cfg))
                    .filter_map(|function| Self::lower_extern(&function, abi.clone()))
                    .collect()
            }
            Item::Use(_) => Vec::new(),
        }
    }

    fn lower_annotation(item: &AnnotationDecl) -> Option<ItemTreeItem> {
        let signature = callable_signature(CallableSignatureInput {
            node: item.syntax(),
            generic_params_node: None,
            params: item.params().collect(),
            return_type: None,
            where_clause: None,
            receiver: None,
            variadic: false,
            abi: None,
        });
        Self::named_item(
            item,
            ItemKind::Annotation,
            visibility(item.is_pub()),
            ItemSourceKind::Definition,
            ItemSignature::Callable(signature),
            Vec::new(),
        )
    }

    fn lower_function(
        item: &FnDecl,
        kind: ItemKind,
        source_kind: ItemSourceKind,
    ) -> Option<ItemTreeItem> {
        let signature = callable_signature(CallableSignatureInput {
            node: item.syntax(),
            generic_params_node: item.generic_params(),
            params: item.params().collect(),
            return_type: item.ret_type(),
            where_clause: item.where_clause(),
            receiver: item.receiver().map(Into::into),
            variadic: false,
            abi: None,
        });
        Self::named_item(
            item,
            kind,
            visibility(item.is_pub()),
            source_kind,
            ItemSignature::Callable(signature),
            Vec::new(),
        )
    }

    fn lower_struct(item: &StructDecl, cfg: &CfgOptions) -> Option<ItemTreeItem> {
        let children = item
            .field_list()
            .into_iter()
            .flat_map(|fields| fields.fields().collect::<Vec<_>>())
            .filter(|field| attributes_active(field.attributes(), cfg))
            .filter_map(|field| {
                Self::named_item(
                    &field,
                    ItemKind::Field,
                    visibility(field.is_pub()),
                    ItemSourceKind::Definition,
                    ItemSignature::Field(TypeRef::from_type(field.ty())),
                    Vec::new(),
                )
            })
            .collect();
        Self::named_item(
            item,
            ItemKind::Struct,
            visibility(item.is_pub()),
            ItemSourceKind::Definition,
            ItemSignature::Aggregate(aggregate_signature(
                item.generic_params(),
                item.where_clause(),
            )),
            children,
        )
    }

    fn lower_enum(item: &EnumDecl, cfg: &CfgOptions) -> Option<ItemTreeItem> {
        let parent_visibility = visibility(item.is_pub());
        let children = item
            .variant_list()
            .into_iter()
            .flat_map(|variants| variants.variants().collect::<Vec<_>>())
            .filter(|variant| attributes_active(variant.attributes(), cfg))
            .filter_map(|variant| Self::lower_variant(&variant, parent_visibility, cfg))
            .collect();
        Self::named_item(
            item,
            ItemKind::Enum,
            parent_visibility,
            ItemSourceKind::Definition,
            ItemSignature::Aggregate(aggregate_signature(
                item.generic_params(),
                item.where_clause(),
            )),
            children,
        )
    }

    fn lower_variant(
        item: &EnumVariant,
        variant_visibility: Visibility,
        cfg: &CfgOptions,
    ) -> Option<ItemTreeItem> {
        let children = item
            .field_list()
            .into_iter()
            .flat_map(|fields| fields.fields().collect::<Vec<_>>())
            .filter(|field| attributes_active(field.attributes(), cfg))
            .filter_map(|field| {
                Self::named_item(
                    &field,
                    ItemKind::Field,
                    visibility(field.is_pub()),
                    ItemSourceKind::Definition,
                    ItemSignature::Field(TypeRef::from_type(field.ty())),
                    Vec::new(),
                )
            })
            .collect();
        let signature = VariantSignature {
            kind: item.variant_kind().into(),
            tuple_types: item
                .tuple_types()
                .map(|ty| TypeRef::from_type(Some(ty)))
                .collect(),
        };
        Self::named_item(
            item,
            ItemKind::Variant,
            variant_visibility,
            ItemSourceKind::Definition,
            ItemSignature::Variant(signature),
            children,
        )
    }

    fn lower_trait(item: &TraitDecl, cfg: &CfgOptions) -> Option<ItemTreeItem> {
        let children = item
            .methods()
            .filter(|method| attributes_active(method.attributes(), cfg))
            .filter_map(|method| Self::lower_trait_method(&method))
            .collect();
        Self::named_item(
            item,
            ItemKind::Trait,
            visibility(item.is_pub()),
            ItemSourceKind::Definition,
            ItemSignature::Aggregate(aggregate_signature(
                item.generic_params(),
                item.where_clause(),
            )),
            children,
        )
    }

    fn lower_trait_method(item: &TraitMethod) -> Option<ItemTreeItem> {
        let source_kind = if item.default_body().is_some() {
            ItemSourceKind::TraitDefault
        } else {
            ItemSourceKind::TraitSignature
        };
        let signature = callable_signature(CallableSignatureInput {
            node: item.syntax(),
            generic_params_node: item.generic_params(),
            params: item.params().collect(),
            return_type: item.ret_type(),
            where_clause: item.where_clause(),
            receiver: item.receiver().map(Into::into),
            variadic: false,
            abi: None,
        });
        Self::named_item(
            item,
            ItemKind::Method,
            Visibility::Public,
            source_kind,
            ItemSignature::Callable(signature),
            Vec::new(),
        )
    }

    fn lower_impl(item: &ImplDecl, cfg: &CfgOptions) -> Option<ItemTreeItem> {
        let name_token = item.type_name()?;
        let (trait_ref, target_type) = impl_header(item);
        let name = match &trait_ref {
            Some(trait_ref) => format!(
                "impl {} for {}",
                trait_ref.syntax().unwrap_or("<missing>"),
                target_type.syntax().unwrap_or("<missing>")
            ),
            None => format!("impl {}", target_type.syntax().unwrap_or("<missing>")),
        };
        let children = item
            .methods()
            .filter(|method| attributes_active(method.attributes(), cfg))
            .filter_map(|method| {
                Self::lower_function(&method, ItemKind::Method, ItemSourceKind::ImplMethod)
            })
            .collect::<Vec<_>>();
        let signature = ItemSignature::Impl(ImplSignature {
            generic_clause: item
                .generic_params()
                .as_ref()
                .map(|params| canonical_syntax(params.syntax())),
            generic_params: generic_params(item.generic_params()),
            trait_ref,
            target_type,
            where_predicates: where_predicates(item.where_clause()),
        });
        Some(ItemTreeItem::new(
            name,
            node_range(item.syntax()),
            token_range(&name_token),
            ItemTreeItemData {
                kind: ItemKind::Impl,
                visibility: Visibility::Private,
                source_kind: ItemSourceKind::Definition,
                signature,
                documentation: rua_syntax::symbols::documentation(item.syntax()),
                children,
            },
        ))
    }

    fn lower_extern(item: &ExternFn, abi: Option<String>) -> Option<ItemTreeItem> {
        let signature = callable_signature(CallableSignatureInput {
            node: item.syntax(),
            generic_params_node: None,
            params: item.params().collect(),
            return_type: item.ret_type(),
            where_clause: None,
            receiver: None,
            variadic: item.variadic(),
            abi,
        });
        Self::named_item(
            item,
            ItemKind::ExternFunction,
            visibility(item.is_pub()),
            ItemSourceKind::Extern,
            ItemSignature::Callable(signature),
            Vec::new(),
        )
    }

    fn named_item(
        item: &impl Named,
        kind: ItemKind,
        visibility: Visibility,
        source_kind: ItemSourceKind,
        signature: ItemSignature,
        children: Vec<ItemTreeItem>,
    ) -> Option<ItemTreeItem> {
        let name = item.name()?;
        Some(ItemTreeItem::new(
            name.text().to_string(),
            node_range(item.syntax()),
            token_range(&name),
            ItemTreeItemData {
                kind,
                visibility,
                source_kind,
                signature,
                documentation: rua_syntax::symbols::documentation(item.syntax()),
                children,
            },
        ))
    }
}

fn attributes_active(
    attributes: impl Iterator<Item = rua_syntax::ast::Attribute>,
    cfg: &CfgOptions,
) -> bool {
    let attributes = attributes
        .map(|attribute| attribute.to_core())
        .collect::<Result<Vec<_>, _>>();
    let Ok(attributes) = attributes else {
        return true;
    };
    expand_cfg_attributes(&attributes, cfg)
        .map(|expanded| expanded.active)
        .unwrap_or(true)
}

fn visibility(is_public: bool) -> Visibility {
    if is_public {
        Visibility::Public
    } else {
        Visibility::Private
    }
}

struct CallableSignatureInput<'a> {
    node: &'a SyntaxNode,
    generic_params_node: Option<GenericParams>,
    params: Vec<Param>,
    return_type: Option<Type>,
    where_clause: Option<WhereClause>,
    receiver: Option<ReceiverKind>,
    variadic: bool,
    abi: Option<String>,
}

fn callable_signature(input: CallableSignatureInput<'_>) -> CallableSignature {
    let CallableSignatureInput {
        node,
        generic_params_node,
        params,
        return_type,
        where_clause,
        receiver,
        variadic,
        abi,
    } = input;
    let generic_clause = generic_params_node
        .as_ref()
        .map(|params| canonical_syntax(params.syntax()))
        .or_else(|| raw_generic_clause(node));
    let mut structured_generic_params = generic_params(generic_params_node);
    if structured_generic_params.is_empty()
        && let Some(clause) = generic_clause.as_ref()
    {
        structured_generic_params = generic_params_from_clause(clause);
    }
    CallableSignature {
        generic_clause,
        generic_params: structured_generic_params,
        receiver,
        params: params
            .into_iter()
            .map(|param| ParameterData {
                name: param.name_text(),
                type_ref: TypeRef::from_type(param.ty()),
            })
            .collect(),
        return_type: TypeRef::unit_if_missing(return_type),
        where_predicates: where_predicates(where_clause),
        variadic,
        abi,
    }
}

fn aggregate_signature(
    generic_params_node: Option<GenericParams>,
    where_clause: Option<WhereClause>,
) -> AggregateSignature {
    AggregateSignature {
        generic_clause: generic_params_node
            .as_ref()
            .map(|params| canonical_syntax(params.syntax())),
        generic_params: generic_params(generic_params_node),
        where_predicates: where_predicates(where_clause),
    }
}

fn generic_params(params: Option<GenericParams>) -> Vec<GenericParamData> {
    params
        .into_iter()
        .flat_map(|params| params.params().collect::<Vec<_>>())
        .map(|param| {
            let declaration = canonical_syntax(param.syntax());
            generic_param_data(param.name_text(), declaration)
        })
        .collect()
}

fn generic_params_from_clause(clause: &SignatureSyntax) -> Vec<GenericParamData> {
    let inner = clause
        .display()
        .strip_prefix('<')
        .and_then(|clause| clause.strip_suffix('>'))
        .unwrap_or_else(|| clause.display());
    split_top_level(inner, ',')
        .into_iter()
        .filter(|declaration| !declaration.is_empty())
        .map(|declaration| {
            let name = split_constraint(declaration).0;
            generic_param_data(
                Some(name.to_string()),
                SignatureSyntax::from_display(declaration),
            )
        })
        .collect()
}

fn generic_param_data(name: Option<String>, declaration: SignatureSyntax) -> GenericParamData {
    let (_, bounds) = split_constraint(declaration.display());
    GenericParamData {
        name,
        bounds: bounds.map_or_else(Vec::new, type_bounds),
        declaration,
    }
}

fn where_predicates(clause: Option<WhereClause>) -> Vec<WherePredicateData> {
    let Some(clause) = clause else {
        return Vec::new();
    };
    let tokens = significant_tokens(clause.syntax());
    let tokens = tokens
        .into_iter()
        .skip_while(|token| !(token.kind() == SyntaxKind::Ident && token.text() == "where"))
        .skip(1);
    let mut predicates = Vec::new();
    let mut current = Vec::new();
    let mut angle_depth = 0_u32;
    for token in tokens {
        match token.kind() {
            SyntaxKind::Lt => angle_depth += 1,
            SyntaxKind::Gt => angle_depth = angle_depth.saturating_sub(1),
            SyntaxKind::Comma if angle_depth == 0 => {
                if !current.is_empty() {
                    predicates.push(where_predicate_data(SignatureSyntax::from_tokens(&current)));
                    current.clear();
                }
                continue;
            }
            _ => {}
        }
        current.push(token);
    }
    if !current.is_empty() {
        predicates.push(where_predicate_data(SignatureSyntax::from_tokens(&current)));
    }
    predicates
}

fn where_predicate_data(declaration: SignatureSyntax) -> WherePredicateData {
    let (target, bounds) = split_constraint(declaration.display());
    WherePredicateData {
        target: if target.is_empty() {
            TypeRef::missing()
        } else {
            TypeRef::from_display(target)
        },
        bounds: bounds.map_or_else(Vec::new, type_bounds),
        declaration,
    }
}

fn type_bounds(bounds: &str) -> Vec<TypeRef> {
    split_top_level(bounds, '+')
        .into_iter()
        .filter(|bound| !bound.is_empty())
        .map(TypeRef::from_display)
        .collect()
}

fn split_constraint(declaration: &str) -> (&str, Option<&str>) {
    let bytes = declaration.as_bytes();
    let mut angle_depth = 0_u32;
    for (index, byte) in bytes.iter().copied().enumerate() {
        match byte {
            b'<' => angle_depth += 1,
            b'>' => angle_depth = angle_depth.saturating_sub(1),
            b':' if angle_depth == 0
                && bytes.get(index.wrapping_sub(1)) != Some(&b':')
                && bytes.get(index + 1) != Some(&b':') =>
            {
                return (&declaration[..index], Some(&declaration[index + 1..]));
            }
            _ => {}
        }
    }
    (declaration, None)
}

fn split_top_level(text: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut angle_depth = 0_u32;
    let mut start = 0;
    for (index, character) in text.char_indices() {
        match character {
            '<' => angle_depth += 1,
            '>' => angle_depth = angle_depth.saturating_sub(1),
            character if character == separator && angle_depth == 0 => {
                parts.push(&text[start..index]);
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

fn raw_generic_clause(node: &SyntaxNode) -> Option<SignatureSyntax> {
    let tokens = significant_tokens(node);
    let function = tokens
        .iter()
        .position(|token| token.kind() == SyntaxKind::KwFn)?;
    let name = tokens
        .iter()
        .enumerate()
        .skip(function + 1)
        .find(|(_, token)| token.kind() == SyntaxKind::Ident)?
        .0;
    collect_angle_clause(&tokens, name + 1)
}

fn collect_angle_clause(tokens: &[SyntaxToken], start: usize) -> Option<SignatureSyntax> {
    if tokens.get(start)?.kind() != SyntaxKind::Lt {
        return None;
    }
    let mut depth = 0_u32;
    let mut end = start;
    for (offset, token) in tokens[start..].iter().enumerate() {
        match token.kind() {
            SyntaxKind::Lt => depth += 1,
            SyntaxKind::Gt => depth = depth.saturating_sub(1),
            _ => {}
        }
        end = start + offset + 1;
        if depth == 0 {
            return Some(SignatureSyntax::from_tokens(&tokens[start..end]));
        }
    }
    Some(SignatureSyntax::from_tokens(&tokens[start..end]))
}

fn impl_header(item: &ImplDecl) -> (Option<TypeRef>, TypeRef) {
    let tokens = significant_tokens(item.syntax());
    let mut index = tokens
        .iter()
        .position(|token| token.kind() == SyntaxKind::KwImpl)
        .map(|index| index + 1)
        .unwrap_or(0);
    if tokens
        .get(index)
        .is_some_and(|token| token.kind() == SyntaxKind::Lt)
    {
        let mut depth = 0_u32;
        while index < tokens.len() {
            match tokens[index].kind() {
                SyntaxKind::Lt => depth += 1,
                SyntaxKind::Gt => depth = depth.saturating_sub(1),
                _ => {}
            }
            index += 1;
            if depth == 0 {
                break;
            }
        }
    }

    let header_end = item
        .where_clause()
        .map(|clause| clause.syntax().text_range().start())
        .or_else(|| {
            tokens
                .iter()
                .find(|token| token.kind() == SyntaxKind::LBrace)
                .map(SyntaxToken::text_range)
                .map(|range| range.start())
        });
    let mut end = index;
    let mut for_index = None;
    let mut angle_depth = 0_u32;
    for (offset, token) in tokens[index..].iter().enumerate() {
        if header_end.is_some_and(|header_end| token.text_range().start() >= header_end) {
            break;
        }
        match token.kind() {
            SyntaxKind::Lt => angle_depth += 1,
            SyntaxKind::Gt => angle_depth = angle_depth.saturating_sub(1),
            SyntaxKind::KwFor if angle_depth == 0 => {
                for_index = Some(index + offset);
            }
            _ => {}
        }
        end = index + offset + 1;
    }
    if let Some(for_index) = for_index {
        (
            Some(TypeRef::from_signature(SignatureSyntax::from_tokens(
                &tokens[index..for_index],
            ))),
            TypeRef::from_signature(SignatureSyntax::from_tokens(&tokens[for_index + 1..end])),
        )
    } else {
        (
            None,
            TypeRef::from_signature(SignatureSyntax::from_tokens(&tokens[index..end])),
        )
    }
}

fn normalized_extern_abi(abi: Option<SyntaxToken>) -> Option<String> {
    Some(
        abi.and_then(|token| {
            let text = token.text();
            text.strip_prefix('"')
                .and_then(|text| text.strip_suffix('"'))
                .map(str::to_string)
        })
        .unwrap_or_else(|| "C".to_string()),
    )
}

fn word_like(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::KwFn
            | SyntaxKind::KwLet
            | SyntaxKind::KwMut
            | SyntaxKind::KwIf
            | SyntaxKind::KwElse
            | SyntaxKind::KwWhile
            | SyntaxKind::KwLoop
            | SyntaxKind::KwFor
            | SyntaxKind::KwIn
            | SyntaxKind::KwReturn
            | SyntaxKind::KwBreak
            | SyntaxKind::KwContinue
            | SyntaxKind::KwTrue
            | SyntaxKind::KwFalse
            | SyntaxKind::KwStruct
            | SyntaxKind::KwEnum
            | SyntaxKind::KwTrait
            | SyntaxKind::KwImpl
            | SyntaxKind::KwPub
            | SyntaxKind::KwUse
            | SyntaxKind::KwMod
            | SyntaxKind::KwAs
            | SyntaxKind::KwMatch
            | SyntaxKind::KwSelf
            | SyntaxKind::KwExtern
            | SyntaxKind::Ident
            | SyntaxKind::Int
            | SyntaxKind::Float
            | SyntaxKind::Str
    )
}

fn significant_tokens(node: &SyntaxNode) -> Vec<SyntaxToken> {
    node.descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| !token.kind().is_trivia())
        .collect()
}

fn canonical_syntax(node: &SyntaxNode) -> SignatureSyntax {
    let tokens = significant_tokens(node);
    SignatureSyntax::from_tokens(&tokens)
}

fn node_range(node: &SyntaxNode) -> TextRange {
    let range = node.text_range();
    TextRange::new(range.start().into(), range.end().into())
}

fn token_range(token: &SyntaxToken) -> TextRange {
    let range = token.text_range();
    TextRange::new(range.start().into(), range.end().into())
}

#[cfg(test)]
mod tests {
    use rua_core::CfgOptions;
    use rua_syntax::parse_source_file;

    use super::{
        ItemKind, ItemSignature, ItemSourceKind, ItemTree, ReceiverKind, VariantKind, Visibility,
    };

    #[test]
    fn item_tree_lowers_top_level_declaration_summaries() {
        let source = concat!(
            "pub fn run() { let body_local = 1; }\n",
            "struct Record { value: i64 }\n",
            "pub enum State { Ready }\n",
            "trait Service { fn call(&self); }\n",
            "extern \"lua\" { pub fn clock() -> i64; }\n",
        );
        let parse = parse_source_file(source);
        assert!(parse.errors().is_empty());

        let tree = ItemTree::lower(parse.tree());
        let summaries: Vec<_> = tree
            .items()
            .iter()
            .map(|item| (item.name(), item.kind(), item.visibility()))
            .collect();

        assert_eq!(
            summaries,
            [
                ("run", ItemKind::Function, Visibility::Public),
                ("Record", ItemKind::Struct, Visibility::Private),
                ("State", ItemKind::Enum, Visibility::Public),
                ("Service", ItemKind::Trait, Visibility::Private),
                ("clock", ItemKind::ExternFunction, Visibility::Public),
            ]
        );
        assert!(tree.items().iter().all(|item| {
            &source[item.name_range().start() as usize..item.name_range().end() as usize]
                == item.name()
        }));
        assert!(tree.items().iter().all(|item| {
            item.range().start() <= item.name_range().start()
                && item.name_range().end() <= item.range().end()
        }));
    }

    #[test]
    fn item_tree_lowers_members_and_complete_signatures() {
        let parse = parse_source_file(concat!(
            "struct Box<T: Clone> where T: Named { pub value: T }\n",
            "enum Message { Quit, Pair(i64, String), Move { x: i64 } }\n",
            "trait Read { fn read<U>(&self, value: U) -> String where U: Clone; }\n",
            "impl<T> Read for Box<T> where T: Clone { fn read<U>(&mut self, value: U) -> String { value } }\n",
            "extern \"lua\" { fn format(template: String, ...) -> String; }\n",
        ));
        assert!(parse.errors().is_empty(), "{:?}", parse.errors());
        let tree = ItemTree::lower(parse.tree());

        let structure = &tree.items()[0];
        assert_eq!(structure.children()[0].kind(), ItemKind::Field);
        let enumeration = &tree.items()[1];
        assert_eq!(enumeration.children().len(), 3);
        let ItemSignature::Variant(pair) = enumeration.children()[1].signature() else {
            panic!("tuple variant signature")
        };
        assert_eq!(pair.kind(), VariantKind::Tuple);
        assert_eq!(pair.tuple_types().len(), 2);

        let trait_method = &tree.items()[2].children()[0];
        let ItemSignature::Callable(signature) = trait_method.signature() else {
            panic!("trait method signature")
        };
        assert_eq!(signature.receiver(), Some(ReceiverKind::SharedRef));
        assert_eq!(signature.generic_clause(), Some("<U>"));
        assert_eq!(signature.where_predicates()[0].declaration(), "U:Clone");
        assert_eq!(trait_method.source_kind(), ItemSourceKind::TraitSignature);

        let implementation = &tree.items()[3];
        assert_eq!(implementation.kind(), ItemKind::Impl);
        assert_eq!(implementation.name(), "impl Read for Box<T>");
        let ItemSignature::Callable(method) = implementation.children()[0].signature() else {
            panic!("impl method signature")
        };
        assert_eq!(method.receiver(), Some(ReceiverKind::MutRef));

        let external = &tree.items()[4];
        assert_eq!(external.kind(), ItemKind::ExternFunction);
        let ItemSignature::Callable(signature) = external.signature() else {
            panic!("extern signature")
        };
        assert_eq!(signature.abi(), Some("lua"));
        assert!(signature.is_variadic());
    }

    #[test]
    fn item_signature_fingerprint_ignores_body_and_trivia() {
        let first = parse_source_file("pub fn map<T>(value: T) -> T { value }");
        let second = parse_source_file(
            "pub /* signature comment */ fn map < T > ( value : T ) -> T { let x = value; x }",
        );
        let changed = parse_source_file("pub fn map<T>(value: T) -> String { value }");
        let first = ItemTree::lower(first.tree());
        let second = ItemTree::lower(second.tree());
        let changed = ItemTree::lower(changed.tree());

        assert_eq!(
            first.items()[0].signature_fingerprint(),
            second.items()[0].signature_fingerprint()
        );
        assert_ne!(
            first.items()[0].signature_fingerprint(),
            changed.items()[0].signature_fingerprint()
        );
    }

    #[test]
    fn item_tree_attaches_only_semantic_documentation() {
        let parsed = parse_source_file(
            "// ordinary\nfn plain() {}\n/// Function docs.\nfn documented() {}\nstruct Point {\n    /// X docs.\n    x: i64,\n}\nenum Color {\n    /// Red docs.\n    Red,\n}\nimpl Point {\n    /// Method docs.\n    fn x(&self) -> i64 { self.x }\n}",
        );
        assert!(parsed.errors().is_empty(), "{:?}", parsed.errors());
        let tree = ItemTree::lower(parsed.tree());
        assert_eq!(tree.items()[0].documentation(), None);
        assert_eq!(tree.items()[1].documentation(), Some("Function docs."));
        assert_eq!(
            tree.items()[2].children()[0].documentation(),
            Some("X docs.")
        );
        assert_eq!(
            tree.items()[3].children()[0].documentation(),
            Some("Red docs.")
        );
        assert_eq!(
            tree.items()[4].children()[0].documentation(),
            Some("Method docs.")
        );
        let changed = parse_source_file("/// Changed docs.\nfn documented() {}");
        let unchanged = parse_source_file("/// Function docs.\nfn documented() {}");
        assert_ne!(
            ItemTree::lower(changed.tree()).items()[0].signature_fingerprint(),
            ItemTree::lower(unchanged.tree()).items()[0].signature_fingerprint()
        );
    }

    #[test]
    fn item_signature_token_encoding_prevents_text_concatenation_collisions() {
        let mutable = parse_source_file("fn take(value: &mut T) {}");
        let named = parse_source_file("fn take(value: &mutT) {}");
        let mutable = ItemTree::lower(mutable.tree());
        let named = ItemTree::lower(named.tree());
        let ItemSignature::Callable(mutable_signature) = mutable.items()[0].signature() else {
            panic!("mutable signature")
        };
        let ItemSignature::Callable(named_signature) = named.items()[0].signature() else {
            panic!("named signature")
        };

        assert_eq!(
            mutable_signature.params()[0].type_ref().syntax(),
            Some("&mut T")
        );
        assert_eq!(
            named_signature.params()[0].type_ref().syntax(),
            Some("&mutT")
        );
        assert_ne!(
            mutable.items()[0].signature_fingerprint(),
            named.items()[0].signature_fingerprint()
        );
    }

    #[test]
    fn item_signature_normalizes_missing_types_extern_abi_and_contextual_where() {
        let missing = parse_source_file("fn broken(value:) {}");
        let missing = ItemTree::lower(missing.tree());
        let ItemSignature::Callable(signature) = missing.items()[0].signature() else {
            panic!("recovered function signature")
        };
        assert_eq!(signature.params()[0].type_ref().syntax(), None);

        let default_abi = parse_source_file("extern { fn call(); }");
        let explicit_abi = parse_source_file("extern \"C\" { fn call(); }");
        let default_abi = ItemTree::lower(default_abi.tree());
        let explicit_abi = ItemTree::lower(explicit_abi.tree());
        let ItemSignature::Callable(signature) = default_abi.items()[0].signature() else {
            panic!("extern signature")
        };
        assert_eq!(signature.abi(), Some("C"));
        assert_eq!(
            default_abi.items()[0].signature_fingerprint(),
            explicit_abi.items()[0].signature_fingerprint()
        );

        let contextual = parse_source_file("struct where {} impl where { fn work() {} }");
        assert!(contextual.errors().is_empty(), "{:?}", contextual.errors());
        let contextual = ItemTree::lower(contextual.tree());
        let implementation = contextual
            .items()
            .iter()
            .find(|item| item.kind() == ItemKind::Impl)
            .expect("contextual identifier impl");
        assert_eq!(implementation.name(), "impl where");
        let ItemSignature::Impl(signature) = implementation.signature() else {
            panic!("impl signature")
        };
        assert_eq!(signature.target_type().syntax(), Some("where"));
    }

    #[test]
    fn item_tree_fingerprint_tracks_imports() {
        let first = ItemTree::lower(parse_source_file("use api::value;").tree());
        let second = ItemTree::lower(parse_source_file("use api::value as renamed;").tree());
        assert_ne!(
            first.declaration_fingerprint(),
            second.declaration_fingerprint()
        );
    }

    #[test]
    fn item_tree_skips_recovered_items_without_names() {
        let parse = parse_source_file("fn () {}\npub struct {}\nfn valid() {}");
        let tree = ItemTree::lower(parse.tree());

        assert_eq!(tree.items().len(), 1);
        assert_eq!(tree.items()[0].name(), "valid");
    }

    #[test]
    fn item_tree_lowers_imports() {
        let parse = parse_source_file("use math::{one, two as second};\n");
        let tree = ItemTree::lower(parse.tree());

        assert_eq!(tree.imports().len(), 2);
        assert_eq!(tree.imports()[0].path(), ["math", "one"]);
        assert_eq!(tree.imports()[0].binding_name(), Some("one"));
        assert_eq!(tree.imports()[1].path(), ["math", "two"]);
        assert_eq!(tree.imports()[1].binding_name(), Some("second"));
    }

    #[test]
    fn item_tree_cfg_view_filters_items_and_members() {
        let parse = parse_source_file(
            r#"
            #[cfg(feature = "server")]
            fn server() {}
            struct Config {
                #[cfg(feature = "server")]
                port: i64,
                name: String,
            }
            "#,
        );
        assert!(parse.errors().is_empty(), "{:?}", parse.errors());
        let default_tree = ItemTree::lower_with_cfg(parse.tree(), &CfgOptions::default());
        assert_eq!(default_tree.items().len(), 1);
        assert_eq!(default_tree.items()[0].name(), "Config");
        assert_eq!(default_tree.items()[0].children().len(), 1);
        assert_eq!(default_tree.items()[0].children()[0].name(), "name");

        let mut cfg = CfgOptions::default();
        cfg.insert_feature("server");
        let server_tree = ItemTree::lower_with_cfg(parse.tree(), &cfg);
        assert_eq!(server_tree.items().len(), 2);
        assert_eq!(server_tree.items()[1].children().len(), 2);
    }
}
