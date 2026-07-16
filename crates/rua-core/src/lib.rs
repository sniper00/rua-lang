//! Small protocol-neutral contracts shared across the Rua toolchain.

use std::fmt;

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
    MacroPrintln,
    MacroPrint,
    MacroFormat,
    MacroVec,
    MacroPanic,
    MacroAssert,
    MacroAssertEq,
    MacroAssertNe,
    MacroUnreachable,
    MacroUnimplemented,
    MacroTodo,
    MacroDbg,
    MacroIncludeStr,
    MacroIncludeBytes,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MacroDelimiter {
    Parentheses,
    Brackets,
    Braces,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BuiltinMacroLowering {
    Vec,
    Format,
    Println,
    Print,
    Panic,
    Passthrough,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuiltinMacroSpec {
    pub id: BuiltinId,
    pub name: &'static str,
    pub delimiter: MacroDelimiter,
    pub signature: &'static str,
    pub return_type: &'static str,
    pub documentation: &'static str,
    pub deprecated: Option<&'static str>,
    pub lowering: BuiltinMacroLowering,
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

pub const BUILTIN_MACROS: &[BuiltinMacroSpec] = &[
    BuiltinMacroSpec {
        id: BuiltinId::MacroPrintln,
        name: "println",
        delimiter: MacroDelimiter::Parentheses,
        signature: "println!(format: &str, ...)",
        return_type: "()",
        documentation: "Print a formatted line to standard output.",
        deprecated: None,
        lowering: BuiltinMacroLowering::Println,
    },
    BuiltinMacroSpec {
        id: BuiltinId::MacroPrint,
        name: "print",
        delimiter: MacroDelimiter::Parentheses,
        signature: "print!(format: &str, ...)",
        return_type: "()",
        documentation: "Print formatted text without a trailing newline.",
        deprecated: None,
        lowering: BuiltinMacroLowering::Print,
    },
    BuiltinMacroSpec {
        id: BuiltinId::MacroFormat,
        name: "format",
        delimiter: MacroDelimiter::Parentheses,
        signature: "format!(format: &str, ...) -> String",
        return_type: "String",
        documentation: "Build a String using Rua formatting placeholders.",
        deprecated: None,
        lowering: BuiltinMacroLowering::Format,
    },
    BuiltinMacroSpec {
        id: BuiltinId::MacroVec,
        name: "vec",
        delimiter: MacroDelimiter::Brackets,
        signature: "vec![values...] -> Vec<T>",
        return_type: "Vec<T>",
        documentation: "Create a Vec containing the supplied values.",
        deprecated: None,
        lowering: BuiltinMacroLowering::Vec,
    },
    BuiltinMacroSpec {
        id: BuiltinId::MacroPanic,
        name: "panic",
        delimiter: MacroDelimiter::Parentheses,
        signature: "panic!(format: &str, ...) -> !",
        return_type: "!",
        documentation: "Abort execution with a formatted error.",
        deprecated: None,
        lowering: BuiltinMacroLowering::Panic,
    },
    passthrough_macro(BuiltinId::MacroAssert, "assert", "assert!(condition)"),
    passthrough_macro(
        BuiltinId::MacroAssertEq,
        "assert_eq",
        "assert_eq!(left, right)",
    ),
    passthrough_macro(
        BuiltinId::MacroAssertNe,
        "assert_ne",
        "assert_ne!(left, right)",
    ),
    passthrough_macro(BuiltinId::MacroUnreachable, "unreachable", "unreachable!()"),
    passthrough_macro(
        BuiltinId::MacroUnimplemented,
        "unimplemented",
        "unimplemented!()",
    ),
    passthrough_macro(BuiltinId::MacroTodo, "todo", "todo!()"),
    passthrough_macro(BuiltinId::MacroDbg, "dbg", "dbg!(value)"),
    passthrough_macro(
        BuiltinId::MacroIncludeStr,
        "include_str",
        "include_str!(path)",
    ),
    passthrough_macro(
        BuiltinId::MacroIncludeBytes,
        "include_bytes",
        "include_bytes!(path)",
    ),
];

const fn passthrough_macro(
    id: BuiltinId,
    name: &'static str,
    signature: &'static str,
) -> BuiltinMacroSpec {
    BuiltinMacroSpec {
        id,
        name,
        delimiter: MacroDelimiter::Parentheses,
        signature,
        return_type: "?",
        documentation: "Host-provided builtin macro.",
        deprecated: None,
        lowering: BuiltinMacroLowering::Passthrough,
    }
}

pub fn builtin_macro(name: &str) -> Option<&'static BuiltinMacroSpec> {
    BUILTIN_MACROS.iter().find(|spec| spec.name == name)
}

pub fn builtin_macro_by_id(id: BuiltinId) -> Option<&'static BuiltinMacroSpec> {
    BUILTIN_MACROS.iter().find(|spec| spec.id == id)
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
    fn builtin_macro_names_are_unique() {
        let mut names = std::collections::BTreeSet::new();
        for spec in BUILTIN_MACROS {
            assert!(names.insert(spec.name));
            assert_eq!(builtin_macro(spec.name), Some(spec));
        }
    }

    #[test]
    fn builtin_manifest_is_complete_and_stable() {
        assert_eq!(BUILTIN_MANIFEST_VERSION, 1);
        assert_eq!(BUILTIN_TYPES.len(), 6);
    }
}
