//! Analysis-owned types and the small constraint vocabulary used by inference.
//!
//! This module deliberately does not depend on compiler types. Type syntax is
//! lowered conservatively: names that the caller cannot prove are types become
//! [`Ty::Unknown`].

use std::{collections::BTreeMap, fmt};

use super::{DefId, TypeRef};

/// Primitive types modeled by native analysis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PrimitiveTy {
    I64,
    F64,
    Bool,
    String,
    Unit,
}

impl PrimitiveTy {
    pub const fn is_numeric(self) -> bool {
        matches!(self, Self::I64 | Self::F64)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::I64 => "i64",
            Self::F64 => "f64",
            Self::Bool => "bool",
            Self::String => "String",
            Self::Unit => "()",
        }
    }
}

/// Identity and instantiated arguments of a user-defined aggregate type.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NamedTy {
    definition: DefId,
    path: String,
    args: Vec<Ty>,
}

impl NamedTy {
    pub fn new(definition: DefId, path: impl Into<String>, args: Vec<Ty>) -> Self {
        Self {
            definition,
            path: path.into(),
            args,
        }
    }

    pub const fn definition(&self) -> DefId {
        self.definition
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn args(&self) -> &[Ty] {
        &self.args
    }

    fn has_same_identity(&self, other: &Self) -> bool {
        self.definition == other.definition
    }
}

/// Stable identity of a generic parameter within its declaring definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GenericParamId {
    owner: DefId,
    index: u32,
}

impl GenericParamId {
    pub const fn new(owner: DefId, index: u32) -> Self {
        Self { owner, index }
    }

    pub const fn owner(self) -> DefId {
        self.owner
    }

    pub const fn index(self) -> u32 {
        self.index
    }
}

/// A generic parameter identity plus its source-level display name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GenericParamTy {
    id: GenericParamId,
    name: String,
}

impl GenericParamTy {
    pub fn new(id: GenericParamId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
        }
    }

    pub const fn id(&self) -> GenericParamId {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Parameter and return types shared by functions and closures.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CallableTy {
    target: Option<DefId>,
    params: Vec<Ty>,
    return_ty: Box<Ty>,
}

impl CallableTy {
    pub fn new(params: Vec<Ty>, return_ty: Ty) -> Self {
        Self {
            target: None,
            params,
            return_ty: Box::new(return_ty),
        }
    }

    pub fn with_target(mut self, target: DefId) -> Self {
        self.target = Some(target);
        self
    }

    pub const fn target(&self) -> Option<DefId> {
        self.target
    }

    pub fn params(&self) -> &[Ty] {
        &self.params
    }

    pub fn return_ty(&self) -> &Ty {
        &self.return_ty
    }
}

/// Semantic types owned by `rua-analysis`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ty {
    Primitive(PrimitiveTy),
    Named(NamedTy),
    GenericParam(GenericParamTy),
    Tuple(Vec<Ty>),
    Function(CallableTy),
    Closure(CallableTy),
    Vec(Box<Ty>),
    HashMap(Box<Ty>, Box<Ty>),
    Option(Box<Ty>),
    Result(Box<Ty>, Box<Ty>),
    Iterator(Box<Ty>),
    Unknown,
    Never,
}

impl Ty {
    pub const I64: Self = Self::Primitive(PrimitiveTy::I64);
    pub const F64: Self = Self::Primitive(PrimitiveTy::F64);
    pub const BOOL: Self = Self::Primitive(PrimitiveTy::Bool);
    pub const STRING: Self = Self::Primitive(PrimitiveTy::String);
    pub const UNIT: Self = Self::Primitive(PrimitiveTy::Unit);

    pub const fn primitive(primitive: PrimitiveTy) -> Self {
        Self::Primitive(primitive)
    }

