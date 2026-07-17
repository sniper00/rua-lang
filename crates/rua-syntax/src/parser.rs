//! Recursive-descent CST parser producing a lossless rowan tree.
//!
//! The grammar is a faithful mirror of the semantic parser in
//! `ruac` (`src/parser.rs`) — same productions, same precedence
//! (see [`binop`]), same `no_struct` rule for conditions/scrutinees — but instead
//! of building a typed AST it emits a rowan green tree via [`TreeBuilder`], with
//! **all trivia retained** so `parse_source_file(src).syntax_node().text() == src`.
//!
//! Differences from the semantic parser, by design:
//!   * error resilience: a parse error never aborts; it is recorded and the
//!     parser recovers by wrapping the offending token in an `ErrorNode`, so the
//!     tree is always complete and round-trips.
//!   * parentheses are kept as a `ParenExpr` node (the semantic parser folds them
//!     away) — required for losslessness.
//!
//! The two grammars are kept in sync by a conformance corpus (see tests + P6-4).

use crate::TreeBuilder;
use crate::ast::{AstNode, SourceFile};
use crate::kind::{SyntaxKind, SyntaxNode};
use crate::lexer::{LexToken, lex};
use rua_core::DiagnosticCode;
use rua_lex::LexErrorKind;

use SyntaxKind as K;

/// A parse error: a message plus the byte offset where it was detected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub code: DiagnosticCode,
    pub message: String,
    pub offset: usize,
}

/// A typed syntax tree plus every error recovered while parsing it.
///
/// The parser always returns a tree, including for malformed input. Consumers
/// that require valid input should check [`errors`](Self::errors) or use
/// [`ok`](Self::ok) before operating on the tree.
#[derive(Debug, Clone)]
#[must_use = "parse results contain a typed tree and recovered errors"]
pub struct Parse<T> {
    pub tree: T,
    pub errors: Vec<ParseError>,
}

impl<T> Parse<T> {
    /// The typed root node.
    pub fn tree(&self) -> &T {
        &self.tree
    }

    /// Parse errors in source order. Empty means the parser accepted the input.
    pub fn errors(&self) -> &[ParseError] {
        &self.errors
    }

    /// Consume the parse result, returning the typed tree only when error-free.
    pub fn ok(self) -> Result<T, Vec<ParseError>> {
        if self.errors.is_empty() {
            Ok(self.tree)
        } else {
            Err(self.errors)
        }
    }
}

impl<T: AstNode> Parse<T> {
    /// The untyped rowan node backing the typed root.
    pub fn syntax_node(&self) -> &SyntaxNode {
        self.tree.syntax()
    }
}

/// Parse Rua `src` into a lossless CST. Never panics; on malformed input the
/// tree still covers the whole source and `errors` is non-empty.
pub fn parse_source_file(src: &str) -> Parse<SourceFile> {
    let tokens = lex(src);
    let lexical_errors = tokens
        .iter()
        .filter_map(|token| {
            let error = token.error?;
            let code = match error {
                LexErrorKind::UnterminatedString => DiagnosticCode::ParseUnterminatedString,
                LexErrorKind::UnterminatedBlockComment => DiagnosticCode::ParseUnterminatedComment,
                LexErrorKind::UnknownCharacter => DiagnosticCode::ParseUnexpectedToken,
            };
            Some(ParseError {
                code,
                message: error.message().to_string(),
                offset: token.start,
            })
        })
        .collect();
    let mut p = Parser {
        src,
        tokens,
        pos: 0,
        builder: TreeBuilder::new(),
        errors: lexical_errors,
        no_struct: false,
    };
    p.source_file();
    let Parser {
        builder, errors, ..
    } = p;
    let syntax = builder.finish();
    let tree = SourceFile::cast_root(syntax)
        .expect("parser invariant: source_file always builds a SourceFile root");
    Parse { tree, errors }
}

/// Compatibility shorthand for [`parse_source_file`]. New parser-facing code
/// should prefer the explicit entry point.
pub fn parse(src: &str) -> Parse<SourceFile> {
    parse_source_file(src)
}

struct Parser<'a> {
    src: &'a str,
    tokens: Vec<LexToken>,
    /// Cursor over `tokens`, including trivia.
    pos: usize,
    builder: TreeBuilder,
    errors: Vec<ParseError>,
    /// When true, `Ident {` is a block boundary, not a struct literal (inside
    /// `if`/`while`/`for`/`match` heads). Mirrors the semantic parser.
    no_struct: bool,
}

impl<'a> Parser<'a> {
    // --- low-level token cursor -------------------------------------------

    /// Index in `tokens` of the `n`-th upcoming non-trivia token.
    fn nth_token(&self, n: usize) -> Option<usize> {
        let mut count = 0;
        let mut i = self.pos;
        while i < self.tokens.len() {
            if self.tokens[i].kind.is_trivia() {
                i += 1;
                continue;
            }
            if count == n {
                return Some(i);
            }
            count += 1;
            i += 1;
        }
        None
    }

    /// Kind of the `n`-th upcoming non-trivia token (`Eof` past the end).
    fn nth(&self, n: usize) -> SyntaxKind {
        self.nth_token(n)
            .map(|i| self.tokens[i].kind)
            .unwrap_or(K::Eof)
    }

    fn current(&self) -> SyntaxKind {
        self.nth(0)
    }

    fn at(&self, kind: SyntaxKind) -> bool {
        self.current() == kind
    }

