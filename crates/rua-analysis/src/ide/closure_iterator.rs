//! Protocol-neutral closure and iterator IDE facts, powered by native inference.

use std::sync::Arc;

use rua_syntax::{
    AstNode, Named, SyntaxKind,
    ast::{ClosureExpr, MethodCallExpr, RangeExpr},
};

use crate::{
    BaseDb,
    hir::{BindingKind, DefMap, Expr, LocalBindingId},
    semantic::Semantics,
    vfs::FileId,
};

use super::{FileRange, SemanticToken, SemanticTokenKind, SemanticTokenModifiers};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosureParameterInfo {
    file_id: FileId,
    name: String,
    range: crate::TextRange,
    ty: String,
}

impl ClosureParameterInfo {
    pub fn file_id(&self) -> FileId {
        self.file_id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn range(&self) -> crate::TextRange {
        self.range
    }

    pub fn ty(&self) -> &str {
        &self.ty
    }
}

pub(super) fn closure_parameters(
    db: &Arc<BaseDb>,
    def_map: &DefMap,
    file_id: FileId,
) -> Vec<ClosureParameterInfo> {
    let mut result = Vec::new();

    for definition in def_map.definitions() {
        if definition.file_id() != file_id {
            continue;
        }
        if !definition.kind().is_body_owner() {
            continue;
        }
        let Some(body) = db.body(definition.id()) else {
            continue;
        };
        let Some(source_map) = db.body_source_map(definition.id()) else {
            continue;
        };
        let Some(inference) = db.infer(definition.id()) else {
            continue;
        };

        for (_expr_id, expr) in body.exprs() {
            if let Expr::Closure { params, .. } = expr {
                for param in params {
                    let name = body
                        .binding(*param)
                        .and_then(|binding| binding.name())
                        .unwrap_or("?");
                    let range = source_map
                        .binding_range(*param)
                        .map(|file_range| file_range.range)
                        .unwrap_or_else(|| crate::TextRange::new(0, 0));
                    let ty = inference
                        .type_of_binding(*param)
                        .map(|ty| ty.to_string())
                        .unwrap_or_else(|| "Unknown".to_string());
                    result.push(ClosureParameterInfo {
                        file_id,
                        name: name.to_string(),
                        range,
                        ty,
                    });
                }
            }
        }
    }
    result
}

pub(super) fn semantic_tokens(
    db: &Arc<BaseDb>,
    def_map: std::sync::Arc<DefMap>,
    file_id: FileId,
) -> Vec<SemanticToken> {
    let parse = db.parse(file_id);
    let root = parse.syntax_node();
    let mut tokens = Vec::new();

    // Closure parameter declarations and references — powered by native local
    // resolution instead of scanning identifier text.
    let semantics = Semantics::new(Arc::clone(db), def_map.clone());

    for closure in root.descendants().filter_map(ClosureExpr::cast) {
        for param in closure.params() {
            if let Some(name) = param.name() {
                let param_range = text_range(name.text_range());
                let definition_file_range = FileRange::new(file_id, param_range);
                // Find the matching local binding target by scanning the file's
                // body data for a binding whose source range matches this parameter.
                if let Some(target) =
                    find_closure_param_target(db, &def_map, file_id, definition_file_range)
                {
                    let include_declaration = true;
                    let references = semantics.local_references(target, include_declaration);
                    for reference in references {
                        let modifiers = if reference == definition_file_range {
                            SemanticTokenModifiers::DECLARATION
                        } else {
                            SemanticTokenModifiers::NONE
                        };
                        tokens.push(SemanticToken::new(
                            reference,
                            SemanticTokenKind::Parameter,
                            modifiers,
                        ));
                    }
                } else {
                    // Fallback: emit at least the declaration token.
                    tokens.push(SemanticToken::new(
                        definition_file_range,
                        SemanticTokenKind::Parameter,
                        SemanticTokenModifiers::DECLARATION,
                    ));
                }
            }
        }
    }

    // Method name tokens.
    for method in root.descendants().filter_map(MethodCallExpr::cast) {
        if let Some(name) = method.method_name() {
            tokens.push(SemanticToken::new(
                FileRange::new(file_id, text_range(name.text_range())),
                SemanticTokenKind::Method,
                SemanticTokenModifiers::NONE,
            ));
        }
    }

    // Range operator tokens.
    for range in root.descendants().filter_map(RangeExpr::cast) {
        if let Some(operator) = range
            .syntax()
            .children_with_tokens()
            .filter_map(|element| element.into_token())
            .find(|token| matches!(token.kind(), SyntaxKind::DotDot | SyntaxKind::DotDotEq))
        {
            tokens.push(SemanticToken::new(
                FileRange::new(file_id, text_range(operator.text_range())),
                SemanticTokenKind::Operator,
                SemanticTokenModifiers::NONE,
            ));
        }
    }

    // Emit mutable keyword tokens for `mut` in let bindings.
    for token in root.descendants_with_tokens().filter_map(|el| match el {
        rowan::NodeOrToken::Token(tok) if tok.kind() == SyntaxKind::KwMut => Some(tok),
        _ => None,
    }) {
        let range = text_range(token.text_range());
        tokens.push(SemanticToken::new(
            FileRange::new(file_id, range),
            SemanticTokenKind::Keyword,
            SemanticTokenModifiers::MUTABLE,
        ));
    }

    // Emit builtin type tokens with defaultLibrary modifier.
    const BUILTIN_TYPES: &[SyntaxKind] =
        &[SyntaxKind::KwTrue, SyntaxKind::KwFalse, SyntaxKind::KwSelf];
    for token in root.descendants_with_tokens().filter_map(|el| match el {
        rowan::NodeOrToken::Token(tok) if BUILTIN_TYPES.contains(&tok.kind()) => Some(tok),
        _ => None,
    }) {
        let range = text_range(token.text_range());
        tokens.push(SemanticToken::new(
            FileRange::new(file_id, range),
            SemanticTokenKind::Keyword,
            SemanticTokenModifiers::DEFAULT_LIBRARY,
        ));
    }

    // Emit variable/parameter declaration and reference tokens.
    for definition in def_map.definitions() {
        if definition.file_id() != file_id {
            continue;
        }
        if !definition.kind().is_body_owner() {
            continue;
        }
        let Some(body) = db.body(definition.id()) else {
            continue;
        };
        let Some(source_map) = db.body_source_map(definition.id()) else {
            continue;
        };
        let Some(resolution) = db.body_resolution(definition.id()) else {
            continue;
        };
        for (binding_id, binding) in body.bindings() {
            let Some(_name) = binding.name() else {
                continue;
            };
            if _name.starts_with('_') {
                continue;
            }
            let Some(fr) = source_map.binding_range(binding_id) else {
                continue;
            };
            let kind = match binding.kind() {
                BindingKind::Parameter => SemanticTokenKind::Parameter,
                _ => SemanticTokenKind::Variable,
            };
            // Emit declaration.
            tokens.push(SemanticToken::new(
                fr,
                kind,
                SemanticTokenModifiers::DECLARATION,
            ));
            // Emit references.
            let lid = crate::hir::LocalBindingId::new(body.id(), binding_id);
            for local_use in resolution.uses_for(lid) {
                if let Some(use_fr) = source_map.name_ref_range(local_use.name_ref())
                    && use_fr != fr
                {
                    tokens.push(SemanticToken::new(
                        use_fr,
                        kind,
                        SemanticTokenModifiers::NONE,
                    ));
                }
            }
        }
    }

    // Emit string/number/comment tokens.
    for token in root.descendants_with_tokens().filter_map(|el| match el {
        rowan::NodeOrToken::Token(tok) => Some(tok),
        _ => None,
    }) {
        let kind = match token.kind() {
            SyntaxKind::Str => Some(SemanticTokenKind::String),
            SyntaxKind::Int | SyntaxKind::Float => Some(SemanticTokenKind::Number),
            SyntaxKind::LineComment | SyntaxKind::BlockComment => Some(SemanticTokenKind::Comment),
            _ => None,
        };
        if let Some(k) = kind {
            let range = text_range(token.text_range());
            tokens.push(SemanticToken::new(
                FileRange::new(file_id, range),
                k,
                SemanticTokenModifiers::NONE,
            ));
        }
    }

    SemanticToken::normalize(&mut tokens);
    tokens
}

/// Locate the [`LocalBindingId`] for a closure parameter at the given source
/// range by scanning function bodies in the file.
fn find_closure_param_target(
    db: &BaseDb,
    def_map: &DefMap,
    file_id: FileId,
    definition_range: FileRange,
) -> Option<LocalBindingId> {
    for definition in def_map.definitions() {
        if definition.file_id() != file_id {
            continue;
        }
        if !definition.kind().is_body_owner() {
            continue;
        }
        let body = db.body(definition.id())?;
        let source_map = db.body_source_map(definition.id())?;
        for (binding_id, binding) in body.bindings() {
            if binding.kind() != BindingKind::ClosureParameter {
                continue;
            }
            if source_map.binding_range(binding_id) == Some(definition_range) {
                return Some(LocalBindingId::new(body.id(), binding_id));
            }
        }
    }
    None
}

fn text_range(range: rowan::TextRange) -> crate::TextRange {
    crate::TextRange::new(range.start().into(), range.end().into())
}