    pub const fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }

    pub const fn is_never(&self) -> bool {
        matches!(self, Self::Never)
    }

    pub const fn is_numeric(&self) -> bool {
        matches!(self, Self::Primitive(primitive) if primitive.is_numeric())
    }

    /// Whether the type has enough information to justify a mismatch fact.
    pub fn is_concrete(&self) -> bool {
        match self {
            Self::Unknown | Self::GenericParam(_) => false,
            Self::Primitive(_) | Self::Never => true,
            Self::Named(named) => named.args.iter().all(Self::is_concrete),
            Self::Tuple(items) => items.iter().all(Self::is_concrete),
            Self::Function(callable) | Self::Closure(callable) => {
                callable.params.iter().all(Self::is_concrete) && callable.return_ty.is_concrete()
            }
            Self::Vec(item) | Self::Option(item) | Self::Iterator(item) => item.is_concrete(),
            Self::HashMap(key, value) | Self::Result(key, value) => {
                key.is_concrete() && value.is_concrete()
            }
        }
    }

    pub fn contains_unknown(&self) -> bool {
        match self {
            Self::Unknown => true,
            Self::Primitive(_) | Self::GenericParam(_) | Self::Never => false,
            Self::Named(named) => named.args.iter().any(Self::contains_unknown),
            Self::Tuple(items) => items.iter().any(Self::contains_unknown),
            Self::Function(callable) | Self::Closure(callable) => {
                callable.params.iter().any(Self::contains_unknown)
                    || callable.return_ty.contains_unknown()
            }
            Self::Vec(item) | Self::Option(item) | Self::Iterator(item) => item.contains_unknown(),
            Self::HashMap(key, value) | Self::Result(key, value) => {
                key.contains_unknown() || value.contains_unknown()
            }
        }
    }

    /// Symmetric, intentionally permissive compatibility used for diagnostics.
    pub fn is_compatible_with(&self, other: &Self) -> bool {
        if matches!(self, Self::Unknown | Self::GenericParam(_) | Self::Never)
            || matches!(other, Self::Unknown | Self::GenericParam(_) | Self::Never)
        {
            return true;
        }
        if self.is_numeric() && other.is_numeric() {
            return true;
        }

        match (self, other) {
            (Self::Primitive(left), Self::Primitive(right)) => left == right,
            (Self::Named(left), Self::Named(right)) => {
                left.has_same_identity(right) && compatible_slices(&left.args, &right.args)
            }
            (Self::Tuple(left), Self::Tuple(right)) => compatible_slices(left, right),
            (Self::Function(left), Self::Function(right))
            | (Self::Function(left), Self::Closure(right))
            | (Self::Closure(left), Self::Function(right))
            | (Self::Closure(left), Self::Closure(right)) => compatible_callables(left, right),
            (Self::Vec(left), Self::Vec(right))
            | (Self::Option(left), Self::Option(right))
            | (Self::Iterator(left), Self::Iterator(right)) => left.is_compatible_with(right),
            (Self::HashMap(left_key, left_value), Self::HashMap(right_key, right_value))
            | (Self::Result(left_key, left_value), Self::Result(right_key, right_value)) => {
                left_key.is_compatible_with(right_key) && left_value.is_compatible_with(right_value)
            }
            _ => false,
        }
    }

    /// Least-informative common type used when control-flow paths merge.
    pub fn join(&self, other: &Self) -> Self {
        if self == other {
            return self.clone();
        }
        match (self, other) {
            (Self::Unknown, _) | (_, Self::Unknown) => Self::Unknown,
            (Self::Never, ty) | (ty, Self::Never) => ty.clone(),
            (left, right) if left.is_numeric() && right.is_numeric() => Self::F64,
            (Self::Named(left), Self::Named(right)) if left.has_same_identity(right) => {
                join_slices(&left.args, &right.args)
                    .map(|args| {
                        Self::Named(NamedTy {
                            definition: left.definition,
                            path: left.path.clone(),
                            args,
                        })
                    })
                    .unwrap_or(Self::Unknown)
            }
            (Self::Tuple(left), Self::Tuple(right)) => join_slices(left, right)
                .map(Self::Tuple)
                .unwrap_or(Self::Unknown),
            (Self::Function(left), Self::Function(right)) => join_callables(left, right)
                .map(Self::Function)
                .unwrap_or(Self::Unknown),
            (Self::Closure(left), Self::Closure(right)) => join_callables(left, right)
                .map(Self::Closure)
                .unwrap_or(Self::Unknown),
            (Self::Function(left), Self::Closure(right))
            | (Self::Closure(left), Self::Function(right)) => join_callables(left, right)
                .map(Self::Function)
                .unwrap_or(Self::Unknown),
            (Self::Vec(left), Self::Vec(right)) => Self::Vec(Box::new(left.join(right))),
            (Self::Option(left), Self::Option(right)) => Self::Option(Box::new(left.join(right))),
            (Self::Iterator(left), Self::Iterator(right)) => {
                Self::Iterator(Box::new(left.join(right)))
            }
            (Self::HashMap(left_key, left_value), Self::HashMap(right_key, right_value)) => {
                Self::HashMap(
                    Box::new(left_key.join(right_key)),
                    Box::new(left_value.join(right_value)),
                )
            }
            (Self::Result(left_ok, left_err), Self::Result(right_ok, right_err)) => Self::Result(
                Box::new(left_ok.join(right_ok)),
                Box::new(left_err.join(right_err)),
            ),
            _ => Self::Unknown,
        }
    }

    pub fn name(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Primitive(primitive) => formatter.write_str(primitive.name()),
            Self::Named(named) => {
                formatter.write_str(&named.path)?;
                format_type_args(formatter, &named.args)
            }
            Self::GenericParam(param) => formatter.write_str(param.name()),
            Self::Tuple(items) => {
                formatter.write_str("(")?;
                format_types(formatter, items)?;
                if items.len() == 1 {
                    formatter.write_str(",")?;
                }
                formatter.write_str(")")
            }
            Self::Function(callable) | Self::Closure(callable) => {
                formatter.write_str("fn(")?;
                format_types(formatter, &callable.params)?;
                write!(formatter, ") -> {}", callable.return_ty)
            }
            Self::Vec(item) => write!(formatter, "Vec<{item}>"),
            Self::HashMap(key, value) => write!(formatter, "HashMap<{key}, {value}>"),
            Self::Option(item) => write!(formatter, "Option<{item}>"),
            Self::Result(ok, error) => write!(formatter, "Result<{ok}, {error}>"),
            Self::Iterator(item) => write!(formatter, "Iterator<{item}>"),
            Self::Unknown => formatter.write_str("?"),
            Self::Never => formatter.write_str("!"),
        }
    }
}