    /// Text of the current non-trivia token (empty at EOF).
    fn current_text(&self) -> &'a str {
        match self.nth_token(0) {
            Some(i) => {
                let t = self.tokens[i];
                &self.src[t.start..t.start + t.len]
            }
            None => "",
        }
    }

    fn current_offset(&self) -> usize {
        self.nth_token(0)
            .map(|i| self.tokens[i].start)
            .unwrap_or(self.src.len())
    }

    /// A contextual keyword (`where`) — lexed as an identifier.
    fn at_contextual(&self, word: &str) -> bool {
        self.at(K::Ident) && self.current_text() == word
    }

    fn push_tok(&mut self, i: usize) {
        let t = self.tokens[i];
        let text = &self.src[t.start..t.start + t.len];
        self.builder.token(t.kind, text);
    }

    /// Emit pending trivia (whitespace/comments) into the current node.
    fn eat_trivia(&mut self) {
        while self.pos < self.tokens.len() && self.tokens[self.pos].kind.is_trivia() {
            self.push_tok(self.pos);
            self.pos += 1;
        }
    }

    /// Emit leading trivia then the next real token, advancing the cursor.
    fn bump(&mut self) {
        self.eat_trivia();
        if self.pos < self.tokens.len() {
            self.push_tok(self.pos);
            self.pos += 1;
        }
    }

    fn accept(&mut self, kind: SyntaxKind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: SyntaxKind) {
        if self.at(kind) {
            self.bump();
        } else {
            let code = if matches!(kind, K::RParen | K::RBrace | K::RBracket | K::Gt) {
                DiagnosticCode::ParseMissingDelimiter
            } else {
                DiagnosticCode::ParseUnexpectedToken
            };
            self.error_with_code(code, &format!("expected `{}`", user_str(kind)));
        }
    }

    /// Names: an identifier or `self`.
    fn expect_ident(&mut self) {
        if self.at(K::Ident) || self.at(K::KwSelf) {
            self.bump();
        } else {
            self.error("expected identifier");
        }
    }

    fn error(&mut self, msg: &str) {
        self.error_with_code(DiagnosticCode::ParseUnexpectedToken, msg);
    }

    fn error_with_code(&mut self, code: DiagnosticCode, msg: &str) {
        self.errors.push(ParseError {
            code,
            message: msg.to_string(),
            offset: self.current_offset(),
        });
    }

    /// Recovery: wrap the current token in an `ErrorNode` and consume it, so the
    /// tree stays complete and every loop makes progress. No-op at EOF.
    fn error_bump(&mut self) {
        if self.current() == K::Eof {
            return;
        }
        self.builder.start_node(K::ErrorNode);
        self.bump();
        self.builder.finish_node();
    }

    fn wrap(&mut self, cp: rowan::Checkpoint, kind: SyntaxKind) {
        self.builder.start_node_at(cp, kind);
        self.builder.finish_node();
    }

    // --- items -------------------------------------------------------------

    fn source_file(&mut self) {
        self.builder.start_node(K::SourceFile);
        while self.current() != K::Eof {
            self.eat_trivia();
            let before = self.pos;
            self.scope_entry();
            if self.pos == before {
                self.error_bump();
            }
        }
        self.eat_trivia(); // trailing trivia
        self.builder.finish_node();
    }

    fn scope_entry(&mut self) {
        if (self.at(K::Hash) && self.nth(1) == K::LBracket)
            || self.at_contextual("annotation")
            || matches!(
                self.current(),
                K::KwPub
                    | K::KwFn
                    | K::KwStruct
                    | K::KwEnum
                    | K::KwTrait
                    | K::KwImpl
                    | K::KwExtern
                    | K::KwUse
            )
        {
            self.item();
        } else {
            self.statement();
        }
    }

    fn item(&mut self) {
        let cp = self.builder.checkpoint();
        self.outer_attributes();
        let _ = self.accept(K::KwPub);
        if self.at_contextual("annotation") {
            self.annotation_decl(cp);
            return;
        }
        match self.current() {
            K::KwFn => {
                self.fn_sig();
                self.block();
                self.wrap(cp, K::FnDecl);
            }
            K::KwStruct => self.struct_decl(cp),
            K::KwEnum => self.enum_decl(cp),
            K::KwTrait => self.trait_decl(cp),
            K::KwImpl => self.impl_decl(cp),
            K::KwExtern => self.extern_block(cp),
            K::KwUse => self.use_decl(cp),
            _ => {
                self.error_with_code(
                    DiagnosticCode::ParseExpectedItem,
                    "expected item (annotation/fn/struct/enum/impl/trait/extern/use)",
                );
                self.error_bump();
            }
        }
    }

    fn annotation_decl(&mut self, cp: rowan::Checkpoint) {
        self.bump(); // contextual `annotation`
        self.expect_ident();
        self.expect(K::LParen);
        while !self.at(K::RParen) && !self.at(K::Eof) {
            let before = self.pos;
            self.param();
            if !self.accept(K::Comma) {
                break;
            }
            if self.pos == before {
                self.error_bump();
            }
        }
        self.expect(K::RParen);
        self.expect(K::Semi);
        self.wrap(cp, K::AnnotationDecl);
    }

    fn outer_attributes(&mut self) {
        while self.at(K::Hash) && self.nth(1) == K::LBracket {
            self.builder.start_node(K::Attribute);
            self.expect(K::Hash);
            self.expect(K::LBracket);
            self.meta_item();
            self.expect(K::RBracket);
            self.builder.finish_node();
        }
    }

    fn meta_item(&mut self) {
        self.builder.start_node(K::MetaItem);
        if matches!(
            self.current(),
            K::Str | K::Int | K::Float | K::KwTrue | K::KwFalse
        ) {
            self.bump();
            self.builder.finish_node();
            return;
        }
        self.expect_meta_name();
        while self.accept(K::ColonColon) {
            self.expect_meta_name();
        }
        if self.accept(K::Eq) {
            self.meta_value();
        } else if self.accept(K::LParen) {
            while !self.at(K::RParen) && !self.at(K::Eof) {
                let before = self.pos;
                self.meta_item();
                if !self.accept(K::Comma) {
                    break;
                }
                if self.pos == before {
                    self.error_bump();
                }
            }
            self.expect(K::RParen);
        }
        self.builder.finish_node();
    }

    fn meta_value(&mut self) {
        self.builder.start_node(K::MetaValue);
        match self.current() {
            K::Str | K::Int | K::Float | K::KwTrue | K::KwFalse => self.bump(),
            K::Ident => {
                self.bump();
                while self.accept(K::ColonColon) {
                    self.expect_meta_name();
                }
            }
            K::LBracket => {
                self.bump();
                while !self.at(K::RBracket) && !self.at(K::Eof) {
                    let before = self.pos;
                    self.meta_value();
                    if !self.accept(K::Comma) {
                        break;
                    }
                    if self.pos == before {
                        self.error_bump();
                    }
                }
                self.expect(K::RBracket);
            }
            _ => {
                self.error("attribute value must be a literal, path, or homogeneous list");
                self.error_bump();
            }
        }
        self.builder.finish_node();
    }

    fn expect_meta_name(&mut self) {
        let text = self.current_text();
        let valid = text
            .bytes()
            .next()
            .is_some_and(|first| first == b'_' || first.is_ascii_alphabetic())
            && text
                .bytes()
                .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
            && !matches!(self.current(), K::KwTrue | K::KwFalse);
        if valid {
            self.bump();
        } else {
            self.error("expected attribute identifier");
        }
    }

    /// `fn name<G>(recv, params) -> Ret where ...` (no body).
    fn fn_sig(&mut self) {
        self.expect(K::KwFn);
        self.expect_ident();
        self.opt_generics();
        self.expect(K::LParen);
        // receiver: `self` / `&self` / `&mut self`
        if self.at(K::Amp) {
            self.bump();
            let _ = self.accept(K::KwMut);
            self.expect(K::KwSelf);
            let _ = self.accept(K::Comma);
        } else if self.at(K::KwSelf) {
            self.bump();
            let _ = self.accept(K::Comma);
        }
        while !self.at(K::RParen) && !self.at(K::Eof) {
            let before = self.pos;
            self.param();
            if !self.accept(K::Comma) {
                break;
            }
            if self.pos == before {
                self.error_bump();
            }
        }
        self.expect(K::RParen);
        if self.accept(K::Arrow) {
            self.ty();
        }
        self.opt_where();
    }

    fn param(&mut self) {
        self.builder.start_node(K::Param);
        self.expect_ident();
        self.expect(K::Colon);
        self.ty();
        self.builder.finish_node();
    }

    fn opt_generics(&mut self) {
        if !self.at(K::Lt) {
            return;
        }
        self.builder.start_node(K::GenericParams);
        self.bump(); // `<`
        while !self.at(K::Gt) && !self.at(K::Eof) {
            let before = self.pos;
            self.generic_param();
            if !self.accept(K::Comma) {
                break;
            }
            if self.pos == before {
                self.error_bump();
            }
        }
        self.expect(K::Gt);
        self.builder.finish_node();
    }

    fn generic_param(&mut self) {
        self.builder.start_node(K::GenericParam);
        self.expect_ident();
        if self.accept(K::Colon) {
            self.bound_list();
        }
        self.builder.finish_node();
    }

    /// `Trait + path::Trait + Iterator<Item = U>` — bounds after `:` or in `where`.
    fn bound_list(&mut self) {
        loop {
            self.expect_ident();
            while self.accept(K::ColonColon) {
                self.expect_ident();
            }
            self.skip_angles(); // tolerate assoc-type args
            if !self.accept(K::Plus) {
                break;
            }
        }
    }

    /// Consume a balanced `<...>` as raw tokens (type args on bounds / impls).
    fn skip_angles(&mut self) {
        if !self.at(K::Lt) {
            return;
        }
        let mut depth = 0i32;
        loop {
            match self.current() {
                K::Lt => depth += 1,
                K::Gt => {
                    depth -= 1;
                    if depth == 0 {
                        self.bump();
                        break;
                    }
                }
                K::Eof => {
                    self.error_with_code(
                        DiagnosticCode::ParseMissingDelimiter,
                        "unterminated `<...>`",
                    );
                    break;
                }
                _ => {}
            }
            self.bump();
        }
    }

    fn opt_where(&mut self) {
        if !self.at_contextual("where") {
            return;
        }
        self.builder.start_node(K::WhereClause);
        self.bump(); // `where`
        loop {
            if matches!(self.current(), K::LBrace | K::Semi | K::Eof) {
                break;
            }
            let before = self.pos;
            self.expect_ident();
            while self.accept(K::ColonColon) {
                self.expect_ident();
            }
            self.skip_angles();
            if self.accept(K::Colon) {
                self.bound_list();
            }
            if !self.accept(K::Comma) {
                break;
            }
            if self.pos == before {
                self.error_bump();
            }
        }
        self.builder.finish_node();
    }

    fn struct_decl(&mut self, cp: rowan::Checkpoint) {
        self.expect(K::KwStruct);
        self.expect_ident();
        self.opt_generics();
        self.opt_where();
        if !self.accept(K::Semi) {
            self.field_list();
        }
        self.wrap(cp, K::StructDecl);
    }

    fn field_list(&mut self) {
        self.builder.start_node(K::FieldList);
        self.expect(K::LBrace);
        while !self.at(K::RBrace) && !self.at(K::Eof) {
            let before = self.pos;
            self.field_decl();
            if !self.accept(K::Comma) {
                break;
            }
            if self.pos == before {
                self.error_bump();
            }
        }
        self.expect(K::RBrace);
        self.builder.finish_node();
    }

    fn field_decl(&mut self) {
        self.builder.start_node(K::FieldDecl);
        self.outer_attributes();
        let _ = self.accept(K::KwPub);
        self.expect_ident();
        self.expect(K::Colon);
        self.ty();
        self.builder.finish_node();
    }

    fn enum_decl(&mut self, cp: rowan::Checkpoint) {
        self.expect(K::KwEnum);
        self.expect_ident();
        self.opt_generics();
        self.opt_where();
        self.builder.start_node(K::VariantList);
        self.expect(K::LBrace);
        while !self.at(K::RBrace) && !self.at(K::Eof) {
            let before = self.pos;
            self.enum_variant();
            if !self.accept(K::Comma) {
                break;
            }
            if self.pos == before {
                self.error_bump();
            }
        }
        self.expect(K::RBrace);
        self.builder.finish_node();
        self.wrap(cp, K::EnumDecl);
    }

    fn enum_variant(&mut self) {
        self.builder.start_node(K::EnumVariant);
        self.outer_attributes();
        self.expect_ident();
        match self.current() {
            K::LParen => {
                self.bump();
                while !self.at(K::RParen) && !self.at(K::Eof) {
                    let before = self.pos;
                    self.ty();
                    if !self.accept(K::Comma) {
                        break;
                    }
                    if self.pos == before {
                        self.error_bump();
                    }
                }
                self.expect(K::RParen);
            }
            K::LBrace => self.field_list(),
            _ => {}
        }
        self.builder.finish_node();
    }

    fn trait_decl(&mut self, cp: rowan::Checkpoint) {
        self.expect(K::KwTrait);
        self.expect_ident();
        self.opt_generics();
        self.opt_where();
        self.expect(K::LBrace);
        while !self.at(K::RBrace) && !self.at(K::Eof) {
            let before = self.pos;
            self.trait_method();
            if self.pos == before {
                self.error_bump();
            }
        }
        self.expect(K::RBrace);
        self.wrap(cp, K::TraitDecl);
    }

    fn trait_method(&mut self) {
        self.builder.start_node(K::TraitMethod);
        self.outer_attributes();
        self.fn_sig();
        if self.at(K::LBrace) {
            self.block();
        } else {
            self.expect(K::Semi);
        }
        self.builder.finish_node();
    }

    fn impl_decl(&mut self, cp: rowan::Checkpoint) {
        self.expect(K::KwImpl);
        self.opt_generics();
        self.expect_ident();
        self.skip_angles();
        if self.accept(K::KwFor) {
            self.expect_ident();
            self.skip_angles();
        }
        self.opt_where();
        self.expect(K::LBrace);
        while !self.at(K::RBrace) && !self.at(K::Eof) {
            let before = self.pos;
            let mcp = self.builder.checkpoint();
            self.outer_attributes();
            let _ = self.accept(K::KwPub);
            self.fn_sig();
            self.block();
            self.wrap(mcp, K::FnDecl);
            if self.pos == before {
                self.error_bump();
            }
        }
        self.expect(K::RBrace);
        self.wrap(cp, K::ImplDecl);
    }

    fn extern_block(&mut self, cp: rowan::Checkpoint) {
        self.expect(K::KwExtern);
        let _ = self.accept(K::Str); // optional ABI string
        self.expect(K::LBrace);
        while !self.at(K::RBrace) && !self.at(K::Eof) {
            let before = self.pos;
            self.extern_fn();
            if self.pos == before {
                self.error_bump();
            }
        }
        self.expect(K::RBrace);
        self.wrap(cp, K::ExternBlock);
    }

    fn extern_fn(&mut self) {
        self.builder.start_node(K::ExternFn);
        self.outer_attributes();
        let _ = self.accept(K::KwPub);
        self.expect(K::KwFn);
        self.expect_ident();
        self.skip_angles();
        self.expect(K::LParen);
        while !self.at(K::RParen) && !self.at(K::Eof) {
            // `...` variadic tail: lexed as `..` then `.`
            if self.at(K::DotDot) {
                self.bump();
                let _ = self.accept(K::Dot);
                break;
            }
            let before = self.pos;
            self.param();
            if !self.accept(K::Comma) {
                break;
            }
            if self.pos == before {
                self.error_bump();
            }
        }
        self.expect(K::RParen);
        if self.accept(K::Arrow) {
            self.ty();
        }
        self.expect(K::Semi);
        self.builder.finish_node();
    }

    fn use_decl(&mut self, cp: rowan::Checkpoint) {
        self.expect(K::KwUse);
        self.expect_ident();
        loop {
            if self.accept(K::ColonColon) {
                if self.at(K::LBrace) {
                    self.bump();
                    while !self.at(K::RBrace) && !self.at(K::Eof) {
                        let before = self.pos;
                        self.expect_ident();
                        if self.accept(K::KwAs) {
                            self.expect_ident();
                        }
                        if !self.accept(K::Comma) {
                            break;
                        }
                        if self.pos == before {
                            self.error_bump();
                        }
                    }
                    self.expect(K::RBrace);
                    break;
                }
                self.expect_ident();
            } else {
                break;
            }
        }
        if self.accept(K::KwAs) {
            self.expect_ident();
        }
        self.expect(K::Semi);
        self.wrap(cp, K::UseDecl);
    }

    // --- types -------------------------------------------------------------

    fn ty(&mut self) {
        self.eat_trivia();
        if self.at(K::Not) {
            let cp = self.builder.checkpoint();
            self.bump();
            self.wrap(cp, K::NeverType);
            return;
        }
        if self.at(K::Amp) {
            let cp = self.builder.checkpoint();
            self.bump();
            let _ = self.accept(K::KwMut);
            let _ = self.accept(K::KwDyn);
            self.ty();
            self.wrap(cp, K::RefType);
            return;
        }
        if self.at(K::KwFn) {
            let cp = self.builder.checkpoint();
            self.bump();
            self.expect(K::LParen);
            while !self.at(K::RParen) && !self.at(K::Eof) {
                let before = self.pos;
                self.ty();
                if !self.accept(K::Comma) {
                    break;
                }
                if self.pos == before {
                    self.error_bump();
                }
            }
            self.expect(K::RParen);
            if self.accept(K::Arrow) {
                self.ty();
            }
            self.wrap(cp, K::CallableType);
            return;
        }
        if self.at(K::LParen) {
            let cp = self.builder.checkpoint();
            self.bump();
            while !self.at(K::RParen) && !self.at(K::Eof) {
                let before = self.pos;
                self.ty();
                if !self.accept(K::Comma) {
                    break;
                }
                if self.pos == before {
                    self.error_bump();
                }
            }
            self.expect(K::RParen);
            self.wrap(cp, K::TupleType);
            return;
        }
        let cp = self.builder.checkpoint();
        self.expect_ident();
        while self.at(K::ColonColon) {
            self.bump();
            self.expect_ident();
        }
        if self.at(K::Lt) {
            self.type_args();
        }
        self.wrap(cp, K::PathType);
    }

    fn type_args(&mut self) {
        self.builder.start_node(K::TypeArgs);
        self.bump(); // `<`
        while !self.at(K::Gt) && !self.at(K::Eof) {
            let before = self.pos;
            self.ty();
            if !self.accept(K::Comma) {
                break;
            }
            if self.pos == before {
                self.error_bump();
            }
        }
        self.expect(K::Gt);
        self.builder.finish_node();
    }

    // --- blocks & statements ----------------------------------------------

    fn block(&mut self) {
        self.builder.start_node(K::Block);
        self.expect(K::LBrace);
        let saved = self.no_struct;
        self.no_struct = false;
        while !self.at(K::RBrace) && !self.at(K::Eof) {
            self.eat_trivia();
            let before = self.pos;
            self.statement();
            if self.pos == before {
                self.error_bump();
            }
        }
        self.expect(K::RBrace);
        self.no_struct = saved;
        self.builder.finish_node();
    }

    fn statement(&mut self) {
        match self.current() {
            K::KwLet => self.let_stmt(),
            K::KwReturn => self.return_stmt(),
            K::KwWhile => self.while_stmt(),
            K::KwFor => self.for_stmt(),
            K::KwBreak => {
                self.builder.start_node(K::BreakExpr);
                self.bump();
                if !self.accept(K::Semi) {
                    self.expr();
                    self.expect(K::Semi);
                }
                self.builder.finish_node();
            }
            K::KwContinue => {
                self.builder.start_node(K::ContinueExpr);
                self.bump();
                self.expect(K::Semi);
                self.builder.finish_node();
            }
            _ => {
                let cp = self.builder.checkpoint();
                self.expr();
                let _ = self.accept(K::Semi); // optional: tail expr has none
                self.wrap(cp, K::ExprStmt);
            }
        }
    }

    fn let_stmt(&mut self) {
        self.builder.start_node(K::LetStmt);
        self.expect(K::KwLet);
        let _ = self.accept(K::KwMut);
        self.expect_ident();
        if self.accept(K::Colon) {
            self.ty();
        }
        self.expect(K::Eq);
        self.expr();
        self.expect(K::Semi);
        self.builder.finish_node();
    }

    fn return_stmt(&mut self) {
        self.builder.start_node(K::ReturnExpr);
        self.expect(K::KwReturn);
        if !self.accept(K::Semi) {
            self.expr();
            self.expect(K::Semi);
        }
        self.builder.finish_node();
    }

    fn while_stmt(&mut self) {
        self.builder.start_node(K::WhileExpr);
        self.expect(K::KwWhile);
        if self.at(K::KwLet) {
            self.bump();
            self.pattern();
            self.expect(K::Eq);
            self.cond_expr();
        } else {
            self.cond_expr();
        }
        self.block();
        self.builder.finish_node();
    }

    fn loop_stmt(&mut self) {
        self.builder.start_node(K::LoopExpr);
        self.expect(K::KwLoop);
        self.block();
        self.builder.finish_node();
    }

    fn for_stmt(&mut self) {
        self.builder.start_node(K::ForExpr);
        self.expect(K::KwFor);
        self.expect_ident();
        self.expect(K::KwIn);
        self.cond_expr();
        self.block();
        self.builder.finish_node();
    }

    // --- expressions -------------------------------------------------------

    /// Expression with struct literals suppressed (conditions, scrutinees).
    fn cond_expr(&mut self) {
        let saved = self.no_struct;
        self.no_struct = true;
        self.expr();
        self.no_struct = saved;
    }

    /// Expression with struct literals re-enabled (args, grouping, arms).
    fn allow_struct_expr(&mut self) {
        let saved = self.no_struct;
        self.no_struct = false;
        self.expr();
        self.no_struct = saved;
    }

    fn expr(&mut self) {
        self.eat_trivia();
        let cp = self.builder.checkpoint();
        self.bin(0);
        match self.current() {
            K::Eq | K::PlusEq | K::MinusEq | K::StarEq | K::SlashEq | K::PercentEq => {
                self.bump();
                self.expr();
                self.wrap(cp, K::AssignExpr);
            }
            _ => {}
        }
    }

    fn bin(&mut self, min_bp: u8) {
        self.eat_trivia();
        let cp = self.builder.checkpoint();
        self.unary();
        loop {
            if matches!(self.current(), K::DotDot | K::DotDotEq) {
                const RANGE_BP: u8 = 4;
                if RANGE_BP < min_bp {
                    break;
                }
                self.bump();
                self.bin(RANGE_BP + 1);
                self.wrap(cp, K::RangeExpr);
                continue;
            }
            let Some(lbp) = infix_bp(self.current()) else {
                break;
            };
            if lbp < min_bp {
                break;
            }
            self.bump(); // operator
            self.bin(lbp + 1);
            self.wrap(cp, K::BinExpr);
        }
    }

    fn unary(&mut self) {
        self.eat_trivia();
        if self.at(K::Minus) || self.at(K::Not) || self.at(K::Amp) {
            let cp = self.builder.checkpoint();
            self.bump();
            self.unary();
            self.wrap(cp, K::UnaryExpr);
        } else {
            self.postfix();
        }
    }

    fn postfix(&mut self) {
        self.eat_trivia();
        let cp = self.builder.checkpoint();
        self.primary();
        loop {
            match self.current() {
                K::LParen => {
                    self.arg_list();
                    self.wrap(cp, K::CallExpr);
                }
                K::Dot | K::QuestionDot => {
                    self.bump();
                    self.expect_ident();
                    if self.accept(K::ColonColon) {
                        if self.at(K::Lt) {
                            self.type_args();
                        } else {
                            self.error("expected `<` after method `::`");
                        }
                    }
                    if self.at(K::LParen) {
                        self.arg_list();
                        self.wrap(cp, K::MethodCallExpr);
                    } else {
                        self.wrap(cp, K::FieldExpr);
                    }
                }
                K::Question => {
                    self.bump();
                    self.wrap(cp, K::TryExpr);
                }
                K::LBracket => {
                    self.bump();
                    self.allow_struct_expr();
                    self.expect(K::RBracket);
                    self.wrap(cp, K::IndexExpr);
                }
                _ => break,
            }
        }
    }

    fn arg_list(&mut self) {
        self.builder.start_node(K::ArgList);
        self.expect(K::LParen);
        while !self.at(K::RParen) && !self.at(K::Eof) {
            let before = self.pos;
            self.allow_struct_expr();
            if !self.accept(K::Comma) {
                break;
            }
            if self.pos == before {
                self.error_bump();
            }
        }
        self.expect(K::RParen);
        self.builder.finish_node();
    }

    fn primary(&mut self) {
        self.eat_trivia();
        match self.current() {
            K::Pipe | K::OrOr => self.closure_expr(),
            K::Int | K::Float | K::Str | K::KwTrue | K::KwFalse => {
                let cp = self.builder.checkpoint();
                self.bump();
                self.wrap(cp, K::LiteralExpr);
            }
            K::Ident | K::KwSelf => {
                let cp = self.builder.checkpoint();
                self.expect_ident();
                while self.at(K::ColonColon) {
                    self.bump();
                    self.expect_ident();
                }
                if !self.no_struct && self.at(K::LBrace) {
                    self.struct_lit_fields();
                    self.wrap(cp, K::StructLitExpr);
                } else {
                    self.wrap(cp, K::PathExpr);
                }
            }
            K::LParen => {
                let cp = self.builder.checkpoint();
                self.bump();
                self.allow_struct_expr();
                self.expect(K::RParen);
                self.wrap(cp, K::ParenExpr);
            }
            K::LBracket => {
                let cp = self.builder.checkpoint();
                self.bump();
                while !self.at(K::RBracket) && !self.at(K::Eof) {
                    let before = self.pos;
                    self.allow_struct_expr();
                    if !self.accept(K::Comma) {
                        break;
                    }
                    if self.pos == before {
                        self.error_bump();
                    }
                }
                self.expect(K::RBracket);
                self.wrap(cp, K::ArrayExpr);
            }
            K::LBrace => self.block(),
            K::Hash => self.map_expr(),
            K::KwLoop => self.loop_stmt(),
            K::KwIf => self.if_expr(),
            K::KwMatch => self.match_expr(),
            _ => {
                self.error("expected expression");
                self.error_bump();
            }
        }
    }

    fn closure_expr(&mut self) {
        let cp = self.builder.checkpoint();
        if self.at(K::OrOr) {
            self.bump();
        } else {
            self.expect(K::Pipe);
            while !self.at(K::Pipe) && !self.at(K::Eof) {
                let before = self.pos;
                self.builder.start_node(K::Param);
                self.expect_ident();
                if self.accept(K::Colon) {
                    self.ty();
                }
                self.builder.finish_node();
                if !self.accept(K::Comma) {
                    break;
                }
                if self.pos == before {
                    self.error_bump();
                }
            }
            self.expect(K::Pipe);
        }
        if self.accept(K::Arrow) {
            self.ty();
        }
        if self.at(K::LBrace) {
            self.block();
        } else {
            self.allow_struct_expr();
        }
        self.wrap(cp, K::ClosureExpr);
    }

    fn map_expr(&mut self) {
        self.builder.start_node(K::MapExpr);
        self.expect(K::Hash);
        self.expect(K::LBrace);
        while !self.at(K::RBrace) && !self.at(K::Eof) {
            let before = self.pos;
            self.builder.start_node(K::MapEntry);
            self.allow_struct_expr();
            self.expect(K::Colon);
            self.allow_struct_expr();
            self.builder.finish_node();
            if !self.accept(K::Comma) {
                break;
            }
            if self.pos == before {
                self.error_bump();
            }
        }
        self.expect(K::RBrace);
        self.builder.finish_node();
    }

    fn struct_lit_fields(&mut self) {
        self.expect(K::LBrace);
        while !self.at(K::RBrace) && !self.at(K::Eof) {
            let before = self.pos;
            self.builder.start_node(K::FieldInit);
            self.expect_ident();
            if self.accept(K::Colon) {
                self.allow_struct_expr();
            }
            self.builder.finish_node();
            if !self.accept(K::Comma) {
                break;
            }
            if self.pos == before {
                self.error_bump();
            }
        }
        self.expect(K::RBrace);
    }

    fn if_expr(&mut self) {
        let cp = self.builder.checkpoint();
        self.expect(K::KwIf);
        if self.at(K::KwLet) {
            self.bump();
            self.pattern();
            self.expect(K::Eq);
            self.cond_expr();
        } else {
            self.cond_expr();
        }
        self.block();
        self.else_branch();
        self.wrap(cp, K::IfExpr);
    }

    fn else_branch(&mut self) {
        if self.accept(K::KwElse) {
            if self.at(K::KwIf) {
                self.if_expr();
            } else {
                self.block();
            }
        }
    }

    fn match_expr(&mut self) {
        let cp = self.builder.checkpoint();
        self.expect(K::KwMatch);
        self.cond_expr();
        self.expect(K::LBrace);
        while !self.at(K::RBrace) && !self.at(K::Eof) {
            let before = self.pos;
            self.match_arm();
            if self.pos == before {
                self.error_bump();
            }
        }
        self.expect(K::RBrace);
        self.wrap(cp, K::MatchExpr);
    }

    fn match_arm(&mut self) {
        self.builder.start_node(K::MatchArm);
        self.pattern();
        while self.accept(K::Pipe) {
            self.pattern();
        }
        if self.accept(K::KwIf) {
            self.cond_expr();
        }
        self.expect(K::FatArrow);
        self.allow_struct_expr();
        let _ = self.accept(K::Comma);
        self.builder.finish_node();
    }

    // --- patterns ----------------------------------------------------------

    fn pattern(&mut self) {
        self.eat_trivia();
        self.builder.start_node(K::Pattern);
        self.pattern_inner();
        self.builder.finish_node();
    }

    fn pattern_inner(&mut self) {
        if matches!(
            self.current(),
            K::Int | K::Float | K::Str | K::KwTrue | K::KwFalse | K::Minus
        ) {
            self.pattern_literal();
            if self.accept(K::DotDotEq) || self.accept(K::DotDot) {
                self.pattern_literal();
            }
            return;
        }
        if self.at(K::Ident) || self.at(K::KwSelf) {
            if self.current_text() == "_" {
                self.bump();
                return;
            }
            self.expect_ident();
            while self.at(K::ColonColon) {
                self.bump();
                self.expect_ident();
            }
            match self.current() {
                K::LParen => {
                    self.bump();
                    while !self.at(K::RParen) && !self.at(K::Eof) {
                        let before = self.pos;
                        self.pattern();
                        if !self.accept(K::Comma) {
                            break;
                        }
                        if self.pos == before {
                            self.error_bump();
                        }
                    }
                    self.expect(K::RParen);
                }
                K::LBrace => {
                    self.bump();
                    while !self.at(K::RBrace) && !self.at(K::Eof) {
                        if self.accept(K::DotDot) {
                            break;
                        }
                        let before = self.pos;
                        self.expect_ident();
                        if self.accept(K::Colon) {
                            self.pattern();
                        }
                        if !self.accept(K::Comma) {
                            break;
                        }
                        if self.pos == before {
                            self.error_bump();
                        }
                    }
                    self.expect(K::RBrace);
                }
                _ => {}
            }
        } else {
            self.error("expected pattern");
            self.error_bump();
        }
    }

    fn pattern_literal(&mut self) {
        if self.accept(K::Minus) {
            self.pattern_literal();
            return;
        }
        match self.current() {
            K::Int | K::Float | K::Str | K::KwTrue | K::KwFalse => self.bump(),
            _ => {
                self.error("expected literal in pattern");
                self.error_bump();
            }
        }
    }
}

