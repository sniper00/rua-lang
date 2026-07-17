//! Small protocol-neutral contracts shared across the Rua toolchain.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
};

macro_rules! index_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u32);

        impl $name {
            pub const fn new(raw: u32) -> Self {
                Self(raw)
            }

            pub const fn index(self) -> u32 {
                self.0
            }
        }
    };
}

index_id!(FileId, "Stable source identity within one owning session.");
index_id!(SourceRootId, "Stable identity of one source root.");
index_id!(ProjectId, "Stable identity of one workspace project.");

/// Logical module identity independent of filesystem spelling.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModulePath(Vec<String>);

impl ModulePath {
    pub fn new(
        segments: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, InvalidModulePath> {
        let segments = segments.into_iter().map(Into::into).collect::<Vec<_>>();
        if segments.iter().all(|segment| valid_identifier(segment)) {
            Ok(Self(segments))
        } else {
            Err(InvalidModulePath)
        }
    }

    pub fn child(&self, name: impl Into<String>) -> Result<Self, InvalidModulePath> {
        let mut segments = self.0.clone();
        segments.push(name.into());
        Self::new(segments)
    }

    pub fn segments(&self) -> &[String] {
        &self.0
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for ModulePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0.join("::"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidModulePath;

impl fmt::Display for InvalidModulePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("module path contains an invalid identifier")
    }
}

impl std::error::Error for InvalidModulePath {}

fn valid_identifier(identifier: &str) -> bool {
    let mut bytes = identifier.bytes();
    matches!(bytes.next(), Some(b'_' | b'a'..=b'z' | b'A'..=b'Z'))
        && bytes.all(|byte| matches!(byte, b'_' | b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9'))
}

/// Compile-time configuration shared by the compiler and native analysis.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CfgOptions {
    flags: BTreeSet<String>,
    values: BTreeMap<String, BTreeSet<String>>,
}

impl CfgOptions {
    pub fn insert_flag(&mut self, name: impl Into<String>) {
        self.flags.insert(name.into());
    }

    pub fn insert_value(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.values
            .entry(key.into())
            .or_default()
            .insert(value.into());
    }

    pub fn insert_feature(&mut self, feature: impl Into<String>) {
        self.insert_value("feature", feature);
    }

    pub fn has_flag(&self, name: &str) -> bool {
        self.flags.contains(name)
    }

    pub fn has_value(&self, key: &str, value: &str) -> bool {
        self.values
            .get(key)
            .is_some_and(|values| values.contains(value))
    }

    pub fn flags(&self) -> impl Iterator<Item = &str> {
        self.flags.iter().map(String::as_str)
    }

    pub fn values(&self) -> impl Iterator<Item = (&str, &BTreeSet<String>)> {
        self.values
            .iter()
            .map(|(key, values)| (key.as_str(), values))
    }

    pub fn matches(&self, expression: &CfgExpr) -> bool {
        match expression {
            CfgExpr::Bool(value) => *value,
            CfgExpr::Flag(name) => self.has_flag(name),
            CfgExpr::KeyValue { key, value } => self.has_value(key, value),
            CfgExpr::All(expressions) => expressions
                .iter()
                .all(|expression| self.matches(expression)),
            CfgExpr::Any(expressions) => expressions
                .iter()
                .any(|expression| self.matches(expression)),
            CfgExpr::Not(expression) => !self.matches(expression),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MetaValue {
    String(String),
    Bool(bool),
    Integer(i64),
    Float(String),
    Path(String),
    List(Vec<MetaValue>),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MetaItem {
    Word(String),
    NameValue { name: String, value: MetaValue },
    List { name: String, items: Vec<MetaItem> },
    Literal(MetaValue),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Attribute {
    pub name: String,
    pub items: Vec<MetaItem>,
}

impl Attribute {
    pub fn new(name: impl Into<String>, items: Vec<MetaItem>) -> Self {
        Self {
            name: name.into(),
            items,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CfgExpr {
    Bool(bool),
    Flag(String),
    KeyValue { key: String, value: String },
    All(Vec<CfgExpr>),
    Any(Vec<CfgExpr>),
    Not(Box<CfgExpr>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpandedAttributes {
    pub active: bool,
    /// Active non-configuration attributes left after expanding `cfg_attr`.
    pub attributes: Vec<Attribute>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CfgError(String);

impl CfgError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for CfgError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for CfgError {}

/// Expand `cfg_attr` recursively and evaluate every resulting `cfg` attribute.
/// Other active attributes are returned to the caller for normal validation.
pub fn expand_cfg_attributes(
    attributes: &[Attribute],
    options: &CfgOptions,
) -> Result<ExpandedAttributes, CfgError> {
    const MAX_EXPANSIONS: usize = 256;

    let mut queue = VecDeque::from(attributes.to_vec());
    let mut retained = Vec::new();
    let mut active = true;
    let mut expansions = 0usize;
    while let Some(attribute) = queue.pop_front() {
        expansions += 1;
        if expansions > MAX_EXPANSIONS {
            return Err(CfgError::new("cfg_attr expansion limit exceeded"));
        }
        match attribute.name.as_str() {
            "cfg" => active &= options.matches(&single_cfg_argument(&attribute)?),
            "cfg_attr" => {
                let (condition, nested) = attribute.items.split_first().ok_or_else(|| {
                    CfgError::new("cfg_attr requires a condition and at least one attribute")
                })?;
                if nested.is_empty() {
                    return Err(CfgError::new(
                        "cfg_attr requires a condition and at least one attribute",
                    ));
                }
                if options.matches(&cfg_expr(condition)?) {
                    for item in nested.iter().rev() {
                        queue.push_front(attribute_from_meta(item)?);
                    }
                }
            }
            _ => retained.push(attribute),
        }
    }
    Ok(ExpandedAttributes {
        active,
        attributes: retained,
    })
}

fn single_cfg_argument(attribute: &Attribute) -> Result<CfgExpr, CfgError> {
    if attribute.items.len() != 1 {
        return Err(CfgError::new("cfg requires exactly one condition"));
    }
    cfg_expr(&attribute.items[0])
}

fn cfg_expr(item: &MetaItem) -> Result<CfgExpr, CfgError> {
    match item {
        MetaItem::Literal(MetaValue::Bool(value)) => Ok(CfgExpr::Bool(*value)),
        MetaItem::Word(name) => Ok(CfgExpr::Flag(name.clone())),
        MetaItem::NameValue {
            name,
            value: MetaValue::String(value),
        } => Ok(CfgExpr::KeyValue {
            key: name.clone(),
            value: value.clone(),
        }),
        MetaItem::NameValue {
            name,
            value: MetaValue::Bool(value),
        } => Ok(if *value {
            CfgExpr::Flag(name.clone())
        } else {
            CfgExpr::Not(Box::new(CfgExpr::Flag(name.clone())))
        }),
        MetaItem::List { name, items } if name == "all" => items
            .iter()
            .map(cfg_expr)
            .collect::<Result<Vec<_>, _>>()
            .map(CfgExpr::All),
        MetaItem::List { name, items } if name == "any" => items
            .iter()
            .map(cfg_expr)
            .collect::<Result<Vec<_>, _>>()
            .map(CfgExpr::Any),
        MetaItem::List { name, items } if name == "not" => {
            if items.len() != 1 {
                return Err(CfgError::new("not requires exactly one condition"));
            }
            Ok(CfgExpr::Not(Box::new(cfg_expr(&items[0])?)))
        }
        MetaItem::List { name, .. } => Err(CfgError::new(format!(
            "unsupported cfg predicate `{name}`; expected all, any, or not"
        ))),
        MetaItem::NameValue {
            value:
                MetaValue::Integer(_) | MetaValue::Float(_) | MetaValue::Path(_) | MetaValue::List(_),
            ..
        }
        | MetaItem::Literal(
            MetaValue::String(_)
            | MetaValue::Integer(_)
            | MetaValue::Float(_)
            | MetaValue::Path(_)
            | MetaValue::List(_),
        ) => Err(CfgError::new(
            "cfg conditions only accept flags, string key/value pairs, and booleans",
        )),
    }
}

fn attribute_from_meta(item: &MetaItem) -> Result<Attribute, CfgError> {
    match item {
        MetaItem::Word(name) => Ok(Attribute::new(name, Vec::new())),
        MetaItem::List { name, items } => Ok(Attribute::new(name, items.clone())),
        MetaItem::NameValue { name, value } => Ok(Attribute::new(
            name,
            vec![MetaItem::NameValue {
                name: name.clone(),
                value: value.clone(),
            }],
        )),
        MetaItem::Literal(_) => Err(CfgError::new(
            "cfg_attr result must be an attribute, not a literal",
        )),
    }
}

/// A UTF-8 byte offset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TextSize(u32);

impl TextSize {
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Half-open UTF-8 byte range in a source file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TextRange {
    start: u32,
    end: u32,
}

impl TextRange {
    pub const fn new(start: u32, end: u32) -> Self {
        assert!(start <= end, "text range start must not exceed end");
        Self { start, end }
    }

    pub const fn at(start: u32, len: u32) -> Self {
        Self::new(start, start + len)
    }

    pub const fn start(self) -> u32 {
        self.start
    }

    pub const fn end(self) -> u32 {
        self.end
    }

    pub const fn len(self) -> u32 {
        self.end - self.start
    }

    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    pub const fn contains(self, offset: u32) -> bool {
        self.start <= offset && offset < self.end
    }

    pub const fn contains_range(self, other: Self) -> bool {
        self.start <= other.start && other.end <= self.end
    }
}

/// Stable diagnostic identifier shared by compiler and native analysis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiagnosticCode {
    ParseUnexpectedToken = 1,
    ParseUnterminatedString = 2,
    ParseUnterminatedComment = 3,
    ParseExpectedItem = 4,
    ParseMissingDelimiter = 5,
    ParseResourceLimit = 6,
    NameUnresolved = 100,
    NameDuplicateDefinition = 101,
    NamePrivateAccess = 102,
    NameModuleNotFound = 103,
    NameAmbiguousImport = 104,
    NameUnknownMember = 105,
    NameModuleCycle = 106,
    NameInvalidModulePath = 107,
    NameInvalidDeclaration = 108,
    TypeMismatch = 200,
    TypeExpectedBool = 201,
    TypeNotCallable = 202,
    TypeArgumentCount = 203,
    TypeNotIterable = 204,
    TypeInvalidUnary = 205,
    TypeInvalidBinary = 206,
    TypeInvalidTry = 207,
    TypeUnsatisfiedTraitBound = 208,
    TypeUnknownField = 209,
    TypeUnknownMethod = 210,
    TypeMissingMatchArm = 211,
    TypeImmutableAssignment = 212,
    TypeInvalidFfiAdapter = 213,
    TypeInvalidBreak = 214,
    TypeInvalidOptionalChain = 215,
    AnnotationUnresolved = 250,
    AnnotationInvalidTarget = 251,
    AnnotationInvalidSchema = 252,
    AnnotationInvalidArguments = 253,
    AnnotationDuplicate = 254,
    AnnotationRuntimePrivate = 255,
    LintUnusedVariable = 300,
    LintRedundantMut = 301,
    LintUnreachableCode = 302,
    LintUnusedFunction = 303,
    LintInfiniteLoop = 304,
    HostSourceRead = 400,
    HostProjectInvalid = 401,
    HostBuiltinInvalid = 402,
}

impl DiagnosticCode {
    pub const fn error_code(self) -> &'static str {
        self.as_str()
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ParseUnexpectedToken => "E0001",
            Self::ParseUnterminatedString => "E0002",
            Self::ParseUnterminatedComment => "E0003",
            Self::ParseExpectedItem => "E0004",
            Self::ParseMissingDelimiter => "E0005",
            Self::ParseResourceLimit => "E0006",
            Self::NameUnresolved => "E0100",
            Self::NameDuplicateDefinition => "E0101",
            Self::NamePrivateAccess => "E0102",
            Self::NameModuleNotFound => "E0103",
            Self::NameAmbiguousImport => "E0104",
            Self::NameUnknownMember => "E0105",
            Self::NameModuleCycle => "E0106",
            Self::NameInvalidModulePath => "E0107",
            Self::NameInvalidDeclaration => "E0108",
            Self::TypeMismatch => "E0200",
            Self::TypeExpectedBool => "E0201",
            Self::TypeNotCallable => "E0202",
            Self::TypeArgumentCount => "E0203",
            Self::TypeNotIterable => "E0204",
            Self::TypeInvalidUnary => "E0205",
            Self::TypeInvalidBinary => "E0206",
            Self::TypeInvalidTry => "E0207",
            Self::TypeUnsatisfiedTraitBound => "E0208",
            Self::TypeUnknownField => "E0209",
            Self::TypeUnknownMethod => "E0210",
            Self::TypeMissingMatchArm => "E0211",
            Self::TypeImmutableAssignment => "E0212",
            Self::TypeInvalidFfiAdapter => "E0213",
            Self::TypeInvalidBreak => "E0214",
            Self::TypeInvalidOptionalChain => "E0215",
            Self::AnnotationUnresolved => "E0250",
            Self::AnnotationInvalidTarget => "E0251",
            Self::AnnotationInvalidSchema => "E0252",
            Self::AnnotationInvalidArguments => "E0253",
            Self::AnnotationDuplicate => "E0254",
            Self::AnnotationRuntimePrivate => "E0255",
            Self::LintUnusedVariable => "W0300",
            Self::LintRedundantMut => "W0301",
            Self::LintUnreachableCode => "W0302",
            Self::LintUnusedFunction => "W0303",
            Self::LintInfiniteLoop => "W0304",
            Self::HostSourceRead => "E0400",
            Self::HostProjectInvalid => "E0401",
            Self::HostBuiltinInvalid => "E0402",
        }
    }

    pub const fn severity(self) -> DiagnosticSeverity {
        match self {
            Self::LintUnusedVariable
            | Self::LintRedundantMut
            | Self::LintUnreachableCode
            | Self::LintUnusedFunction
            | Self::LintInfiniteLoop => DiagnosticSeverity::Warning,
            _ => DiagnosticSeverity::Error,
        }
    }

    pub const fn category(self) -> DiagnosticCategory {
        match self {
            Self::ParseUnexpectedToken
            | Self::ParseUnterminatedString
            | Self::ParseUnterminatedComment
            | Self::ParseExpectedItem
            | Self::ParseMissingDelimiter
            | Self::ParseResourceLimit => DiagnosticCategory::Parse,
            Self::NameUnresolved
            | Self::NameDuplicateDefinition
            | Self::NamePrivateAccess
            | Self::NameModuleNotFound
            | Self::NameAmbiguousImport
            | Self::NameUnknownMember
            | Self::NameModuleCycle
            | Self::NameInvalidModulePath
            | Self::NameInvalidDeclaration => DiagnosticCategory::NameResolution,
            Self::TypeMismatch
            | Self::TypeExpectedBool
            | Self::TypeNotCallable
            | Self::TypeArgumentCount
            | Self::TypeNotIterable
            | Self::TypeInvalidUnary
            | Self::TypeInvalidBinary
            | Self::TypeInvalidTry
            | Self::TypeUnsatisfiedTraitBound
            | Self::TypeUnknownField
            | Self::TypeUnknownMethod
            | Self::TypeMissingMatchArm
            | Self::TypeImmutableAssignment
            | Self::TypeInvalidBreak
            | Self::TypeInvalidOptionalChain
            | Self::TypeInvalidFfiAdapter => DiagnosticCategory::Type,
            Self::AnnotationUnresolved
            | Self::AnnotationInvalidTarget
            | Self::AnnotationInvalidSchema
            | Self::AnnotationInvalidArguments
            | Self::AnnotationDuplicate
            | Self::AnnotationRuntimePrivate => DiagnosticCategory::Annotation,
            Self::LintUnusedVariable
            | Self::LintRedundantMut
            | Self::LintUnreachableCode
            | Self::LintUnusedFunction
            | Self::LintInfiniteLoop => DiagnosticCategory::Lint,
            Self::HostSourceRead | Self::HostProjectInvalid | Self::HostBuiltinInvalid => {
                DiagnosticCategory::Host
            }
        }
    }
}

impl fmt::Display for DiagnosticCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiagnosticCategory {
    Parse,
    NameResolution,
    Type,
    Annotation,
    Lint,
    Host,
}

/// One named diagnostic argument. Human renderers localize and arrange these
/// values; protocol layers never parse them back out of a message string.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DiagnosticArgument {
    pub name: String,
    pub value: String,
}

/// Shared structured diagnostic payload without presentation text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructuredDiagnostic {
    pub code: DiagnosticCode,
    pub file: Option<FileId>,
    pub range: Option<TextRange>,
    pub arguments: Vec<DiagnosticArgument>,
}

impl StructuredDiagnostic {
    pub fn new(code: DiagnosticCode, file: Option<FileId>, range: Option<TextRange>) -> Self {
        Self {
            code,
            file,
            range,
            arguments: Vec::new(),
        }
    }

    pub fn with_argument(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.arguments.push(DiagnosticArgument {
            name: name.into(),
            value: value.into(),
        });
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BuiltinId {
    TypeString,
    TypeOption,
    TypeResult,
    TypeVec,
    TypeHashMap,
    TypeIter,
    VariantOptionSome,
    VariantOptionNone,
    VariantResultOk,
    VariantResultErr,
}

/// Stable identity of a declaration loaded from `std.toml`.
///
/// The hash is derived from the declaration's canonical resource path and
/// symbol path. Resource loaders reject collisions before semantic use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StdSymbolId(u64);

impl StdSymbolId {
    pub fn new(canonical_path: &str) -> Self {
        let mut value = 0xcbf29ce484222325_u64;
        for byte in canonical_path.bytes() {
            value ^= u64::from(byte);
            value = value.wrapping_mul(0x100000001b3);
        }
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BuiltinTraitId {
    Copy,
    Clone,
    Debug,
    Display,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Sized,
    Send,
    Sync,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Neg,
    Not,
    Iterator,
    IntoIterator,
    ToString,
    From,
    Into,
}

impl BuiltinTraitId {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Copy => "Copy",
            Self::Clone => "Clone",
            Self::Debug => "Debug",
            Self::Display => "Display",
            Self::Default => "Default",
            Self::PartialEq => "PartialEq",
            Self::Eq => "Eq",
            Self::PartialOrd => "PartialOrd",
            Self::Ord => "Ord",
            Self::Hash => "Hash",
            Self::Sized => "Sized",
            Self::Send => "Send",
            Self::Sync => "Sync",
            Self::Add => "Add",
            Self::Sub => "Sub",
            Self::Mul => "Mul",
            Self::Div => "Div",
            Self::Rem => "Rem",
            Self::Neg => "Neg",
            Self::Not => "Not",
            Self::Iterator => "Iterator",
            Self::IntoIterator => "IntoIterator",
            Self::ToString => "ToString",
            Self::From => "From",
            Self::Into => "Into",
        }
    }
}

pub fn builtin_trait(name: &str) -> Option<BuiltinTraitId> {
    Some(match name {
        "Copy" => BuiltinTraitId::Copy,
        "Clone" => BuiltinTraitId::Clone,
        "Debug" => BuiltinTraitId::Debug,
        "Display" => BuiltinTraitId::Display,
        "Default" => BuiltinTraitId::Default,
        "PartialEq" => BuiltinTraitId::PartialEq,
        "Eq" => BuiltinTraitId::Eq,
        "PartialOrd" => BuiltinTraitId::PartialOrd,
        "Ord" => BuiltinTraitId::Ord,
        "Hash" => BuiltinTraitId::Hash,
        "Sized" => BuiltinTraitId::Sized,
        "Send" => BuiltinTraitId::Send,
        "Sync" => BuiltinTraitId::Sync,
        "Add" => BuiltinTraitId::Add,
        "Sub" => BuiltinTraitId::Sub,
        "Mul" => BuiltinTraitId::Mul,
        "Div" => BuiltinTraitId::Div,
        "Rem" => BuiltinTraitId::Rem,
        "Neg" => BuiltinTraitId::Neg,
        "Not" => BuiltinTraitId::Not,
        "Iterator" => BuiltinTraitId::Iterator,
        "IntoIterator" => BuiltinTraitId::IntoIterator,
        "ToString" => BuiltinTraitId::ToString,
        "From" => BuiltinTraitId::From,
        "Into" => BuiltinTraitId::Into,
        _ => return None,
    })
}

/// Version of the shared builtin manifest schema.
pub const BUILTIN_MANIFEST_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuiltinTypeSpec {
    pub id: BuiltinId,
    pub name: &'static str,
}

pub const BUILTIN_TYPES: &[BuiltinTypeSpec] = &[
    BuiltinTypeSpec {
        id: BuiltinId::TypeString,
        name: "String",
    },
    BuiltinTypeSpec {
        id: BuiltinId::TypeOption,
        name: "Option",
    },
    BuiltinTypeSpec {
        id: BuiltinId::TypeResult,
        name: "Result",
    },
    BuiltinTypeSpec {
        id: BuiltinId::TypeVec,
        name: "Vec",
    },
    BuiltinTypeSpec {
        id: BuiltinId::TypeHashMap,
        name: "HashMap",
    },
    BuiltinTypeSpec {
        id: BuiltinId::TypeIter,
        name: "Iter",
    },
];

pub fn builtin_type(name: &str) -> Option<BuiltinId> {
    BUILTIN_TYPES
        .iter()
        .find(|specification| specification.name == name)
        .map(|specification| specification.id)
}

pub fn builtin_value(name: &str) -> Option<BuiltinId> {
    match name {
        "Some" => Some(BuiltinId::VariantOptionSome),
        "None" => Some(BuiltinId::VariantOptionNone),
        "Ok" => Some(BuiltinId::VariantResultOk),
        "Err" => Some(BuiltinId::VariantResultErr),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_are_half_open() {
        let range = TextRange::new(2, 5);
        assert!(range.contains(2));
        assert!(range.contains(4));
        assert!(!range.contains(5));
    }

    #[test]
    fn diagnostic_codes_have_stable_rendering() {
        assert_eq!(DiagnosticCode::NameUnresolved.as_str(), "E0100");
        assert_eq!(DiagnosticCode::LintUnusedFunction.as_str(), "W0303");
        assert_eq!(
            DiagnosticCode::TypeMismatch.category(),
            DiagnosticCategory::Type
        );
    }

    #[test]
    fn module_paths_reject_invalid_segments() {
        let path = ModulePath::new(["network", "client2"]).unwrap();
        assert_eq!(path.to_string(), "network::client2");
        assert!(ModulePath::new(["bad-name"]).is_err());
    }

    #[test]
    fn builtin_manifest_is_complete_and_stable() {
        assert_eq!(BUILTIN_MANIFEST_VERSION, 1);
        assert_eq!(BUILTIN_TYPES.len(), 6);
    }

    #[test]
    fn cfg_options_evaluate_boolean_composition() {
        let mut options = CfgOptions::default();
        options.insert_feature("http");
        options.insert_value("runtime", "moon");
        let expression = CfgExpr::All(vec![
            CfgExpr::KeyValue {
                key: "feature".into(),
                value: "http".into(),
            },
            CfgExpr::Not(Box::new(CfgExpr::Flag("embedded".into()))),
            CfgExpr::Any(vec![
                CfgExpr::Flag("missing".into()),
                CfgExpr::KeyValue {
                    key: "runtime".into(),
                    value: "moon".into(),
                },
            ]),
        ]);
        assert!(options.matches(&expression));
    }

    #[test]
    fn cfg_attr_expands_before_cfg_is_evaluated() {
        let mut options = CfgOptions::default();
        options.insert_feature("server");
        let attributes = [Attribute::new(
            "cfg_attr",
            vec![
                MetaItem::NameValue {
                    name: "feature".into(),
                    value: MetaValue::String("server".into()),
                },
                MetaItem::List {
                    name: "cfg".into(),
                    items: vec![MetaItem::Word("enabled".into())],
                },
            ],
        )];
        assert!(!expand_cfg_attributes(&attributes, &options).unwrap().active);
        options.insert_flag("enabled");
        assert!(expand_cfg_attributes(&attributes, &options).unwrap().active);
    }
}