fn format_type_args(formatter: &mut fmt::Formatter<'_>, args: &[Ty]) -> fmt::Result {
    if args.is_empty() {
        return Ok(());
    }
    formatter.write_str("<")?;
    format_types(formatter, args)?;
    formatter.write_str(">")
}

fn format_types(formatter: &mut fmt::Formatter<'_>, types: &[Ty]) -> fmt::Result {
    for (index, ty) in types.iter().enumerate() {
        if index > 0 {
            formatter.write_str(", ")?;
        }
        write!(formatter, "{ty}")?;
    }
    Ok(())
}

fn compatible_slices(left: &[Ty], right: &[Ty]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.is_compatible_with(right))
}

fn compatible_callables(left: &CallableTy, right: &CallableTy) -> bool {
    compatible_slices(&left.params, &right.params)
        && left.return_ty.is_compatible_with(&right.return_ty)
}

fn join_slices(left: &[Ty], right: &[Ty]) -> Option<Vec<Ty>> {
    (left.len() == right.len()).then(|| {
        left.iter()
            .zip(right)
            .map(|(left, right)| left.join(right))
            .collect()
    })
}

fn join_callables(left: &CallableTy, right: &CallableTy) -> Option<CallableTy> {
    let mut callable = CallableTy::new(
        join_slices(&left.params, &right.params)?,
        left.return_ty.join(&right.return_ty),
    );
    if left.target == right.target {
        callable.target = left.target;
    }
    Some(callable)
}

/// Context required to turn a syntax-only [`TypeRef`] into a semantic [`Ty`].
pub type NamedTypeResolver<'a> = dyn Fn(&str) -> Option<DefId> + 'a;

#[derive(Default)]
pub struct TypeLoweringContext<'a> {
    generic_params: BTreeMap<String, GenericParamId>,
    named_resolver: Option<&'a NamedTypeResolver<'a>>,
}

impl fmt::Debug for TypeLoweringContext<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TypeLoweringContext")
            .field("generic_params", &self.generic_params)
            .field("has_named_resolver", &self.named_resolver.is_some())
            .finish()
    }
}

