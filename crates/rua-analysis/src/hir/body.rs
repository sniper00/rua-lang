//! Per-owner expression HIR and syntax-to-HIR source mappings.
//!
//! Bodies contain only semantic data. Source coordinates live in
//! [`BodySourceMap`] so a trivia-only edit can refresh locations while reusing
//! an equal [`Body`].

use std::{collections::HashMap, ops::Index};

use rua_syntax::{
    AstNode, Named, SyntaxKind, SyntaxNode, SyntaxToken,
    ast::{
        self, Block as AstBlock, Expr as AstExpr, FnDecl, Pattern as AstPattern,
        PatternKind as AstPatternKind, Stmt as AstStmt, TraitMethod,
    },
};

use crate::{
    base::{FileRange, TextRange},
    hir::{DefId, TypeRef},
    vfs::FileId,
};

macro_rules! arena_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u32);

        impl $name {
            fn new(index: usize) -> Self {
                Self(u32::try_from(index).expect("body arena exhausted"))
            }

            pub const fn index(self) -> u32 {
                self.0
            }
        }
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BodyId(DefId);

impl BodyId {
    pub const fn new(owner: DefId) -> Self {
        Self(owner)
    }

    pub const fn owner(self) -> DefId {
        self.0
    }
}

arena_id!(ExprId);
arena_id!(PatId);
arena_id!(BindingId);
arena_id!(NameRefId);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Arena<T> {
    values: Vec<T>,
}

impl<T> Arena<T> {
    fn new() -> Self {
        Self { values: Vec::new() }
    }

    fn alloc<I>(&mut self, value: T) -> I
    where
        I: ArenaIndex,
    {
        let id = I::from_index(self.values.len());
        self.values.push(value);
        id
    }

    fn get<I>(&self, id: I) -> Option<&T>
    where
        I: ArenaIndex,
    {
        self.values.get(id.to_index())
    }

    fn len(&self) -> usize {
        self.values.len()
    }
}

trait ArenaIndex: Copy {
    fn from_index(index: usize) -> Self;
    fn to_index(self) -> usize;
}

macro_rules! impl_arena_index {
    ($name:ident) => {
        impl ArenaIndex for $name {
            fn from_index(index: usize) -> Self {
                Self::new(index)
            }

            fn to_index(self) -> usize {
                self.index() as usize
            }
        }
    };
}

impl_arena_index!(ExprId);
impl_arena_index!(PatId);
impl_arena_index!(BindingId);
impl_arena_index!(NameRefId);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Body {
    id: BodyId,
    params: Vec<BindingId>,
    root_expr: ExprId,
    exprs: Arena<Expr>,
    patterns: Arena<Pat>,
    bindings: Arena<Binding>,
    name_refs: Arena<NameRef>,
}

impl Body {
    pub const fn id(&self) -> BodyId {
        self.id
    }

    pub const fn owner(&self) -> DefId {
        self.id.owner()
    }

    pub fn params(&self) -> &[BindingId] {
        &self.params
    }

    pub const fn root_expr(&self) -> ExprId {
        self.root_expr
    }

    pub fn expr(&self, id: ExprId) -> Option<&Expr> {
        self.exprs.get(id)
    }

    pub fn pattern(&self, id: PatId) -> Option<&Pat> {
        self.patterns.get(id)
    }

    pub fn binding(&self, id: BindingId) -> Option<&Binding> {
        self.bindings.get(id)
    }

    pub fn name_ref(&self, id: NameRefId) -> Option<&NameRef> {
        self.name_refs.get(id)
    }

    pub fn exprs(&self) -> impl ExactSizeIterator<Item = (ExprId, &Expr)> {
        self.exprs
            .values
            .iter()
            .enumerate()
            .map(|(index, value)| (ExprId::new(index), value))
    }

    pub fn patterns(&self) -> impl ExactSizeIterator<Item = (PatId, &Pat)> {
        self.patterns
            .values
            .iter()
            .enumerate()
            .map(|(index, value)| (PatId::new(index), value))
    }

    pub fn bindings(&self) -> impl ExactSizeIterator<Item = (BindingId, &Binding)> {
        self.bindings
            .values
            .iter()
            .enumerate()
            .map(|(index, value)| (BindingId::new(index), value))
    }

    pub fn name_refs(&self) -> impl ExactSizeIterator<Item = (NameRefId, &NameRef)> {
        self.name_refs
            .values
            .iter()
            .enumerate()
            .map(|(index, value)| (NameRefId::new(index), value))
    }
}

impl Index<ExprId> for Body {
    type Output = Expr;

    fn index(&self, id: ExprId) -> &Self::Output {
        &self.exprs.values[id.index() as usize]
    }
}

impl Index<PatId> for Body {
    type Output = Pat;

    fn index(&self, id: PatId) -> &Self::Output {
        &self.patterns.values[id.index() as usize]
    }
}

impl Index<BindingId> for Body {
    type Output = Binding;

    fn index(&self, id: BindingId) -> &Self::Output {
        &self.bindings.values[id.index() as usize]
    }
}

impl Index<NameRefId> for Body {
    type Output = NameRef;

