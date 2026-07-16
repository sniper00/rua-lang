use std::collections::{HashMap, HashSet};

use super::{Body, Condition, Expr, ExprId, Pat, Statement};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StatementId {
    block: ExprId,
    index: usize,
}

impl StatementId {
    pub const fn block(self) -> ExprId {
        self.block
    }

    pub const fn index(self) -> usize {
        self.index
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Node {
    successors: Vec<usize>,
}

#[derive(Clone, Copy)]
struct LoopTargets {
    break_target: usize,
    continue_target: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LoopRecord {
    body: ExprId,
    body_entry: usize,
    exit: usize,
}

/// Per-body control-flow graph. Closures are independent callable entries but
/// share the same graph arena so diagnostics can query every HIR statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlFlowGraph {
    nodes: Vec<Node>,
    entry: usize,
    exit: usize,
    statement_nodes: HashMap<StatementId, usize>,
    loops: Vec<LoopRecord>,
}

impl ControlFlowGraph {
    pub fn build(body: &Body) -> Self {
        let mut builder = Builder {
            body,
            graph: Self {
                nodes: Vec::new(),
                entry: 0,
                exit: 0,
                statement_nodes: HashMap::new(),
                loops: Vec::new(),
            },
        };
        let entry = builder.node();
        let exit = builder.node();
        builder.graph.entry = entry;
        builder.graph.exit = exit;

        let root = builder.expr(body.root_expr(), exit, None);
        builder.edge(entry, root);

        let closures = body
            .exprs()
            .filter_map(|(_, expression)| match expression {
                Expr::Closure { body, .. } => Some(*body),
                _ => None,
            })
            .collect::<Vec<_>>();
        for closure_body in closures {
            let closure_entry = builder.expr(closure_body, exit, None);
            builder.edge(entry, closure_entry);
        }
        builder.graph
    }

    pub fn unreachable_statements(&self) -> impl Iterator<Item = StatementId> + '_ {
        let reachable = self.reachable_from(self.entry);
        self.statement_nodes
            .iter()
            .filter_map(move |(statement, node)| (!reachable.contains(node)).then_some(*statement))
    }

    pub fn infinite_loops(&self) -> impl Iterator<Item = ExprId> + '_ {
        self.loops.iter().filter_map(|record| {
            let reaches_exit = self.path_exists(record.body_entry, record.exit)
                || self.path_exists(record.body_entry, self.exit);
            (!reaches_exit).then_some(record.body)
        })
    }

    fn reachable_from(&self, start: usize) -> HashSet<usize> {
        let mut reachable = HashSet::new();
        let mut pending = vec![start];
        while let Some(node) = pending.pop() {
            if !reachable.insert(node) {
                continue;
            }
            pending.extend(self.nodes[node].successors.iter().copied());
        }
        reachable
    }

    fn path_exists(&self, start: usize, target: usize) -> bool {
        self.reachable_from(start).contains(&target)
    }
}

struct Builder<'a> {
    body: &'a Body,
    graph: ControlFlowGraph,
}

