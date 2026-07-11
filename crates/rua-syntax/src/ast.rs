//! Typed `AstNode` accessors over the untyped rowan CST (rust-analyzer style).
//!
//! Each grammar node gets a thin newtype wrapping a [`SyntaxNode`] plus typed
//! getters (`name()`, `params()`, `body()`, …). Tooling (formatter, LSP) walks
//! the tree through these instead of matching raw `SyntaxKind`s. Getters are
//! **best-effort and total**: they return `Option`/iterators and never panic,
//! matching the parser's error-resilient trees.

use crate::kind::{SyntaxKind as K, SyntaxElement, SyntaxNode, SyntaxToken};

/// A typed view of a [`SyntaxNode`] of a specific kind.
pub trait AstNode: Sized {
    fn can_cast(kind: SyntaxKind) -> bool;
    fn cast(node: SyntaxNode) -> Option<Self>;
    fn syntax(&self) -> &SyntaxNode;
}

pub use crate::kind::SyntaxKind;

// --- traversal helpers -----------------------------------------------------

fn child<N: AstNode>(node: &SyntaxNode) -> Option<N> {
    node.children().find_map(N::cast)
}

fn children<N: AstNode + 'static>(node: &SyntaxNode) -> impl Iterator<Item = N> + '_ {
    node.children().filter_map(N::cast)
}

fn nth_child<N: AstNode>(node: &SyntaxNode, n: usize) -> Option<N> {
    node.children().filter_map(N::cast).nth(n)
}

/// First direct child token of the given kind.
fn token(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == kind)
}

/// All direct child tokens of the given kind, in order.
fn tokens(node: &SyntaxNode, kind: SyntaxKind) -> impl Iterator<Item = SyntaxToken> + '_ {
    node.children_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(move |t| t.kind() == kind)
}

/// The identifier that names a declaration (its first direct `Ident` token).
fn name_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    token(node, K::Ident)
}

/// Declare a simple newtype `AstNode` for one node kind.
macro_rules! ast_node {
    ($(#[$m:meta])* $name:ident = $kind:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name {
            syntax: SyntaxNode,
        }
        impl AstNode for $name {
            fn can_cast(kind: SyntaxKind) -> bool {
                kind == K::$kind
            }
            fn cast(node: SyntaxNode) -> Option<Self> {
                if node.kind() == K::$kind {
                    Some($name { syntax: node })
                } else {
                    None
                }
            }
            fn syntax(&self) -> &SyntaxNode {
                &self.syntax
            }
        }
    };
}

/// Convenience: the text of a declaration's name.
pub trait Named: AstNode {
    fn name(&self) -> Option<SyntaxToken> {
        name_token(self.syntax())
    }
    fn name_text(&self) -> Option<String> {
        self.name().map(|t| t.text().to_string())
    }
}

// --- root & items ----------------------------------------------------------

ast_node!(
    /// The whole file returned by [`parse_source_file`](crate::parse_source_file).
    SourceFile = SourceFile
);

impl SourceFile {
    pub fn cast_root(node: SyntaxNode) -> Option<Self> {
        Self::cast(node)
    }
    pub fn items(&self) -> impl Iterator<Item = Item> + '_ {
        children::<Item>(&self.syntax)
    }
}

ast_node!(FnDecl = FnDecl);
ast_node!(StructDecl = StructDecl);
ast_node!(EnumDecl = EnumDecl);
ast_node!(TraitDecl = TraitDecl);
ast_node!(ImplDecl = ImplDecl);
ast_node!(ExternBlock = ExternBlock);
ast_node!(ExternFn = ExternFn);
ast_node!(ModDecl = ModDecl);
ast_node!(UseDecl = UseDecl);

impl Named for FnDecl {}
impl Named for StructDecl {}
impl Named for EnumDecl {}
impl Named for TraitDecl {}
impl Named for ExternFn {}
impl Named for ModDecl {}

/// Any top-level (or module-level) item.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Item {
    Fn(FnDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Trait(TraitDecl),
    Impl(ImplDecl),
    Extern(ExternBlock),
    Mod(ModDecl),
    Use(UseDecl),
}

impl AstNode for Item {
    fn can_cast(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            K::FnDecl
                | K::StructDecl
                | K::EnumDecl
                | K::TraitDecl
                | K::ImplDecl
                | K::ExternBlock
                | K::ModDecl
                | K::UseDecl
        )
    }
    fn cast(node: SyntaxNode) -> Option<Self> {
        Some(match node.kind() {
            K::FnDecl => Item::Fn(FnDecl { syntax: node }),
            K::StructDecl => Item::Struct(StructDecl { syntax: node }),
            K::EnumDecl => Item::Enum(EnumDecl { syntax: node }),
            K::TraitDecl => Item::Trait(TraitDecl { syntax: node }),
            K::ImplDecl => Item::Impl(ImplDecl { syntax: node }),
            K::ExternBlock => Item::Extern(ExternBlock { syntax: node }),
            K::ModDecl => Item::Mod(ModDecl { syntax: node }),
            K::UseDecl => Item::Use(UseDecl { syntax: node }),
            _ => return None,
        })
    }
    fn syntax(&self) -> &SyntaxNode {
        match self {
            Item::Fn(n) => n.syntax(),
            Item::Struct(n) => n.syntax(),
            Item::Enum(n) => n.syntax(),
            Item::Trait(n) => n.syntax(),
            Item::Impl(n) => n.syntax(),
            Item::Extern(n) => n.syntax(),
            Item::Mod(n) => n.syntax(),
            Item::Use(n) => n.syntax(),
        }
    }
}

impl FnDecl {
    pub fn is_pub(&self) -> bool {
        token(&self.syntax, K::KwPub).is_some()
    }
    pub fn generic_params(&self) -> Option<GenericParams> {
        child(&self.syntax)
    }
    pub fn params(&self) -> impl Iterator<Item = Param> + '_ {
        children::<Param>(&self.syntax)
    }
    /// Return type node (the type after `->`; it is the only `Type` that is a
    /// direct child, since parameter types are nested inside `Param`).
    pub fn ret_type(&self) -> Option<Type> {
        child::<Type>(&self.syntax)
    }
    pub fn where_clause(&self) -> Option<WhereClause> {
        child(&self.syntax)
    }
    pub fn body(&self) -> Option<Block> {
        child(&self.syntax)
    }
    /// True when the signature declares a `self` receiver.
    pub fn has_self(&self) -> bool {
        self_receiver_token(&self.syntax).is_some()
    }

    pub fn receiver(&self) -> Option<ReceiverKind> {
        receiver_kind(&self.syntax)
    }
    pub fn receiver_token(&self) -> Option<SyntaxToken> {
        self_receiver_token(&self.syntax)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReceiverKind {
    Value,
    SharedRef,
    MutRef,
}

fn receiver_kind(node: &SyntaxNode) -> Option<ReceiverKind> {
    let tokens: Vec<_> = node
        .children_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| !token.kind().is_trivia())
        .collect();
    let receiver_token = self_receiver_token(node)?;
    let receiver = tokens
        .iter()
        .position(|token| token == &receiver_token)?;
    Some(match tokens.get(receiver.wrapping_sub(1)).map(|token| token.kind()) {
        Some(K::KwMut)
            if tokens
                .get(receiver.wrapping_sub(2))
                .is_some_and(|token| token.kind() == K::Amp) =>
        {
            ReceiverKind::MutRef
        }
        Some(K::Amp) => ReceiverKind::SharedRef,
        _ => ReceiverKind::Value,
    })
}

fn self_receiver_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    let left = token(node, K::LParen)?.text_range().end();
    let right = token(node, K::RParen).map(|token| token.text_range().start());
    tokens(node, K::KwSelf).find(|token| {
        left <= token.text_range().start()
            && right.is_none_or(|right| token.text_range().end() <= right)
    })
}

ast_node!(GenericParams = GenericParams);
ast_node!(GenericParam = GenericParam);
ast_node!(WhereClause = WhereClause);
ast_node!(Param = Param);
ast_node!(FieldList = FieldList);
ast_node!(FieldDecl = FieldDecl);
ast_node!(VariantList = VariantList);
ast_node!(EnumVariant = EnumVariant);
ast_node!(TraitMethod = TraitMethod);

impl Named for GenericParam {}
impl Named for Param {}
impl Named for FieldDecl {}
impl Named for EnumVariant {}
impl Named for TraitMethod {}

impl GenericParams {
    pub fn params(&self) -> impl Iterator<Item = GenericParam> + '_ {
        children::<GenericParam>(&self.syntax)
    }
}

impl GenericParam {
    /// Trait bounds: `T: Clone + Eq` → ["Clone", "Eq"].
    /// Looks for identifiers after the `:` token, separated by `+`.
    pub fn bounds(&self) -> impl Iterator<Item = SyntaxToken> + '_ {
        // Only return bounds if there's a colon; without one, there are no
        // bounds (the sole Ident is the param name).
        let colon = token(&self.syntax, K::Colon);
        let empty: Box<dyn Iterator<Item = SyntaxToken>> = Box::new(std::iter::empty());
        match colon {
            Some(c) => {
                let after = c.text_range().start();
                Box::new(
                    tokens(&self.syntax, K::Ident)
                        .filter(move |t| t.text_range().start() > after),
                )
                    as Box<dyn Iterator<Item = SyntaxToken>>
            }
            None => empty,
        }
    }
}

impl WhereClause {
    /// Iterate where-predicate left-hand sides. Each predicate is of the form
    /// `T::Assoc` or `T` followed by `:` and bounds. Returns the first
    /// identifier of each predicate (the type being constrained).
    pub fn predicates(&self) -> impl Iterator<Item = WherePred> + '_ {
        // Skip the initial `where` contextual keyword (first Ident token).
        let first_ident = self
            .syntax
            .children_with_tokens()
            .filter_map(|e| e.into_token())
            .position(|t| t.kind() == K::Ident);
        WherePredIter {
            tokens: self.syntax.children_with_tokens().collect::<Vec<_>>(),
            pos: first_ident.map(|p| p + 1).unwrap_or(0), // +1 to skip the `where`
        }
    }
}