    fn index(&self, id: NameRefId) -> &Self::Output {
        &self.name_refs.values[id.index() as usize]
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    Missing,
    Literal(Literal),
    Path(Vec<NameRefId>),
    Unary {
        op: UnaryOp,
        expr: ExprId,
    },
    Binary {
        op: BinaryOp,
        lhs: ExprId,
        rhs: ExprId,
    },
    Range {
        start: ExprId,
        end: ExprId,
        inclusive: bool,
    },
    Closure {
        params: Vec<BindingId>,
        return_type: Option<TypeRef>,
        body: ExprId,
    },
    Assign {
        target: ExprId,
        value: ExprId,
    },
    Try {
        expr: ExprId,
    },
    Call {
        callee: ExprId,
        args: Vec<ExprId>,
    },
    MethodCall {
        receiver: ExprId,
        method: NameRefId,
        type_args: Vec<TypeRef>,
        args: Vec<ExprId>,
    },
    Field {
        base: ExprId,
        field: NameRefId,
    },
    Index {
        base: ExprId,
        index: ExprId,
    },
    Paren {
        expr: ExprId,
    },
    If {
        condition: Condition,
        then_branch: ExprId,
        else_branch: Option<ExprId>,
    },
    Match {
        scrutinee: ExprId,
        arms: Vec<MatchArm>,
    },
    StructLiteral {
        path: Vec<NameRefId>,
        fields: Vec<StructField>,
    },
    MacroCall {
        macro_name: NameRefId,
        args: Vec<ExprId>,
    },
    Block(Block),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    statements: Vec<Statement>,
    tail: Option<ExprId>,
}

impl Block {
    pub fn statements(&self) -> &[Statement] {
        &self.statements
    }

