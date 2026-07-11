//! Protocol-neutral closure and iterator IDE facts, powered by native inference.

use std::rc::Rc;

use rua_syntax::{
    AstNode, Named, SyntaxKind,
    ast::{ClosureExpr, MethodCallExpr, RangeExpr},
};

use crate::{
    BaseDb,
    hir::{BindingKind, DefKind, Expr, LocalBindingId},
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

pub(super) fn closure_parameters(db: &Rc<BaseDb>, file_id: FileId) -> Vec<ClosureParameterInfo> {
    let def_map = db.def_map(file_id);
    let mut result = Vec::new();

    for definition in def_map.definitions() {
        if definition.file_id() != file_id {
            continue;
        }
        if !matches!(definition.kind(), DefKind::Function | DefKind::Method) {
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

pub(super) fn semantic_tokens(db: &Rc<BaseDb>, file_id: FileId) -> Vec<SemanticToken> {
    let parse = db.parse(file_id);
    let root = parse.syntax_node();
    let mut tokens = Vec::new();

    // Closure parameter declarations and references — powered by native local
    // resolution instead of rua_syntax::nameres::references_at.
    let def_map = db.def_map(file_id);
    let semantics = Semantics::new(Rc::clone(db), def_map);

    for closure in root.descendants().filter_map(ClosureExpr::cast) {
        for param in closure.params() {
            if let Some(name) = param.name() {
                let param_range = text_range(name.text_range());
                let definition_file_range = FileRange::new(file_id, param_range);
                // Find the matching local binding target by scanning the file's
                // body data for a binding whose source range matches this parameter.
                if let Some(target) = find_closure_param_target(
                    db,
                    file_id,
                    definition_file_range,
                ) {
                    let include_declaration = true;
                    let references =
                        semantics.local_references(target, include_declaration);
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

    SemanticToken::normalize(&mut tokens);
    tokens
}

/// Locate the [`LocalBindingId`] for a closure parameter at the given source
/// range by scanning function bodies in the file.
fn find_closure_param_target(
    db: &BaseDb,
    file_id: FileId,
    definition_range: FileRange,
) -> Option<LocalBindingId> {
    let def_map = db.def_map(file_id);
    for definition in def_map.definitions() {
        if definition.file_id() != file_id {
            continue;
        }
        if !matches!(definition.kind(), DefKind::Function | DefKind::Method) {
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