/// A single predicate inside a [`WhereClause`]: `T: A + B` or `T::Item: A`.
pub struct WherePred {
    /// All identifiers in the left-hand side (e.g. `T::Item` → ["T", "Item"]).
    pub lhs: Vec<SyntaxToken>,
    /// Identifiers that are trait bounds (right-hand side after `:`).
    pub bounds: Vec<SyntaxToken>,
}

struct WherePredIter {
    tokens: Vec<SyntaxElement>,
    pos: usize,
}

impl Iterator for WherePredIter {
    type Item = WherePred;

    fn next(&mut self) -> Option<WherePred> {
        // Skip until we find an Ident that starts a new predicate.
        while self.pos < self.tokens.len() {
            let t = &self.tokens[self.pos];
            if let Some(tok) = t.as_token()
                && tok.kind() == K::Ident
            {
                break;
            }
            self.pos += 1;
        }
        if self.pos >= self.tokens.len() {
            return None;
        }

        let mut lhs = Vec::new();
        let mut bounds = Vec::new();
        let mut in_bounds = false;

        while self.pos < self.tokens.len() {
            let t = &self.tokens[self.pos];
            if let Some(tok) = t.as_token() {
                match tok.kind() {
                    K::Ident => {
                        if in_bounds {
                            bounds.push(tok.clone());
                        } else {
                            lhs.push(tok.clone());
                        }
                    }
                    K::Colon => {
                        // The ident we just added was the last LHS part.
                        in_bounds = true;
                    }
                    K::ColonColon => {
                        // Path separator, next ident is still LHS.
                    }
                    K::Plus => {
                        // Bound separator, next ident is another bound.
                    }
                    K::Comma => {
                        // End of this predicate.
                        self.pos += 1;
                        break;
                    }
                    K::Lt => {
                        // Skip type arguments on bounds (e.g. `Iterator<Item=U>`).
                        let mut depth = 1i32;
                        self.pos += 1;
                        while self.pos < self.tokens.len() && depth > 0 {
                            if let Some(t2) = self.tokens[self.pos].as_token() {
                                match t2.kind() {
                                    K::Lt => depth += 1,
                                    K::Gt => depth -= 1,
                                    _ => {}
                                }
                            }
                            self.pos += 1;
                        }
                        continue;
                    }
                    _ => {}
                }
            }
            self.pos += 1;
        }

        if lhs.is_empty() {
            None
        } else {
            Some(WherePred { lhs, bounds })
        }
    }
}

impl Param {
    pub fn ty(&self) -> Option<Type> {
        child::<Type>(&self.syntax)
    }
}

impl FieldDecl {
    pub fn ty(&self) -> Option<Type> {
        child::<Type>(&self.syntax)
    }
    pub fn is_pub(&self) -> bool {
        token(&self.syntax, K::KwPub).is_some()
    }
}

impl StructDecl {
    pub fn is_pub(&self) -> bool {
        token(&self.syntax, K::KwPub).is_some()
    }
    pub fn generic_params(&self) -> Option<GenericParams> {
        child(&self.syntax)
    }
    pub fn where_clause(&self) -> Option<WhereClause> {
        child(&self.syntax)
    }
    pub fn field_list(&self) -> Option<FieldList> {
        child(&self.syntax)
    }
}

impl FieldList {
    pub fn fields(&self) -> impl Iterator<Item = FieldDecl> + '_ {
        children::<FieldDecl>(&self.syntax)
    }
}

impl EnumDecl {
    pub fn is_pub(&self) -> bool {
        token(&self.syntax, K::KwPub).is_some()
    }
    pub fn generic_params(&self) -> Option<GenericParams> {
        child(&self.syntax)
    }
    pub fn where_clause(&self) -> Option<WhereClause> {
        child(&self.syntax)
    }
    pub fn variant_list(&self) -> Option<VariantList> {
        child(&self.syntax)
    }
}

impl VariantList {
    pub fn variants(&self) -> impl Iterator<Item = EnumVariant> + '_ {
        children::<EnumVariant>(&self.syntax)
    }
}

impl EnumVariant {
    /// Tuple payload types, if this is a tuple variant.
    pub fn tuple_types(&self) -> impl Iterator<Item = Type> + '_ {
        children::<Type>(&self.syntax)
    }
    /// Struct payload fields, if this is a struct variant.
    pub fn field_list(&self) -> Option<FieldList> {
        child(&self.syntax)
    }

    pub fn variant_kind(&self) -> VariantKind {
        if self.field_list().is_some() {
            VariantKind::Struct
        } else if token(&self.syntax, K::LParen).is_some() {
            VariantKind::Tuple
        } else {
            VariantKind::Unit
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VariantKind {
    Unit,
    Tuple,
    Struct,
}

impl TraitDecl {
    pub fn is_pub(&self) -> bool {
        token(&self.syntax, K::KwPub).is_some()
    }
    pub fn generic_params(&self) -> Option<GenericParams> {
        child(&self.syntax)
    }
    pub fn where_clause(&self) -> Option<WhereClause> {
        child(&self.syntax)
    }
    pub fn methods(&self) -> impl Iterator<Item = TraitMethod> + '_ {
        children::<TraitMethod>(&self.syntax)
    }
}

impl TraitMethod {
    /// Method-level generic parameters (e.g. `fn wrap<U: Clone>(...)`).
    pub fn generic_params(&self) -> Option<GenericParams> {
        child(&self.syntax)
    }
    /// Method-level where clause.
    pub fn where_clause(&self) -> Option<WhereClause> {
        child(&self.syntax)
    }
    /// True when the signature declares a `self` receiver.
    pub fn has_self(&self) -> bool {
        self_receiver_token(&self.syntax).is_some()
    }
    pub fn receiver(&self) -> Option<ReceiverKind> {
        receiver_kind(&self.syntax)
    }
    pub fn receiver_token(&self) -> Option<SyntaxToken> {
        self_receiver_token(&self.syntax)
    }
    pub fn params(&self) -> impl Iterator<Item = Param> + '_ {
        children::<Param>(&self.syntax)
    }
    pub fn ret_type(&self) -> Option<Type> {
        child::<Type>(&self.syntax)
    }
    /// Present only for a default method (`fn f() { .. }`).
    pub fn default_body(&self) -> Option<Block> {
        child(&self.syntax)
    }
}

impl ImplDecl {
    pub fn generic_params(&self) -> Option<GenericParams> {
        child(&self.syntax)
    }
    pub fn where_clause(&self) -> Option<WhereClause> {
        child(&self.syntax)
    }
    fn is_trait_impl(&self) -> bool {
        token(&self.syntax, K::KwFor).is_some()
    }
    /// `impl Trait for Type` → the trait name; `None` for an inherent `impl Type`.
    pub fn trait_name(&self) -> Option<SyntaxToken> {
        if self.is_trait_impl() {
            token(&self.syntax, K::Ident)
        } else {
            None
        }
    }
    /// The type the methods are attached to (the `Type` position of the `impl`).
    pub fn type_name(&self) -> Option<SyntaxToken> {
        if self.is_trait_impl() {
            // First identifier appearing after the `for` keyword.
            let for_offset = token(&self.syntax, K::KwFor)?.text_range().start();
            self.syntax
                .children_with_tokens()
                .filter_map(|e| e.into_token())
                .find(|t| t.kind() == K::Ident && t.text_range().start() > for_offset)
        } else {
            token(&self.syntax, K::Ident)
        }
    }
    pub fn methods(&self) -> impl Iterator<Item = FnDecl> + '_ {
        children::<FnDecl>(&self.syntax)
    }
}

impl ExternBlock {
    /// The optional ABI string token (e.g. `"lua"`).
    pub fn abi(&self) -> Option<SyntaxToken> {
        token(&self.syntax, K::Str)
    }
    pub fn fns(&self) -> impl Iterator<Item = ExternFn> + '_ {
        children::<ExternFn>(&self.syntax)
    }
}

impl ExternFn {
    pub fn is_pub(&self) -> bool {
        token(&self.syntax, K::KwPub).is_some()
    }
    /// True when the last parameter is `...` (variadic Lua function).
    pub fn variadic(&self) -> bool {
        token(&self.syntax, K::DotDot).is_some()
    }
    pub fn params(&self) -> impl Iterator<Item = Param> + '_ {
        children::<Param>(&self.syntax)
    }
    pub fn ret_type(&self) -> Option<Type> {
        child::<Type>(&self.syntax)
    }
}

impl UseDecl {
    /// Iterate the individual imports declared in this `use` statement.
    /// Each yielded [`UseImport`] carries the path segments and an optional
    /// alias (`as name`).
    pub fn imports(&self) -> impl Iterator<Item = UseImport> + '_ {
        UseImportIter {
            tokens: self
                .syntax
                .children_with_tokens()
                .filter_map(|e| e.into_token())
                .skip_while(|t| t.kind() != K::Ident) // skip `use`
                .collect::<Vec<_>>(),
            pos: 0,
            group_prefix: Vec::new(),
        }
    }
}

/// A single import in a `use` declaration: `path::to::item` with optional
/// `as alias`.
pub struct UseImport {
    pub path: Vec<SyntaxToken>,
    pub alias: Option<SyntaxToken>,
}

struct UseImportIter {
    tokens: Vec<SyntaxToken>,
    pos: usize,
    group_prefix: Vec<SyntaxToken>,
}

impl Iterator for UseImportIter {
    type Item = UseImport;

