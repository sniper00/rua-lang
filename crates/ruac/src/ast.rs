//! Abstract syntax tree for the Rua subset implemented in this pass.
//!
//! Scope (P0 + core of P1): items are free functions; statements cover
//! `let`/`let mut`, expression statements, `return`, `while`, `loop`, `break`,
//! `continue`; expressions cover literals, identifiers/paths, unary/binary ops,
//! calls, method-less field access, `if`/`else` (as expression), blocks, and
//! assignment. `struct`/`enum`/`trait`/`impl`/`match`/generics come in later
//! passes (see docs/rua-design.md roadmap).

use crate::token::SourceRange;

#[derive(Debug, Clone)]
pub struct Program {
    pub items: Vec<Item>,
}

#[derive(Debug, Clone)]
pub enum Item {
    Fn(FnDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Impl(ImplDecl),
    Trait(TraitDecl),
    Extern(ExternBlock),
    /// `mod name { items }` (inline module; nested modules allowed).
    Mod(ModDecl),
    /// `use a::b::c;` / `use a::b as c;` / `use a::b::{c, d};`.
    Use(UseDecl),
}

#[derive(Debug, Clone)]
pub struct ModDecl {
    pub name: String,
    pub items: Vec<Item>,
    pub is_pub: bool,
    /// `true` for a file module (`mod name;`) whose `items` are loaded from a
    /// sibling `.rua` file during resolution; `false` for an inline `mod { .. }`.
    pub is_file: bool,
    /// `true` when the module's items came from a `.ruai` declaration file: they
    /// are registered with the checkers but emit **no** Lua (references resolve to
    /// host-provided globals, e.g. `moon`). Set during resolution.
    pub is_decl: bool,
}

#[derive(Debug, Clone)]
pub struct UseDecl {
    pub imports: Vec<UseImport>,
}

#[derive(Debug, Clone)]
pub struct UseImport {
    /// Fully-qualified path segments; the last is the imported leaf name.
    pub path: Vec<String>,
    /// Optional local alias (`use a::b as c`); defaults to the last segment.
    pub alias: Option<String>,
}

/// `extern "lua" { fn name(params) -> R; ... }` — declares ambient Lua symbols
/// so the checker knows they exist. No Lua code is emitted for the block.
#[derive(Debug, Clone)]
pub struct ExternBlock {
    pub abi: String,
    pub fns: Vec<ExternFn>,
}

#[derive(Debug, Clone)]
pub struct ExternFn {
    pub name: String,
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
    pub name: String,
    pub bounds: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TraitDecl {
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub methods: Vec<TraitMethod>,
    pub is_pub: bool,
}

#[derive(Debug, Clone)]
pub struct TraitMethod {
    pub name: String,
    /// Byte-span of the method name identifier (definition site).
    pub name_span: SourceRange,
    /// Method-level generic parameters, e.g. `fn wrap<U: Clone>(&self, x: U)`.
    pub generics: Vec<GenericParam>,
    pub has_self: bool,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    /// `Some` for a default method with a body; `None` for a signature only.
    pub default: Option<Block>,
}

#[derive(Debug, Clone)]
pub struct StructDecl {
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub fields: Vec<Field>,
    pub is_pub: bool,
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub ty: Type,
    /// Byte-span of the field name identifier (definition site).
    pub name_span: SourceRange,
}

#[derive(Debug, Clone)]
pub struct EnumDecl {
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub variants: Vec<Variant>,
    pub is_pub: bool,
}

#[derive(Debug, Clone)]
pub struct Variant {
    pub name: String,
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
    /// Byte-span of the function name identifier (definition site).
    pub name_span: SourceRange,
    pub generics: Vec<GenericParam>,
    pub is_pub: bool,
    /// True when the first parameter is a `self`/`&self`/`&mut self` receiver
    /// (that receiver is not included in `params`).
    pub has_self: bool,
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
    Path { name: String, args: Vec<Type> },
    /// Reference type `&T` / `&mut T` (the `mut` flag is retained for later).
    Ref { mutable: bool, inner: Box<Type> },
    /// Unit `()`.
    Unit,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    /// Trailing expression (block value), if the block ends without `;`.
    pub tail: Option<Box<Expr>>,
}

#[derive(Debug, Clone)]
pub enum Stmt {
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
    Break,
    Continue,
}

/// An expression plus its source span (for diagnostics and, later, typing).
#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: SourceRange,
}

impl Expr {
    pub fn new(kind: ExprKind, span: SourceRange) -> Self {
        Expr { kind, span }
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
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    /// `recv.method(args)` — always a method call in Rust syntax.
    MethodCall {
        recv: Box<Expr>,
        method: String,
        args: Vec<Expr>,
        /// Byte-span of the method name identifier (use site), for LSP member
        /// resolution. Not used by codegen/checks.
        method_span: SourceRange,
    },
    Field {
        base: Box<Expr>,
        name: String,
        /// Byte-span of the field name identifier (use site), for LSP member
        /// resolution. Not used by codegen/checks.
        name_span: SourceRange,
    },
    /// Struct / struct-variant literal: `Path { f: e, .. }`.
    StructLit {
        path: Vec<String>,
        fields: Vec<(String, Expr)>,
    },
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
    /// `base[index]` (0-based, matching Rust).
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    /// `name!(args)` — built-in macros: `vec!`, `println!`, `print!`,
    /// `format!`, `panic!`.
    MacroCall {
        name: String,
        args: Vec<Expr>,
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
    Path(Vec<String>),
    /// Tuple variant / built-in payload: `Shape::Circle(r)`, `Some(x)`, `Ok(v)`.
    TupleVariant {
        path: Vec<String>,
        elems: Vec<Pattern>,
    },
    /// Struct pattern / struct variant: `Point { x, y }`, `Shape::Rect { w, h }`.
    StructVariant {
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
}