impl<'a> TypeLoweringContext<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_generic_params<I, S>(mut self, params: I) -> Self
    where
        I: IntoIterator<Item = (S, GenericParamId)>,
        S: Into<String>,
    {
        self.generic_params
            .extend(params.into_iter().map(|(name, id)| (name.into(), id)));
        self
    }

    pub fn with_generic_param(mut self, name: impl Into<String>, id: GenericParamId) -> Self {
        self.generic_params.insert(name.into(), id);
        self
    }

    pub fn with_named_resolver(mut self, resolver: &'a NamedTypeResolver<'a>) -> Self {
        self.named_resolver = Some(resolver);
        self
    }

    pub fn lower(&self, type_ref: &TypeRef) -> Ty {
        let Some(syntax) = type_ref.syntax() else {
            return Ty::Unknown;
        };
        self.lower_syntax(syntax)
    }

    pub fn lower_syntax(&self, syntax: &str) -> Ty {
        TypeParser::new(syntax, self).parse().unwrap_or(Ty::Unknown)
    }

    fn lower_path(&self, path: String, args: Vec<Ty>) -> Ty {
        if !path.contains("::")
            && args.is_empty()
            && let Some(id) = self.generic_params.get(&path)
        {
            return Ty::GenericParam(GenericParamTy::new(*id, path));
        }

        // A project definition is stronger evidence than a builtin spelling.
        // This keeps workspace/library precedence intact for legal names such
        // as a user-defined `Vec`, while unresolved spellings still fall back
        // to the analysis-owned builtin model below.
        if let Some(definition) = self.named_resolver.and_then(|resolve| resolve(&path)) {
            return Ty::Named(NamedTy::new(definition, path, args));
        }

        let builtin = match path.as_str() {
            "i8" | "i16" | "i32" | "i64" | "isize" | "u8" | "u16" | "u32" | "u64" | "usize"
                if args.is_empty() =>
            {
                Some(Ty::I64)
            }
            "f32" | "f64" if args.is_empty() => Some(Ty::F64),
            "bool" if args.is_empty() => Some(Ty::BOOL),
            "String" | "str" if args.is_empty() => Some(Ty::STRING),
            "Vec" if args.len() == 1 => Some(Ty::Vec(Box::new(args[0].clone()))),
            "HashMap" if args.len() == 2 => Some(Ty::HashMap(
                Box::new(args[0].clone()),
                Box::new(args[1].clone()),
            )),
            "Option" if args.len() == 1 => Some(Ty::Option(Box::new(args[0].clone()))),
            "Result" if args.len() == 2 => Some(Ty::Result(
                Box::new(args[0].clone()),
                Box::new(args[1].clone()),
            )),
            "Iterator" if args.len() == 1 => Some(Ty::Iterator(Box::new(args[0].clone()))),
            _ => None,
        };
        if let Some(builtin) = builtin {
            return builtin;
        }
        if matches!(
            path.as_str(),
            "i8" | "i16"
                | "i32"
                | "i64"
                | "isize"
                | "u8"
                | "u16"
                | "u32"
                | "u64"
                | "usize"
                | "f32"
                | "f64"
                | "bool"
                | "String"
                | "str"
                | "Vec"
                | "HashMap"
                | "Option"
                | "Result"
                | "Iterator"
        ) {
            return Ty::Unknown;
        }

        Ty::Unknown
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TypeToken<'a> {
    Ident(&'a str),
    ColonColon,
    Less,
    Greater,
    Comma,
    LeftParen,
    RightParen,
    Ampersand,
    Arrow,
    Bang,
}

struct TypeParser<'source, 'context> {
    tokens: Vec<TypeToken<'source>>,
    cursor: usize,
    context: &'context TypeLoweringContext<'context>,
}

impl<'source, 'context> TypeParser<'source, 'context> {
    fn new(source: &'source str, context: &'context TypeLoweringContext<'context>) -> Self {
        Self {
            tokens: lex_type(source).unwrap_or_default(),
            cursor: 0,
            context,
        }
    }

    fn parse(mut self) -> Option<Ty> {
        let ty = self.ty()?;
        (self.cursor == self.tokens.len()).then_some(ty)
    }

    fn ty(&mut self) -> Option<Ty> {
        if self.eat(TypeToken::Ampersand) {
            if matches!(self.peek(), Some(TypeToken::Ident("mut"))) {
                self.cursor += 1;
            }
            return self.ty();
        }
        if self.eat(TypeToken::Bang) {
            return Some(Ty::Never);
        }
        if self.eat(TypeToken::LeftParen) {
            return self.tuple();
        }

        let TypeToken::Ident(first) = self.next()? else {
            return None;
        };
        if first == "fn" && self.eat(TypeToken::LeftParen) {
            return self.function();
        }

        let mut path = first.to_string();
        while self.eat(TypeToken::ColonColon) {
            let TypeToken::Ident(segment) = self.next()? else {
                return None;
            };
            path.push_str("::");
            path.push_str(segment);
        }
        let args = if self.eat(TypeToken::Less) {
            self.comma_separated(TypeToken::Greater)?
        } else {
            Vec::new()
        };
        if path == "Box"
            && args.len() == 1
            && self
                .context
                .named_resolver
                .is_none_or(|resolve| resolve(&path).is_none())
        {
            return args.into_iter().next();
        }
        Some(self.context.lower_path(path, args))
    }

    fn tuple(&mut self) -> Option<Ty> {
        if self.eat(TypeToken::RightParen) {
            return Some(Ty::UNIT);
        }
        let first = self.ty()?;
        if self.eat(TypeToken::RightParen) {
            return Some(first);
        }
        if !self.eat(TypeToken::Comma) {
            return None;
        }
        let mut items = vec![first];
        while !self.eat(TypeToken::RightParen) {
            items.push(self.ty()?);
            if self.eat(TypeToken::RightParen) {
                break;
            }
            if !self.eat(TypeToken::Comma) {
                return None;
            }
        }
        Some(Ty::Tuple(items))
    }

    fn function(&mut self) -> Option<Ty> {
        let params = self.comma_separated(TypeToken::RightParen)?;
        let return_ty = if self.eat(TypeToken::Arrow) {
            self.ty()?
        } else {
            Ty::UNIT
        };
        Some(Ty::Function(CallableTy::new(params, return_ty)))
    }

    fn comma_separated(&mut self, end: TypeToken<'source>) -> Option<Vec<Ty>> {
        if self.eat(end) {
            return Some(Vec::new());
        }
        let mut items = Vec::new();
        loop {
            items.push(self.ty()?);
            if self.eat(end) {
                return Some(items);
            }
            if !self.eat(TypeToken::Comma) {
                return None;
            }
            if self.eat(end) {
                return Some(items);
            }
        }
    }

    fn peek(&self) -> Option<TypeToken<'source>> {
        self.tokens.get(self.cursor).copied()
    }

    fn next(&mut self) -> Option<TypeToken<'source>> {
        let token = self.peek()?;
        self.cursor += 1;
        Some(token)
    }

    fn eat(&mut self, expected: TypeToken<'source>) -> bool {
        if self.peek() == Some(expected) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }
}