    fn next(&mut self) -> Option<UseImport> {
        // Skip non-Ident tokens until we hit an ident that starts the next leaf.
        while self.pos < self.tokens.len() {
            let kind = self.tokens[self.pos].kind();
            if kind == K::Ident || kind == K::KwSelf {
                break;
            }
            if kind == K::LBrace {
                self.pos += 1;
                continue;
            }
            if kind == K::RBrace {
                self.group_prefix.clear();
                self.pos += 1;
                continue;
            }
            if kind == K::Semi || kind == K::KwAs {
                self.pos += 1;
                continue;
            }
            self.pos += 1;
        }
        if self.pos >= self.tokens.len() {
            return None;
        }

        // Collect path segments, skipping trivia between `::` separators.
        let mut path = self.group_prefix.clone();
        loop {
            let kind = self.tokens[self.pos].kind();
            if kind == K::Ident || kind == K::KwSelf {
                path.push(self.tokens[self.pos].clone());
                self.pos += 1;
                // Skip trivia after this ident.
                self.skip_trivia();
            } else {
                break;
            }
            // Look ahead for `::` to continue the path.
            if self.pos < self.tokens.len() && self.tokens[self.pos].kind() == K::ColonColon {
                self.pos += 1;
                self.skip_trivia();
                // After `::` we may have `{` (group).
                if self.pos < self.tokens.len() && self.tokens[self.pos].kind() == K::LBrace {
                    self.pos += 1;
                    self.skip_trivia();
                    self.group_prefix = path.clone();
                }
            } else {
                break;
            }
        }

        if path.is_empty() {
            return None;
        }

        // Check for `as` alias.
        if self.pos < self.tokens.len() && self.tokens[self.pos].kind() == K::KwAs {
            self.pos += 1;
            self.skip_trivia();
            if self.pos < self.tokens.len() && self.tokens[self.pos].kind() == K::Ident {
                let a = self.tokens[self.pos].clone();
                self.pos += 1;
                self.skip_trivia();
                return Some(UseImport {
                    path,
                    alias: Some(a),
                });
            }
        }

        Some(UseImport { path, alias: None })
    }
}

impl UseImportIter {
    fn skip_trivia(&mut self) {
        while self.pos < self.tokens.len() && self.tokens[self.pos].kind().is_trivia() {
            self.pos += 1;
        }
    }
}

impl ModDecl {
    pub fn is_pub(&self) -> bool {
        token(&self.syntax, K::KwPub).is_some()
    }
    /// `true` for a file module (`mod name;`), `false` for inline `mod { .. }`.
    pub fn is_file(&self) -> bool {
        token(&self.syntax, K::LBrace).is_none()
    }
    pub fn items(&self) -> impl Iterator<Item = Item> + '_ {
        children::<Item>(&self.syntax)
    }
}

// --- types -----------------------------------------------------------------

ast_node!(PathType = PathType);
ast_node!(RefType = RefType);
ast_node!(TupleType = TupleType);
ast_node!(TypeArgs = TypeArgs);

/// A type: `Path<..>`, `&T`, or `()`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Path(PathType),
    Ref(RefType),
    Tuple(TupleType),
}

impl AstNode for Type {
    fn can_cast(kind: SyntaxKind) -> bool {
        matches!(kind, K::PathType | K::RefType | K::TupleType)
    }
    fn cast(node: SyntaxNode) -> Option<Self> {
        Some(match node.kind() {
            K::PathType => Type::Path(PathType { syntax: node }),
            K::RefType => Type::Ref(RefType { syntax: node }),
            K::TupleType => Type::Tuple(TupleType { syntax: node }),
            _ => return None,
        })
    }
    fn syntax(&self) -> &SyntaxNode {
        match self {
            Type::Path(n) => n.syntax(),
            Type::Ref(n) => n.syntax(),
            Type::Tuple(n) => n.syntax(),
        }
    }
}

impl PathType {
    /// Path segments (`std`, `Vec`) as identifier tokens.
    pub fn segments(&self) -> impl Iterator<Item = SyntaxToken> + '_ {
        tokens(&self.syntax, K::Ident)
    }
    pub fn type_args(&self) -> Option<TypeArgs> {
        child(&self.syntax)
    }
}

impl RefType {
    pub fn is_mut(&self) -> bool {
        token(&self.syntax, K::KwMut).is_some()
    }
    pub fn inner(&self) -> Option<Type> {
        child::<Type>(&self.syntax)
    }
}

impl TypeArgs {
    pub fn args(&self) -> impl Iterator<Item = Type> + '_ {
        children::<Type>(&self.syntax)
    }
}

// --- statements ------------------------------------------------------------

ast_node!(Block = Block);
ast_node!(LetStmt = LetStmt);
ast_node!(ExprStmt = ExprStmt);
ast_node!(ReturnStmt = ReturnExpr);
ast_node!(WhileStmt = WhileExpr);
ast_node!(LoopStmt = LoopExpr);
ast_node!(ForStmt = ForExpr);
ast_node!(BreakStmt = BreakExpr);
ast_node!(ContinueStmt = ContinueExpr);

/// A statement inside a [`Block`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Stmt {
    Let(LetStmt),
    Expr(ExprStmt),
    Return(ReturnStmt),
    While(WhileStmt),
    Loop(LoopStmt),
    For(ForStmt),
    Break(BreakStmt),
    Continue(ContinueStmt),
}

impl AstNode for Stmt {
    fn can_cast(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            K::LetStmt
                | K::ExprStmt
                | K::ReturnExpr
                | K::WhileExpr
                | K::LoopExpr
                | K::ForExpr
                | K::BreakExpr
                | K::ContinueExpr
        )
    }
    fn cast(node: SyntaxNode) -> Option<Self> {
        Some(match node.kind() {
            K::LetStmt => Stmt::Let(LetStmt { syntax: node }),
            K::ExprStmt => Stmt::Expr(ExprStmt { syntax: node }),
            K::ReturnExpr => Stmt::Return(ReturnStmt { syntax: node }),
            K::WhileExpr => Stmt::While(WhileStmt { syntax: node }),
            K::LoopExpr => Stmt::Loop(LoopStmt { syntax: node }),
            K::ForExpr => Stmt::For(ForStmt { syntax: node }),
            K::BreakExpr => Stmt::Break(BreakStmt { syntax: node }),
            K::ContinueExpr => Stmt::Continue(ContinueStmt { syntax: node }),
            _ => return None,
        })
    }
    fn syntax(&self) -> &SyntaxNode {
        match self {
            Stmt::Let(n) => n.syntax(),
            Stmt::Expr(n) => n.syntax(),
            Stmt::Return(n) => n.syntax(),
            Stmt::While(n) => n.syntax(),
            Stmt::Loop(n) => n.syntax(),
            Stmt::For(n) => n.syntax(),
            Stmt::Break(n) => n.syntax(),
            Stmt::Continue(n) => n.syntax(),
        }
    }
}