    pub const fn tail(&self) -> Option<ExprId> {
        self.tail
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Statement {
    Missing,
    Let {
        binding: BindingId,
        initializer: ExprId,
    },
    Expr {
        expr: ExprId,
        has_semicolon: bool,
    },
    Return {
        value: Option<ExprId>,
    },
    While {
        condition: Condition,
        body: ExprId,
    },
    Loop {
        body: ExprId,
    },
    For {
        binding: BindingId,
        iterable: ExprId,
        body: ExprId,
    },
    Break,
    Continue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Condition {
    Expr(ExprId),
    Let { pattern: PatId, scrutinee: ExprId },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchArm {
    patterns: Vec<PatId>,
    guard: Option<ExprId>,
    body: ExprId,
}

impl MatchArm {
    pub fn patterns(&self) -> &[PatId] {
        &self.patterns
    }

    pub const fn guard(&self) -> Option<ExprId> {
        self.guard
    }

    pub const fn body(&self) -> ExprId {
        self.body
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StructField {
    name: NameRefId,
    value: ExprId,
    shorthand: bool,
}

impl StructField {
    pub const fn name(self) -> NameRefId {
        self.name
    }

    pub const fn value(self) -> ExprId {
        self.value
    }

    pub const fn is_shorthand(self) -> bool {
        self.shorthand
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pat {
    Missing,
    Wildcard,
    Binding {
        binding: BindingId,
    },
    Literal(Literal),
    Range {
        start: Literal,
        end: Literal,
        inclusive: bool,
    },
    Path(Vec<NameRefId>),
    TupleVariant {
        path: Vec<NameRefId>,
        subpatterns: Vec<PatId>,
    },
    StructVariant {
        path: Vec<NameRefId>,
        fields: Vec<PatternField>,
        has_rest: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PatternField {
    name: NameRefId,
    pattern: PatId,
    shorthand: bool,
}

impl PatternField {
    pub const fn name(self) -> NameRefId {
        self.name
    }

    pub const fn pattern(self) -> PatId {
        self.pattern
    }

    pub const fn is_shorthand(self) -> bool {
        self.shorthand
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Literal {
    kind: LiteralKind,
    text: String,
}

impl Literal {
    pub const fn kind(&self) -> LiteralKind {
        self.kind
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LiteralKind {
    Integer,
    Float,
    String,
    Boolean,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Negate,
    Not,
    Missing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    And,
    Or,
    Missing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    name: Option<String>,
    kind: BindingKind,
    mutable: bool,
    type_ref: Option<TypeRef>,
}

impl Binding {
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub const fn kind(&self) -> BindingKind {
        self.kind
    }

    pub const fn is_mutable(&self) -> bool {
        self.mutable
    }

    pub fn type_ref(&self) -> Option<&TypeRef> {
        self.type_ref.as_ref()
    }

    pub const fn is_missing(&self) -> bool {
        self.name.is_none()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingKind {
    SelfParameter,
    Parameter,
    ClosureParameter,
    Let,
    For,
    Pattern,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameRef {
    name: Option<String>,
    kind: NameRefKind,
}

impl NameRef {
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub const fn kind(&self) -> NameRefKind {
        self.kind
    }

    pub const fn is_missing(&self) -> bool {
        self.name.is_none()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameRefKind {
    Path,
    Method,
    Field,
    StructPath,
    StructField,
    PatternPath,
    PatternField,
    Macro,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BodySourceId {
    Body(BodyId),
    Expr(ExprId),
    Pat(PatId),
    Binding(BindingId),
    NameRef(NameRefId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodySourceMap {
    body_id: BodyId,
    body_range: FileRange,
    expr_ranges: Vec<FileRange>,
    pat_ranges: Vec<FileRange>,
    binding_ranges: Vec<FileRange>,
    name_ref_ranges: Vec<FileRange>,
    reverse: HashMap<FileRange, Vec<BodySourceId>>,
}

impl BodySourceMap {
    pub const fn body_id(&self) -> BodyId {
        self.body_id
    }

    pub const fn body_range(&self) -> FileRange {
        self.body_range
    }

    pub fn expr_range(&self, id: ExprId) -> Option<FileRange> {
        self.expr_ranges.get(id.index() as usize).copied()
    }

    pub fn pat_range(&self, id: PatId) -> Option<FileRange> {
        self.pat_ranges.get(id.index() as usize).copied()
    }

    pub fn binding_range(&self, id: BindingId) -> Option<FileRange> {
        self.binding_ranges.get(id.index() as usize).copied()
    }

    pub fn name_ref_range(&self, id: NameRefId) -> Option<FileRange> {
        self.name_ref_ranges.get(id.index() as usize).copied()
    }

    pub fn source(&self, id: BodySourceId) -> Option<FileRange> {
        match id {
            BodySourceId::Body(id) => (id == self.body_id).then_some(self.body_range),
            BodySourceId::Expr(id) => self.expr_range(id),
            BodySourceId::Pat(id) => self.pat_range(id),
            BodySourceId::Binding(id) => self.binding_range(id),
            BodySourceId::NameRef(id) => self.name_ref_range(id),
        }
    }

    pub fn ids_for_range(&self, range: FileRange) -> &[BodySourceId] {
        self.reverse.get(&range).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn ids_at(&self, file_id: FileId, offset: u32) -> Vec<BodySourceId> {
        let mut matches = self
            .reverse
            .iter()
            .filter(|(range, _)| {
                range.file_id == file_id
                    && (range.range.contains(offset)
                        || (range.range.is_empty() && range.range.start() == offset))
            })
            .flat_map(|(range, ids)| {
                ids.iter()
                    .copied()
                    .map(move |id| (range.range.len(), range.range.start(), id))
            })
            .collect::<Vec<_>>();
        matches.sort_by_key(|(length, start, id)| (*length, *start, *id));
        matches.into_iter().map(|(_, _, id)| id).collect()
    }
}

pub(crate) fn lower_fn_body(
    owner: DefId,
    file_id: FileId,
    function: &FnDecl,
) -> (Body, BodySourceMap) {
    let mut lower = BodyLowerer::new(owner, file_id, function.syntax());
    lower.lower_callable(
        function.receiver_token(),
        function.receiver(),
        function.params(),
        function.body(),
    );
    lower.finish()
}

pub(crate) fn lower_trait_method_body(
    owner: DefId,
    file_id: FileId,
    method: &TraitMethod,
) -> (Body, BodySourceMap) {
    let mut lower = BodyLowerer::new(owner, file_id, method.syntax());
    lower.lower_callable(
        method.receiver_token(),
        method.receiver(),
        method.params(),
        method.default_body(),
    );
    lower.finish()
}

struct BodyLowerer {
    file_id: FileId,
    body: Body,
    source_map: BodySourceMap,
}

impl BodyLowerer {
    fn new(owner: DefId, file_id: FileId, owner_syntax: &SyntaxNode) -> Self {
        let body_id = BodyId::new(owner);
        let body_range = FileRange::new(file_id, node_range(owner_syntax));
        let mut reverse = HashMap::new();
        reverse.insert(body_range, vec![BodySourceId::Body(body_id)]);
        Self {
            file_id,
            body: Body {
                id: body_id,
                params: Vec::new(),
                root_expr: ExprId::new(0),
                exprs: Arena::new(),
                patterns: Arena::new(),
                bindings: Arena::new(),
                name_refs: Arena::new(),
            },
            source_map: BodySourceMap {
                body_id,
                body_range,
                expr_ranges: Vec::new(),
                pat_ranges: Vec::new(),
                binding_ranges: Vec::new(),
                name_ref_ranges: Vec::new(),
                reverse,
            },
        }
    }

    fn finish(self) -> (Body, BodySourceMap) {
        debug_assert_eq!(self.body.exprs.len(), self.source_map.expr_ranges.len());
        debug_assert_eq!(self.body.patterns.len(), self.source_map.pat_ranges.len());
        debug_assert_eq!(
            self.body.bindings.len(),
            self.source_map.binding_ranges.len()
        );
        debug_assert_eq!(
            self.body.name_refs.len(),
            self.source_map.name_ref_ranges.len()
        );
        (self.body, self.source_map)
    }

    fn lower_callable<I>(
        &mut self,
        receiver: Option<SyntaxToken>,
        receiver_kind: Option<ast::ReceiverKind>,
        params: I,
        body: Option<AstBlock>,
    ) where
        I: IntoIterator<Item = ast::Param>,
    {
        if let Some(receiver) = receiver {
            let binding = Binding {
                name: Some(receiver.text().to_string()),
                kind: BindingKind::SelfParameter,
                mutable: receiver_kind == Some(ast::ReceiverKind::MutRef),
                type_ref: None,
            };
            let id = self.alloc_binding(binding, token_range(&receiver));
            self.body.params.push(id);
        }

        for param in params {
            let name = param.name();
            let range = name
                .as_ref()
                .map(token_range)
                .unwrap_or_else(|| insertion_before_token(param.syntax(), SyntaxKind::Colon));
            let binding = Binding {
                name: name.map(|name| name.text().to_string()),
                kind: BindingKind::Parameter,
                mutable: false,
                type_ref: Some(TypeRef::from_type(param.ty())),
            };
            let id = self.alloc_binding(binding, range);
            self.body.params.push(id);
        }

        self.body.root_expr = match body {
            Some(body) => self.lower_block(body),
            None => self.alloc_expr(Expr::Missing, self.owner_insertion_range()),
        };
    }

    fn lower_block(&mut self, block: AstBlock) -> ExprId {
        let range = node_range(block.syntax());
        let statements = block.stmts().collect::<Vec<_>>();
        if statements.is_empty() && !has_direct_token(block.syntax(), SyntaxKind::LBrace) {
            return self.alloc_expr(Expr::Missing, range);
        }
        let tail_index = statements.len().checked_sub(1).filter(
            |index| matches!(&statements[*index], AstStmt::Expr(stmt) if !stmt.has_semicolon()),
        );

        let mut lowered_statements = Vec::with_capacity(statements.len());
        let mut tail = None;
        for (index, statement) in statements.into_iter().enumerate() {
            if Some(index) == tail_index {
                match statement {
                    AstStmt::Expr(statement) => {
                        tail = Some(self.lower_required_expr(statement.expr(), statement.syntax()));
                    }
                    statement => lowered_statements.push(self.lower_statement(statement)),
                }
            } else {
                lowered_statements.push(self.lower_statement(statement));
            }
        }

        self.alloc_expr(
            Expr::Block(Block {
                statements: lowered_statements,
                tail,
            }),
            range,
        )
    }

    fn lower_statement(&mut self, statement: AstStmt) -> Statement {
        match statement {
            AstStmt::Let(statement) => {
                let name = statement.name();
                let range = name.as_ref().map(token_range).unwrap_or_else(|| {
                    insertion_before_first_token(
                        statement.syntax(),
                        &[SyntaxKind::Colon, SyntaxKind::Eq, SyntaxKind::Semi],
                    )
                });
                let binding = self.alloc_binding(
                    Binding {
                        name: name.map(|name| name.text().to_string()),
                        kind: BindingKind::Let,
                        mutable: statement.is_mut(),
                        type_ref: statement.ty().map(|ty| TypeRef::from_type(Some(ty))),
                    },
                    range,
                );
                let initializer = self.lower_required_expr_at(
                    statement.init(),
                    insertion_after_token_before_boundary(
                        statement.syntax(),
                        SyntaxKind::Eq,
                        &[SyntaxKind::Semi],
                    ),
                );
                Statement::Let {
                    binding,
                    initializer,
                }
            }
            AstStmt::Expr(statement) => {
                let expr = self.lower_required_expr(statement.expr(), statement.syntax());
                Statement::Expr {
                    expr,
                    has_semicolon: statement.has_semicolon(),
                }
            }
            AstStmt::Return(statement) => {
                let value = match statement.value() {
                    Some(value) => Some(self.lower_expr(value)),
                    None if !has_direct_token(statement.syntax(), SyntaxKind::Semi) => {
                        Some(self.alloc_expr(Expr::Missing, insertion_range(statement.syntax())))
                    }
                    None => None,
                };
                Statement::Return { value }
            }
            AstStmt::While(statement) => {
                let condition_slot = statement
                    .body()
                    .as_ref()
                    .map(|body| insertion_before_node(body.syntax()))
                    .unwrap_or_else(|| insertion_range(statement.syntax()));
                let condition = if statement.is_while_let() {
                    let pattern = self.lower_required_pattern_at(
                        statement.let_pattern(),
                        insertion_before_token(statement.syntax(), SyntaxKind::Eq),
                    );
                    let scrutinee =
                        self.lower_required_expr_at(statement.condition(), condition_slot);
                    Condition::Let { pattern, scrutinee }
                } else {
                    Condition::Expr(
                        self.lower_required_expr_at(statement.condition(), condition_slot),
                    )
                };
                let body = self.lower_required_block(statement.body(), statement.syntax());
                Statement::While { condition, body }
            }
            AstStmt::Loop(statement) => {
                let body = self.lower_required_block(statement.body(), statement.syntax());
                Statement::Loop { body }
            }
            AstStmt::For(statement) => {
                let name = statement.var();
                let range = name.as_ref().map(token_range).unwrap_or_else(|| {
                    insertion_before_token(statement.syntax(), SyntaxKind::KwIn)
                });
                let binding = self.alloc_binding(
                    Binding {
                        name: name.map(|name| name.text().to_string()),
                        kind: BindingKind::For,
                        mutable: false,
                        type_ref: None,
                    },
                    range,
                );
                let iterable_slot = statement
                    .body()
                    .as_ref()
                    .map(|body| insertion_before_node(body.syntax()))
                    .unwrap_or_else(|| insertion_range(statement.syntax()));
                let iterable = self.lower_required_expr_at(statement.iter(), iterable_slot);
                let body = self.lower_required_block(statement.body(), statement.syntax());
                Statement::For {
                    binding,
                    iterable,
                    body,
                }
            }
            AstStmt::Break(_) => Statement::Break,
            AstStmt::Continue(_) => Statement::Continue,
        }
    }

    fn lower_required_block(&mut self, block: Option<AstBlock>, parent: &SyntaxNode) -> ExprId {
        self.lower_required_block_at(block, insertion_range(parent))
    }

    fn lower_required_block_at(&mut self, block: Option<AstBlock>, missing: TextRange) -> ExprId {
        match block {
            Some(block) => self.lower_block(block),
            None => self.alloc_expr(Expr::Missing, missing),
        }
    }

    fn lower_required_expr(&mut self, expr: Option<AstExpr>, parent: &SyntaxNode) -> ExprId {
        self.lower_required_expr_at(expr, insertion_range(parent))
    }

    fn lower_required_expr_at(&mut self, expr: Option<AstExpr>, missing: TextRange) -> ExprId {
        match expr {
            Some(expr) => self.lower_expr(expr),
            None => self.alloc_expr(Expr::Missing, missing),
        }
    }

    fn lower_expr(&mut self, expr: AstExpr) -> ExprId {
        let range = node_range(expr.syntax());
        let lowered = match expr {
            AstExpr::Bin(expr) => {
                let operator = expr.op();
                let lhs_slot = operator
                    .as_ref()
                    .map(insertion_before_syntax_token)
                    .unwrap_or_else(|| insertion_range(expr.syntax()));
                let rhs_slot = operator
                    .as_ref()
                    .map(insertion_after_syntax_token)
                    .unwrap_or_else(|| insertion_range(expr.syntax()));
                let lhs = self.lower_required_expr_at(expr.lhs(), lhs_slot);
                let rhs = self.lower_required_expr_at(expr.rhs(), rhs_slot);
                Expr::Binary {
                    op: operator.map_or(BinaryOp::Missing, lower_binary_op),
                    lhs,
                    rhs,
                }
            }
            AstExpr::Unary(expr) => {
                let operator = expr.op();
                let operand_slot = operator
                    .as_ref()
                    .map(insertion_after_syntax_token)
                    .unwrap_or_else(|| insertion_range(expr.syntax()));
                let operand = self.lower_required_expr_at(expr.operand(), operand_slot);
                Expr::Unary {
                    op: operator.map_or(UnaryOp::Missing, lower_unary_op),
                    expr: operand,
                }
            }
            AstExpr::Range(expr) => {
                let operator =
                    direct_token_any(expr.syntax(), &[SyntaxKind::DotDot, SyntaxKind::DotDotEq]);
                let start_slot = operator
                    .as_ref()
                    .map(insertion_before_syntax_token)
                    .unwrap_or_else(|| insertion_range(expr.syntax()));
                let end_slot = operator
                    .as_ref()
                    .map(insertion_after_syntax_token)
                    .unwrap_or_else(|| insertion_range(expr.syntax()));
                let start = self.lower_required_expr_at(expr.start(), start_slot);
                let end = self.lower_required_expr_at(expr.end(), end_slot);
                Expr::Range {
                    start,
                    end,
                    inclusive: expr.is_inclusive(),
                }
            }
            AstExpr::Closure(expr) => {
                let mut params = Vec::new();
                for param in expr.params() {
                    let name = param.name();
                    let range = name.as_ref().map(token_range).unwrap_or_else(|| {
                        insertion_before_token(param.syntax(), SyntaxKind::Colon)
                    });
                    params.push(self.alloc_binding(
                        Binding {
                            name: name.map(|name| name.text().to_string()),
                            kind: BindingKind::ClosureParameter,
                            mutable: false,
                            type_ref: param.ty().map(|ty| TypeRef::from_type(Some(ty))),
                        },
                        range,
                    ));
                }
                let return_type = expr.ret_type().map(|ty| TypeRef::from_type(Some(ty)));
                let body = self.lower_required_expr(expr.body(), expr.syntax());
                Expr::Closure {
                    params,
                    return_type,
                    body,
                }
            }
            AstExpr::Assign(expr) => {
                let target = self.lower_required_expr_at(
                    expr.target(),
                    insertion_before_token(expr.syntax(), SyntaxKind::Eq),
                );
                let value = self.lower_required_expr_at(
                    expr.value(),
                    insertion_after_token(expr.syntax(), SyntaxKind::Eq),
                );
                Expr::Assign { target, value }
            }
            AstExpr::Try(expr) => {
                let inner = self.lower_required_expr_at(
                    expr.expr(),
                    insertion_before_token(expr.syntax(), SyntaxKind::Question),
                );
                Expr::Try { expr: inner }
            }
            AstExpr::Call(expr) => {
                let callee = self.lower_required_expr_at(
                    expr.callee(),
                    insertion_before_token(expr.syntax(), SyntaxKind::LParen),
                );
                let args = expr
                    .arg_list()
                    .map(|args| self.lower_argument_children(args.syntax()))
                    .unwrap_or_default();
                Expr::Call { callee, args }
            }
            AstExpr::MethodCall(expr) => {
                let receiver = self.lower_required_expr_at(
                    expr.receiver(),
                    insertion_before_token(expr.syntax(), SyntaxKind::Dot),
                );
                let method_token = expr.method_name();
                let method = match method_token {
                    Some(token) => self.alloc_name_ref(
                        Some(token.text().to_string()),
                        NameRefKind::Method,
                        token_range(&token),
                    ),
                    None => self.alloc_name_ref(
                        None,
                        NameRefKind::Method,
                        insertion_after_token(expr.syntax(), SyntaxKind::Dot),
                    ),
                };
                let type_args = expr
                    .type_args()
                    .map(|args| args.args().map(|ty| TypeRef::from_type(Some(ty))).collect())
                    .unwrap_or_default();
                let args = expr
                    .arg_list()
                    .map(|args| self.lower_argument_children(args.syntax()))
                    .unwrap_or_default();
                Expr::MethodCall {
                    receiver,
                    method,
                    type_args,
                    args,
                }
            }
            AstExpr::Field(expr) => {
                let base = self.lower_required_expr_at(
                    expr.base(),
                    insertion_before_token(expr.syntax(), SyntaxKind::Dot),
                );
                let field = match expr.field_name() {
                    Some(token) => self.alloc_name_ref(
                        Some(token.text().to_string()),
                        NameRefKind::Field,
                        token_range(&token),
                    ),
                    None => self.alloc_name_ref(
                        None,
                        NameRefKind::Field,
                        insertion_after_token(expr.syntax(), SyntaxKind::Dot),
                    ),
                };
                Expr::Field { base, field }
            }
            AstExpr::Index(expr) => {
                let base = self.lower_required_expr_at(
                    expr.base(),
                    insertion_before_token(expr.syntax(), SyntaxKind::LBracket),
                );
                let index = self.lower_required_expr_at(
                    expr.index(),
                    insertion_before_token(expr.syntax(), SyntaxKind::RBracket),
                );
                Expr::Index { base, index }
            }
            AstExpr::Path(expr) => {
                let path = expr
                    .segments()
                    .map(|segment| {
                        self.alloc_name_ref(
                            Some(segment.text().to_string()),
                            NameRefKind::Path,
                            token_range(&segment),
                        )
                    })
                    .collect::<Vec<_>>();
                if path.is_empty() {
                    Expr::Missing
                } else {
                    Expr::Path(path)
                }
            }
            AstExpr::Literal(expr) => match expr.value().and_then(|token| literal(&[token])) {
                Some(literal) => Expr::Literal(literal),
                None => Expr::Missing,
            },
            AstExpr::Paren(expr) => {
                let inner = self.lower_required_expr_at(
                    expr.inner(),
                    insertion_before_token(expr.syntax(), SyntaxKind::RParen),
                );
                Expr::Paren { expr: inner }
            }
            AstExpr::If(expr) => {
                let then_block = expr.then_block();
                let condition_slot = then_block
                    .as_ref()
                    .map(|body| insertion_before_node(body.syntax()))
                    .unwrap_or_else(|| insertion_before_token(expr.syntax(), SyntaxKind::KwElse));
                let condition = if expr.is_if_let() {
                    let pattern = self.lower_required_pattern_at(
                        expr.let_pattern(),
                        insertion_before_token(expr.syntax(), SyntaxKind::Eq),
                    );
                    let scrutinee = self.lower_required_expr_at(expr.condition(), condition_slot);
                    Condition::Let { pattern, scrutinee }
                } else {
                    Condition::Expr(self.lower_required_expr_at(expr.condition(), condition_slot))
                };
                let then_branch = self.lower_required_block_at(
                    then_block,
                    insertion_before_token(expr.syntax(), SyntaxKind::KwElse),
                );
                let else_branch = expr
                    .else_if()
                    .map(|else_if| self.lower_expr(AstExpr::If(else_if)))
                    .or_else(|| expr.else_block().map(|block| self.lower_block(block)))
                    .or_else(|| {
                        direct_token(expr.syntax(), SyntaxKind::KwElse).map(|else_token| {
                            self.alloc_expr(
                                Expr::Missing,
                                insertion_after_syntax_token(&else_token),
                            )
                        })
                    });
                Expr::If {
                    condition,
                    then_branch,
                    else_branch,
                }
            }
            AstExpr::Match(expr) => {
                let scrutinee = self.lower_required_expr_at(
                    expr.scrutinee(),
                    insertion_before_token(expr.syntax(), SyntaxKind::LBrace),
                );
                let arms = expr.arms().map(|arm| self.lower_match_arm(arm)).collect();
                Expr::Match { scrutinee, arms }
            }
            AstExpr::StructLit(expr) => {
                let mut path = expr
                    .path_segments()
                    .map(|segment| {
                        self.alloc_name_ref(
                            Some(segment.text().to_string()),
                            NameRefKind::StructPath,
                            token_range(&segment),
                        )
                    })
                    .collect::<Vec<_>>();
                if path.is_empty() {
                    path.push(self.alloc_name_ref(
                        None,
                        NameRefKind::StructPath,
                        insertion_before_token(expr.syntax(), SyntaxKind::LBrace),
                    ));
                }
                let fields = expr
                    .fields()
                    .map(|field| self.lower_struct_field(field))
                    .collect();
                Expr::StructLiteral { path, fields }
            }
            AstExpr::MacroCall(expr) => {
                let macro_name =
                    self.alloc_name_ref_token(expr.name(), NameRefKind::Macro, expr.syntax());
                let args = self.lower_argument_children(expr.syntax());
                Expr::MacroCall { macro_name, args }
            }
            AstExpr::Block(block) => return self.lower_block(block),
        };
        self.alloc_expr(lowered, range)
    }

    fn lower_match_arm(&mut self, arm: ast::MatchArm) -> MatchArm {
        let mut patterns = arm
            .patterns()
            .map(|pattern| self.lower_pattern(pattern))
            .collect::<Vec<_>>();
        if patterns.is_empty() {
            patterns.push(self.alloc_pat(
                Pat::Missing,
                insertion_before_first_token(
                    arm.syntax(),
                    &[SyntaxKind::KwIf, SyntaxKind::FatArrow, SyntaxKind::Comma],
                ),
            ));
        }
        let guard = match arm.guard() {
            Some(guard) => Some(self.lower_expr(guard)),
            None => direct_token(arm.syntax(), SyntaxKind::KwIf).map(|if_token| {
                let end: u32 = if_token.text_range().end().into();
                self.alloc_expr(Expr::Missing, TextRange::new(end, end))
            }),
        };
        let body = self.lower_required_expr_at(
            arm.body(),
            insertion_after_token(arm.syntax(), SyntaxKind::FatArrow),
        );
        MatchArm {
            patterns,
            guard,
            body,
        }
    }

    fn lower_struct_field(&mut self, field: ast::FieldInit) -> StructField {
        let name_token = field.name();
        let name_range = name_token
            .as_ref()
            .map(token_range)
            .unwrap_or_else(|| insertion_before_token(field.syntax(), SyntaxKind::Colon));
        let name = self.alloc_name_ref(
            name_token.as_ref().map(|name| name.text().to_string()),
            NameRefKind::StructField,
            name_range,
        );
        let shorthand = field.is_shorthand();
        let value = if shorthand {
            let path_name = self.alloc_name_ref(
                name_token.map(|name| name.text().to_string()),
                NameRefKind::Path,
                name_range,
            );
            self.alloc_expr(Expr::Path(vec![path_name]), name_range)
        } else {
            self.lower_required_expr_at(
                field.value(),
                insertion_after_token(field.syntax(), SyntaxKind::Colon),
            )
        };
        StructField {
            name,
            value,
            shorthand,
        }
    }

    fn lower_argument_children(&mut self, syntax: &SyntaxNode) -> Vec<ExprId> {
        syntax
            .children()
            .filter_map(|child| {
                if let Some(expr) = AstExpr::cast(child.clone()) {
                    Some(self.lower_expr(expr))
                } else if child.kind() == SyntaxKind::ErrorNode {
                    Some(self.alloc_expr(Expr::Missing, node_range(&child)))
                } else {
                    None
                }
            })
            .collect()
    }

    fn lower_required_pattern_at(
        &mut self,
        pattern: Option<AstPattern>,
        missing: TextRange,
    ) -> PatId {
        match pattern {
            Some(pattern) => self.lower_pattern(pattern),
            None => self.alloc_pat(Pat::Missing, missing),
        }
    }

    fn lower_pattern(&mut self, pattern: AstPattern) -> PatId {
        let range = node_range(pattern.syntax());
        let lowered = match pattern.kind() {
            AstPatternKind::Missing => Pat::Missing,
            AstPatternKind::Wildcard => Pat::Wildcard,
            AstPatternKind::Binding => {
                let name = pattern.binding_name();
                let binding_range = name
                    .as_ref()
                    .map(token_range)
                    .unwrap_or_else(|| insertion_range(pattern.syntax()));
                let binding = self.alloc_binding(
                    Binding {
                        name: name.map(|name| name.text().to_string()),
                        kind: BindingKind::Pattern,
                        mutable: false,
                        type_ref: None,
                    },
                    binding_range,
                );
                Pat::Binding { binding }
            }
            AstPatternKind::Literal => literal_from_pattern(pattern.syntax())
                .map(Pat::Literal)
                .unwrap_or(Pat::Missing),
            AstPatternKind::Range => {
                let (start, end, inclusive) = range_pattern_literals(pattern.syntax());
                match (start, end) {
                    (Some(start), Some(end)) => Pat::Range {
                        start,
                        end,
                        inclusive,
                    },
                    _ => Pat::Missing,
                }
            }
            AstPatternKind::Path => {
                let path = self.lower_pattern_path(&pattern);
                if path.is_empty() {
                    Pat::Missing
                } else {
                    Pat::Path(path)
                }
            }
            AstPatternKind::TupleVariant => {
                let path = self.lower_pattern_path(&pattern);
                let subpatterns = pattern
                    .sub_patterns()
                    .map(|pattern| self.lower_pattern(pattern))
                    .collect();
                Pat::TupleVariant { path, subpatterns }
            }
            AstPatternKind::StructVariant => {
                let path = self.lower_pattern_path(&pattern);
                let fields = pattern
                    .pattern_fields()
                    .map(|field| {
                        let name_token = field.name();
                        let name_range = token_range(&name_token);
                        let name = self.alloc_name_ref(
                            Some(name_token.text().to_string()),
                            NameRefKind::PatternField,
                            name_range,
                        );
                        let shorthand = field.is_shorthand();
                        let pattern = if shorthand {
                            let binding = self.alloc_binding(
                                Binding {
                                    name: Some(name_token.text().to_string()),
                                    kind: BindingKind::Pattern,
                                    mutable: false,
                                    type_ref: None,
                                },
                                name_range,
                            );
                            self.alloc_pat(Pat::Binding { binding }, name_range)
                        } else {
                            match field.pattern() {
                                Some(pattern) => self.lower_pattern(pattern),
                                None => {
                                    let missing_range = field
                                        .colon_token()
                                        .as_ref()
                                        .map(|colon| {
                                            let end: u32 = colon.text_range().end().into();
                                            TextRange::new(end, end)
                                        })
                                        .unwrap_or_else(|| {
                                            TextRange::new(name_range.end(), name_range.end())
                                        });
                                    self.alloc_pat(Pat::Missing, missing_range)
                                }
                            }
                        };
                        PatternField {
                            name,
                            pattern,
                            shorthand,
                        }
                    })
                    .collect();
                Pat::StructVariant {
                    path,
                    fields,
                    has_rest: pattern.rest_token().is_some(),
                }
            }
        };
        self.alloc_pat(lowered, range)
    }

    fn lower_pattern_path(&mut self, pattern: &AstPattern) -> Vec<NameRefId> {
        pattern
            .path_segments()
            .map(|segment| {
                self.alloc_name_ref(
                    Some(segment.text().to_string()),
                    NameRefKind::PatternPath,
                    token_range(&segment),
                )
            })
            .collect()
    }

    fn alloc_expr(&mut self, expr: Expr, range: TextRange) -> ExprId {
        let id = self.body.exprs.alloc(expr);
        self.source_map.expr_ranges.push(self.file_range(range));
        self.record_source(BodySourceId::Expr(id), range);
        id
    }

    fn alloc_pat(&mut self, pattern: Pat, range: TextRange) -> PatId {
        let id = self.body.patterns.alloc(pattern);
        self.source_map.pat_ranges.push(self.file_range(range));
        self.record_source(BodySourceId::Pat(id), range);
        id
    }

    fn alloc_binding(&mut self, binding: Binding, range: TextRange) -> BindingId {
        let id = self.body.bindings.alloc(binding);
        self.source_map.binding_ranges.push(self.file_range(range));
        self.record_source(BodySourceId::Binding(id), range);
        id
    }

    fn alloc_name_ref(
        &mut self,
        name: Option<String>,
        kind: NameRefKind,
        range: TextRange,
    ) -> NameRefId {
        let id = self.body.name_refs.alloc(NameRef { name, kind });
        self.source_map.name_ref_ranges.push(self.file_range(range));
        self.record_source(BodySourceId::NameRef(id), range);
        id
    }

    fn alloc_name_ref_token(
        &mut self,
        token: Option<SyntaxToken>,
        kind: NameRefKind,
        parent: &SyntaxNode,
    ) -> NameRefId {
        match token {
            Some(token) => {
                self.alloc_name_ref(Some(token.text().to_string()), kind, token_range(&token))
            }
            None => self.alloc_name_ref(None, kind, insertion_range(parent)),
        }
    }

    fn record_source(&mut self, id: BodySourceId, range: TextRange) {
        self.source_map
            .reverse
            .entry(self.file_range(range))
            .or_default()
            .push(id);
    }

    fn file_range(&self, range: TextRange) -> FileRange {
        FileRange::new(self.file_id, range)
    }

    fn owner_insertion_range(&self) -> TextRange {
        let end = self.source_map.body_range.range.end();
        TextRange::new(end, end)
    }
}

fn lower_unary_op(token: SyntaxToken) -> UnaryOp {
    match token.kind() {
        SyntaxKind::Minus => UnaryOp::Negate,
        SyntaxKind::Not => UnaryOp::Not,
        _ => UnaryOp::Missing,
    }
}

fn lower_binary_op(token: SyntaxToken) -> BinaryOp {
    match token.kind() {
        SyntaxKind::Plus => BinaryOp::Add,
        SyntaxKind::Minus => BinaryOp::Subtract,
        SyntaxKind::Star => BinaryOp::Multiply,
        SyntaxKind::Slash => BinaryOp::Divide,
        SyntaxKind::Percent => BinaryOp::Remainder,
        SyntaxKind::EqEq => BinaryOp::Equal,
        SyntaxKind::Ne => BinaryOp::NotEqual,
        SyntaxKind::Lt => BinaryOp::Less,
        SyntaxKind::Le => BinaryOp::LessOrEqual,
        SyntaxKind::Gt => BinaryOp::Greater,
        SyntaxKind::Ge => BinaryOp::GreaterOrEqual,
        SyntaxKind::AndAnd => BinaryOp::And,
        SyntaxKind::OrOr => BinaryOp::Or,
        _ => BinaryOp::Missing,
    }
}

fn literal_from_pattern(pattern: &SyntaxNode) -> Option<Literal> {
    literal(&significant_tokens(pattern))
}

fn range_pattern_literals(pattern: &SyntaxNode) -> (Option<Literal>, Option<Literal>, bool) {
    let tokens = significant_tokens(pattern);
    let Some(separator) = tokens
        .iter()
        .position(|token| matches!(token.kind(), SyntaxKind::DotDot | SyntaxKind::DotDotEq))
    else {
        return (None, None, false);
    };
    let inclusive = tokens[separator].kind() == SyntaxKind::DotDotEq;
    (
        literal(&tokens[..separator]),
        literal(&tokens[separator + 1..]),
        inclusive,
    )
}

fn literal(tokens: &[SyntaxToken]) -> Option<Literal> {
    let value = tokens.iter().find(|token| {
        matches!(
            token.kind(),
            SyntaxKind::Int
                | SyntaxKind::Float
                | SyntaxKind::Str
                | SyntaxKind::KwTrue
                | SyntaxKind::KwFalse
        )
    })?;
    let kind = match value.kind() {
        SyntaxKind::Int => LiteralKind::Integer,
        SyntaxKind::Float => LiteralKind::Float,
        SyntaxKind::Str => LiteralKind::String,
        SyntaxKind::KwTrue | SyntaxKind::KwFalse => LiteralKind::Boolean,
        _ => return None,
    };
    let text = tokens
        .iter()
        .filter(|token| {
            matches!(
                token.kind(),
                SyntaxKind::Minus
                    | SyntaxKind::Int
                    | SyntaxKind::Float
                    | SyntaxKind::Str
                    | SyntaxKind::KwTrue
                    | SyntaxKind::KwFalse
            )
        })
        .map(SyntaxToken::text)
        .collect();
    Some(Literal { kind, text })
}

fn significant_tokens(node: &SyntaxNode) -> Vec<SyntaxToken> {
    node.descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| !token.kind().is_trivia())
        .collect()
}

fn has_direct_token(node: &SyntaxNode, kind: SyntaxKind) -> bool {
    direct_token(node, kind).is_some()
}

fn direct_token(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| token.kind() == kind)
}

fn direct_token_any(node: &SyntaxNode, kinds: &[SyntaxKind]) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| kinds.contains(&token.kind()))
}

fn insertion_before_first_token(node: &SyntaxNode, kinds: &[SyntaxKind]) -> TextRange {
    kinds
        .iter()
        .filter_map(|kind| slot_token(node, *kind))
        .min_by_key(|token| token.text_range().start())
        .as_ref()
        .map(insertion_before_syntax_token)
        .unwrap_or_else(|| insertion_range(node))
}

fn insertion_before_token(node: &SyntaxNode, kind: SyntaxKind) -> TextRange {
    slot_token(node, kind)
        .as_ref()
        .map(insertion_before_syntax_token)
        .unwrap_or_else(|| insertion_range(node))
}

fn insertion_after_token(node: &SyntaxNode, kind: SyntaxKind) -> TextRange {
    slot_token(node, kind)
        .as_ref()
        .map(insertion_after_syntax_token)
        .unwrap_or_else(|| insertion_range(node))
}

fn slot_token(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxToken> {
    direct_token(node, kind).or_else(|| {
        node.children()
            .filter(|child| child.kind() == SyntaxKind::ErrorNode)
            .flat_map(|error| error.descendants_with_tokens())
            .filter_map(|element| element.into_token())
            .find(|token| token.kind() == kind)
    })
}

fn insertion_after_token_before_boundary(
    node: &SyntaxNode,
    after: SyntaxKind,
    boundaries: &[SyntaxKind],
) -> TextRange {
    let Some(after_token) = slot_token(node, after) else {
        return insertion_range(node);
    };
    let after_end = after_token.text_range().end();
    node.descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| !token.kind().is_trivia() && token.text_range().start() >= after_end)
        .find(|token| boundaries.contains(&token.kind()))
        .as_ref()
        .map(insertion_before_syntax_token)
        .unwrap_or_else(|| insertion_after_syntax_token(&after_token))
}

fn insertion_before_syntax_token(token: &SyntaxToken) -> TextRange {
    let start = token.text_range().start().into();
    TextRange::new(start, start)
}

fn insertion_after_syntax_token(token: &SyntaxToken) -> TextRange {
    let end = token.text_range().end().into();
    TextRange::new(end, end)
}

fn insertion_before_node(node: &SyntaxNode) -> TextRange {
    let start = node.text_range().start().into();
    TextRange::new(start, start)
}

fn insertion_range(node: &SyntaxNode) -> TextRange {
    let boundary = node
        .children_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| !token.kind().is_trivia())
        .last()
        .filter(|token| {
            matches!(
                token.kind(),
                SyntaxKind::Semi
                    | SyntaxKind::Comma
                    | SyntaxKind::RParen
                    | SyntaxKind::RBracket
                    | SyntaxKind::RBrace
            )
        });
    boundary.as_ref().map_or_else(
        || {
            let end = node.text_range().end().into();
            TextRange::new(end, end)
        },
        insertion_before_syntax_token,
    )
}

fn node_range(node: &SyntaxNode) -> TextRange {
    rowan_range(node.text_range())
}

fn token_range(token: &SyntaxToken) -> TextRange {
    rowan_range(token.text_range())
}

fn rowan_range(range: rowan::TextRange) -> TextRange {
    TextRange::new(range.start().into(), range.end().into())
}
