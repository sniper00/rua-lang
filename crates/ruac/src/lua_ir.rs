//! Structured Lua source IR and its deterministic pretty-printer.

use crate::token::SourceRange;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Expr {
    Nil,
    Bool(bool),
    Integer(String),
    Number(String),
    StringLiteral(String),
    Name(String),
    Field {
        base: Box<Expr>,
        name: String,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    MethodCall {
        receiver: Box<Expr>,
        method: String,
        args: Vec<Expr>,
    },
    Unary {
        op: UnaryOp,
        expression: Box<Expr>,
    },
    Binary {
        left: Box<Expr>,
        op: BinaryOp,
        right: Box<Expr>,
    },
    Table(Vec<TableField>),
    Parenthesized(Box<Expr>),
}

impl Expr {
    pub(crate) fn name(name: impl Into<String>) -> Self {
        let name = name.into();
        assert!(is_lua_identifier(&name), "invalid Lua identifier `{name}`");
        Self::Name(name)
    }

    pub(crate) fn integer(source: impl Into<String>) -> Self {
        Self::Integer(source.into())
    }

    pub(crate) fn number(source: impl Into<String>) -> Self {
        Self::Number(source.into())
    }

    /// A string token already validated by the Rua lexer. Rua and Lua use the
    /// same quoted-string surface syntax.
    pub(crate) fn string_literal(source: impl Into<String>) -> Self {
        Self::StringLiteral(source.into())
    }

    pub(crate) fn string(value: &str) -> Self {
        Self::StringLiteral(lua_string(value))
    }

    pub(crate) fn field(self, name: impl Into<String>) -> Self {
        let name = name.into();
        assert!(
            is_lua_identifier(&name),
            "invalid Lua field identifier `{name}`"
        );
        Self::Field {
            base: Box::new(self),
            name,
        }
    }

    pub(crate) fn index(self, index: Expr) -> Self {
        Self::Index {
            base: Box::new(self),
            index: Box::new(index),
        }
    }

    pub(crate) fn call(self, args: Vec<Expr>) -> Self {
        Self::Call {
            callee: Box::new(self),
            args,
        }
    }

    pub(crate) fn method_call(self, method: impl Into<String>, args: Vec<Expr>) -> Self {
        let method = method.into();
        assert!(
            is_lua_identifier(&method),
            "invalid Lua method identifier `{method}`"
        );
        Self::MethodCall {
            receiver: Box::new(self),
            method,
            args,
        }
    }

    pub(crate) fn unary(op: UnaryOp, expression: Expr) -> Self {
        Self::Unary {
            op,
            expression: Box::new(expression),
        }
    }

    pub(crate) fn binary(self, op: BinaryOp, right: Expr) -> Self {
        Self::Binary {
            left: Box::new(self),
            op,
            right: Box::new(right),
        }
    }

    pub(crate) fn parenthesized(self) -> Self {
        Self::Parenthesized(Box::new(self))
    }

    pub(crate) fn named_table(fields: Vec<(String, Expr)>) -> Self {
        Self::Table(
            fields
                .into_iter()
                .map(|(name, value)| TableField::Named(name, value))
                .collect(),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TableField {
    Named(String, Expr),
    Indexed(Expr, Expr),
    Value(Expr),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnaryOp {
    Neg,
    Not,
    Len,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinaryOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Concat,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FunctionTarget {
    Local(String),
    Path(Vec<String>),
    Method {
        receiver: Vec<String>,
        method: String,
    },
}

impl FunctionTarget {
    pub(crate) fn local(name: impl Into<String>) -> Self {
        let name = name.into();
        assert!(
            is_lua_identifier(&name),
            "invalid Lua function name `{name}`"
        );
        Self::Local(name)
    }

    pub(crate) fn path(segments: Vec<String>) -> Self {
        assert!(!segments.is_empty(), "Lua function path cannot be empty");
        assert!(
            segments.iter().all(|segment| is_lua_identifier(segment)),
            "invalid Lua function path"
        );
        Self::Path(segments)
    }

    pub(crate) fn method(receiver: Vec<String>, method: impl Into<String>) -> Self {
        let method = method.into();
        assert!(!receiver.is_empty(), "Lua method receiver cannot be empty");
        assert!(
            receiver.iter().all(|segment| is_lua_identifier(segment)) && is_lua_identifier(&method),
            "invalid Lua method target"
        );
        Self::Method { receiver, method }
    }

    fn source(&self) -> String {
        match self {
            Self::Local(name) => name.clone(),
            Self::Path(segments) => segments.join("."),
            Self::Method { receiver, method } => format!("{}:{method}", receiver.join(".")),
        }
    }

    fn is_local(&self) -> bool {
        matches!(self, Self::Local(_))
    }
}

impl BinaryOp {
    fn source(self) -> &'static str {
        match self {
            Self::Or => "or",
            Self::And => "and",
            Self::Eq => "==",
            Self::Ne => "~=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Concat => "..",
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Rem => "%",
        }
    }

    fn precedence(self) -> u8 {
        match self {
            Self::Or => 1,
            Self::And => 2,
            Self::Eq | Self::Ne | Self::Lt | Self::Le | Self::Gt | Self::Ge => 3,
            Self::Concat => 4,
            Self::Add | Self::Sub => 5,
            Self::Mul | Self::Div | Self::Rem => 6,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Block {
    entries: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Entry {
    Blank,
    Statement {
        anchor: Option<SourceRange>,
        statement: Statement,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Statement {
    Annotation(String),
    Expression(Expr),
    Local {
        names: Vec<String>,
        values: Vec<Expr>,
    },
    Assign {
        targets: Vec<Expr>,
        values: Vec<Expr>,
    },
    Return(Vec<Expr>),
    Break,
    Goto(String),
    Label(String),
    Do(Block),
    While {
        condition: Expr,
        body: Block,
    },
    NumericFor {
        variable: String,
        start: Expr,
        stop: Expr,
        body: Block,
    },
    Function {
        target: FunctionTarget,
        params: Vec<String>,
        body: Block,
    },
    If {
        branches: Vec<IfBranch>,
        else_body: Option<Block>,
    },
    CompactIf {
        condition: Expr,
        statements: Vec<InlineStatement>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InlineStatement {
    Expression(Expr),
    Assign { target: Expr, value: Expr },
    Return(Vec<Expr>),
    Break,
}

impl InlineStatement {
    pub(crate) fn expression(expression: Expr) -> Self {
        Self::Expression(expression)
    }

    pub(crate) fn assign(target: Expr, value: Expr) -> Self {
        Self::Assign { target, value }
    }

    pub(crate) fn return_value(value: Expr) -> Self {
        Self::Return(vec![value])
    }

    pub(crate) fn return_nil() -> Self {
        Self::Return(vec![Expr::Nil])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SourceMapping {
    pub(crate) generated_start: usize,
    pub(crate) generated_end: usize,
    pub(crate) source: SourceRange,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Printed {
    pub(crate) source: String,
    pub(crate) mappings: Vec<SourceMapping>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IfBranch {
    condition: Expr,
    body: Block,
}

#[derive(Debug)]
enum Frame {
    Root(Block),
    Do(Block),
    While {
        condition: Expr,
        body: Block,
    },
    NumericFor {
        variable: String,
        start: Expr,
        stop: Expr,
        body: Block,
    },
    Function {
        target: FunctionTarget,
        params: Vec<String>,
        body: Block,
    },
    If {
        branches: Vec<IfBranch>,
        else_body: Option<Block>,
        active: IfActive,
    },
}

#[derive(Clone, Copy, Debug)]
enum IfActive {
    Branch(usize),
    Else,
}

/// Stack builder used by codegen. A frame can only be closed as the same
/// structured node that opened it, so malformed indentation cannot be emitted.
#[derive(Debug)]
pub(crate) struct Builder {
    frames: Vec<Frame>,
    anchors: Vec<SourceRange>,
}

impl Builder {
    pub(crate) fn new() -> Self {
        Self {
            frames: vec![Frame::Root(Block::default())],
            anchors: Vec::new(),
        }
    }

    pub(crate) fn push_anchor(&mut self, source: SourceRange) {
        self.anchors.push(source);
    }

    pub(crate) fn pop_anchor(&mut self) {
        self.anchors
            .pop()
            .expect("Lua source anchor stack underflow");
    }

    pub(crate) fn annotation(&mut self, source: impl Into<String>) {
        self.statement(Statement::Annotation(source.into()));
    }

    pub(crate) fn expression(&mut self, expression: Expr) {
        self.statement(Statement::Expression(expression));
    }

    pub(crate) fn local(&mut self, names: Vec<String>, values: Vec<Expr>) {
        assert!(!names.is_empty(), "Lua local declaration needs a name");
        assert!(
            values.is_empty() || values.len() <= names.len(),
            "Lua local declaration has more values than names"
        );
        self.statement(Statement::Local { names, values });
    }

    pub(crate) fn assign(&mut self, target: Expr, value: Expr) {
        self.statement(Statement::Assign {
            targets: vec![target],
            values: vec![value],
        });
    }

    pub(crate) fn return_values(&mut self, values: Vec<Expr>) {
        self.statement(Statement::Return(values));
    }

    pub(crate) fn break_statement(&mut self) {
        self.statement(Statement::Break);
    }

    pub(crate) fn goto(&mut self, label: impl Into<String>) {
        self.statement(Statement::Goto(label.into()));
    }

    pub(crate) fn label(&mut self, label: impl Into<String>) {
        self.statement(Statement::Label(label.into()));
    }

    pub(crate) fn blank(&mut self) {
        self.current_block_mut().entries.push(Entry::Blank);
    }

    pub(crate) fn begin_do(&mut self) {
        self.frames.push(Frame::Do(Block::default()));
    }

    pub(crate) fn begin_while(&mut self, condition: Expr) {
        self.frames.push(Frame::While {
            condition,
            body: Block::default(),
        });
    }

    pub(crate) fn begin_numeric_for(
        &mut self,
        variable: impl Into<String>,
        start: Expr,
        stop: Expr,
    ) {
        self.frames.push(Frame::NumericFor {
            variable: variable.into(),
            start,
            stop,
            body: Block::default(),
        });
    }

    pub(crate) fn begin_function(&mut self, target: FunctionTarget, params: Vec<String>) {
        self.frames.push(Frame::Function {
            target,
            params,
            body: Block::default(),
        });
    }

    pub(crate) fn begin_if(&mut self, condition: Expr) {
        self.frames.push(Frame::If {
            branches: vec![IfBranch {
                condition,
                body: Block::default(),
            }],
            else_body: None,
            active: IfActive::Branch(0),
        });
    }

    pub(crate) fn begin_else_if(&mut self, condition: Expr) {
        let Some(Frame::If {
            branches,
            else_body,
            active,
        }) = self.frames.last_mut()
        else {
            panic!("elseif outside an if frame");
        };
        assert!(else_body.is_none(), "elseif after else");
        branches.push(IfBranch {
            condition,
            body: Block::default(),
        });
        *active = IfActive::Branch(branches.len() - 1);
    }

    pub(crate) fn begin_else(&mut self) {
        let Some(Frame::If {
            else_body, active, ..
        }) = self.frames.last_mut()
        else {
            panic!("else outside an if frame");
        };
        assert!(else_body.is_none(), "duplicate else");
        *else_body = Some(Block::default());
        *active = IfActive::Else;
    }

    pub(crate) fn end_block(&mut self) {
        assert!(self.frames.len() > 1, "cannot close the root Lua block");
        let frame = self.frames.pop().expect("non-root frame exists");
        let statement = match frame {
            Frame::Root(_) => unreachable!(),
            Frame::Do(body) => Statement::Do(body),
            Frame::While { condition, body } => Statement::While { condition, body },
            Frame::NumericFor {
                variable,
                start,
                stop,
                body,
            } => Statement::NumericFor {
                variable,
                start,
                stop,
                body,
            },
            Frame::Function {
                target,
                params,
                body,
            } => Statement::Function {
                target,
                params,
                body,
            },
            Frame::If {
                branches,
                else_body,
                ..
            } => Statement::If {
                branches,
                else_body,
            },
        };
        self.statement(statement);
    }

    pub(crate) fn return_table(&mut self, fields: Vec<(String, Expr)>) {
        self.statement(Statement::Return(vec![Expr::named_table(fields)]));
    }

    pub(crate) fn compact_if(&mut self, condition: Expr, statements: Vec<InlineStatement>) {
        assert!(!statements.is_empty(), "compact if needs a body");
        self.statement(Statement::CompactIf {
            condition,
            statements,
        });
    }

    pub(crate) fn finish(self) -> Block {
        assert_eq!(self.frames.len(), 1, "unclosed Lua IR frame");
        match self.frames.into_iter().next().expect("root frame exists") {
            Frame::Root(block) => block,
            _ => unreachable!(),
        }
    }

    fn current_block_mut(&mut self) -> &mut Block {
        match self.frames.last_mut().expect("root frame exists") {
            Frame::Root(block)
            | Frame::Do(block)
            | Frame::While { body: block, .. }
            | Frame::NumericFor { body: block, .. }
            | Frame::Function { body: block, .. } => block,
            Frame::If {
                branches,
                else_body,
                active,
            } => match *active {
                IfActive::Branch(index) => &mut branches[index].body,
                IfActive::Else => else_body.as_mut().expect("active else body exists"),
            },
        }
    }

    fn statement(&mut self, statement: Statement) {
        let anchor = self.anchors.last().copied();
        self.current_block_mut()
            .entries
            .push(Entry::Statement { anchor, statement });
    }
}

pub(crate) fn print_with_source_map(block: &Block) -> Printed {
    let mut printer = Printer {
        output: String::new(),
        mappings: Vec::new(),
    };
    printer.block(block, 0);
    printer
        .mappings
        .sort_by_key(|mapping| (mapping.generated_start, mapping.generated_end));
    Printed {
        source: printer.output,
        mappings: printer.mappings,
    }
}

struct Printer {
    output: String,
    mappings: Vec<SourceMapping>,
}

impl Printer {
    fn block(&mut self, block: &Block, indent: usize) {
        for entry in &block.entries {
            match entry {
                Entry::Blank => self.output.push('\n'),
                Entry::Statement { anchor, statement } => {
                    let generated_start = self.output.len();
                    self.statement(statement, indent);
                    if let Some(source) = anchor {
                        self.mappings.push(SourceMapping {
                            generated_start,
                            generated_end: self.output.len(),
                            source: *source,
                        });
                    }
                }
            }
        }
    }

    fn statement(&mut self, statement: &Statement, indent: usize) {
        match statement {
            Statement::Annotation(source) => self.line(indent, source),
            Statement::Expression(expression) => {
                let source = self.expression(expression);
                self.line(indent, &source);
            }
            Statement::Local { names, values } => {
                let mut source = format!("local {}", names.join(", "));
                if !values.is_empty() {
                    source.push_str(" = ");
                    source.push_str(
                        &values
                            .iter()
                            .map(|value| self.expression(value))
                            .collect::<Vec<_>>()
                            .join(", "),
                    );
                }
                self.line(indent, &source);
            }
            Statement::Assign { targets, values } => {
                let targets = targets
                    .iter()
                    .map(|target| self.expression(target))
                    .collect::<Vec<_>>()
                    .join(", ");
                let values = values
                    .iter()
                    .map(|value| self.expression(value))
                    .collect::<Vec<_>>()
                    .join(", ");
                self.line(indent, &format!("{targets} = {values}"));
            }
            Statement::Return(values) => {
                if values.is_empty() {
                    self.line(indent, "return");
                } else if values.len() == 1 && matches!(values[0], Expr::Table(_)) {
                    self.table_return(indent, &values[0]);
                } else {
                    let values = values
                        .iter()
                        .map(|value| self.expression(value))
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.line(indent, &format!("return {values}"));
                }
            }
            Statement::Break => self.line(indent, "break"),
            Statement::Goto(label) => self.line(indent, &format!("goto {label}")),
            Statement::Label(label) => self.line(indent, &format!("::{label}::")),
            Statement::Do(body) => {
                self.line(indent, "do");
                self.block(body, indent + 1);
                self.line(indent, "end");
            }
            Statement::While { condition, body } => {
                self.line(indent, &format!("while {} do", self.expression(condition)));
                self.block(body, indent + 1);
                self.line(indent, "end");
            }
            Statement::NumericFor {
                variable,
                start,
                stop,
                body,
            } => {
                self.line(
                    indent,
                    &format!(
                        "for {variable} = {}, {} do",
                        self.expression(start),
                        self.expression(stop)
                    ),
                );
                self.block(body, indent + 1);
                self.line(indent, "end");
            }
            Statement::Function {
                target,
                params,
                body,
            } => {
                let prefix = if target.is_local() {
                    "local function"
                } else {
                    "function"
                };
                self.line(
                    indent,
                    &format!("{prefix} {}({})", target.source(), params.join(", ")),
                );
                self.block(body, indent + 1);
                self.line(indent, "end");
            }
            Statement::If {
                branches,
                else_body,
            } => {
                for (index, branch) in branches.iter().enumerate() {
                    let keyword = if index == 0 { "if" } else { "elseif" };
                    self.line(
                        indent,
                        &format!("{keyword} {} then", self.expression(&branch.condition)),
                    );
                    self.block(&branch.body, indent + 1);
                }
                if let Some(body) = else_body {
                    self.line(indent, "else");
                    self.block(body, indent + 1);
                }
                self.line(indent, "end");
            }
            Statement::CompactIf {
                condition,
                statements,
            } => {
                let condition = self.expression(condition);
                let statements = statements
                    .iter()
                    .map(|statement| self.inline_statement(statement))
                    .collect::<Vec<_>>()
                    .join("; ");
                self.line(indent, &format!("if {condition} then {statements} end"));
            }
        }
    }

    fn expression(&self, expression: &Expr) -> String {
        self.expression_at(expression, 0, false)
    }

    fn expression_at(&self, expression: &Expr, parent_precedence: u8, right_child: bool) -> String {
        match expression {
            Expr::Nil => "nil".to_string(),
            Expr::Bool(value) => value.to_string(),
            Expr::Integer(source) | Expr::Number(source) | Expr::StringLiteral(source) => {
                source.clone()
            }
            Expr::Name(name) => name.clone(),
            Expr::Field { base, name } => {
                format!("{}.{}", self.prefix_expression(base), name)
            }
            Expr::Index { base, index } => format!(
                "{}[{}]",
                self.prefix_expression(base),
                self.expression(index)
            ),
            Expr::Call { callee, args } => format!(
                "{}({})",
                self.prefix_expression(callee),
                self.expression_list(args)
            ),
            Expr::MethodCall {
                receiver,
                method,
                args,
            } => format!(
                "{}:{method}({})",
                self.prefix_expression(receiver),
                self.expression_list(args)
            ),
            Expr::Unary { op, expression } => {
                let source = self.expression_at(expression, 7, false);
                let rendered = match op {
                    UnaryOp::Neg => format!("-{source}"),
                    UnaryOp::Not => format!("not {source}"),
                    UnaryOp::Len => format!("#{source}"),
                };
                if 7 < parent_precedence {
                    format!("({rendered})")
                } else {
                    rendered
                }
            }
            Expr::Binary { left, op, right } => {
                let precedence = op.precedence();
                let left = self.expression_at(left, precedence, false);
                let right = self.expression_at(right, precedence, true);
                let rendered = format!("{left} {} {right}", op.source());
                // Lua concatenation is right-associative; the remaining binary
                // operators used by Rua are left-associative or non-associative.
                let needs_parens = precedence < parent_precedence
                    || (precedence == parent_precedence
                        && if *op == BinaryOp::Concat {
                            !right_child
                        } else {
                            right_child
                        });
                if needs_parens {
                    format!("({rendered})")
                } else {
                    rendered
                }
            }
            Expr::Table(fields) if fields.is_empty() => "{}".to_string(),
            Expr::Table(fields) => {
                let fields = fields
                    .iter()
                    .map(|field| match field {
                        TableField::Named(name, value) => {
                            let key = if is_lua_identifier(name) {
                                name.clone()
                            } else {
                                format!("[{}]", lua_string(name))
                            };
                            format!("{key} = {}", self.expression(value))
                        }
                        TableField::Indexed(index, value) => {
                            format!("[{}] = {}", self.expression(index), self.expression(value))
                        }
                        TableField::Value(value) => self.expression(value),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{{ {fields} }}")
            }
            Expr::Parenthesized(expression) => format!("({})", self.expression(expression)),
        }
    }

    fn prefix_expression(&self, expression: &Expr) -> String {
        let source = self.expression(expression);
        if matches!(
            expression,
            Expr::Name(_)
                | Expr::Field { .. }
                | Expr::Index { .. }
                | Expr::Call { .. }
                | Expr::MethodCall { .. }
                | Expr::Parenthesized(_)
        ) {
            source
        } else {
            format!("({source})")
        }
    }

    fn expression_list(&self, expressions: &[Expr]) -> String {
        expressions
            .iter()
            .map(|expression| self.expression(expression))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn inline_statement(&self, statement: &InlineStatement) -> String {
        match statement {
            InlineStatement::Expression(expression) => self.expression(expression),
            InlineStatement::Assign { target, value } => {
                format!("{} = {}", self.expression(target), self.expression(value))
            }
            InlineStatement::Return(values) if values.is_empty() => "return".to_string(),
            InlineStatement::Return(values) => format!(
                "return {}",
                values
                    .iter()
                    .map(|value| self.expression(value))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            InlineStatement::Break => "break".to_string(),
        }
    }

    fn table_return(&mut self, indent: usize, expression: &Expr) {
        let Expr::Table(fields) = expression else {
            unreachable!("table return requires a table expression")
        };
        self.line(indent, "return {");
        for field in fields {
            let source = match field {
                TableField::Named(name, value) => {
                    let key = if is_lua_identifier(name) {
                        name.clone()
                    } else {
                        format!("[{}]", lua_string(name))
                    };
                    format!("{key} = {},", self.expression(value))
                }
                TableField::Indexed(index, value) => {
                    format!("[{}] = {},", self.expression(index), self.expression(value))
                }
                TableField::Value(value) => format!("{},", self.expression(value)),
            };
            self.line(indent + 1, &source);
        }
        self.line(indent, "}");
    }

    fn line(&mut self, indent: usize, source: &str) {
        for _ in 0..indent {
            self.output.push_str("    ");
        }
        self.output.push_str(source);
        self.output.push('\n');
    }
}

fn lua_string(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    )
}

fn is_lua_identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && ![
            "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto",
            "if", "in", "local", "nil", "not", "or", "repeat", "return", "then", "true", "until",
            "while",
        ]
        .contains(&value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printer_owns_nested_control_flow_and_indentation() {
        let mut builder = Builder::new();
        builder.begin_function(
            FunctionTarget::path(vec!["M".into(), "run".into()]),
            vec!["x".into()],
        );
        builder.begin_if(Expr::name("x").binary(BinaryOp::Gt, Expr::integer("0")));
        builder.begin_while(Expr::name("x").binary(BinaryOp::Gt, Expr::integer("1")));
        builder.assign(
            Expr::name("x"),
            Expr::name("x").binary(BinaryOp::Sub, Expr::integer("1")),
        );
        builder.end_block();
        builder.begin_else_if(Expr::name("x").binary(BinaryOp::Eq, Expr::integer("0")));
        builder.return_values(vec![Expr::name("x")]);
        builder.begin_else();
        builder.begin_do();
        builder.return_values(vec![Expr::Nil]);
        builder.end_block();
        builder.end_block();
        builder.end_block();

        assert_eq!(
            print_with_source_map(&builder.finish()).source,
            "function M.run(x)\n\
             \x20   if x > 0 then\n\
             \x20       while x > 1 do\n\
             \x20           x = x - 1\n\
             \x20       end\n\
             \x20   elseif x == 0 then\n\
             \x20       return x\n\
             \x20   else\n\
             \x20       do\n\
             \x20           return nil\n\
             \x20       end\n\
             \x20   end\n\
             end\n"
        );
    }

    #[test]
    fn printer_preserves_source_anchors_for_structured_statements() {
        let mut builder = Builder::new();
        let source = SourceRange {
            start: 7,
            len: 3,
            line: 2,
            file: 4,
        };
        builder.push_anchor(source);
        builder.local(vec!["value".into()], vec![Expr::integer("1")]);
        builder.pop_anchor();

        let printed = print_with_source_map(&builder.finish());
        assert_eq!(printed.source, "local value = 1\n");
        assert_eq!(
            printed.mappings,
            vec![SourceMapping {
                generated_start: 0,
                generated_end: printed.source.len(),
                source,
            }]
        );
    }
}