impl Block {
    pub fn stmts(&self) -> impl Iterator<Item = Stmt> + '_ {
        children::<Stmt>(&self.syntax)
    }
    /// The trailing expression, if the block's last statement is an expression
    /// without a trailing semicolon (Rust-style block value).
    pub fn tail(&self) -> Option<Expr> {
        let last_stmt = self.syntax.children().filter_map(Stmt::cast).last()?;
        match last_stmt {
            Stmt::Expr(es) => {
                // An ExprStmt with no semicolon is a tail expression.
                if token(es.syntax(), K::Semi).is_none() {
                    es.expr()
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

impl LetStmt {
    pub fn is_mut(&self) -> bool {
        token(&self.syntax, K::KwMut).is_some()
    }
    pub fn name(&self) -> Option<SyntaxToken> {
        name_token(&self.syntax)
    }
    pub fn ty(&self) -> Option<Type> {
        child::<Type>(&self.syntax)
    }
    pub fn init(&self) -> Option<Expr> {
        child::<Expr>(&self.syntax)
    }
}

impl ExprStmt {
    pub fn expr(&self) -> Option<Expr> {
        child::<Expr>(&self.syntax)
    }
    pub fn has_semicolon(&self) -> bool {
        token(&self.syntax, K::Semi).is_some()
    }
}

impl ReturnStmt {
    pub fn value(&self) -> Option<Expr> {
        child::<Expr>(&self.syntax)
    }
}

impl WhileStmt {
    /// `true` for `while let PAT = EXPR { ... }`.
    pub fn is_while_let(&self) -> bool {
        token(&self.syntax, K::KwLet).is_some()
    }
    /// The pattern for `while let PAT = ...`. Returns `None` for plain `while`.
    pub fn let_pattern(&self) -> Option<Pattern> {
        child::<Pattern>(&self.syntax)
    }
    /// The condition (`while COND`) or scrutinee expression (`while let PAT = EXPR`).
    pub fn condition(&self) -> Option<Expr> {
        let body_start = self.body()?.syntax().text_range().start();
        self.syntax
            .children()
            .filter_map(Expr::cast)
            .take_while(|expr| expr.syntax().text_range().start() < body_start)
            .last()
    }
    pub fn body(&self) -> Option<Block> {
        children::<Block>(&self.syntax).last()
    }
}

impl LoopStmt {
    pub fn body(&self) -> Option<Block> {
        child(&self.syntax)
    }
}

impl ForStmt {
    pub fn var(&self) -> Option<SyntaxToken> {
        name_token(&self.syntax)
    }
    pub fn iter(&self) -> Option<Expr> {
        let body_start = self.body()?.syntax().text_range().start();
        self.syntax
            .children()
            .filter_map(Expr::cast)
            .take_while(|expr| expr.syntax().text_range().start() < body_start)
            .last()
    }
    pub fn body(&self) -> Option<Block> {
        children::<Block>(&self.syntax).last()
    }
}

// --- expressions -----------------------------------------------------------

ast_node!(BinExpr = BinExpr);
ast_node!(UnaryExpr = UnaryExpr);
ast_node!(RangeExpr = RangeExpr);
ast_node!(ClosureExpr = ClosureExpr);
ast_node!(AssignExpr = AssignExpr);
ast_node!(TryExpr = TryExpr);
ast_node!(CallExpr = CallExpr);
ast_node!(MethodCallExpr = MethodCallExpr);
ast_node!(FieldExpr = FieldExpr);
ast_node!(IndexExpr = IndexExpr);
ast_node!(PathExpr = PathExpr);
ast_node!(LiteralExpr = LiteralExpr);
ast_node!(ParenExpr = ParenExpr);
ast_node!(IfExpr = IfExpr);
ast_node!(MatchExpr = MatchExpr);
ast_node!(MatchArm = MatchArm);
ast_node!(StructLitExpr = StructLitExpr);
ast_node!(FieldInit = FieldInit);
ast_node!(MacroCallExpr = MacroCallExpr);
ast_node!(ArgList = ArgList);
ast_node!(Pattern = Pattern);

/// Any expression node.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Expr {
    Bin(BinExpr),
    Unary(UnaryExpr),
    Range(RangeExpr),
    Closure(ClosureExpr),
    Assign(AssignExpr),
    Try(TryExpr),
    Call(CallExpr),
    MethodCall(MethodCallExpr),
    Field(FieldExpr),
    Index(IndexExpr),
    Path(PathExpr),
    Literal(LiteralExpr),
    Paren(ParenExpr),
    If(IfExpr),
    Match(MatchExpr),
    StructLit(StructLitExpr),
    MacroCall(MacroCallExpr),
    Block(Block),
}

impl AstNode for Expr {
    fn can_cast(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            K::BinExpr
                | K::UnaryExpr
                | K::RangeExpr
                | K::ClosureExpr
                | K::AssignExpr
                | K::TryExpr
                | K::CallExpr
                | K::MethodCallExpr
                | K::FieldExpr
                | K::IndexExpr
                | K::PathExpr
                | K::LiteralExpr
                | K::ParenExpr
                | K::IfExpr
                | K::MatchExpr
                | K::StructLitExpr
                | K::MacroCallExpr
                | K::Block
        )
    }
    fn cast(node: SyntaxNode) -> Option<Self> {
        Some(match node.kind() {
            K::BinExpr => Expr::Bin(BinExpr { syntax: node }),
            K::UnaryExpr => Expr::Unary(UnaryExpr { syntax: node }),
            K::RangeExpr => Expr::Range(RangeExpr { syntax: node }),
            K::ClosureExpr => Expr::Closure(ClosureExpr { syntax: node }),
            K::AssignExpr => Expr::Assign(AssignExpr { syntax: node }),
            K::TryExpr => Expr::Try(TryExpr { syntax: node }),
            K::CallExpr => Expr::Call(CallExpr { syntax: node }),
            K::MethodCallExpr => Expr::MethodCall(MethodCallExpr { syntax: node }),
            K::FieldExpr => Expr::Field(FieldExpr { syntax: node }),
            K::IndexExpr => Expr::Index(IndexExpr { syntax: node }),
            K::PathExpr => Expr::Path(PathExpr { syntax: node }),
            K::LiteralExpr => Expr::Literal(LiteralExpr { syntax: node }),
            K::ParenExpr => Expr::Paren(ParenExpr { syntax: node }),
            K::IfExpr => Expr::If(IfExpr { syntax: node }),
            K::MatchExpr => Expr::Match(MatchExpr { syntax: node }),
            K::StructLitExpr => Expr::StructLit(StructLitExpr { syntax: node }),
            K::MacroCallExpr => Expr::MacroCall(MacroCallExpr { syntax: node }),
            K::Block => Expr::Block(Block { syntax: node }),
            _ => return None,
        })
    }
    fn syntax(&self) -> &SyntaxNode {
        match self {
            Expr::Bin(n) => n.syntax(),
            Expr::Unary(n) => n.syntax(),
            Expr::Range(n) => n.syntax(),
            Expr::Closure(n) => n.syntax(),
            Expr::Assign(n) => n.syntax(),
            Expr::Try(n) => n.syntax(),
            Expr::Call(n) => n.syntax(),
            Expr::MethodCall(n) => n.syntax(),
            Expr::Field(n) => n.syntax(),
            Expr::Index(n) => n.syntax(),
            Expr::Path(n) => n.syntax(),
            Expr::Literal(n) => n.syntax(),
            Expr::Paren(n) => n.syntax(),
            Expr::If(n) => n.syntax(),
            Expr::Match(n) => n.syntax(),
            Expr::StructLit(n) => n.syntax(),
            Expr::MacroCall(n) => n.syntax(),
            Expr::Block(n) => n.syntax(),
        }
    }
}

impl BinExpr {
    pub fn lhs(&self) -> Option<Expr> {
        nth_child::<Expr>(&self.syntax, 0)
    }
    pub fn rhs(&self) -> Option<Expr> {
        nth_child::<Expr>(&self.syntax, 1)
    }
    /// The operator token (`+`, `==`, `&&`, …).
    pub fn op(&self) -> Option<SyntaxToken> {
        self.syntax
            .children_with_tokens()
            .filter_map(|e| e.into_token())
            .find(|t| is_binop_token(t.kind()))
    }
}

impl UnaryExpr {
    pub fn op(&self) -> Option<SyntaxToken> {
        self.syntax
            .children_with_tokens()
            .filter_map(|e| e.into_token())
            .find(|t| matches!(t.kind(), K::Minus | K::Not))
    }
    pub fn operand(&self) -> Option<Expr> {
        child::<Expr>(&self.syntax)
    }
}

impl RangeExpr {
    pub fn start(&self) -> Option<Expr> {
        nth_child::<Expr>(&self.syntax, 0)
    }
    pub fn end(&self) -> Option<Expr> {
        nth_child::<Expr>(&self.syntax, 1)
    }
    pub fn is_inclusive(&self) -> bool {
        token(&self.syntax, K::DotDotEq).is_some()
    }
}

impl ClosureExpr {
    pub fn params(&self) -> impl Iterator<Item = Param> + '_ {
        children::<Param>(&self.syntax)
    }

    pub fn ret_type(&self) -> Option<Type> {
        child::<Type>(&self.syntax)
    }

    pub fn body(&self) -> Option<Expr> {
        child::<Expr>(&self.syntax)
    }
}

impl AssignExpr {
    pub fn target(&self) -> Option<Expr> {
        nth_child::<Expr>(&self.syntax, 0)
    }
    pub fn value(&self) -> Option<Expr> {
        nth_child::<Expr>(&self.syntax, 1)
    }
}

impl TryExpr {
    pub fn expr(&self) -> Option<Expr> {
        child::<Expr>(&self.syntax)
    }
}

impl CallExpr {
    pub fn callee(&self) -> Option<Expr> {
        child::<Expr>(&self.syntax)
    }
    pub fn arg_list(&self) -> Option<ArgList> {
        child(&self.syntax)
    }
}

impl MethodCallExpr {
    pub fn receiver(&self) -> Option<Expr> {
        child::<Expr>(&self.syntax)
    }
    pub fn method_name(&self) -> Option<SyntaxToken> {
        name_token(&self.syntax)
    }
    pub fn type_args(&self) -> Option<TypeArgs> {
        child(&self.syntax)
    }
    pub fn arg_list(&self) -> Option<ArgList> {
        child(&self.syntax)
    }
}

impl FieldExpr {
    pub fn base(&self) -> Option<Expr> {
        child::<Expr>(&self.syntax)
    }
    pub fn field_name(&self) -> Option<SyntaxToken> {
        name_token(&self.syntax)
    }
}

impl IndexExpr {
    pub fn base(&self) -> Option<Expr> {
        nth_child::<Expr>(&self.syntax, 0)
    }
    pub fn index(&self) -> Option<Expr> {
        nth_child::<Expr>(&self.syntax, 1)
    }
}

impl PathExpr {
    pub fn segments(&self) -> impl Iterator<Item = SyntaxToken> + '_ {
        self.syntax
            .children_with_tokens()
            .filter_map(|e| e.into_token())
            .filter(|t| matches!(t.kind(), K::Ident | K::KwSelf))
    }
}

impl LiteralExpr {
    /// The single literal token (`Int`/`Float`/`Str`/`true`/`false`).
    pub fn value(&self) -> Option<SyntaxToken> {
        self.syntax
            .children_with_tokens()
            .filter_map(|e| e.into_token())
            .find(|t| !t.kind().is_trivia())
    }
}

impl ParenExpr {
    pub fn inner(&self) -> Option<Expr> {
        child::<Expr>(&self.syntax)
    }
}

impl IfExpr {
    /// `true` for `if let PAT = EXPR { ... }`.
    pub fn is_if_let(&self) -> bool {
        token(&self.syntax, K::KwLet).is_some()
    }
    /// The pattern for `if let PAT = ...`. Returns `None` for plain `if`.
    pub fn let_pattern(&self) -> Option<Pattern> {
        child::<Pattern>(&self.syntax)
    }
    /// The condition (`if COND`) or scrutinee expression (`if let PAT = EXPR`).
    pub fn condition(&self) -> Option<Expr> {
        let then_start = self.then_block()?.syntax().text_range().start();
        self.syntax
            .children()
            .filter_map(Expr::cast)
            .take_while(|expr| expr.syntax().text_range().start() < then_start)
            .last()
    }
    pub fn then_block(&self) -> Option<Block> {
        let else_start = token(&self.syntax, K::KwElse).map(|token| token.text_range().start());
        self.syntax
            .children()
            .filter_map(Block::cast)
            .take_while(|block| {
                else_start.is_none_or(|start| block.syntax().text_range().start() < start)
            })
            .last()
    }
    /// The `else` branch, either a `Block` or a nested `IfExpr` (`else if`).
    pub fn else_block(&self) -> Option<Block> {
        let else_end = token(&self.syntax, K::KwElse)?.text_range().end();
        self.syntax
            .children()
            .filter_map(Block::cast)
            .find(|block| block.syntax().text_range().start() >= else_end)
    }
    pub fn else_if(&self) -> Option<IfExpr> {
        let else_end = token(&self.syntax, K::KwElse)?.text_range().end();
        self.syntax.children().filter_map(IfExpr::cast).find(|branch| {
            branch.syntax().text_range().start() >= else_end
        })
    }
}

impl MatchExpr {
    pub fn scrutinee(&self) -> Option<Expr> {
        child::<Expr>(&self.syntax)
    }
    pub fn arms(&self) -> impl Iterator<Item = MatchArm> + '_ {
        children::<MatchArm>(&self.syntax)
    }
}