fn lex_type(source: &str) -> Option<Vec<TypeToken<'_>>> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        let start = cursor;
        match bytes[cursor] {
            byte if byte.is_ascii_whitespace() => cursor += 1,
            b':' if bytes.get(cursor + 1) == Some(&b':') => {
                tokens.push(TypeToken::ColonColon);
                cursor += 2;
            }
            b'-' if bytes.get(cursor + 1) == Some(&b'>') => {
                tokens.push(TypeToken::Arrow);
                cursor += 2;
            }
            b'<' => {
                tokens.push(TypeToken::Less);
                cursor += 1;
            }
            b'>' => {
                tokens.push(TypeToken::Greater);
                cursor += 1;
            }
            b',' => {
                tokens.push(TypeToken::Comma);
                cursor += 1;
            }
            b'(' => {
                tokens.push(TypeToken::LeftParen);
                cursor += 1;
            }
            b')' => {
                tokens.push(TypeToken::RightParen);
                cursor += 1;
            }
            b'&' => {
                tokens.push(TypeToken::Ampersand);
                cursor += 1;
            }
            b'!' => {
                tokens.push(TypeToken::Bang);
                cursor += 1;
            }
            byte if byte == b'_' || byte.is_ascii_alphabetic() => {
                cursor += 1;
                while bytes
                    .get(cursor)
                    .is_some_and(|byte| *byte == b'_' || byte.is_ascii_alphanumeric())
                {
                    cursor += 1;
                }
                tokens.push(TypeToken::Ident(&source[start..cursor]));
            }
            _ => return None,
        }
    }
    Some(tokens)
}

/// Generic parameter bindings inferred at a call or instantiation site.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Substitution {
    bindings: BTreeMap<GenericParamId, Ty>,
}