impl Builder<'_> {
    fn node(&mut self) -> usize {
        let id = self.graph.nodes.len();
        self.graph.nodes.push(Node::default());
        id
    }

    fn edge(&mut self, from: usize, to: usize) {
        if !self.graph.nodes[from].successors.contains(&to) {
            self.graph.nodes[from].successors.push(to);
        }
    }

    fn expr(
        &mut self,
        expression: ExprId,
        next: usize,
        loop_targets: Option<LoopTargets>,
    ) -> usize {
        match self.body.expr(expression) {
            Some(Expr::Unary { expr, .. })
            | Some(Expr::Try { expr })
            | Some(Expr::Paren { expr })
            | Some(Expr::Field { base: expr, .. }) => self.expr(*expr, next, loop_targets),
            Some(Expr::Binary { lhs, rhs, .. })
            | Some(Expr::Range {
                start: lhs,
                end: rhs,
                ..
            }) => {
                let rhs = self.expr(*rhs, next, loop_targets);
                self.expr(*lhs, rhs, loop_targets)
            }
            Some(Expr::Loop { body }) => {
                let head = self.node();
                let body_entry = self.expr(
                    *body,
                    head,
                    Some(LoopTargets {
                        break_target: next,
                        continue_target: head,
                    }),
                );
                self.edge(head, body_entry);
                self.graph.loops.push(LoopRecord {
                    body: *body,
                    body_entry,
                    exit: next,
                });
                head
            }
            Some(Expr::Assign { target, value, .. }) => {
                let value = self.expr(*value, next, loop_targets);
                self.expr(*target, value, loop_targets)
            }
            Some(Expr::Call { callee, args }) => {
                let args = self.expr_sequence(args, next, loop_targets);
                self.expr(*callee, args, loop_targets)
            }
            Some(Expr::MethodCall { receiver, args, .. }) => {
                let args = self.expr_sequence(args, next, loop_targets);
                self.expr(*receiver, args, loop_targets)
            }
            Some(Expr::Index { base, index }) => {
                let index = self.expr(*index, next, loop_targets);
                self.expr(*base, index, loop_targets)
            }
            Some(Expr::MapLiteral { entries }) => {
                let mut continuation = next;
                for entry in entries.iter().rev() {
                    continuation = self.expr(entry.value(), continuation, loop_targets);
                    continuation = self.expr(entry.key(), continuation, loop_targets);
                }
                continuation
            }
            Some(Expr::If {
                condition,
                then_branch,
                else_branch,
            }) => {
                let branch = self.node();
                let then_entry = self.expr(*then_branch, next, loop_targets);
                self.edge(branch, then_entry);
                let else_entry = else_branch
                    .map(|expression| self.expr(expression, next, loop_targets))
                    .unwrap_or(next);
                self.edge(branch, else_entry);
                self.condition(*condition, branch, loop_targets)
            }
            Some(Expr::Match { scrutinee, arms }) => {
                let branch = self.node();
                let mut exhaustive = false;
                for arm in arms {
                    let body_entry = self.expr(arm.body(), next, loop_targets);
                    let arm_entry = arm
                        .guard()
                        .map(|guard| self.expr(guard, body_entry, loop_targets))
                        .unwrap_or(body_entry);
                    self.edge(branch, arm_entry);
                    exhaustive |= arm.guard().is_none()
                        && arm.patterns().iter().any(|pattern| {
                            matches!(
                                self.body.pattern(*pattern),
                                Some(Pat::Wildcard | Pat::Binding { .. })
                            )
                        });
                }
                if !exhaustive {
                    self.edge(branch, next);
                }
                self.expr(*scrutinee, branch, loop_targets)
            }
            Some(Expr::StructLiteral { fields, .. }) => {
                let values = fields.iter().map(|field| field.value()).collect::<Vec<_>>();
                self.expr_sequence(&values, next, loop_targets)
            }
            Some(Expr::MacroCall { args, .. }) => self.expr_sequence(args, next, loop_targets),
            Some(Expr::Block(block)) => self.block(
                expression,
                block.statements(),
                block.tail(),
                next,
                loop_targets,
            ),
            Some(Expr::Missing | Expr::Literal(_) | Expr::Path(_) | Expr::Closure { .. })
            | None => next,
        }
    }

    fn expr_sequence(
        &mut self,
        expressions: &[ExprId],
        next: usize,
        loop_targets: Option<LoopTargets>,
    ) -> usize {
        expressions.iter().rev().fold(next, |next, expression| {
            self.expr(*expression, next, loop_targets)
        })
    }

    fn condition(
        &mut self,
        condition: Condition,
        next: usize,
        loop_targets: Option<LoopTargets>,
    ) -> usize {
        let expression = match condition {
            Condition::Expr(expression) => expression,
            Condition::Let { scrutinee, .. } => scrutinee,
        };
        self.expr(expression, next, loop_targets)
    }

    fn block(
        &mut self,
        block: ExprId,
        statements: &[Statement],
        tail: Option<ExprId>,
        next: usize,
        loop_targets: Option<LoopTargets>,
    ) -> usize {
        let mut continuation = tail
            .map(|tail| self.expr(tail, next, loop_targets))
            .unwrap_or(next);
        for (index, statement) in statements.iter().enumerate().rev() {
            let statement_id = StatementId { block, index };
            let node = self.node();
            self.graph.statement_nodes.insert(statement_id, node);
            match statement {
                Statement::Missing => self.edge(node, continuation),
                Statement::Let { initializer, .. } => {
                    let expression = self.expr(*initializer, continuation, loop_targets);
                    self.edge(node, expression);
                }
                Statement::Expr { expr, .. } => {
                    let expression = self.expr(*expr, continuation, loop_targets);
                    self.edge(node, expression);
                }
                Statement::Return { value } => {
                    let expression = value
                        .map(|value| self.expr(value, self.graph.exit, loop_targets))
                        .unwrap_or(self.graph.exit);
                    self.edge(node, expression);
                }
                Statement::Break { value } => {
                    let target = loop_targets
                        .map(|targets| targets.break_target)
                        .unwrap_or(self.graph.exit);
                    let entry = value
                        .map(|value| self.expr(value, target, loop_targets))
                        .unwrap_or(target);
                    self.edge(node, entry);
                }
                Statement::Continue => {
                    self.edge(
                        node,
                        loop_targets
                            .map(|targets| targets.continue_target)
                            .unwrap_or(self.graph.exit),
                    );
                }
                Statement::While { condition, body } => {
                    let branch = self.node();
                    let body_entry = self.expr(
                        *body,
                        node,
                        Some(LoopTargets {
                            break_target: continuation,
                            continue_target: node,
                        }),
                    );
                    self.edge(branch, body_entry);
                    self.edge(branch, continuation);
                    let condition = self.condition(*condition, branch, loop_targets);
                    self.edge(node, condition);
                }
                Statement::Loop { body } => {
                    let body_entry = self.expr(
                        *body,
                        node,
                        Some(LoopTargets {
                            break_target: continuation,
                            continue_target: node,
                        }),
                    );
                    self.edge(node, body_entry);
                    self.graph.loops.push(LoopRecord {
                        body: *body,
                        body_entry,
                        exit: continuation,
                    });
                }
                Statement::For { iterable, body, .. } => {
                    let branch = self.node();
                    let body_entry = self.expr(
                        *body,
                        branch,
                        Some(LoopTargets {
                            break_target: continuation,
                            continue_target: branch,
                        }),
                    );
                    self.edge(branch, body_entry);
                    self.edge(branch, continuation);
                    let iterable = self.expr(*iterable, branch, loop_targets);
                    self.edge(node, iterable);
                }
            }
            continuation = node;
        }
        continuation
    }
}