impl MatchArm {
    pub fn patterns(&self) -> impl Iterator<Item = Pattern> + '_ {
        children::<Pattern>(&self.syntax)
    }
    /// An optional `if` guard expression, e.g. `pat if x > 0 => ...`.
    pub fn guard(&self) -> Option<Expr> {
        token(&self.syntax, K::KwIf)?;
        if let Some(fat) = token(&self.syntax, K::FatArrow) {
            let fa = fat.text_range().start();
            self.syntax
                .children()
                .filter_map(Expr::cast)
                .filter(|e| e.syntax().text_range().start() < fa)
                .last()
        } else {
            // Recovery: without `=>`, the first expression still belongs to
            // the syntactically present `if` guard. It must not become the arm body.
            child::<Expr>(&self.syntax)
        }
    }
    /// The arm body expression (after `=>`). A guarded arm has two `Expr`
    /// children in order `[guard, body]`, so we must select the one that starts
    /// after the `=>` token rather than the first `Expr` child.
    pub fn body(&self) -> Option<Expr> {
        if let Some(fat) = token(&self.syntax, K::FatArrow) {
            let fa = fat.text_range().end();
            self.syntax
                .children()
                .filter_map(Expr::cast)
                .find(|e| e.syntax().text_range().start() >= fa)
        } else {
            // A guarded arm reserves its first expression for the guard even
            // when the arrow is missing. An unguarded arm's first expression
            // is its recovered body.
            nth_child::<Expr>(
                &self.syntax,
                usize::from(token(&self.syntax, K::KwIf).is_some()),
            )
        }
    }
}

impl StructLitExpr {
    /// Path segments identifying the struct or variant being constructed
    /// (e.g. `geo::Point` → ["geo", "Point"]). These are the identifier tokens
    /// before the opening `{`.
    pub fn path_segments(&self) -> impl Iterator<Item = SyntaxToken> + '_ {
        let brace = token(&self.syntax, K::LBrace);
        let limit = brace
            .map(|b| b.text_range().start())
            .unwrap_or(rowan::TextSize::from(u32::MAX));
        self.syntax
            .children_with_tokens()
            .filter_map(|element| element.into_token())
            .filter(|token| matches!(token.kind(), K::Ident | K::KwSelf))
            .filter(move |token| token.text_range().start() < limit)
    }
    pub fn fields(&self) -> impl Iterator<Item = FieldInit> + '_ {
        children::<FieldInit>(&self.syntax)
    }
}

impl FieldInit {
    pub fn name(&self) -> Option<SyntaxToken> {
        name_token(&self.syntax)
    }
    pub fn value(&self) -> Option<Expr> {
        child::<Expr>(&self.syntax)
    }
    pub fn is_shorthand(&self) -> bool {
        token(&self.syntax, K::Colon).is_none()
    }
}

impl MacroCallExpr {
    pub fn name(&self) -> Option<SyntaxToken> {
        name_token(&self.syntax)
    }
    pub fn args(&self) -> impl Iterator<Item = Expr> + '_ {
        children::<Expr>(&self.syntax)
    }
}

impl ArgList {
    pub fn args(&self) -> impl Iterator<Item = Expr> + '_ {
        children::<Expr>(&self.syntax)
    }
}

// --- patterns --------------------------------------------------------------

/// The kind of a [`Pattern`] node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternKind {
    /// Missing or recovered pattern syntax.
    Missing,
    /// `_` wildcard.
    Wildcard,
    /// `x` — a variable binding.
    Binding,
    /// A literal: `1`, `2.0`, `"hello"`, `true`, `false`.
    Literal,
    /// A range: `1..=9`, `1..9`.
    Range,
    /// A path with no payload: `None`, `Shape::Unit`.
    Path,
    /// Tuple variant: `Some(x)`, `Shape::Circle(r)`.
    TupleVariant,
    /// Struct variant / struct pattern: `Point { x, y }`, `Shape::Rect { w, h }`.
    StructVariant,
}

impl Pattern {
    /// Classify this pattern.
    pub fn kind(&self) -> PatternKind {
        let syntax = self.syntax();

        // Trivial token-based checks.
        if token(syntax, K::LParen).is_some() {
            return PatternKind::TupleVariant;
        }
        if token(syntax, K::LBrace).is_some() {
            return PatternKind::StructVariant;
        }
        if token(syntax, K::DotDot).is_some() || token(syntax, K::DotDotEq).is_some() {
            return PatternKind::Range;
        }
        if token(syntax, K::ColonColon).is_some() {
            return PatternKind::Path;
        }

        // Single token patterns: wildcard, binding, or literal.
        let first_tok = syntax
            .children_with_tokens()
            .filter_map(|e| e.into_token())
            .find(|t| !t.kind().is_trivia());
        match first_tok {
            Some(t) if t.text() == "_" => PatternKind::Wildcard,
            Some(t)
                if matches!(t.kind(), K::Ident | K::KwSelf)
                    && t.text()
                        .chars()
                        .next()
                        .is_some_and(|first| first == '_' || first.is_lowercase()) =>
            {
                PatternKind::Binding
            }
            Some(t) if matches!(t.kind(), K::Ident | K::KwSelf) => PatternKind::Path,
            Some(t)
                if matches!(
                    t.kind(),
                    K::Int | K::Float | K::Str | K::KwTrue | K::KwFalse | K::Minus
                ) =>
            {
                PatternKind::Literal
            }
            _ => PatternKind::Missing,
        }
    }

    /// For `Binding` patterns: the identifier token being bound.
    pub fn binding_name(&self) -> Option<SyntaxToken> {
        if self.kind() == PatternKind::Binding {
            self.syntax()
                .children_with_tokens()
                .filter_map(|e| e.into_token())
                .find(|t| matches!(t.kind(), K::Ident | K::KwSelf))
        } else {
            None
        }
    }