impl Substitution {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, generic: GenericParamId) -> Option<&Ty> {
        self.bindings.get(&generic)
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    pub fn insert(&mut self, generic: GenericParamId, ty: Ty) -> Option<Ty> {
        self.bindings.insert(generic, ty)
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = (GenericParamId, &Ty)> {
        self.bindings.iter().map(|(generic, ty)| (*generic, ty))
    }

    pub fn apply(&self, ty: &Ty) -> Ty {
        self.apply_with(ty, false)
    }

    /// Substitute known generic parameters and erase unbound ones to Unknown.
    pub fn instantiate(&self, ty: &Ty) -> Ty {
        self.apply_with(ty, true)
    }

    fn apply_with(&self, ty: &Ty, erase_unbound: bool) -> Ty {
        match ty {
            Ty::GenericParam(param) => match self.get(param.id()) {
                Some(ty) => ty.clone(),
                None if erase_unbound => Ty::Unknown,
                None => ty.clone(),
            },
            Ty::Named(named) => Ty::Named(NamedTy {
                definition: named.definition,
                path: named.path.clone(),
                args: named
                    .args
                    .iter()
                    .map(|arg| self.apply_with(arg, erase_unbound))
                    .collect(),
            }),
            Ty::Tuple(items) => Ty::Tuple(
                items
                    .iter()
                    .map(|item| self.apply_with(item, erase_unbound))
                    .collect(),
            ),
            Ty::Function(callable) => Ty::Function(self.apply_callable(callable, erase_unbound)),
            Ty::Closure(callable) => Ty::Closure(self.apply_callable(callable, erase_unbound)),
            Ty::Vec(item) => Ty::Vec(Box::new(self.apply_with(item, erase_unbound))),
            Ty::HashMap(key, value) => Ty::HashMap(
                Box::new(self.apply_with(key, erase_unbound)),
                Box::new(self.apply_with(value, erase_unbound)),
            ),
            Ty::Option(item) => Ty::Option(Box::new(self.apply_with(item, erase_unbound))),
            Ty::Result(ok, error) => Ty::Result(
                Box::new(self.apply_with(ok, erase_unbound)),
                Box::new(self.apply_with(error, erase_unbound)),
            ),
            Ty::Iterator(item) => Ty::Iterator(Box::new(self.apply_with(item, erase_unbound))),
            Ty::Primitive(_) | Ty::Unknown | Ty::Never => ty.clone(),
        }
    }

    fn apply_callable(&self, callable: &CallableTy, erase_unbound: bool) -> CallableTy {
        let mut applied = CallableTy::new(
            callable
                .params
                .iter()
                .map(|param| self.apply_with(param, erase_unbound))
                .collect(),
            self.apply_with(&callable.return_ty, erase_unbound),
        );
        applied.target = callable.target;
        applied
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UnifyResult {
    Exact,
    Uncertain,
    Mismatch,
}

impl UnifyResult {
    pub const fn is_match(self) -> bool {
        !matches!(self, Self::Mismatch)
    }

    fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::Mismatch, _) | (_, Self::Mismatch) => Self::Mismatch,
            (Self::Uncertain, _) | (_, Self::Uncertain) => Self::Uncertain,
            (Self::Exact, Self::Exact) => Self::Exact,
        }
    }
}

/// Structurally match an expected type against an actual type.
///
/// The operation is transactional: a mismatch leaves `substitution` unchanged.
/// Unknown inputs match without creating a binding, preventing speculative type
/// information from leaking into later diagnostics.
pub fn unify(expected: &Ty, actual: &Ty, substitution: &mut Substitution) -> UnifyResult {
    let mut candidate = substitution.clone();
    let result = unify_inner(expected, actual, &mut candidate);
    if result.is_match() {
        *substitution = candidate;
    }
    result
}

