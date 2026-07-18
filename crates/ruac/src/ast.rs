//! Compact compiler-owned AST. It preserves source order, semantic ranges, and
//! normalized API documentation without retaining Rowan trivia.

use crate::token::SourceRange;
use rua_core::Attribute;

#[derive(Debug, Clone)]
pub struct Program {
    pub items: Vec<Item>,
    /// Executable statements in the root Lua-style chunk.
    pub chunk: Block,
    /// Source ordering between declarations and executable statements.
    pub source_order: Vec<ChunkEntry>,
    /// The root source is a declaration-only `.ruai` input.
    pub is_decl: bool,
    /// Versioned standard-library bindings installed by the host. Parsers leave
    /// this empty; `load_builtins` fills it together with declaration items.
    pub(crate) standard_library: Option<crate::builtins::StandardMetadata>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkEntry {
    Item(usize),
    Statement(usize),
}

/// Stable expression identity within one parsed file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExprId {
    pub file: u32,
    pub local: u32,
}

/// Stable identity for a path-bearing pattern within one parsed file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PatternId {
    pub file: u32,
    pub local: u32,
}

/// Stable identity for a path-bearing type use within one parsed file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeId {
    pub file: u32,
    pub local: u32,
}

/// Stable identity for one trait path used as a generic bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TraitRefId {
    pub file: u32,
    pub local: u32,
}

/// Stable identity for a declared generic type parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GenericParamId {
    pub file: u32,
    pub local: u32,
}

#[derive(Debug, Clone)]
pub enum Item {
    Annotation(AnnotationDecl),
    Fn(FnDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Impl(ImplDecl),
    Trait(TraitDecl),
    Extern(ExternBlock),
    /// Compiler-internal module node synthesized from a source path.
    Mod(ModDecl),
    /// `use a::b::c;` / `use a::b as c;` / `use a::b::{c, d};`.
    Use(UseDecl),
}

impl Item {
    pub fn attributes(&self) -> &[Attribute] {
        match self {
            Self::Annotation(item) => &item.attributes,
            Self::Fn(item) => &item.attributes,
            Self::Struct(item) => &item.attributes,
            Self::Enum(item) => &item.attributes,
            Self::Impl(item) => &item.attributes,
            Self::Trait(item) => &item.attributes,
            Self::Extern(item) => &item.attributes,
            Self::Mod(item) => &item.attributes,
            Self::Use(item) => &item.attributes,
        }
    }