    /// For `Path`, `TupleVariant`, and `StructVariant` patterns: the path
    /// segments (identifier tokens) before any payload.
    pub fn path_segments(&self) -> impl Iterator<Item = SyntaxToken> + '_ {
        // Collect the leading idents/self tokens. For WILDCARD/BINDING/LITERAL/RANGE
        // there are no path segments, so this returns nothing.
        self.syntax()
            .children_with_tokens()
            .filter_map(|e| e.into_token())
            .filter(|t| !t.kind().is_trivia())
            .take_while(|t| matches!(t.kind(), K::Ident | K::KwSelf | K::ColonColon))
            .filter(|t| matches!(t.kind(), K::Ident | K::KwSelf))
    }

    /// Sub-patterns (for `TupleVariant` payloads and nested patterns inside
    /// `StructVariant` fields).
    pub fn sub_patterns(&self) -> impl Iterator<Item = Pattern> + '_ {
        children::<Pattern>(self.syntax())
    }

    pub fn rest_token(&self) -> Option<SyntaxToken> {
        (self.kind() == PatternKind::StructVariant)
            .then(|| token(self.syntax(), K::DotDot))
            .flatten()
    }

    pub fn has_rest(&self) -> bool {
        self.rest_token().is_some()
    }

    /// Structured fields for a struct pattern. Unlike [`Pattern::struct_fields`],
    /// this preserves whether a colon was written when its sub-pattern is missing.
    pub fn pattern_fields(&self) -> impl Iterator<Item = PatternField> + '_ {
        PatternFieldIter {
            elements: self
                .syntax()
                .children_with_tokens()
                .collect::<Vec<_>>(),
            pos: 0,
        }
    }

    /// For `StructVariant` patterns: iterate `(field_name, optional sub-pattern)`.
    /// Returns field identifier tokens paired with their sub-pattern (`None` for
    /// shorthand `Point { x }` where the binding reuses the field name).
    pub fn struct_fields(
        &self,
    ) -> impl Iterator<Item = (SyntaxToken, Option<Pattern>)> + '_ {
        self.pattern_fields()
            .map(|field| (field.name, field.pattern))
    }

    /// For `Literal` patterns: the literal expression. Returns the token text
    /// wrapped as appropriate.
    pub fn literal(&self) -> Option<PatternLiteral> {
        if self.kind() != PatternKind::Literal {
            return None;
        }
        pattern_literal(
            self.syntax()
                .children_with_tokens()
                .filter_map(|element| element.into_token()),
        )
    }

    /// Legacy value-token accessor. Use [`Pattern::literal`] when the sign matters.
    pub fn literal_token(&self) -> Option<SyntaxToken> {
        self.literal().map(|literal| literal.token)
    }

    pub fn range(&self) -> Option<PatternRange> {
        if self.kind() != PatternKind::Range {
            return None;
        }
        let elements: Vec<_> = self.syntax().children_with_tokens().collect();
        let operator_index = elements.iter().position(|element| {
            element
                .as_token()
                .is_some_and(|token| matches!(token.kind(), K::DotDot | K::DotDotEq))
        })?;
        let operator = elements[operator_index].as_token()?.clone();
        let start = pattern_literal(
            elements[..operator_index]
                .iter()
                .filter_map(SyntaxElement::as_token)
                .cloned(),
        );
        let end = pattern_literal(
            elements[operator_index + 1..]
                .iter()
                .filter_map(SyntaxElement::as_token)
                .cloned(),
        );
        Some(PatternRange {
            start,
            operator,
            end,
        })
    }

    /// For `Range` patterns: the lower and upper bound tokens, plus whether the
    /// range is inclusive (`..=`). Use [`Pattern::range`] when signs matter.
    pub fn range_bounds(&self) -> Option<(SyntaxToken, SyntaxToken, bool)> {
        let range = self.range()?;
        let inclusive = range.is_inclusive();
        Some((range.start?.token, range.end?.token, inclusive))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PatternLiteral {
    minus_tokens: Vec<SyntaxToken>,
    token: SyntaxToken,
}

impl PatternLiteral {
    pub fn token(&self) -> SyntaxToken {
        self.token.clone()
    }

    pub fn minus_tokens(&self) -> &[SyntaxToken] {
        &self.minus_tokens
    }

    pub fn is_negative(&self) -> bool {
        self.minus_tokens.len() % 2 == 1
    }

    pub fn text(&self) -> String {
        let mut text = String::new();
        for minus in &self.minus_tokens {
            text.push_str(minus.text());
        }
        text.push_str(self.token.text());
        text
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PatternRange {
    start: Option<PatternLiteral>,
    operator: SyntaxToken,
    end: Option<PatternLiteral>,
}

impl PatternRange {
    pub fn start(&self) -> Option<PatternLiteral> {
        self.start.clone()
    }

    pub fn operator_token(&self) -> SyntaxToken {
        self.operator.clone()
    }

    pub fn end(&self) -> Option<PatternLiteral> {
        self.end.clone()
    }

    pub fn is_inclusive(&self) -> bool {
        self.operator.kind() == K::DotDotEq
    }
}

fn pattern_literal(tokens: impl IntoIterator<Item = SyntaxToken>) -> Option<PatternLiteral> {
    let mut minus_tokens = Vec::new();
    for token in tokens {
        match token.kind() {
            K::Minus => minus_tokens.push(token),
            K::Int | K::Float | K::Str | K::KwTrue | K::KwFalse => {
                return Some(PatternLiteral {
                    minus_tokens,
                    token,
                });
            }
            kind if kind.is_trivia() => {}
            _ => {}
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PatternField {
    name: SyntaxToken,
    colon: Option<SyntaxToken>,
    pattern: Option<Pattern>,
}

impl PatternField {
    pub fn name(&self) -> SyntaxToken {
        self.name.clone()
    }

    pub fn colon_token(&self) -> Option<SyntaxToken> {
        self.colon.clone()
    }

    pub fn pattern(&self) -> Option<Pattern> {
        self.pattern.clone()
    }

    pub const fn is_shorthand(&self) -> bool {
        self.colon.is_none()
    }
}

struct PatternFieldIter {
    elements: Vec<SyntaxElement>,
    pos: usize,
}

impl PatternFieldIter {
    fn skip_trivia(&mut self) {
        while self.pos < self.elements.len()
            && self.elements[self.pos]
                .as_token()
                .is_some_and(|token| token.kind().is_trivia())
        {
            self.pos += 1;
        }
    }
}

impl Iterator for PatternFieldIter {
    type Item = PatternField;

    fn next(&mut self) -> Option<Self::Item> {
        // On first call, skip everything before the opening `{`.
        if self.pos == 0 {
            while self.pos < self.elements.len() {
                if let Some(tok) = self.elements[self.pos].as_token()
                    && tok.kind() == K::LBrace
                {
                    self.pos += 1;
                    break;
                }
                self.pos += 1;
            }
        }

        // Skip until we find an Ident that names a field (skip commas, trivia).
        while self.pos < self.elements.len() {
            let e = &self.elements[self.pos];
            if let Some(tok) = e.as_token() {
                if matches!(tok.kind(), K::Ident | K::KwSelf) {
                    break;
                }
                if matches!(tok.kind(), K::RBrace | K::DotDot) {
                    // End of struct pattern.
                    self.pos = self.elements.len();
                    return None;
                }
            }
            self.pos += 1;
        }
        if self.pos >= self.elements.len() {
            return None;
        }
        let name = self.elements[self.pos].as_token()?.clone();
        self.pos += 1;
        self.skip_trivia();

        // Check for `:` then sub-pattern, or just binding shorthand.
        let colon = if self.pos < self.elements.len()
            && self.elements[self.pos]
                .as_token()
                .is_some_and(|token| token.kind() == K::Colon)
        {
            let colon = self.elements[self.pos].as_token().cloned();
            self.pos += 1;
            self.skip_trivia();
            colon
        } else {
            None
        };

        let mut pattern = None;
        if colon.is_some() {
            while self.pos < self.elements.len() {
                if let Some(node) = self.elements[self.pos].as_node()
                    && let Some(found) = Pattern::cast(node.clone())
                {
                    pattern = Some(found);
                    self.pos += 1;
                    break;
                }
                if self.elements[self.pos].as_token().is_some_and(|token| {
                    matches!(token.kind(), K::Comma | K::RBrace | K::DotDot)
                }) {
                    break;
                }
                self.pos += 1;
            }
        }

        Some(PatternField {
            name,
            colon,
            pattern,
        })
    }
}

fn is_binop_token(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        K::Plus
            | K::Minus
            | K::Star
            | K::Slash
            | K::Percent
            | K::EqEq
            | K::Ne
            | K::Lt
            | K::Le
            | K::Gt
            | K::Ge
            | K::AndAnd
            | K::OrOr
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_source_file;

    fn source_file(src: &str) -> SourceFile {
        parse_source_file(src).tree
    }

    #[test]
    fn walk_fn_signature() {
        let sf = source_file("pub fn add(a: i64, b: i64) -> i64 { a + b }");
        let items: Vec<_> = sf.items().collect();
        assert_eq!(items.len(), 1);
        let Item::Fn(f) = &items[0] else {
            panic!("expected fn")
        };
        assert!(f.is_pub());
        assert_eq!(f.name_text().as_deref(), Some("add"));
        let params: Vec<_> = f.params().collect();
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name_text().as_deref(), Some("a"));
        assert!(f.ret_type().is_some());
        assert!(f.body().is_some());
        assert!(!f.has_self());
    }

    #[test]
    fn closure_accessors_cover_params_return_and_body_forms() {
        let sf = source_file(concat!(
            "fn main() {\n",
            "  let inferred = |left, right| left + right;\n",
            "  let typed = |value: i64| -> i64 { value + 1 };\n",
            "  let empty = || 42;\n",
            "}\n",
        ));
        let Item::Fn(function) = sf.items().next().expect("function") else {
            panic!("expected function");
        };
        let closures: Vec<_> = function
            .body()
            .expect("function body")
            .stmts()
            .map(|statement| {
                let Stmt::Let(binding) = statement else {
                    panic!("expected closure binding");
                };
                let Expr::Closure(closure) = binding.init().expect("closure initializer") else {
                    panic!("expected closure expression");
                };
                closure
            })
            .collect();

        assert_eq!(closures[0].params().count(), 2);
        assert!(closures[0].params().all(|parameter| parameter.ty().is_none()));
        assert!(closures[0].ret_type().is_none());
        assert!(matches!(closures[0].body(), Some(Expr::Bin(_))));
        assert_eq!(closures[1].params().count(), 1);
        assert!(closures[1].params().next().expect("typed parameter").ty().is_some());
        assert!(closures[1].ret_type().is_some());
        assert!(matches!(closures[1].body(), Some(Expr::Block(_))));
        assert_eq!(closures[2].params().count(), 0);
        assert!(matches!(closures[2].body(), Some(Expr::Literal(_))));
    }

    #[test]
    fn walk_struct_fields() {
        let sf = source_file("struct P { x: i64, pub y: f64 }");
        let Item::Struct(s) = sf.items().next().unwrap() else {
            panic!()
        };
        assert_eq!(s.name_text().as_deref(), Some("P"));
        let fields: Vec<_> = s.field_list().unwrap().fields().collect();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name_text().as_deref(), Some("x"));
        assert!(fields[1].is_pub());
    }

    #[test]
    fn walk_impl_and_method_body() {
        let sf = source_file("impl P {\n  fn get(&self) -> i64 { self.x }\n}\n");
        let Item::Impl(i) = sf.items().next().unwrap() else {
            panic!()
        };
        assert_eq!(i.type_name().map(|t| t.text().to_string()).as_deref(), Some("P"));
        assert!(i.trait_name().is_none());
        let m = i.methods().next().unwrap();
        assert_eq!(m.name_text().as_deref(), Some("get"));
        assert!(m.has_self());
    }

    #[test]
    fn trait_impl_names() {
        let sf = source_file("impl Draw for Circle {\n  fn draw(&self) {}\n}\n");
        let Item::Impl(i) = sf.items().next().unwrap() else {
            panic!()
        };
        assert_eq!(i.trait_name().map(|t| t.text().to_string()).as_deref(), Some("Draw"));
        assert_eq!(i.type_name().map(|t| t.text().to_string()).as_deref(), Some("Circle"));
    }

    #[test]
    fn walk_binexpr_operator_and_operands() {
        let sf = source_file("fn f() -> i64 { 1 + 2 * 3 }");
        let Item::Fn(f) = sf.items().next().unwrap() else {
            panic!()
        };
        let tail = f.body().unwrap().stmts().next().unwrap();
        let Stmt::Expr(es) = tail else { panic!("expected expr stmt") };
        let Expr::Bin(add) = es.expr().unwrap() else {
            panic!("expected bin")
        };
        assert_eq!(add.op().unwrap().text(), "+");
        // rhs is the `2 * 3` multiplication.
        let Expr::Bin(mul) = add.rhs().unwrap() else {
            panic!("expected nested bin")
        };
        assert_eq!(mul.op().unwrap().text(), "*");
    }

    #[test]
    fn walk_let_and_call() {
        let sf = source_file("fn f() { let x = g(1, 2); }");
        let Item::Fn(f) = sf.items().next().unwrap() else {
            panic!()
        };
        let Stmt::Let(l) = f.body().unwrap().stmts().next().unwrap() else {
            panic!()
        };
        assert_eq!(l.name().unwrap().text(), "x");
        let Expr::Call(call) = l.init().unwrap() else {
            panic!("expected call")
        };
        assert_eq!(call.arg_list().unwrap().args().count(), 2);
    }

    #[test]
    fn typed_views_are_pointer_sized() {
        use std::mem::size_of;
        // The typed layer is a set of *views* over the rowan tree: every wrapper
        // holds a single `SyntaxNode` handle (a cheap cursor), and the enum
        // wrappers are just a tag + that handle. They never own children, so
        // sizes are tiny and constant regardless of tree depth — the opposite of
        // an owned recursive `enum` AST (which would bloat every node to the
        // largest variant and allocate a Box per node).
        let word = size_of::<usize>();
        assert!(size_of::<SyntaxNode>() <= 2 * word);
        assert!(size_of::<Expr>() <= 3 * word, "Expr = {}", size_of::<Expr>());
        assert!(size_of::<Item>() <= 3 * word, "Item = {}", size_of::<Item>());
        assert!(size_of::<Stmt>() <= 3 * word, "Stmt = {}", size_of::<Stmt>());
        assert!(size_of::<Type>() <= 3 * word, "Type = {}", size_of::<Type>());
    }

    #[test]
    fn types_and_generics() {
        let sf = source_file("fn f(v: Vec<i64>) -> Option<String> { v }");
        let Item::Fn(f) = sf.items().next().unwrap() else {
            panic!()
        };
        let p = f.params().next().unwrap();
        let Type::Path(pt) = p.ty().unwrap() else {
            panic!("path type")
        };
        assert_eq!(pt.segments().next().unwrap().text(), "Vec");
        assert!(pt.type_args().is_some());
    }

    // --- P7-1: new accessor tests -------------------------------------------

    #[test]
    fn generic_param_bounds() {
        let sf = source_file("fn f<T: Clone + Eq>(x: T) {}");
        let Item::Fn(f) = sf.items().next().unwrap() else { panic!() };
        let gp = f.generic_params().unwrap();
        let mut params = gp.params();
        let p0 = params.next().unwrap();
        assert_eq!(p0.name_text().as_deref(), Some("T"));
        let bounds: Vec<_> = p0.bounds().map(|t| t.text().to_string()).collect();
        assert_eq!(bounds, vec!["Clone", "Eq"]);
    }

    #[test]
    fn where_clause_predicates() {
        let sf = source_file("fn f<T>(x: T) where T: Clone + Eq {}");
        let Item::Fn(f) = sf.items().next().unwrap() else { panic!() };
        let wc = f.where_clause().unwrap();
        let preds: Vec<_> = wc.predicates().collect();
        assert_eq!(preds.len(), 1);
        let lhs: Vec<_> = preds[0].lhs.iter().map(|t| t.text().to_string()).collect();
        assert_eq!(lhs, vec!["T"]);
        let bounds: Vec<_> = preds[0].bounds.iter().map(|t| t.text().to_string()).collect();
        assert_eq!(bounds, vec!["Clone", "Eq"]);
    }

    #[test]
    fn is_pub_on_items() {
        let sf = source_file(
            "pub struct S {}\n\
             pub enum E { A }\n\
             pub trait T {}\n\
             pub mod m {}\n",
        );
        let items: Vec<_> = sf.items().collect();
        assert!(matches!(&items[0], Item::Struct(s) if s.is_pub()));
        assert!(matches!(&items[1], Item::Enum(e) if e.is_pub()));
        assert!(matches!(&items[2], Item::Trait(t) if t.is_pub()));
        assert!(matches!(&items[3], Item::Mod(m) if m.is_pub()));
    }

    #[test]
    fn trait_method_generics_and_has_self() {
        let sf = source_file(
            "trait Store {\n  fn put<U: Clone>(&self, v: U);\n  fn count() -> i64 { 0 }\n}\n",
        );
        let Item::Trait(t) = sf.items().next().unwrap() else { panic!() };
        let methods: Vec<_> = t.methods().collect();
        assert_eq!(methods.len(), 2);

        let m0 = &methods[0];
        assert_eq!(m0.name_text().as_deref(), Some("put"));
        assert!(m0.has_self());
        let mgp = m0.generic_params().unwrap();
        let mgp_names: Vec<_> = mgp.params().map(|p| p.name_text().unwrap()).collect();
        assert_eq!(mgp_names, vec!["U"]);

        let m1 = &methods[1];
        assert_eq!(m1.name_text().as_deref(), Some("count"));
        assert!(!m1.has_self());
        assert!(m1.default_body().is_some());
        assert!(m1.generic_params().is_none());
    }

    #[test]
    fn extern_fn_variadic() {
        let sf = source_file("extern \"lua\" {\n  fn printf(fmt: &str, ...);\n}\n");
        let Item::Extern(b) = sf.items().next().unwrap() else { panic!() };
        let fns: Vec<_> = b.fns().collect();
        assert_eq!(fns.len(), 1);
        assert!(fns[0].variadic());
    }

    #[test]
    fn use_imports_simple() {
        let sf = source_file("use a::b::c;\n");
        let Item::Use(u) = sf.items().next().unwrap() else { panic!() };
        let imports: Vec<_> = u.imports().collect();
        assert_eq!(imports.len(), 1);
        let path: Vec<_> = imports[0].path.iter().map(|t| t.text().to_string()).collect();
        assert_eq!(path, vec!["a", "b", "c"]);
        assert!(imports[0].alias.is_none());
    }

    #[test]
    fn use_imports_with_alias() {
        let sf = source_file("use a::b as c;\n");
        let Item::Use(u) = sf.items().next().unwrap() else { panic!() };
        let imports: Vec<_> = u.imports().collect();
        assert_eq!(imports.len(), 1);
        assert_eq!(
            imports[0].alias.as_ref().map(|t| t.text().to_string()).as_deref(),
            Some("c")
        );
    }

    #[test]
    fn use_imports_grouped_retain_common_prefix() {
        let sf = source_file("use math::{one, two as second};\n");
        let Item::Use(u) = sf.items().next().unwrap() else { panic!() };
        let imports: Vec<_> = u.imports().collect();

        assert_eq!(imports.len(), 2);
        assert_eq!(
            imports[0]
                .path
                .iter()
                .map(|token| token.text().to_string())
                .collect::<Vec<_>>(),
            ["math", "one"]
        );
        assert_eq!(
            imports[1]
                .path
                .iter()
                .map(|token| token.text().to_string())
                .collect::<Vec<_>>(),
            ["math", "two"]
        );
        assert_eq!(
            imports[1].alias.as_ref().map(|token| token.text()),
            Some("second")
        );
    }

    #[test]
    fn block_tail_expression() {
        let sf = source_file("fn f() -> i64 { 1 + 2 }");
        let Item::Fn(f) = sf.items().next().unwrap() else { panic!() };
        let body = f.body().unwrap();
        let tail = body.tail().unwrap();
        let Expr::Bin(bin) = tail else { panic!("expected bin") };
        assert_eq!(bin.op().unwrap().text(), "+");
    }

    #[test]
    fn block_no_tail_when_semicolon() {
        let sf = source_file("fn f() { let x = 1; }");
        let Item::Fn(f) = sf.items().next().unwrap() else { panic!() };
        let body = f.body().unwrap();
        assert!(body.tail().is_none());
    }

    #[test]
    fn if_let_accessors() {
        let sf =
            source_file("fn f() -> i64 { if let Some(x) = g() { x } else { 0 } }");
        let Item::Fn(f) = sf.items().next().unwrap() else { panic!() };
        let body = f.body().unwrap();
        let tail = body.tail().unwrap();
        let Expr::If(ife) = tail else { panic!("expected if") };
        assert!(ife.is_if_let());
        assert!(ife.let_pattern().is_some());
        assert!(ife.condition().is_some());
        assert!(ife.then_block().is_some());
        assert!(ife.else_block().is_some());
    }

    #[test]
    fn plain_if_no_let() {
        let sf = source_file("fn f() -> i64 { if x > 0 { 1 } else { 0 } }");
        let Item::Fn(f) = sf.items().next().unwrap() else { panic!() };
        let body = f.body().unwrap();
        let tail = body.tail().unwrap();
        let Expr::If(ife) = tail else { panic!("expected if") };
        assert!(!ife.is_if_let());
        assert!(ife.let_pattern().is_none());
    }

    #[test]
    fn while_let_accessors() {
        let sf = source_file("fn f() { while let Some(x) = pop() { use_it(x); } }");
        let Item::Fn(f) = sf.items().next().unwrap() else { panic!() };
        let Stmt::While(w) = f.body().unwrap().stmts().next().unwrap() else {
            panic!("expected while")
        };
        assert!(w.is_while_let());
        assert!(w.let_pattern().is_some());
        assert!(w.condition().is_some());
        assert!(w.body().is_some());
    }

    #[test]
    fn match_arm_guard() {
        let sf = source_file("fn f(x: i64) -> i64 {\n  match x {\n    n if n > 0 => n,\n    _ => 0,\n  }\n}\n");
        let Item::Fn(f) = sf.items().next().unwrap() else { panic!() };
        let tail = f.body().unwrap().tail().unwrap();
        let Expr::Match(m) = tail else { panic!("expected match") };
        let arms: Vec<_> = m.arms().collect();
        assert_eq!(arms.len(), 2);
        assert!(arms[0].guard().is_some());
        assert!(arms[1].guard().is_none());
    }

    #[test]
    fn match_arm_body_is_after_guard() {
        // Regression: a guarded arm has two Expr children [guard, body]; body()
        // must return the expression after `=>`, not the guard.
        let sf = source_file(
            "fn f(x: i64) -> i64 {\n  match x {\n    n if n > 0 => n + 1,\n    _ => 0,\n  }\n}\n",
        );
        let arms = match_arms(&sf);
        // Guarded arm: guard is `n > 0`, body is `n + 1`.
        let Expr::Bin(g) = arms[0].guard().unwrap() else { panic!("guard should be bin") };
        assert_eq!(g.op().unwrap().text(), ">");
        let Expr::Bin(b) = arms[0].body().unwrap() else {
            panic!("body should be `n + 1`, not the guard")
        };
        assert_eq!(b.op().unwrap().text(), "+");
        // Non-guarded arm: body is the literal `0`.
        assert!(matches!(arms[1].body().unwrap(), Expr::Literal(_)));
    }

    #[test]
    fn match_arm_without_arrow_keeps_guard_out_of_body() {
        let sf = source_file("fn f(x: i64) { match x { n if n > 0 } }");
        let arms = match_arms(&sf);
        assert!(matches!(arms[0].guard(), Some(Expr::Bin(_))));
        assert!(arms[0].body().is_none());
    }

    #[test]
    fn block_conditions_are_not_mistaken_for_control_flow_bodies() {
        let sf = source_file(
            "fn f() { if {} {} else {} while {} {} for value in {} {} }",
        );
        let Item::Fn(function) = sf.items().next().unwrap() else { panic!() };
        let statements: Vec<_> = function.body().unwrap().stmts().collect();

        let Stmt::Expr(if_statement) = &statements[0] else { panic!() };
        let Expr::If(if_expression) = if_statement.expr().unwrap() else { panic!() };
        let Expr::Block(condition) = if_expression.condition().unwrap() else { panic!() };
        assert_ne!(
            condition.syntax().text_range(),
            if_expression.then_block().unwrap().syntax().text_range()
        );
        assert!(if_expression.else_block().is_some());

        let Stmt::While(while_statement) = &statements[1] else { panic!() };
        let Expr::Block(condition) = while_statement.condition().unwrap() else { panic!() };
        assert_ne!(
            condition.syntax().text_range(),
            while_statement.body().unwrap().syntax().text_range()
        );

        let Stmt::For(for_statement) = &statements[2] else { panic!() };
        let Expr::Block(iterable) = for_statement.iter().unwrap() else { panic!() };
        assert_ne!(
            iterable.syntax().text_range(),
            for_statement.body().unwrap().syntax().text_range()
        );
    }

    // --- Pattern accessor tests --------------------------------------------

    #[test]
    fn pattern_wildcard() {
        let sf = source_file("fn f(x: i64) -> i64 {\n  match x {\n    _ => 0,\n  }\n}\n");
        let arms = match_arms(&sf);
        let pat = arms[0].patterns().next().unwrap();
        assert_eq!(pat.kind(), PatternKind::Wildcard);
        assert!(pat.binding_name().is_none());
    }

    #[test]
    fn pattern_binding() {
        let sf = source_file("fn f(x: i64) -> i64 {\n  match x {\n    n => n,\n  }\n}\n");
        let arms = match_arms(&sf);
        let pat = arms[0].patterns().next().unwrap();
        assert_eq!(pat.kind(), PatternKind::Binding);
        assert_eq!(pat.binding_name().unwrap().text(), "n");
    }

    #[test]
    fn pattern_classifies_missing_and_single_segment_names() {
        let sf = source_file(
            "fn f(x: i64) { match x { _value => 0, self => 1, Ready => 2, => 3 } }",
        );
        let arms = match_arms(&sf);
        let patterns: Vec<_> = arms
            .iter()
            .map(|arm| arm.patterns().next().unwrap())
            .collect();

        assert_eq!(patterns[0].kind(), PatternKind::Binding);
        assert_eq!(patterns[0].binding_name().unwrap().text(), "_value");
        assert_eq!(patterns[1].kind(), PatternKind::Binding);
        assert_eq!(patterns[1].binding_name().unwrap().text(), "self");
        assert_eq!(patterns[2].kind(), PatternKind::Path);
        assert_eq!(patterns[3].kind(), PatternKind::Missing);
    }

    #[test]
    fn pattern_literal() {
        let sf = source_file("fn f(x: i64) -> i64 {\n  match x {\n    0 => 1,\n  }\n}\n");
        let arms = match_arms(&sf);
        let pat = arms[0].patterns().next().unwrap();
        assert_eq!(pat.kind(), PatternKind::Literal);
        assert_eq!(pat.literal_token().unwrap().text(), "0");
    }

    #[test]
    fn pattern_range() {
        let sf =
            source_file("fn f(x: i64) -> i64 {\n  match x {\n    1..=9 => 1,\n  }\n}\n");
        let arms = match_arms(&sf);
        let pat = arms[0].patterns().next().unwrap();
        assert_eq!(pat.kind(), PatternKind::Range);
        let (lo, hi, incl) = pat.range_bounds().unwrap();
        assert_eq!(lo.text(), "1");
        assert_eq!(hi.text(), "9");
        assert!(incl);
    }

    #[test]
    fn pattern_range_preserves_signed_and_string_bounds() {
        let sf = source_file(
            "fn f(x: i64) { match x { -10..=-1 => 0, \"a\"..=\"z\" => 1 } }",
        );
        let arms = match_arms(&sf);

        let signed = arms[0].patterns().next().unwrap().range().unwrap();
        let start = signed.start().unwrap();
        let end = signed.end().unwrap();
        assert_eq!(start.text(), "-10");
        assert!(start.is_negative());
        assert_eq!(end.text(), "-1");
        assert!(end.is_negative());
        assert!(signed.is_inclusive());

        let strings = arms[1].patterns().next().unwrap().range().unwrap();
        assert_eq!(strings.start().unwrap().text(), "\"a\"");
        assert_eq!(strings.end().unwrap().text(), "\"z\"");
        assert!(strings.is_inclusive());
        let (start, end, inclusive) = arms[1]
            .patterns()
            .next()
            .unwrap()
            .range_bounds()
            .unwrap();
        assert_eq!(start.text(), "\"a\"");
        assert_eq!(end.text(), "\"z\"");
        assert!(inclusive);
    }

    #[test]
    fn pattern_path_unit_variant() {
        let sf = source_file(
            "enum Shape { Unit, Circle(f64) }\n\
             fn f(s: Shape) -> f64 {\n  match s {\n    Shape::Unit => 0.0,\n    _ => 1.0,\n  }\n}\n",
        );
        let arms = match_arms(&sf);
        let pat = arms[0].patterns().next().unwrap();
        assert_eq!(pat.kind(), PatternKind::Path);
        let segs: Vec<_> = pat.path_segments().map(|t| t.text().to_string()).collect();
        assert_eq!(segs, vec!["Shape", "Unit"]);
    }

    #[test]
    fn pattern_tuple_variant() {
        let sf = source_file(
            "enum Shape { Circle(f64), Unit }\n\
             fn f(s: Shape) -> f64 {\n  match s {\n    Shape::Circle(r) => r,\n    _ => 0.0,\n  }\n}\n",
        );
        let arms = match_arms(&sf);
        let pat = arms[0].patterns().next().unwrap();
        assert_eq!(pat.kind(), PatternKind::TupleVariant);
        let segs: Vec<_> = pat.path_segments().map(|t| t.text().to_string()).collect();
        assert_eq!(segs, vec!["Shape", "Circle"]);
        let subs: Vec<_> = pat.sub_patterns().collect();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].kind(), PatternKind::Binding);
        assert_eq!(subs[0].binding_name().unwrap().text(), "r");
    }

    #[test]
    fn pattern_struct_variant() {
        let sf = source_file(
            "enum Shape { Rect { w: f64, h: f64 }, Unit }\n\
             fn f(s: Shape) -> f64 {\n  match s {\n    Shape::Rect { w, h } => w * h,\n    _ => 0.0,\n  }\n}\n",
        );
        let arms = match_arms(&sf);
        let pat = arms[0].patterns().next().unwrap();
        assert_eq!(pat.kind(), PatternKind::StructVariant);
        let segs: Vec<_> = pat.path_segments().map(|t| t.text().to_string()).collect();
        assert_eq!(segs, vec!["Shape", "Rect"]);
        let fields: Vec<_> = pat.struct_fields().collect();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].0.text(), "w");
        assert!(fields[0].1.is_none()); // shorthand binding
        assert_eq!(fields[1].0.text(), "h");
        assert!(fields[1].1.is_none());
    }

    #[test]
    fn pattern_struct_with_sub_patterns() {
        let sf = source_file(
            "struct Outer { x: i64 }\n\
             fn f(o: Outer) -> i64 {\n  match o {\n    Outer { x: 42 } => 1,\n    _ => 0,\n  }\n}\n",
        );
        let arms = match_arms(&sf);
        let pat = arms[0].patterns().next().unwrap();
        let fields: Vec<_> = pat.struct_fields().collect();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].0.text(), "x");
        let sub = fields[0].1.as_ref().unwrap();
        assert_eq!(sub.kind(), PatternKind::Literal);
    }

    #[test]
    fn pattern_fields_preserve_colons_missing_patterns_and_rest() {
        let sf = source_file(
            "fn f(value: Rec) { match value { Rec { short, explicit /*c*/ : nested, .. } => 0 } }",
        );
        let pattern = match_arms(&sf)[0].patterns().next().unwrap();
        let fields: Vec<_> = pattern.pattern_fields().collect();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name().text(), "short");
        assert!(fields[0].is_shorthand());
        assert!(fields[0].colon_token().is_none());
        assert!(fields[0].pattern().is_none());
        assert_eq!(fields[1].name().text(), "explicit");
        assert!(!fields[1].is_shorthand());
        assert_eq!(fields[1].colon_token().unwrap().text(), ":");
        assert_eq!(fields[1].pattern().unwrap().kind(), PatternKind::Binding);
        assert_eq!(pattern.rest_token().unwrap().text(), "..");
        assert_eq!(pattern.struct_fields().count(), 2);

        let missing = source_file(
            "fn f(value: Rec) { match value { Rec { missing: } => 0 } }",
        );
        let pattern = match_arms(&missing)[0].patterns().next().unwrap();
        let field = pattern.pattern_fields().next().unwrap();
        assert!(!field.is_shorthand());
        assert_eq!(field.pattern().unwrap().kind(), PatternKind::Missing);
    }

    #[test]
    fn pattern_size_guard() {
        use std::mem::size_of;
        // Pattern is a thin wrapper around a SyntaxNode.
        assert!(
            size_of::<Pattern>() <= 2 * size_of::<usize>(),
            "Pattern = {}",
            size_of::<Pattern>()
        );
    }

    // --- StructLitExpr path_segments ---------------------------------------

    #[test]
    fn struct_lit_path_segments() {
        let sf = source_file("fn f() { g(geo::Point { x: 1, y: 2 }); }");
        let Item::Fn(f) = sf.items().next().unwrap() else { panic!() };
        let Stmt::Expr(es) = f.body().unwrap().stmts().next().unwrap() else {
            panic!()
        };
        let Expr::Call(call) = es.expr().unwrap() else { panic!() };
        let Expr::StructLit(sl) = call.arg_list().unwrap().args().next().unwrap() else {
            panic!()
        };
        let segs: Vec<_> = sl.path_segments().map(|t| t.text().to_string()).collect();
        assert_eq!(segs, vec!["geo", "Point"]);
    }

    #[test]
    fn struct_lit_path_segments_include_self() {
        let sf = source_file("fn f() { g(self::Point { x: 1 }); }");
        let Item::Fn(function) = sf.items().next().unwrap() else {
            panic!()
        };
        let Stmt::Expr(statement) = function.body().unwrap().stmts().next().unwrap() else {
            panic!()
        };
        let Expr::Call(call) = statement.expr().unwrap() else {
            panic!()
        };
        let Expr::StructLit(literal) = call.arg_list().unwrap().args().next().unwrap() else {
            panic!()
        };
        let segments = literal
            .path_segments()
            .map(|token| token.text().to_string())
            .collect::<Vec<_>>();
        assert_eq!(segments, ["self", "Point"]);
    }

    // --- Helpers ------------------------------------------------------------

    fn match_arms(sf: &SourceFile) -> Vec<MatchArm> {
        for item in sf.items() {
            if let Item::Fn(f) = item
                && let Some(tail) = f.body().and_then(|b| b.tail())
                && let Expr::Match(m) = tail
            {
                return m.arms().collect();
            }
        }
        panic!("no match found in source");
    }
}