fn unify_inner(expected: &Ty, actual: &Ty, substitution: &mut Substitution) -> UnifyResult {
    if expected == actual {
        return UnifyResult::Exact;
    }
    match (expected, actual) {
        (Ty::Unknown, _) | (_, Ty::Unknown) => UnifyResult::Uncertain,
        (Ty::Never, _) | (_, Ty::Never) => UnifyResult::Exact,
        (Ty::GenericParam(param), actual) => bind_generic(param, actual, substitution),
        (_, Ty::GenericParam(_)) => UnifyResult::Uncertain,
        (left, right) if left.is_numeric() && right.is_numeric() => UnifyResult::Exact,
        (Ty::Primitive(_), Ty::Primitive(_)) => UnifyResult::Mismatch,
        (Ty::Named(left), Ty::Named(right)) if left.has_same_identity(right) => {
            unify_slices(&left.args, &right.args, substitution)
        }
        (Ty::Tuple(left), Ty::Tuple(right)) => unify_slices(left, right, substitution),
        (Ty::Function(left), Ty::Function(right))
        | (Ty::Function(left), Ty::Closure(right))
        | (Ty::Closure(left), Ty::Function(right))
        | (Ty::Closure(left), Ty::Closure(right)) => unify_callables(left, right, substitution),
        (Ty::Vec(left), Ty::Vec(right))
        | (Ty::Option(left), Ty::Option(right))
        | (Ty::Iterator(left), Ty::Iterator(right)) => unify_inner(left, right, substitution),
        (Ty::HashMap(left_key, left_value), Ty::HashMap(right_key, right_value))
        | (Ty::Result(left_key, left_value), Ty::Result(right_key, right_value)) => unify_inner(
            left_key,
            right_key,
            substitution,
        )
        .combine(unify_inner(left_value, right_value, substitution)),
        _ => UnifyResult::Mismatch,
    }
}

fn bind_generic(
    param: &GenericParamTy,
    actual: &Ty,
    substitution: &mut Substitution,
) -> UnifyResult {
    if actual.is_unknown() {
        return UnifyResult::Uncertain;
    }
    if let Ty::GenericParam(actual_param) = actual {
        return if actual_param.id() == param.id() {
            UnifyResult::Exact
        } else {
            UnifyResult::Uncertain
        };
    }
    if let Some(bound) = substitution.get(param.id()).cloned() {
        if matches!(&bound, Ty::GenericParam(bound_param) if bound_param.id() == param.id()) {
            substitution.insert(param.id(), actual.clone());
            return UnifyResult::Exact;
        }
        return unify_inner(&bound, actual, substitution);
    }
    substitution.insert(param.id(), actual.clone());
    UnifyResult::Exact
}

fn unify_slices(left: &[Ty], right: &[Ty], substitution: &mut Substitution) -> UnifyResult {
    if left.len() != right.len() {
        return UnifyResult::Mismatch;
    }
    left.iter()
        .zip(right)
        .fold(UnifyResult::Exact, |result, (left, right)| {
            result.combine(unify_inner(left, right, substitution))
        })
}

