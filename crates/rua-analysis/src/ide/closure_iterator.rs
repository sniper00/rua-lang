//! Protocol-neutral closure and iterator IDE facts.

use rua_syntax::{
    AstNode, Named, SyntaxKind,
    ast::{ClosureExpr, MethodCallExpr, RangeExpr},
};

use crate::{BaseDb, FileId, TextRange};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosureParameterInfo {
    file_id: FileId,
    name: String,
    range: TextRange,
    ty: String,
}

impl ClosureParameterInfo {
    pub fn file_id(&self) -> FileId {
        self.file_id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn range(&self) -> TextRange {
        self.range
    }

    pub fn ty(&self) -> &str {
        &self.ty
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SemanticTokenKind {
    ClosureParameter,
    Method,
    RangeOperator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticToken {
    file_id: FileId,
    range: TextRange,
    kind: SemanticTokenKind,
    declaration: bool,
}

impl SemanticToken {
    pub fn file_id(&self) -> FileId {
        self.file_id
    }

    pub fn range(&self) -> TextRange {
        self.range
    }

    pub fn kind(&self) -> SemanticTokenKind {
        self.kind
    }

    pub fn is_declaration(&self) -> bool {
        self.declaration
    }
}

pub(super) fn closure_parameters(db: &BaseDb, file_id: FileId) -> Vec<ClosureParameterInfo> {
    let Some(text) = db.file_text(file_id) else {
        return Vec::new();
    };
    let parse = db.parse(file_id);

    // Phase 4A keeps type parity with the compiler-backed binding index already
    // isolated inside rua-syntax. Phase 5 replaces this transition query with
    // native body inference while preserving this protocol-neutral result.
    let typed = rua_syntax::analysis::Analysis::new(&text);
    parse
        .syntax_node()
        .descendants()
        .filter_map(ClosureExpr::cast)
        .flat_map(|closure| closure.params().collect::<Vec<_>>())
        .filter_map(|param| {
            let name = param.name()?;
            let range = text_range(name.text_range());
            let marker = format!("closure parameter {}: ", name.text());
            let ty = typed
                .definition_at(range.start() as usize)
                .and_then(|resolution| resolution.detail.strip_prefix(&marker).map(str::to_owned))
                .unwrap_or_else(|| "Unknown".to_string());
            Some(ClosureParameterInfo {
                file_id,
                name: name.text().to_string(),
                range,
                ty,
            })
        })
        .collect()
}

pub(super) fn semantic_tokens(db: &BaseDb, file_id: FileId) -> Vec<SemanticToken> {
    let parse = db.parse(file_id);
    let root = parse.syntax_node();
    let mut tokens = Vec::new();
    let symbols = rua_syntax::symbols::collect_symbols(parse.tree());

    for closure in root.descendants().filter_map(ClosureExpr::cast) {
        for param in closure.params() {
            if let Some(name) = param.name() {
                let definition = text_range(name.text_range());
                for (start, end) in rua_syntax::nameres::references_at(
                    parse.tree(),
                    &symbols,
                    definition.start() as usize,
                ) {
                    let range = TextRange::new(start as u32, end as u32);
                    tokens.push(SemanticToken {
                        file_id,
                        range,
                        kind: SemanticTokenKind::ClosureParameter,
                        declaration: range == definition,
                    });
                }
            }
        }
    }

    for method in root.descendants().filter_map(MethodCallExpr::cast) {
        if let Some(name) = method.method_name() {
            tokens.push(SemanticToken {
                file_id,
                range: text_range(name.text_range()),
                kind: SemanticTokenKind::Method,
                declaration: false,
            });
        }
    }

    for range in root.descendants().filter_map(RangeExpr::cast) {
        if let Some(operator) = range
            .syntax()
            .children_with_tokens()
            .filter_map(|element| element.into_token())
            .find(|token| matches!(token.kind(), SyntaxKind::DotDot | SyntaxKind::DotDotEq))
        {
            tokens.push(SemanticToken {
                file_id,
                range: text_range(operator.text_range()),
                kind: SemanticTokenKind::RangeOperator,
                declaration: false,
            });
        }
    }

    tokens.sort_by_key(|token| (token.range.start(), token.range.end()));
    tokens.dedup_by_key(|token| (token.range, token.kind));
    tokens
}

fn text_range(range: rowan::TextRange) -> TextRange {
    TextRange::new(range.start().into(), range.end().into())
}