    pub fn set_attributes(&mut self, attributes: Vec<Attribute>) {
        match self {
            Self::Annotation(item) => item.attributes = attributes,
            Self::Fn(item) => item.attributes = attributes,
            Self::Struct(item) => item.attributes = attributes,
            Self::Enum(item) => item.attributes = attributes,
            Self::Impl(item) => item.attributes = attributes,
            Self::Trait(item) => item.attributes = attributes,
            Self::Extern(item) => item.attributes = attributes,
            Self::Mod(item) => item.attributes = attributes,
            Self::Use(item) => item.attributes = attributes,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AnnotationDecl {
    pub name: String,
    pub name_span: SourceRange,
    pub attributes: Vec<Attribute>,
    pub documentation: Option<String>,
    pub params: Vec<Param>,
    pub is_pub: bool,
}

#[derive(Debug, Clone)]
pub struct ModDecl {
    pub name: String,
    pub attributes: Vec<Attribute>,
    pub documentation: Option<String>,
    pub items: Vec<Item>,
    /// Executable statements owned by this module's initialization chunk.
    pub chunk: Block,
    /// Source ordering between module declarations and executable statements.
    pub source_order: Vec<ChunkEntry>,
    pub is_pub: bool,
    /// `true` when this module has a physical source or declaration file.
    pub is_file: bool,
    /// `true` when the module's items came from a `.ruai` declaration file: they
    /// are registered with the checkers but emit no definitions. Referenced
    /// declaration modules are loaded with `require` by codegen.
    pub is_decl: bool,
}

#[derive(Debug, Clone)]
pub struct UseDecl {
    pub attributes: Vec<Attribute>,
    pub imports: Vec<UseImport>,
}

#[derive(Debug, Clone)]
pub struct UseImport {
    /// Fully-qualified path segments; the last is the imported leaf name.
    pub path: Vec<String>,
    /// Optional local alias (`use a::b as c`); defaults to the last segment.
    pub alias: Option<String>,
}

/// Declares ambient Lua symbols. `lua` binds a host global directly, while
/// `lua-result` emits an explicit tagged-Result/multi-return adapter.
#[derive(Debug, Clone)]
pub struct ExternBlock {
    pub attributes: Vec<Attribute>,
    pub abi: String,
    pub documentation: Option<String>,
    pub fns: Vec<ExternFn>,
}

#[derive(Debug, Clone)]
pub struct ExternFn {
    pub name: String,
    pub attributes: Vec<Attribute>,
    pub name_span: SourceRange,
    pub documentation: Option<String>,
    pub generics: Vec<GenericParam>,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    /// `true` when the last parameter is `...` (variadic Lua function).
    pub variadic: bool,
}

/// A generic type parameter with optional trait bounds, e.g. `T: Add + Clone`.
/// Bounds are validated (must name real traits) but otherwise erased at codegen;
/// the type checker uses them to resolve method calls on generic-typed values.
#[derive(Debug, Clone)]
pub struct GenericParam {
    pub id: GenericParamId,
    pub name: String,
    pub bounds: Vec<TraitRef>,
}

#[derive(Debug, Clone)]
pub struct TraitRef {
    pub id: TraitRefId,
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct TraitDecl {
    pub name: String,
    pub attributes: Vec<Attribute>,
    pub documentation: Option<String>,
    pub generics: Vec<GenericParam>,
    pub methods: Vec<TraitMethod>,
    pub is_pub: bool,
}

#[derive(Debug, Clone)]
pub struct TraitMethod {
    pub name: String,
    pub attributes: Vec<Attribute>,
    pub documentation: Option<String>,
    /// Byte-span of the method name identifier (definition site).
    pub name_span: SourceRange,
    /// Method-level generic parameters, e.g. `fn wrap<U: Clone>(&self, x: U)`.
    pub generics: Vec<GenericParam>,
    pub has_self: bool,
    pub receiver_mutable: bool,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    /// `Some` for a default method with a body; `None` for a signature only.
    pub default: Option<Block>,
}

#[derive(Debug, Clone)]
pub struct StructDecl {
    pub name: String,
    pub attributes: Vec<Attribute>,
    pub documentation: Option<String>,
    pub generics: Vec<GenericParam>,
    pub fields: Vec<Field>,
    pub is_pub: bool,
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub attributes: Vec<Attribute>,
    pub documentation: Option<String>,
    pub ty: Type,
    /// Byte-span of the field name identifier (definition site).
    pub name_span: SourceRange,
}

#[derive(Debug, Clone)]
pub struct EnumDecl {
    pub name: String,
    pub attributes: Vec<Attribute>,
    pub documentation: Option<String>,
    pub generics: Vec<GenericParam>,
    pub variants: Vec<Variant>,
    pub is_pub: bool,
}

#[derive(Debug, Clone)]
pub struct Variant {
    pub name: String,
    pub attributes: Vec<Attribute>,
    pub documentation: Option<String>,
    pub kind: VariantKind,
}

#[derive(Debug, Clone)]
pub enum VariantKind {
    Unit,
    Tuple(Vec<Type>),
    Struct(Vec<Field>),
}

#[derive(Debug, Clone)]
pub struct ImplDecl {
    pub attributes: Vec<Attribute>,
    /// Generic parameters of the impl itself, e.g. `impl<T> Foo<T>`.
    pub generics: Vec<GenericParam>,
    /// Type the methods are attached to.
    pub type_name: String,
    /// `impl Trait for Type` records the trait name (unused by codegen for now).
    pub trait_name: Option<String>,
    pub methods: Vec<FnDecl>,
}

#[derive(Debug, Clone)]
pub struct FnDecl {
    pub name: String,
    pub attributes: Vec<Attribute>,
    pub documentation: Option<String>,
    /// Byte-span of the function name identifier (definition site).
    pub name_span: SourceRange,
    pub generics: Vec<GenericParam>,
    pub is_pub: bool,
    /// True when the first parameter is a `self`/`&self`/`&mut self` receiver
    /// (that receiver is not included in `params`).
    pub has_self: bool,
    /// True only for an explicit `&mut self` receiver.
    pub receiver_mutable: bool,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    pub body: Block,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    /// Byte span of the parameter name identifier (for LSP hover/typing).
    pub name_span: SourceRange,
    pub ty: Type,
}

/// Types are parsed but only lightly used in this pass (erased at codegen).
#[derive(Debug, Clone)]
pub enum Type {
    /// A named/path type, optionally with generic args, e.g. `Vec<i64>`.
    Path {
        id: TypeId,
        name: String,
        args: Vec<Type>,
    },
    /// Reference type `&T` / `&mut T` (the `mut` flag is retained for later).
    Ref { mutable: bool, inner: Box<Type> },
    /// Callable type `fn(A, B) -> R`.
    Function { params: Vec<Type>, ret: Box<Type> },
    /// Tuple type `(A, B, ...)`.
    Tuple(Vec<Type>),
    /// Never type `!` for expressions that do not return.
    Never,
    /// Unit `()`.
    Unit,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    /// Whether each statement was preceded by at least one empty source line.
    /// This stays parallel to `stmts` and is consumed only by presentation.
    pub statement_blank_before: Vec<bool>,
    /// Trailing expression (block value), if the block ends without `;`.
    pub tail: Option<Box<Expr>>,
    /// Whether the trailing expression was preceded by an empty source line.
    pub tail_blank_before: bool,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    /// Raw Lua emitted verbatim inside the surrounding generated chunk.
    Lua {
        code: String,
        span: SourceRange,
    },
    Let {
        name: String,
        /// Byte span of the binding name identifier (for LSP hover/typing).
        name_span: SourceRange,
        mutable: bool,
        ty: Option<Type>,
        init: Expr,
    },
    /// An expression used for its effect (call, assignment, `if` used as stmt).
    Expr(Expr),
    Return(Option<Expr>),
    While {
        cond: Expr,
        body: Block,
    },
    Loop {
        body: Block,
    },
    /// `for <var> in <iter> { ... }` (single-identifier binding for now).
    For {
        var: String,
        /// Byte span of the loop-variable identifier (for LSP hover/typing).
        var_span: SourceRange,
        iter: Expr,
        body: Block,
    },
    /// `while let PAT = EXPR { ... }`.
    WhileLet {
        pat: Box<Pattern>,
        expr: Expr,
        body: Block,
    },
    Break(Option<Expr>),
    Continue,
}

/// An expression plus its source span (for diagnostics and, later, typing).
#[derive(Debug, Clone)]
pub struct Expr {
    pub id: ExprId,
    pub kind: ExprKind,
    pub span: SourceRange,
}

impl Expr {
    pub fn new(id: ExprId, kind: ExprKind, span: SourceRange) -> Self {
        Expr { id, kind, span }
    }
}

#[derive(Debug, Clone)]
pub struct ClosureParam {
    pub name: String,
    pub name_span: SourceRange,
    pub ty: Option<Type>,
}

#[derive(Debug, Clone)]
pub enum ClosureBody {
    Expr(Box<Expr>),
    Block(Block),
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    Int(String),
    Float(String),
    Str(String),
    Bool(bool),
    VecLit(Vec<Expr>),
    Closure {
        params: Vec<ClosureParam>,
        ret: Option<Type>,
        body: ClosureBody,
    },
    /// A bare name or `::`-joined path, e.g. `x`, `Foo::bar`.
    Path(Vec<String>),
    Unary {
        op: UnOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `loop { ... }`, whose value is supplied by `break value;`.
    Loop(Block),
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    /// `recv.method(args)` — always a method call in Rust syntax.
    MethodCall {
        recv: Box<Expr>,
        method: String,
        optional: bool,
        /// Explicit method type arguments from a turbofish, e.g. the
        /// `Vec<i64>` in `.collect::<Vec<i64>>()`.
        type_args: Vec<Type>,
        args: Vec<Expr>,
        /// Byte-span of the method name identifier (use site), for LSP member
        /// resolution. Not used by codegen/checks.
        method_span: SourceRange,
    },
    Field {
        base: Box<Expr>,
        name: String,
        optional: bool,
        /// Byte-span of the field name identifier (use site), for LSP member
        /// resolution. Not used by codegen/checks.
        name_span: SourceRange,
    },
    /// Struct / struct-variant literal: `Path { f: e, .. }`.
    StructLit {
        path: Vec<String>,
        fields: Vec<(String, Expr)>,
    },
    /// Strongly typed `HashMap` literal: `#{ key: value, ... }`.
    MapLit(Vec<(Expr, Expr)>),
    /// The `?` postfix operator (Result propagation; see docs §4.8).
    Try {
        expr: Box<Expr>,
    },
    Match {
        scrut: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    /// `start..end` / `start..=end` (mainly for `for` loops).
    Range {
        start: Box<Expr>,
        end: Box<Expr>,
        inclusive: bool,
    },
    /// `base[index]` (1-based, matching Lua tables).
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    If {
        cond: Box<Expr>,
        then_block: Block,
        else_block: Option<Box<ElseBranch>>,
    },
    /// `if let PAT = EXPR { .. } else { .. }`.
    IfLet {
        pat: Box<Pattern>,
        expr: Box<Expr>,
        then_block: Block,
        else_block: Option<Box<ElseBranch>>,
    },
    Block(Block),
    Assign {
        /// `None` for `=`, otherwise the arithmetic operation in `op=`.
        op: Option<BinOp>,
        target: Box<Expr>,
        value: Box<Expr>,
    },
}

#[derive(Debug, Clone)]
pub enum ElseBranch {
    /// `else { ... }`
    Block(Block),
    /// `else if ...`
    If(Expr),
}

#[derive(Debug, Clone)]
pub struct MatchArm {
    /// Or-patterns: `a | b | c`. Multiple patterns are only supported when they
    /// bind no variables.
    pub pats: Vec<Pattern>,
    pub guard: Option<Expr>,
    pub body: Expr,
}

#[derive(Debug, Clone)]
pub enum Pattern {
    /// `_`
    Wildcard,
    /// A lowercase binding, e.g. `x`. Carries the binding name's byte span
    /// (for LSP hover/typing).
    Binding(String, SourceRange),
    /// A literal: int/float/str/bool.
    Literal(Expr),
    /// Range `lo..=hi` or `lo..hi`.
    Range {
        lo: Box<Expr>,
        hi: Box<Expr>,
        inclusive: bool,
    },
    /// A path with no payload: unit enum variant or `None`, e.g. `Shape::Unit`.
    Path { id: PatternId, path: Vec<String> },
    /// Tuple variant / built-in payload: `Shape::Circle(r)`, `Some(x)`, `Ok(v)`.
    TupleVariant {
        id: PatternId,
        path: Vec<String>,
        elems: Vec<Pattern>,
    },
    /// Struct pattern / struct variant: `Point { x, y }`, `Shape::Rect { w, h }`.
    StructVariant {
        id: PatternId,
        path: Vec<String>,
        fields: Vec<(String, Pattern)>,
        rest: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg, // -
    Not, // !
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Coalesce,
    Contains,
}

impl BinOp {
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Rem => "%",
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::And => "&&",
            Self::Or => "||",
            Self::Coalesce => "??",
            Self::Contains => "in",
        }
    }
}