fn unify_callables(
    left: &CallableTy,
    right: &CallableTy,
    substitution: &mut Substitution,
) -> UnifyResult {
    unify_slices(&left.params, &right.params, substitution).combine(unify_inner(
        &left.return_ty,
        &right.return_ty,
        substitution,
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        CallableTy, GenericParamId, GenericParamTy, PrimitiveTy, Substitution, Ty,
        TypeLoweringContext, UnifyResult, unify,
    };
    use crate::hir::DefId;

    fn generic(id: GenericParamId, name: &str) -> Ty {
        Ty::GenericParam(GenericParamTy::new(id, name))
    }

    #[test]
    fn lowers_builtin_nested_and_generic_types() {
        let t = GenericParamId::new(DefId::new(1), 0);
        let context = TypeLoweringContext::new().with_generic_params([("T", t)]);
        assert_eq!(context.lower_syntax("i64"), Ty::I64);
        assert_eq!(context.lower_syntax("u8"), Ty::I64);
        assert_eq!(context.lower_syntax("f32"), Ty::F64);
        assert_eq!(context.lower_syntax("str"), Ty::STRING);
        assert_eq!(context.lower_syntax("&mut String"), Ty::STRING);
        assert_eq!(
            context.lower_syntax("Vec<Option<Result<T, String>>>"),
            Ty::Vec(Box::new(Ty::Option(Box::new(Ty::Result(
                Box::new(generic(t, "T")),
                Box::new(Ty::STRING),
            )))))
        );
        assert_eq!(
            context.lower_syntax("Box<(i64, f64)>"),
            Ty::Tuple(vec![Ty::I64, Ty::F64])
        );
    }

    #[test]
    fn lowering_unknown_names_requires_proof() {
        let strict = TypeLoweringContext::new();
        assert_eq!(strict.lower_syntax("Widget"), Ty::Unknown);

        let widget = DefId::new(2);
        let resolve = |path: &str| (path == "model::Widget").then_some(widget);
        let known = TypeLoweringContext::new().with_named_resolver(&resolve);
        let Ty::Named(named) = known.lower_syntax("model::Widget<i64>") else {
            panic!("known aggregate should lower to a named type");
        };
        assert_eq!(named.path(), "model::Widget");
        assert_eq!(named.args(), &[Ty::I64]);
        assert_eq!(named.definition(), widget);

        let user_vec = DefId::new(3);
        let user_box = DefId::new(4);
        let resolve = |path: &str| match path {
            "Vec" => Some(user_vec),
            "Box" => Some(user_box),
            _ => None,
        };
        let known = TypeLoweringContext::new().with_named_resolver(&resolve);
        let Ty::Named(named) = known.lower_syntax("Vec") else {
            panic!("a proven project type must shadow builtin metadata");
        };
        assert_eq!(named.definition(), user_vec);
        let Ty::Named(named) = known.lower_syntax("Box<i64>") else {
            panic!("a proven project Box must not be lowered as transparent syntax");
        };
        assert_eq!(named.definition(), user_box);
        assert_eq!(named.args(), &[Ty::I64]);

        let string_param = GenericParamId::new(DefId::new(5), 0);
        let generic_context = TypeLoweringContext::new().with_generic_param("String", string_param);
        assert_eq!(
            generic_context.lower_syntax("String"),
            generic(string_param, "String")
        );
    }

    #[test]
    fn compatibility_is_permissive_only_for_uncertain_types() {
        assert!(Ty::I64.is_compatible_with(&Ty::F64));
        assert!(Ty::Never.is_compatible_with(&Ty::STRING));
        assert!(Ty::Unknown.is_compatible_with(&Ty::BOOL));
        assert!(!Ty::BOOL.is_compatible_with(&Ty::STRING));
        assert!(
            Ty::Function(CallableTy::new(vec![Ty::I64], Ty::BOOL))
                .is_compatible_with(&Ty::Closure(CallableTy::new(vec![Ty::F64], Ty::BOOL)))
        );
        assert_eq!(Ty::I64.join(&Ty::F64), Ty::F64);
        assert_eq!(Ty::Unknown.join(&Ty::BOOL), Ty::Unknown);
        assert_eq!(Ty::Never.join(&Ty::BOOL), Ty::BOOL);
    }

    #[test]
    fn generic_unification_and_substitution_are_structural() {
        let owner = DefId::new(3);
        let t = GenericParamId::new(owner, 0);
        let e = GenericParamId::new(owner, 1);
        let expected = Ty::Function(CallableTy::new(
            vec![Ty::Vec(Box::new(generic(t, "T")))],
            Ty::Option(Box::new(generic(t, "T"))),
        ));
        let actual = Ty::Closure(CallableTy::new(
            vec![Ty::Vec(Box::new(Ty::I64))],
            Ty::Option(Box::new(Ty::I64)),
        ));
        let mut substitution = Substitution::new();
        assert_eq!(
            unify(&expected, &actual, &mut substitution),
            UnifyResult::Exact
        );
        assert_eq!(substitution.get(t), Some(&Ty::I64));
        assert_eq!(
            substitution.instantiate(&Ty::Result(
                Box::new(generic(t, "T")),
                Box::new(generic(e, "E")),
            )),
            Ty::Result(Box::new(Ty::I64), Box::new(Ty::Unknown))
        );

        let mut cyclic = Substitution::new();
        cyclic.insert(t, generic(t, "T"));
        assert_eq!(
            unify(&generic(t, "T"), &Ty::I64, &mut cyclic),
            UnifyResult::Exact
        );
        assert_eq!(cyclic.get(t), Some(&Ty::I64));
    }

    #[test]
    fn mismatch_does_not_partially_update_substitution() {
        let t = GenericParamId::new(DefId::new(4), 0);
        let expected = Ty::Tuple(vec![generic(t, "T"), Ty::BOOL]);
        let actual = Ty::Tuple(vec![Ty::I64, Ty::STRING]);
        let mut substitution = Substitution::new();
        assert_eq!(
            unify(&expected, &actual, &mut substitution),
            UnifyResult::Mismatch
        );
        assert_eq!(substitution.iter().len(), 0);
    }

    #[test]
    fn primitive_names_are_stable() {
        assert_eq!(Ty::primitive(PrimitiveTy::Unit).name(), "()");
        assert_eq!(
            TypeLoweringContext::new()
                .lower_syntax("fn(i64, bool) -> String")
                .name(),
            "fn(i64, bool) -> String"
        );
    }
}