/// Left binding power of an infix operator token, identical to the semantic
/// parser's `binop` precedence table (`None` = not an infix operator).
fn infix_bp(t: SyntaxKind) -> Option<u8> {
    Some(match t {
        K::QuestionQuestion => 0,
        K::OrOr => 1,
        K::AndAnd => 2,
        K::EqEq | K::Ne | K::Lt | K::Le | K::Gt | K::Ge | K::KwIn => 3,
        K::Plus | K::Minus => 5,
        K::Star | K::Slash | K::Percent => 6,
        _ => return None,
    })
}

/// Human-readable token spelling for diagnostics.
fn user_str(k: SyntaxKind) -> &'static str {
    match k {
        K::KwFn => "fn",
        K::KwLet => "let",
        K::KwSelf => "self",
        K::Ident => "identifier",
        K::LParen => "(",
        K::RParen => ")",
        K::LBrace => "{",
        K::RBrace => "}",
        K::LBracket => "[",
        K::RBracket => "]",
        K::Lt => "<",
        K::Gt => ">",
        K::Colon => ":",
        K::Semi => ";",
        K::Comma => ",",
        K::Eq => "=",
        K::PlusEq => "+=",
        K::MinusEq => "-=",
        K::StarEq => "*=",
        K::SlashEq => "/=",
        K::PercentEq => "%=",
        K::QuestionQuestion => "??",
        K::QuestionDot => "?.",
        K::Arrow => "->",
        K::FatArrow => "=>",
        K::Not => "!",
        _ => "token",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(src: &str) -> String {
        let parsed = parse_source_file(src);
        let mut out = String::new();
        fmt_node(parsed.syntax_node(), 0, &mut out);
        out
    }

    fn fmt_node(node: &SyntaxNode, depth: usize, out: &mut String) {
        use std::fmt::Write;
        let _ = writeln!(out, "{}{:?}", "  ".repeat(depth), node.kind());
        for child in node.children() {
            fmt_node(&child, depth + 1, out);
        }
    }

    #[test]
    fn simple_fn_structure() {
        let src = "fn add(a: i64, b: i64) -> i64 { a + b }";
        let parsed = parse_source_file(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let t = tree(src);
        assert!(t.contains("SourceFile"));
        assert!(t.contains("FnDecl"));
        assert!(t.contains("Param"));
        assert!(t.contains("Block"));
        assert!(t.contains("BinExpr"));
    }

    #[test]
    fn struct_and_impl_structure() {
        let src = "struct P { x: i64 }\nimpl P {\n  fn get(&self) -> i64 { self.x }\n}\n";
        let parsed = parse_source_file(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let t = tree(src);
        assert!(t.contains("StructDecl"));
        assert!(t.contains("FieldList"));
        assert!(t.contains("ImplDecl"));
        assert!(t.contains("FieldExpr"));
    }

    #[test]
    fn comments_preserved_inside_fn() {
        let src = "fn f() {\n    // a line comment\n    let x = 1; /* inline */\n}\n";
        let parsed = parse_source_file(src);
        assert_eq!(parsed.syntax_node().text().to_string(), src);
        assert!(parsed.errors.is_empty());
    }

    #[test]
    fn precedence_left_assoc_and_nesting() {
        // `1 + 2 * 3` => Add(1, Mul(2,3)); left assoc `a - b - c` => Sub(Sub,c).
        let t = tree("fn f() -> i64 { 1 + 2 * 3 }");
        // Outer BinExpr contains a nested BinExpr (the `2 * 3`).
        let binexprs = t.matches("BinExpr").count();
        assert_eq!(binexprs, 2, "one add + one mul\n{t}");
    }

    #[test]
    fn call_and_method_chain() {
        let parsed = parse_source_file("fn f() { a.b().c(1, 2)?; }");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let t = tree("fn f() { a.b().c(1, 2)?; }");
        assert!(t.contains("MethodCallExpr"));
        assert!(t.contains("TryExpr"));
    }

    #[test]
    fn match_and_patterns() {
        let src = "fn f(x: i64) -> i64 {\n  match x {\n    0 => 1,\n    1..=9 => 2,\n    _ => 3,\n  }\n}\n";
        let parsed = parse_source_file(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        assert_eq!(parsed.syntax_node().text().to_string(), src);
        let t = tree(src);
        assert!(t.contains("MatchExpr"));
        assert!(t.contains("MatchArm"));
        assert!(t.contains("Pattern"));
    }

    #[test]
    fn struct_literal_vs_block_in_condition() {
        // `if cond { }` — the `{` is a block, not a struct literal.
        let parsed = parse_source_file("fn f() { if x { 1 } else { 2 } }");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        // In an argument position a struct literal is allowed again.
        let parsed2 = parse_source_file("fn f() { g(P { x: 1 }); }");
        assert!(parsed2.errors.is_empty(), "{:?}", parsed2.errors);
        assert!(tree("fn f() { g(P { x: 1 }); }").contains("StructLitExpr"));
    }

    #[test]
    fn error_recovery_is_lossless_and_nonfatal() {
        let src = "fn f( { @@@ }";
        let parsed = parse_source_file(src);
        // Malformed, but the tree still covers every byte and does not panic.
        assert_eq!(parsed.syntax_node().text().to_string(), src);
        assert!(!parsed.errors.is_empty());
        assert!(parsed.clone().ok().is_err());
    }

    #[test]
    fn public_parse_api_returns_typed_source_file() {
        let parsed: Parse<SourceFile> = parse_source_file("fn main() {}");
        assert!(parsed.errors().is_empty());
        assert_eq!(parsed.syntax_node().kind(), K::SourceFile);
        assert_eq!(parsed.tree().items().count(), 1);
        assert!(parsed.ok().is_ok());
    }

    #[test]
    fn attributes_are_lossless_structured_children() {
        let source = "#[cfg_attr(feature = \"server\", cfg(enabled))]\nfn serve() {}";
        let parsed = parse_source_file(source);
        assert!(parsed.errors().is_empty(), "{:?}", parsed.errors());
        assert_eq!(parsed.syntax_node().text().to_string(), source);
        let item = parsed.tree().items().next().unwrap();
        let attribute = item.attributes().next().unwrap().to_core().unwrap();
        assert_eq!(attribute.name, "cfg_attr");
        assert_eq!(attribute.items.len(), 2);
    }

    #[test]
    fn annotation_declaration_is_a_contextual_item() {
        let source =
            "#[targets(function, method)]\npub annotation Route(method: String, path: String);";
        let parsed = parse_source_file(source);
        assert!(parsed.errors().is_empty(), "{:?}", parsed.errors());
        assert_eq!(parsed.syntax_node().text().to_string(), source);
        let item = parsed.tree().items().next().unwrap();
        let crate::ast::Item::Annotation(annotation) = item else {
            panic!("expected annotation declaration");
        };
        assert_eq!(
            crate::ast::Named::name_text(&annotation).as_deref(),
            Some("Route")
        );
        assert_eq!(annotation.params().count(), 2);
    }
}
