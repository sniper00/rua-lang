//! Recursive-descent parser with precedence climbing for expressions.
//!
//! Structure follows lua-rs (`compiler/statement.rs` + `compiler/expr_parser.rs`):
//! item/statement parsing calls into `parse_expr`, which uses a `subexpr`-style
//! precedence loop (`parse_bin`). Adapted to the Rust-subset grammar.

use crate::ast::*;
use crate::lexer::RuaLexer;
use crate::token::{RuaTokenKind as T, SourceRange};
use crate::tokenize::StrictTokenStream;
use crate::tokenize::TokenizeError;
use rua_core::{DiagnosticCode, FileId, StructuredDiagnostic, TextRange};
use rua_lex::LexErrorKind;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    diagnostic: StructuredDiagnostic,
    line: usize,
    message: String,
}

impl ParseError {
    fn at(range: SourceRange, message: impl Into<String>) -> Self {
        Self::at_code(range, DiagnosticCode::ParseUnexpectedToken, message)
    }

    fn at_code(range: SourceRange, code: DiagnosticCode, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            diagnostic: StructuredDiagnostic::new(
                code,
                Some(FileId::new(range.file)),
                Some(TextRange::at(range.start as u32, range.len as u32)),
            )
            .with_argument("message", &message),
            line: range.line,
            message,
        }
    }

    pub fn diagnostic(&self) -> &StructuredDiagnostic {
        &self.diagnostic
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub const fn line(&self) -> usize {
        self.line
    }
}

impl From<TokenizeError> for ParseError {
    fn from(error: TokenizeError) -> Self {
        let code = match error.kind {
            LexErrorKind::UnterminatedString => DiagnosticCode::ParseUnterminatedString,
            LexErrorKind::UnterminatedBlockComment => DiagnosticCode::ParseUnterminatedComment,
            LexErrorKind::UnknownCharacter => DiagnosticCode::ParseUnexpectedToken,
        };
        Self::at_code(error.range, code, error.kind.message())
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            formatter.write_str(&self.message)
        } else {
            write!(formatter, "{}: {}", self.line, self.message)
        }
    }
}

impl std::error::Error for ParseError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseBudget {
    pub max_tokens: usize,
    pub max_nesting: usize,
}

impl Default for ParseBudget {
    fn default() -> Self {
        Self {
            max_tokens: 1_000_000,
            max_nesting: 512,
        }
    }
}

/// A parsed function signature including receiver presence and mutability.
type FnSig = (
    String,
    SourceRange,
    Vec<GenericParam>,
    bool,
    bool,
    Vec<Param>,
    Option<Type>,
);

pub fn parse(src: &str) -> Result<Program, ParseError> {
    parse_with_budget(src, ParseBudget::default())
}

pub fn parse_with_budget(src: &str, budget: ParseBudget) -> Result<Program, ParseError> {
    parse_with_semantic_file_and_budget(src, 0, budget)
}

pub(crate) fn parse_with_semantic_file(
    src: &str,
    semantic_file: u32,
) -> Result<Program, ParseError> {
    parse_with_semantic_file_and_budget(src, semantic_file, ParseBudget::default())
}

pub(crate) fn parse_with_semantic_file_and_budget(
    src: &str,
    semantic_file: u32,
    budget: ParseBudget,
) -> Result<Program, ParseError> {
    let stream = StrictTokenStream::new(src, budget.max_tokens).map_err(|error| {
        let start = error.range.start() as usize;
        let line = 1 + src[..start].bytes().filter(|byte| *byte == b'\n').count();
        let mut range = SourceRange::new(start, error.range.len() as usize, line);
        range.file = semantic_file;
        ParseError::at_code(
            range,
            DiagnosticCode::ParseResourceLimit,
            format!("token budget exceeded (limit {})", budget.max_tokens),
        )
    })?;
    parse_token_stream_with_file(stream, semantic_file, budget)
}

pub fn parse_token_stream(
    stream: StrictTokenStream<'_>,
    budget: ParseBudget,
) -> Result<Program, ParseError> {
    parse_token_stream_with_file(stream, 0, budget)
}

fn parse_token_stream_with_file(
    stream: StrictTokenStream<'_>,
    semantic_file: u32,
    budget: ParseBudget,
) -> Result<Program, ParseError> {
    let lexer = RuaLexer::from_stream(stream).map_err(|error| {
        let mut parsed = ParseError::from(error);
        parsed.diagnostic.file = Some(FileId::new(semantic_file));
        parsed
    })?;
    let mut p = Parser {
        lexer,
        semantic_file,
        budget,
        nesting: 0,
        no_struct: false,
        next_expr: 0,
        next_pattern: 0,
        next_type: 0,
        next_trait_ref: 0,
        next_generic: 0,
    };
    p.parse_program()
}

struct Parser<'a> {
    lexer: RuaLexer<'a>,
    semantic_file: u32,
    budget: ParseBudget,
    nesting: usize,
    /// When true, an `Ident {` is NOT parsed as a struct literal (used while
    /// parsing `if`/`while` conditions and `match` scrutinees, where `{` starts
    /// a block). Reset to false inside delimited sub-expressions.
    no_struct: bool,
    next_expr: u32,
    next_pattern: u32,
    next_type: u32,
    next_trait_ref: u32,
    next_generic: u32,
}

impl<'a> Parser<'a> {
    // --- token helpers -----------------------------------------------------

    fn cur(&self) -> T {
        self.lexer.current()
    }

    fn text(&self) -> &'a str {
        self.lexer.current_text()
    }

    fn bump(&mut self) -> Result<(), ParseError> {
        self.lexer.bump().map_err(ParseError::from)
    }

    fn accept(&mut self, kind: T) -> Result<bool, ParseError> {
        if self.cur() == kind {
            self.bump()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn expect(&mut self, kind: T) -> Result<(), ParseError> {
        if self.cur() == kind {
            self.bump()
        } else {
            Err(self.err(&format!(
                "expected `{}`, found `{}`",
                kind.user_string(),
                self.cur().user_string()
            )))
        }
    }

    fn expect_ident(&mut self) -> Result<String, ParseError> {
        if self.cur() == T::Ident || self.cur() == T::KwSelf {
            let s = self.text().to_string();
            self.bump()?;
            Ok(s)
        } else {
            Err(self.err(&format!(
                "expected identifier, found `{}`",
                self.cur().user_string()
            )))
        }
    }

    fn err(&self, msg: &str) -> ParseError {
        let mut range = self.lexer.current_range();
        range.file = self.semantic_file;
        ParseError::at(range, msg)
    }

    fn with_nesting<T>(
        &mut self,
        parse: impl FnOnce(&mut Self) -> Result<T, ParseError>,
    ) -> Result<T, ParseError> {
        if self.nesting >= self.budget.max_nesting {
            let mut range = self.lexer.current_range();
            range.file = self.semantic_file;
            return Err(ParseError::at_code(
                range,
                DiagnosticCode::ParseResourceLimit,
                format!(
                    "parser nesting budget exceeded (limit {})",
                    self.budget.max_nesting
                ),
            ));
        }
        self.nesting += 1;
        let result = parse(self);
        self.nesting -= 1;
        result
    }

    fn leading_documentation(&self) -> Option<String> {
        render_leading_documentation(self.lexer.current_leading_trivia(), false)
    }

    fn blank_line_before_current(&self) -> bool {
        leading_trivia_has_blank_line(self.lexer.current_leading_trivia())
    }

    /// Wrap an expression kind with the span running from `start` (captured
    /// before parsing began) through the last consumed token.
    fn mk(&mut self, kind: ExprKind, start: SourceRange) -> Expr {
        let end = self.lexer.previous_range().end();
        let len = end.saturating_sub(start.start);
        let id = ExprId {
            file: self.semantic_file,
            local: self.next_expr,
        };
        self.next_expr = self
            .next_expr
            .checked_add(1)
            .expect("expression ID overflow");
        Expr::new(id, kind, SourceRange::new(start.start, len, start.line))
    }

    fn next_pattern_id(&mut self) -> PatternId {
        let id = PatternId {
            file: self.semantic_file,
            local: self.next_pattern,
        };
        self.next_pattern = self
            .next_pattern
            .checked_add(1)
            .expect("pattern ID overflow");
        id
    }

    fn next_type_id(&mut self) -> TypeId {
        let id = TypeId {
            file: self.semantic_file,
            local: self.next_type,
        };
        self.next_type = self.next_type.checked_add(1).expect("type ID overflow");
        id
    }

    fn trait_ref(&mut self, path: String) -> TraitRef {
        let id = TraitRefId {
            file: self.semantic_file,
            local: self.next_trait_ref,
        };
        self.next_trait_ref = self
            .next_trait_ref
            .checked_add(1)
            .expect("trait reference ID overflow");
        TraitRef { id, path }
    }

    fn next_generic_id(&mut self) -> GenericParamId {
        let id = GenericParamId {
            file: self.semantic_file,
            local: self.next_generic,
        };
        self.next_generic = self
            .next_generic
            .checked_add(1)
            .expect("generic parameter ID overflow");
        id
    }

    /// Skip an optional generic parameter/argument list `<...>` (angle-balanced).
    fn skip_generics(&mut self) -> Result<(), ParseError> {
        if self.cur() != T::Lt {
            return Ok(());
        }
        let mut depth = 0i32;
        loop {
            match self.cur() {
                T::Lt => depth += 1,
                T::Gt => {
                    depth -= 1;
                    if depth == 0 {
                        self.bump()?;
                        break;
                    }
                }
                T::Eof => return Err(self.err("unterminated `<...>`")),
                _ => {}
            }
            self.bump()?;
        }
        Ok(())
    }

    /// Parse a generic parameter list `<T, U: Add + Clone, ...>` at a declaration
    /// site, capturing each parameter's name and trait bounds. Returns an empty
    /// vec when there is no `<...>`. Associated-type args inside a bound (e.g.
    /// `Iterator<Item = U>`) and lifetimes are tolerated but not recorded.
    fn parse_generics(&mut self) -> Result<Vec<GenericParam>, ParseError> {
        if self.cur() != T::Lt {
            return Ok(Vec::new());
        }
        self.bump()?; // `<`
        let mut params = Vec::new();
        while self.cur() != T::Gt {
            let name = self.expect_ident()?;
            let mut bounds = Vec::new();
            if self.accept(T::Colon)? {
                loop {
                    // Preserve the full source path; semantic resolution assigns
                    // its trait identity in the declaration's module.
                    let mut path = self.expect_ident()?;
                    while self.accept(T::ColonColon)? {
                        path.push_str("::");
                        path.push_str(&self.expect_ident()?);
                    }
                    // Tolerate associated-type args, e.g. `Iterator<Item = U>`.
                    self.skip_generics()?;
                    bounds.push(self.trait_ref(path));
                    if !self.accept(T::Plus)? {
                        break;
                    }
                }
            }
            params.push(GenericParam {
                id: self.next_generic_id(),
                name,
                bounds,
            });
            if !self.accept(T::Comma)? {
                break;
            }
        }
        self.expect(T::Gt)?;
        Ok(params)
    }

    /// Parse an optional `where` clause and merge its predicates into the already
    /// parsed generic parameters. `where` is not a keyword (lexed as an
    /// identifier). Predicates whose left-hand side is not a bare generic name
    /// (e.g. associated types `T::Item: Bound` or `Vec<T>: Bound`) are tolerated
    /// but their bounds are dropped, keeping this conservative.
    fn parse_where(&mut self, generics: &mut [GenericParam]) -> Result<(), ParseError> {
        if !(self.cur() == T::Ident && self.text() == "where") {
            return Ok(());
        }
        self.bump()?; // `where`
        loop {
            if matches!(self.cur(), T::LBrace | T::Semi | T::Eof) {
                break;
            }
            // Left-hand side: a bare generic name is `simple`; anything richer
            // (`T::Item`, `Vec<T>`) is consumed but not merged.
            let name = self.expect_ident()?;
            let mut simple = true;
            while self.accept(T::ColonColon)? {
                let _ = self.expect_ident()?;
                simple = false;
            }
            if self.cur() == T::Lt {
                self.skip_generics()?;
                simple = false;
            }
            let mut bounds = Vec::new();
            if self.accept(T::Colon)? {
                loop {
                    let mut path = self.expect_ident()?;
                    while self.accept(T::ColonColon)? {
                        path.push_str("::");
                        path.push_str(&self.expect_ident()?);
                    }
                    self.skip_generics()?; // tolerate `Iterator<Item = U>`
                    bounds.push(self.trait_ref(path));
                    if !self.accept(T::Plus)? {
                        break;
                    }
                }
            }
            if simple && let Some(g) = generics.iter_mut().find(|g| g.name == name) {
                g.bounds.extend(bounds);
            }
            if !self.accept(T::Comma)? {
                break;
            }
        }
        Ok(())
    }

    // --- items -------------------------------------------------------------

    fn parse_program(&mut self) -> Result<Program, ParseError> {
        let (items, chunk, source_order) = self.parse_chunk_contents(T::Eof)?;
        Ok(Program {
            items,
            chunk,
            source_order,
            is_decl: false,
            standard_library: None,
        })
    }

    fn parse_chunk_contents(
        &mut self,
        terminator: T,
    ) -> Result<(Vec<Item>, Block, Vec<ChunkEntry>), ParseError> {
        let mut items = Vec::new();
        let mut statements = Vec::new();
        let mut statement_blank_before = Vec::new();
        let mut source_order = Vec::new();
        while self.cur() != terminator && self.cur() != T::Eof {
            if self.at_item_start() {
                source_order.push(ChunkEntry::Item(items.len()));
                items.push(self.parse_item()?);
            } else {
                statement_blank_before.push(self.blank_line_before_current());
                source_order.push(ChunkEntry::Statement(statements.len()));
                statements.push(self.parse_chunk_stmt(terminator)?);
            }
        }
        Ok((
            items,
            Block {
                stmts: statements,
                statement_blank_before,
                tail: None,
                tail_blank_before: false,
            },
            source_order,
        ))
    }

    fn at_item_start(&self) -> bool {
        let is_item = matches!(
            self.cur(),
            T::KwPub
                | T::KwFn
                | T::KwStruct
                | T::KwEnum
                | T::KwImpl
                | T::KwTrait
                | T::KwExtern
                | T::KwUse
        );
        #[cfg(test)]
        {
            is_item || self.cur() == T::KwMod
        }
        #[cfg(not(test))]
        {
            is_item
        }
    }

    fn parse_chunk_stmt(&mut self, terminator: T) -> Result<Stmt, ParseError> {
        match self.cur() {
            T::KwLet => self.parse_let(),
            T::KwReturn => self.parse_return(),
            T::KwWhile => self.parse_while(),
            T::KwFor => self.parse_for(),
            T::KwBreak => {
                self.bump()?;
                let value = if self.accept(T::Semi)? {
                    None
                } else {
                    let value = self.parse_expr()?;
                    self.expect(T::Semi)?;
                    Some(value)
                };
                Ok(Stmt::Break(value))
            }
            T::KwContinue => {
                self.bump()?;
                self.expect(T::Semi)?;
                Ok(Stmt::Continue)
            }
            _ => {
                let expression = self.parse_expr()?;
                if self.accept(T::Semi)? || is_block_like(&expression) {
                    Ok(Stmt::Expr(expression))
                } else {
                    Err(self.err(&format!(
                        "expected `;` or `{}`, found `{}`",
                        terminator.user_string(),
                        self.cur().user_string()
                    )))
                }
            }
        }
    }

    fn parse_item(&mut self) -> Result<Item, ParseError> {
        let documentation = self.leading_documentation();
        let is_pub = self.accept(T::KwPub)?;
        match self.cur() {
            T::KwFn => {
                let mut f = self.parse_fn()?;
                f.is_pub = is_pub;
                f.documentation = documentation;
                Ok(Item::Fn(f))
            }
            T::KwStruct => {
                let mut s = self.parse_struct()?;
                s.is_pub = is_pub;
                s.documentation = documentation;
                Ok(Item::Struct(s))
            }
            T::KwEnum => {
                let mut e = self.parse_enum()?;
                e.is_pub = is_pub;
                e.documentation = documentation;
                Ok(Item::Enum(e))
            }
            T::KwImpl => Ok(Item::Impl(self.parse_impl()?)),
            T::KwTrait => {
                let mut t = self.parse_trait()?;
                t.is_pub = is_pub;
                t.documentation = documentation;
                Ok(Item::Trait(t))
            }
            T::KwExtern => {
                let mut block = self.parse_extern()?;
                block.documentation = documentation;
                Ok(Item::Extern(block))
            }
            #[cfg(test)]
            T::KwMod => {
                let mut module = self.parse_test_module(is_pub)?;
                module.documentation = module.documentation.or(documentation);
                Ok(Item::Mod(module))
            }
            T::KwUse => Ok(Item::Use(self.parse_use()?)),
            other => Err(self.err(&format!(
                "expected item (`fn`/`struct`/`enum`/`impl`/`trait`/`extern`/`use`), found `{}`",
                other.user_string()
            ))),
        }
    }

    /// Unit tests use compact inline fixtures to exercise the compiler-internal
    /// module IR. Production parsers never compile this source syntax.
    #[cfg(test)]
    fn parse_test_module(&mut self, is_pub: bool) -> Result<ModDecl, ParseError> {
        self.expect(T::KwMod)?;
        let name = self.expect_ident()?;
        if self.accept(T::Semi)? {
            return Ok(ModDecl {
                name,
                documentation: None,
                items: Vec::new(),
                chunk: Block {
                    stmts: Vec::new(),
                    statement_blank_before: Vec::new(),
                    tail: None,
                    tail_blank_before: false,
                },
                source_order: Vec::new(),
                is_pub,
                is_file: true,
                is_decl: false,
            });
        }
        self.expect(T::LBrace)?;
        let documentation = render_leading_documentation(self.lexer.current_leading_trivia(), true);
        let (items, chunk, source_order) = self.parse_chunk_contents(T::RBrace)?;
        self.expect(T::RBrace)?;
        Ok(ModDecl {
            name,
            documentation,
            items,
            chunk,
            source_order,
            is_pub,
            is_file: false,
            is_decl: false,
        })
    }

    /// `use a::b::c;` / `use a::b as c;` / `use a::b::{c, d as e};`.
    fn parse_use(&mut self) -> Result<UseDecl, ParseError> {
        self.expect(T::KwUse)?;
        let mut prefix = vec![self.expect_ident()?];
        loop {
            if self.accept(T::ColonColon)? {
                if self.cur() == T::LBrace {
                    // Grouped import: `prefix::{ a, b as c }`.
                    self.expect(T::LBrace)?;
                    let mut imports = Vec::new();
                    while self.cur() != T::RBrace {
                        let leaf = self.expect_ident()?;
                        let alias = if self.accept(T::KwAs)? {
                            Some(self.expect_ident()?)
                        } else {
                            None
                        };
                        let mut path = prefix.clone();
                        path.push(leaf);
                        imports.push(UseImport { path, alias });
                        if !self.accept(T::Comma)? {
                            break;
                        }
                    }
                    self.expect(T::RBrace)?;
                    self.expect(T::Semi)?;
                    return Ok(UseDecl { imports });
                }
                prefix.push(self.expect_ident()?);
            } else {
                break;
            }
        }
        let alias = if self.accept(T::KwAs)? {
            Some(self.expect_ident()?)
        } else {
            None
        };
        self.expect(T::Semi)?;
        Ok(UseDecl {
            imports: vec![UseImport {
                path: prefix,
                alias,
            }],
        })
    }

    /// Parse `fn name(...) -> R` up to (but not including) the body/`;`.
    fn parse_fn_sig(&mut self) -> Result<FnSig, ParseError> {
        self.expect(T::KwFn)?;
        let name_span = self.lexer.current_range();
        let name = self.expect_ident()?;
        let mut generics = self.parse_generics()?;
        self.expect(T::LParen)?;

        // Optional `self` / `&self` / `&mut self` receiver.
        let mut has_self = false;
        let mut receiver_mutable = false;
        if self.cur() == T::Amp {
            // `&self` or `&mut self` (a `&Type` param would need a name first).
            self.bump()?;
            receiver_mutable = self.accept(T::KwMut)?;
            self.expect(T::KwSelf)?;
            has_self = true;
            let _ = self.accept(T::Comma)?;
        } else if self.cur() == T::KwSelf {
            self.bump()?;
            has_self = true;
            let _ = self.accept(T::Comma)?;
        }

        let mut params = Vec::new();
        while self.cur() != T::RParen {
            let pspan = self.lexer.current_range();
            let pname = self.expect_ident()?;
            self.expect(T::Colon)?;
            let ty = self.parse_type()?;
            params.push(Param {
                name: pname,
                name_span: pspan,
                ty,
            });
            if !self.accept(T::Comma)? {
                break;
            }
        }
        self.expect(T::RParen)?;
        let ret = if self.accept(T::Arrow)? {
            Some(self.parse_type()?)
        } else {
            None
        };
        self.parse_where(&mut generics)?;
        Ok((
            name,
            name_span,
            generics,
            has_self,
            receiver_mutable,
            params,
            ret,
        ))
    }

    fn parse_fn(&mut self) -> Result<FnDecl, ParseError> {
        let (name, name_span, generics, has_self, receiver_mutable, params, ret) =
            self.parse_fn_sig()?;
        let body = self.parse_block()?;
        Ok(FnDecl {
            name,
            documentation: None,
            name_span,
            generics,
            is_pub: false,
            has_self,
            receiver_mutable,
            params,
            ret,
            body,
        })
    }

    /// `extern "lua" { fn name(params) -> R; ... }`.
    fn parse_extern(&mut self) -> Result<ExternBlock, ParseError> {
        self.expect(T::KwExtern)?;
        let abi = if self.cur() == T::Str {
            let s = self.text().to_string();
            self.bump()?;
            s.trim_matches('"').to_string()
        } else {
            "C".to_string()
        };
        self.expect(T::LBrace)?;
        let mut fns = Vec::new();
        while self.cur() != T::RBrace && self.cur() != T::Eof {
            let documentation = self.leading_documentation();
            let _ = self.accept(T::KwPub)?;
            self.expect(T::KwFn)?;
            let name_span = self.lexer.current_range();
            let name = self.expect_ident()?;
            self.skip_generics()?;
            self.expect(T::LParen)?;
            let mut params = Vec::new();
            let mut variadic = false;
            while self.cur() != T::RParen {
                // `...` (tokenized as `..` then `.`) marks a variadic tail.
                if self.cur() == T::DotDot {
                    self.bump()?;
                    let _ = self.accept(T::Dot)?;
                    variadic = true;
                    break;
                }
                let pspan = self.lexer.current_range();
                let pname = self.expect_ident()?;
                self.expect(T::Colon)?;
                let ty = self.parse_type()?;
                params.push(Param {
                    name: pname,
                    name_span: pspan,
                    ty,
                });
                if !self.accept(T::Comma)? {
                    break;
                }
            }
            self.expect(T::RParen)?;
            let ret = if self.accept(T::Arrow)? {
                Some(self.parse_type()?)
            } else {
                None
            };
            self.expect(T::Semi)?;
            fns.push(ExternFn {
                name,
                name_span,
                documentation,
                params,
                ret,
                variadic,
            });
        }
        self.expect(T::RBrace)?;
        Ok(ExternBlock {
            abi,
            documentation: None,
            fns,
        })
    }

    fn parse_trait(&mut self) -> Result<TraitDecl, ParseError> {
        self.expect(T::KwTrait)?;
        let name = self.expect_ident()?;
        let mut generics = self.parse_generics()?;
        self.parse_where(&mut generics)?;
        self.expect(T::LBrace)?;
        let mut methods = Vec::new();
        while self.cur() != T::RBrace && self.cur() != T::Eof {
            let documentation = self.leading_documentation();
            let (mname, name_span, mgen, has_self, receiver_mutable, params, ret) =
                self.parse_fn_sig()?;
            let default = if self.cur() == T::LBrace {
                Some(self.parse_block()?)
            } else {
                self.expect(T::Semi)?;
                None
            };
            methods.push(TraitMethod {
                name: mname,
                documentation,
                name_span,
                generics: mgen,
                has_self,
                receiver_mutable,
                params,
                ret,
                default,
            });
        }
        self.expect(T::RBrace)?;
        Ok(TraitDecl {
            name,
            documentation: None,
            generics,
            methods,
            is_pub: false,
        })
    }

    fn parse_struct(&mut self) -> Result<StructDecl, ParseError> {
        self.expect(T::KwStruct)?;
        let name = self.expect_ident()?;
        let mut generics = self.parse_generics()?;
        self.parse_where(&mut generics)?;
        let fields = if self.accept(T::Semi)? {
            Vec::new() // unit struct `struct Foo;`
        } else {
            self.parse_field_list()?
        };
        Ok(StructDecl {
            name,
            documentation: None,
            generics,
            fields,
            is_pub: false,
        })
    }

    fn parse_field_list(&mut self) -> Result<Vec<Field>, ParseError> {
        self.expect(T::LBrace)?;
        let mut fields = Vec::new();
        while self.cur() != T::RBrace {
            let documentation = self.leading_documentation();
            let _ = self.accept(T::KwPub)?;
            let name_span = self.lexer.current_range();
            let fname = self.expect_ident()?;
            self.expect(T::Colon)?;
            let ty = self.parse_type()?;
            fields.push(Field {
                name: fname,
                documentation,
                ty,
                name_span,
            });
            if !self.accept(T::Comma)? {
                break;
            }
        }
        self.expect(T::RBrace)?;
        Ok(fields)
    }

    fn parse_enum(&mut self) -> Result<EnumDecl, ParseError> {
        self.expect(T::KwEnum)?;
        let name = self.expect_ident()?;
        let mut generics = self.parse_generics()?;
        self.parse_where(&mut generics)?;
        self.expect(T::LBrace)?;
        let mut variants = Vec::new();
        while self.cur() != T::RBrace {
            let documentation = self.leading_documentation();
            let vname = self.expect_ident()?;
            let kind = match self.cur() {
                T::LParen => {
                    self.bump()?;
                    let mut tys = Vec::new();
                    while self.cur() != T::RParen {
                        tys.push(self.parse_type()?);
                        if !self.accept(T::Comma)? {
                            break;
                        }
                    }
                    self.expect(T::RParen)?;
                    VariantKind::Tuple(tys)
                }
                T::LBrace => VariantKind::Struct(self.parse_field_list()?),
                _ => VariantKind::Unit,
            };
            variants.push(Variant {
                name: vname,
                documentation,
                kind,
            });
            if !self.accept(T::Comma)? {
                break;
            }
        }
        self.expect(T::RBrace)?;
        Ok(EnumDecl {
            name,
            documentation: None,
            generics,
            variants,
            is_pub: false,
        })
    }

    fn parse_impl(&mut self) -> Result<ImplDecl, ParseError> {
        self.expect(T::KwImpl)?;
        let mut generics = self.parse_generics()?;
        let first = self.expect_ident()?;
        self.skip_generics()?; // type args on the trait / type, e.g. `Foo<T>`
        let (trait_name, type_name) = if self.accept(T::KwFor)? {
            let ty = self.expect_ident()?;
            self.skip_generics()?;
            (Some(first), ty)
        } else {
            (None, first)
        };
        self.parse_where(&mut generics)?;
        self.expect(T::LBrace)?;
        let mut methods = Vec::new();
        while self.cur() != T::RBrace && self.cur() != T::Eof {
            let documentation = self.leading_documentation();
            let is_pub = self.accept(T::KwPub)?;
            let mut method = self.parse_fn()?;
            method.is_pub = is_pub;
            method.documentation = documentation;
            methods.push(method);
        }
        self.expect(T::RBrace)?;
        Ok(ImplDecl {
            generics,
            type_name,
            trait_name,
            methods,
        })
    }

    // --- types -------------------------------------------------------------

    fn parse_type(&mut self) -> Result<Type, ParseError> {
        self.with_nesting(|parser| parser.parse_type_inner())
    }

    fn parse_type_inner(&mut self) -> Result<Type, ParseError> {
        if self.accept(T::KwFn)? {
            self.expect(T::LParen)?;
            let mut params = Vec::new();
            while self.cur() != T::RParen {
                params.push(self.parse_type()?);
                if !self.accept(T::Comma)? {
                    break;
                }
            }
            self.expect(T::RParen)?;
            let ret = if self.accept(T::Arrow)? {
                self.parse_type()?
            } else {
                Type::Unit
            };
            return Ok(Type::Function {
                params,
                ret: Box::new(ret),
            });
        }
        if self.accept(T::Amp)? {
            let mutable = self.accept(T::KwMut)?;
            // `&dyn Trait` — consume `dyn` keyword before the trait name
            let _dyn = self.accept(T::KwDyn)?;
            let inner = Box::new(self.parse_type()?);
            return Ok(Type::Ref { mutable, inner });
        }
        if self.cur() == T::LParen {
            self.bump()?;
            if self.accept(T::RParen)? {
                return Ok(Type::Unit);
            }
            let first = self.parse_type()?;
            if !self.accept(T::Comma)? {
                self.expect(T::RParen)?;
                return Ok(first);
            }
            let mut items = vec![first];
            while self.cur() != T::RParen {
                items.push(self.parse_type()?);
                if !self.accept(T::Comma)? {
                    break;
                }
            }
            self.expect(T::RParen)?;
            return Ok(Type::Tuple(items));
        }
        let mut name = self.expect_ident()?;
        while self.cur() == T::ColonColon {
            self.bump()?;
            name.push_str("::");
            name.push_str(&self.expect_ident()?);
        }
        let mut args = Vec::new();
        if self.accept(T::Lt)? {
            while self.cur() != T::Gt {
                args.push(self.parse_type()?);
                if !self.accept(T::Comma)? {
                    break;
                }
            }
            self.expect(T::Gt)?;
        }
        Ok(Type::Path {
            id: self.next_type_id(),
            name,
            args,
        })
    }

    // --- blocks & statements ----------------------------------------------

    fn parse_block(&mut self) -> Result<Block, ParseError> {
        self.with_nesting(|parser| parser.parse_block_inner())
    }

    fn parse_block_inner(&mut self) -> Result<Block, ParseError> {
        self.expect(T::LBrace)?;
        // Inside a block, struct literals are allowed again.
        let saved = self.no_struct;
        self.no_struct = false;
        let mut stmts = Vec::new();
        let mut statement_blank_before = Vec::new();
        let mut tail = None;
        let mut tail_blank_before = false;
        while self.cur() != T::RBrace && self.cur() != T::Eof {
            let blank_before = self.blank_line_before_current();
            let statement_count = stmts.len();
            match self.cur() {
                T::KwLet => stmts.push(self.parse_let()?),
                T::KwReturn => stmts.push(self.parse_return()?),
                T::KwWhile => stmts.push(self.parse_while()?),
                T::KwFor => stmts.push(self.parse_for()?),
                T::KwBreak => {
                    self.bump()?;
                    let value = if self.accept(T::Semi)? {
                        None
                    } else {
                        let value = self.parse_expr()?;
                        self.expect(T::Semi)?;
                        Some(value)
                    };
                    stmts.push(Stmt::Break(value));
                }
                T::KwContinue => {
                    self.bump()?;
                    self.expect(T::Semi)?;
                    stmts.push(Stmt::Continue);
                }
                _ => {
                    let e = self.parse_expr()?;
                    if self.accept(T::Semi)? {
                        stmts.push(Stmt::Expr(e));
                    } else if self.cur() == T::RBrace {
                        tail = Some(Box::new(e));
                        tail_blank_before = blank_before;
                    } else if is_block_like(&e) {
                        stmts.push(Stmt::Expr(e));
                    } else {
                        self.no_struct = saved;
                        return Err(self.err(&format!(
                            "expected `;` or `}}`, found `{}`",
                            self.cur().user_string()
                        )));
                    }
                }
            }
            if stmts.len() > statement_count {
                statement_blank_before.push(blank_before);
            }
        }
        self.expect(T::RBrace)?;
        self.no_struct = saved;
        Ok(Block {
            stmts,
            statement_blank_before,
            tail,
            tail_blank_before,
        })
    }

    fn parse_let(&mut self) -> Result<Stmt, ParseError> {
        self.expect(T::KwLet)?;
        let mutable = self.accept(T::KwMut)?;
        let name_span = self.lexer.current_range();
        let name = self.expect_ident()?;
        let ty = if self.accept(T::Colon)? {
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect(T::Eq)?;
        let init = self.parse_expr()?;
        self.expect(T::Semi)?;
        Ok(Stmt::Let {
            name,
            name_span,
            mutable,
            ty,
            init,
        })
    }

    fn parse_return(&mut self) -> Result<Stmt, ParseError> {
        self.expect(T::KwReturn)?;
        if self.accept(T::Semi)? {
            return Ok(Stmt::Return(None));
        }
        let e = self.parse_expr()?;
        self.expect(T::Semi)?;
        Ok(Stmt::Return(Some(e)))
    }

    fn parse_while(&mut self) -> Result<Stmt, ParseError> {
        self.expect(T::KwWhile)?;
        if self.cur() == T::KwLet {
            self.expect(T::KwLet)?;
            let pat = self.parse_pattern()?;
            self.expect(T::Eq)?;
            let expr = self.parse_cond()?;
            let body = self.parse_block()?;
            return Ok(Stmt::WhileLet {
                pat: Box::new(pat),
                expr,
                body,
            });
        }
        let cond = self.parse_cond()?;
        let body = self.parse_block()?;
        Ok(Stmt::While { cond, body })
    }

    fn parse_for(&mut self) -> Result<Stmt, ParseError> {
        self.expect(T::KwFor)?;
        let var_span = self.lexer.current_range();
        let var = self.expect_ident()?;
        self.expect(T::KwIn)?;
        let iter = self.parse_cond()?; // no struct literal before the `{` body
        let body = self.parse_block()?;
        Ok(Stmt::For {
            var,
            var_span,
            iter,
            body,
        })
    }

    // --- expressions -------------------------------------------------------

    /// Parse an expression with struct literals suppressed (for conditions and
    /// match scrutinees).
    fn parse_cond(&mut self) -> Result<Expr, ParseError> {
        let saved = self.no_struct;
        self.no_struct = true;
        let e = self.parse_expr();
        self.no_struct = saved;
        e
    }

    /// Parse a sub-expression where struct literals are allowed again (call
    /// args, grouping, struct field values, match bodies).
    fn parse_expr_allow_struct(&mut self) -> Result<Expr, ParseError> {
        let saved = self.no_struct;
        self.no_struct = false;
        let e = self.parse_expr();
        self.no_struct = saved;
        e
    }

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.with_nesting(|parser| parser.parse_expr_inner())
    }

    fn parse_expr_inner(&mut self) -> Result<Expr, ParseError> {
        let start = self.lexer.current_range();
        let lhs = self.parse_bin(0)?;
        if let Some(op) = assignop(self.cur()) {
            self.bump()?;
            let value = self.parse_expr()?;
            return Ok(self.mk(
                ExprKind::Assign {
                    op,
                    target: Box::new(lhs),
                    value: Box::new(value),
                },
                start,
            ));
        }
        Ok(lhs)
    }

    fn parse_bin(&mut self, min_bp: u8) -> Result<Expr, ParseError> {
        self.with_nesting(|parser| parser.parse_bin_inner(min_bp))
    }

    fn parse_bin_inner(&mut self, min_bp: u8) -> Result<Expr, ParseError> {
        let start = self.lexer.current_range();
        let mut lhs = self.parse_unary()?;
        loop {
            if matches!(self.cur(), T::DotDot | T::DotDotEq) {
                const RANGE_BP: u8 = 4;
                if RANGE_BP < min_bp {
                    break;
                }
                let inclusive = self.cur() == T::DotDotEq;
                self.bump()?;
                let rhs = self.parse_bin(RANGE_BP + 1)?;
                lhs = self.mk(
                    ExprKind::Range {
                        start: Box::new(lhs),
                        end: Box::new(rhs),
                        inclusive,
                    },
                    start,
                );
                continue;
            }
            let Some((op, lbp)) = binop(self.cur()) else {
                break;
            };
            if lbp < min_bp {
                break;
            }
            self.bump()?;
            let rhs = self.parse_bin(lbp + 1)?;
            lhs = self.mk(
                ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                start,
            );
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        self.with_nesting(|parser| parser.parse_unary_inner())
    }

    fn parse_unary_inner(&mut self) -> Result<Expr, ParseError> {
        let start = self.lexer.current_range();
        match self.cur() {
            T::Minus => {
                self.bump()?;
                let expr = Box::new(self.parse_unary()?);
                Ok(self.mk(
                    ExprKind::Unary {
                        op: UnOp::Neg,
                        expr,
                    },
                    start,
                ))
            }
            T::Not => {
                self.bump()?;
                let expr = Box::new(self.parse_unary()?);
                Ok(self.mk(
                    ExprKind::Unary {
                        op: UnOp::Not,
                        expr,
                    },
                    start,
                ))
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr, ParseError> {
        let start = self.lexer.current_range();
        let mut e = self.parse_primary()?;
        loop {
            match self.cur() {
                T::LParen => {
                    let args = self.parse_call_args()?;
                    e = self.mk(
                        ExprKind::Call {
                            callee: Box::new(e),
                            args,
                        },
                        start,
                    );
                }
                T::Dot | T::QuestionDot => {
                    let optional = self.cur() == T::QuestionDot;
                    self.bump()?;
                    let member_span = self.lexer.current_range();
                    let name = self.expect_ident()?;
                    let mut type_args = Vec::new();
                    if self.accept(T::ColonColon)? {
                        self.expect(T::Lt)?;
                        while self.cur() != T::Gt {
                            type_args.push(self.parse_type()?);
                            if !self.accept(T::Comma)? {
                                break;
                            }
                        }
                        self.expect(T::Gt)?;
                    }
                    if self.cur() == T::LParen {
                        let args = self.parse_call_args()?;
                        e = self.mk(
                            ExprKind::MethodCall {
                                recv: Box::new(e),
                                method: name,
                                optional,
                                type_args,
                                args,
                                method_span: member_span,
                            },
                            start,
                        );
                    } else if !type_args.is_empty() {
                        return Err(self.err("method turbofish must be followed by `(...)`"));
                    } else {
                        e = self.mk(
                            ExprKind::Field {
                                base: Box::new(e),
                                name,
                                optional,
                                name_span: member_span,
                            },
                            start,
                        );
                    }
                }
                T::Question => {
                    self.bump()?;
                    e = self.mk(ExprKind::Try { expr: Box::new(e) }, start);
                }
                T::LBracket => {
                    self.bump()?;
                    let index = self.parse_expr_allow_struct()?;
                    self.expect(T::RBracket)?;
                    e = self.mk(
                        ExprKind::Index {
                            base: Box::new(e),
                            index: Box::new(index),
                        },
                        start,
                    );
                }
                _ => break,
            }
        }
        Ok(e)
    }

    fn parse_call_args(&mut self) -> Result<Vec<Expr>, ParseError> {
        self.expect(T::LParen)?;
        let mut args = Vec::new();
        while self.cur() != T::RParen {
            args.push(self.parse_expr_allow_struct()?);
            if !self.accept(T::Comma)? {
                break;
            }
        }
        self.expect(T::RParen)?;
        Ok(args)
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        let start = self.lexer.current_range();
        match self.cur() {
            T::Pipe | T::OrOr => self.parse_closure(),
            T::Int => {
                let s = self.text().to_string();
                self.bump()?;
                Ok(self.mk(ExprKind::Int(s), start))
            }
            T::Float => {
                let s = self.text().to_string();
                self.bump()?;
                Ok(self.mk(ExprKind::Float(s), start))
            }
            T::Str => {
                let s = self.text().to_string();
                self.bump()?;
                Ok(self.mk(ExprKind::Str(s), start))
            }
            T::KwTrue => {
                self.bump()?;
                Ok(self.mk(ExprKind::Bool(true), start))
            }
            T::KwFalse => {
                self.bump()?;
                Ok(self.mk(ExprKind::Bool(false), start))
            }
            T::KwLoop => {
                self.bump()?;
                let body = self.parse_block()?;
                Ok(self.mk(ExprKind::Loop(body), start))
            }
            T::Hash => self.parse_map_literal(start),
            T::Ident | T::KwSelf => {
                // Macro call: `name!(...)` / `name![...]`.
                if self.cur() == T::Ident && self.lexer.peek_next() == T::Not {
                    return self.parse_macro();
                }
                let mut segs = vec![self.expect_ident()?];
                while self.cur() == T::ColonColon {
                    self.bump()?;
                    segs.push(self.expect_ident()?);
                }
                if !self.no_struct && self.cur() == T::LBrace {
                    let fields = self.parse_struct_lit_fields()?;
                    Ok(self.mk(ExprKind::StructLit { path: segs, fields }, start))
                } else {
                    Ok(self.mk(ExprKind::Path(segs), start))
                }
            }
            T::LParen => {
                self.bump()?;
                let e = self.parse_expr_allow_struct()?;
                self.expect(T::RParen)?;
                Ok(e)
            }
            T::LBrace => {
                let b = self.parse_block()?;
                Ok(self.mk(ExprKind::Block(b), start))
            }
            T::KwIf => self.parse_if(),
            T::KwMatch => self.parse_match(),
            other => Err(self.err(&format!(
                "expected expression, found `{}`",
                other.user_string()
            ))),
        }
    }

    fn parse_closure(&mut self) -> Result<Expr, ParseError> {
        let start = self.lexer.current_range();
        let mut params = Vec::new();
        if self.cur() == T::OrOr {
            self.bump()?;
        } else {
            self.expect(T::Pipe)?;
            while self.cur() != T::Pipe && self.cur() != T::Eof {
                let name_span = self.lexer.current_range();
                let name = self.expect_ident()?;
                let ty = if self.accept(T::Colon)? {
                    Some(self.parse_type()?)
                } else {
                    None
                };
                params.push(ClosureParam {
                    name,
                    name_span,
                    ty,
                });
                if !self.accept(T::Comma)? {
                    break;
                }
            }
            self.expect(T::Pipe)?;
        }
        let ret = if self.accept(T::Arrow)? {
            Some(self.parse_type()?)
        } else {
            None
        };
        let body = if self.cur() == T::LBrace {
            ClosureBody::Block(self.parse_block()?)
        } else {
            ClosureBody::Expr(Box::new(self.parse_expr_allow_struct()?))
        };
        Ok(self.mk(ExprKind::Closure { params, ret, body }, start))
    }

    fn parse_map_literal(&mut self, start: SourceRange) -> Result<Expr, ParseError> {
        self.expect(T::Hash)?;
        self.expect(T::LBrace)?;
        let mut entries = Vec::new();
        while self.cur() != T::RBrace {
            let key = self.parse_expr_allow_struct()?;
            self.expect(T::Colon)?;
            let value = self.parse_expr_allow_struct()?;
            entries.push((key, value));
            if !self.accept(T::Comma)? {
                break;
            }
        }
        self.expect(T::RBrace)?;
        Ok(self.mk(ExprKind::MapLit(entries), start))
    }

    fn parse_macro(&mut self) -> Result<Expr, ParseError> {
        let start = self.lexer.current_range();
        let name = self.expect_ident()?;
        self.expect(T::Not)?;
        let (open, close) = match self.cur() {
            T::LParen => (T::LParen, T::RParen),
            T::LBracket => (T::LBracket, T::RBracket),
            other => {
                return Err(self.err(&format!(
                    "expected `(` or `[` after `{}!`, found `{}`",
                    name,
                    other.user_string()
                )));
            }
        };
        self.expect(open)?;
        let mut args = Vec::new();
        while self.cur() != close {
            args.push(self.parse_expr_allow_struct()?);
            if !self.accept(T::Comma)? {
                break;
            }
        }
        self.expect(close)?;
        Ok(self.mk(ExprKind::MacroCall { name, args }, start))
    }

    fn parse_struct_lit_fields(&mut self) -> Result<Vec<(String, Expr)>, ParseError> {
        self.expect(T::LBrace)?;
        let mut fields = Vec::new();
        while self.cur() != T::RBrace {
            let fstart = self.lexer.current_range();
            let fname = self.expect_ident()?;
            let value = if self.accept(T::Colon)? {
                self.parse_expr_allow_struct()?
            } else {
                // field shorthand: `Point { x, y }`
                self.mk(ExprKind::Path(vec![fname.clone()]), fstart)
            };
            fields.push((fname, value));
            if !self.accept(T::Comma)? {
                break;
            }
        }
        self.expect(T::RBrace)?;
        Ok(fields)
    }

    fn parse_if(&mut self) -> Result<Expr, ParseError> {
        let start = self.lexer.current_range();
        self.expect(T::KwIf)?;
        if self.cur() == T::KwLet {
            return self.parse_if_let(start);
        }
        let cond = self.parse_cond()?;
        let then_block = self.parse_block()?;
        let else_block = self.parse_else()?;
        Ok(self.mk(
            ExprKind::If {
                cond: Box::new(cond),
                then_block,
                else_block,
            },
            start,
        ))
    }

    fn parse_if_let(&mut self, start: SourceRange) -> Result<Expr, ParseError> {
        self.expect(T::KwLet)?;
        let pat = self.parse_pattern()?;
        self.expect(T::Eq)?;
        let expr = self.parse_cond()?; // no struct literal before the `{` body
        let then_block = self.parse_block()?;
        let else_block = self.parse_else()?;
        Ok(self.mk(
            ExprKind::IfLet {
                pat: Box::new(pat),
                expr: Box::new(expr),
                then_block,
                else_block,
            },
            start,
        ))
    }

    fn parse_else(&mut self) -> Result<Option<Box<ElseBranch>>, ParseError> {
        if self.accept(T::KwElse)? {
            if self.cur() == T::KwIf {
                Ok(Some(Box::new(ElseBranch::If(self.parse_if()?))))
            } else {
                Ok(Some(Box::new(ElseBranch::Block(self.parse_block()?))))
            }
        } else {
            Ok(None)
        }
    }

    fn parse_match(&mut self) -> Result<Expr, ParseError> {
        let start = self.lexer.current_range();
        self.expect(T::KwMatch)?;
        let scrut = self.parse_cond()?;
        self.expect(T::LBrace)?;
        let mut arms = Vec::new();
        while self.cur() != T::RBrace && self.cur() != T::Eof {
            let mut pats = vec![self.parse_pattern()?];
            while self.accept(T::Pipe)? {
                pats.push(self.parse_pattern()?);
            }
            let guard = if self.accept(T::KwIf)? {
                Some(self.parse_cond()?)
            } else {
                None
            };
            self.expect(T::FatArrow)?;
            let body = self.parse_expr_allow_struct()?;
            let _ = self.accept(T::Comma)?;
            arms.push(MatchArm { pats, guard, body });
        }
        self.expect(T::RBrace)?;
        Ok(self.mk(
            ExprKind::Match {
                scrut: Box::new(scrut),
                arms,
            },
            start,
        ))
    }

    // --- patterns ----------------------------------------------------------

    fn parse_pattern(&mut self) -> Result<Pattern, ParseError> {
        self.with_nesting(|parser| parser.parse_pattern_inner())
    }

    fn parse_pattern_inner(&mut self) -> Result<Pattern, ParseError> {
        // literal / range
        if matches!(
            self.cur(),
            T::Int | T::Float | T::Str | T::KwTrue | T::KwFalse | T::Minus
        ) {
            let lo = self.parse_pattern_literal()?;
            if self.accept(T::DotDotEq)? {
                let hi = self.parse_pattern_literal()?;
                return Ok(Pattern::Range {
                    lo: Box::new(lo),
                    hi: Box::new(hi),
                    inclusive: true,
                });
            }
            if self.accept(T::DotDot)? {
                let hi = self.parse_pattern_literal()?;
                return Ok(Pattern::Range {
                    lo: Box::new(lo),
                    hi: Box::new(hi),
                    inclusive: false,
                });
            }
            return Ok(Pattern::Literal(lo));
        }

        // wildcard / path / binding / variant
        if self.cur() == T::Ident || self.cur() == T::KwSelf {
            let first = self.text().to_string();
            if first == "_" {
                self.bump()?;
                return Ok(Pattern::Wildcard);
            }
            let first_span = self.lexer.current_range();
            let mut segs = vec![self.expect_ident()?];
            while self.cur() == T::ColonColon {
                self.bump()?;
                segs.push(self.expect_ident()?);
            }
            match self.cur() {
                T::LParen => {
                    self.bump()?;
                    let mut elems = Vec::new();
                    while self.cur() != T::RParen {
                        elems.push(self.parse_pattern()?);
                        if !self.accept(T::Comma)? {
                            break;
                        }
                    }
                    self.expect(T::RParen)?;
                    Ok(Pattern::TupleVariant {
                        id: self.next_pattern_id(),
                        path: segs,
                        elems,
                    })
                }
                T::LBrace => {
                    self.bump()?;
                    let mut fields = Vec::new();
                    let mut rest = false;
                    while self.cur() != T::RBrace {
                        if self.accept(T::DotDot)? {
                            rest = true;
                            break;
                        }
                        let fname_span = self.lexer.current_range();
                        let fname = self.expect_ident()?;
                        let pat = if self.accept(T::Colon)? {
                            self.parse_pattern()?
                        } else {
                            Pattern::Binding(fname.clone(), fname_span)
                        };
                        fields.push((fname, pat));
                        if !self.accept(T::Comma)? {
                            break;
                        }
                    }
                    self.expect(T::RBrace)?;
                    Ok(Pattern::StructVariant {
                        id: self.next_pattern_id(),
                        path: segs,
                        fields,
                        rest,
                    })
                }
                _ => {
                    // Single lowercase-initial identifier => binding; otherwise a
                    // path (unit variant / const / None). Multi-segment is always
                    // a path.
                    if segs.len() == 1 && starts_lowercase(&segs[0]) {
                        Ok(Pattern::Binding(segs.pop().unwrap(), first_span))
                    } else {
                        Ok(Pattern::Path {
                            id: self.next_pattern_id(),
                            path: segs,
                        })
                    }
                }
            }
        } else {
            Err(self.err(&format!(
                "expected pattern, found `{}`",
                self.cur().user_string()
            )))
        }
    }

    fn parse_pattern_literal(&mut self) -> Result<Expr, ParseError> {
        self.with_nesting(|parser| parser.parse_pattern_literal_inner())
    }

    fn parse_pattern_literal_inner(&mut self) -> Result<Expr, ParseError> {
        let start = self.lexer.current_range();
        if self.accept(T::Minus)? {
            let inner = self.parse_pattern_literal()?;
            return Ok(self.mk(
                ExprKind::Unary {
                    op: UnOp::Neg,
                    expr: Box::new(inner),
                },
                start,
            ));
        }
        match self.cur() {
            T::Int => {
                let s = self.text().to_string();
                self.bump()?;
                Ok(self.mk(ExprKind::Int(s), start))
            }
            T::Float => {
                let s = self.text().to_string();
                self.bump()?;
                Ok(self.mk(ExprKind::Float(s), start))
            }
            T::Str => {
                let s = self.text().to_string();
                self.bump()?;
                Ok(self.mk(ExprKind::Str(s), start))
            }
            T::KwTrue => {
                self.bump()?;
                Ok(self.mk(ExprKind::Bool(true), start))
            }
            T::KwFalse => {
                self.bump()?;
                Ok(self.mk(ExprKind::Bool(false), start))
            }
            other => Err(self.err(&format!(
                "expected literal in pattern, found `{}`",
                other.user_string()
            ))),
        }
    }
}

fn leading_trivia_has_blank_line(trivia: &str) -> bool {
    rua_lex::lex(trivia).into_iter().any(|token| {
        if token.kind != T::Whitespace {
            return false;
        }
        let raw = &trivia[token.range.start() as usize..token.range.end() as usize];
        raw.replace("\r\n", "\n")
            .bytes()
            .filter(|byte| matches!(byte, b'\n' | b'\r'))
            .count()
            >= 2
    })
}

fn render_leading_documentation(trivia: &str, inner: bool) -> Option<String> {
    let mut docs = Vec::new();
    for token in rua_lex::lex(trivia) {
        let raw = &trivia[token.range.start() as usize..token.range.end() as usize];
        match token.kind {
            T::Whitespace => {
                let normalized = raw.replace("\r\n", "\n");
                if normalized
                    .bytes()
                    .filter(|byte| matches!(byte, b'\n' | b'\r'))
                    .count()
                    >= 2
                {
                    if inner && !docs.is_empty() {
                        break;
                    }
                    docs.clear();
                }
            }
            T::LineComment | T::BlockComment => {
                if let Some(documentation) = normalize_doc_comment(raw, inner) {
                    docs.push(documentation);
                } else if inner && !docs.is_empty() {
                    break;
                } else {
                    docs.clear();
                }
            }
            _ => docs.clear(),
        }
    }
    let documentation = docs.join("\n").trim().to_string();
    (!documentation.is_empty()).then_some(documentation)
}

fn normalize_doc_comment(raw: &str, inner: bool) -> Option<String> {
    let text = raw.trim();
    let line_prefix = if inner { "//!" } else { "///" };
    let block_prefix = if inner { "/*!" } else { "/**" };
    if let Some(body) = text.strip_prefix(line_prefix) {
        return Some(body.trim().to_string());
    }
    let body = text.strip_prefix(block_prefix)?.strip_suffix("*/")?;
    Some(
        body.lines()
            .map(|line| line.trim().trim_start_matches('*').trim())
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string(),
    )
}

fn starts_lowercase(s: &str) -> bool {
    s.chars()
        .next()
        .map(|c| c.is_lowercase() || c == '_')
        .unwrap_or(false)
}

fn is_block_like(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::If { .. }
            | ExprKind::IfLet { .. }
            | ExprKind::Block(_)
            | ExprKind::Loop(_)
            | ExprKind::Match { .. }
    )
}

fn assignop(t: T) -> Option<Option<BinOp>> {
    Some(match t {
        T::Eq => None,
        T::PlusEq => Some(BinOp::Add),
        T::MinusEq => Some(BinOp::Sub),
        T::StarEq => Some(BinOp::Mul),
        T::SlashEq => Some(BinOp::Div),
        T::PercentEq => Some(BinOp::Rem),
        _ => return None,
    })
}

/// Binary operator + left binding power for the current token.
fn binop(t: T) -> Option<(BinOp, u8)> {
    Some(match t {
        T::QuestionQuestion => (BinOp::Coalesce, 0),
        T::OrOr => (BinOp::Or, 1),
        T::AndAnd => (BinOp::And, 2),
        T::EqEq => (BinOp::Eq, 3),
        T::Ne => (BinOp::Ne, 3),
        T::Lt => (BinOp::Lt, 3),
        T::Le => (BinOp::Le, 3),
        T::Gt => (BinOp::Gt, 3),
        T::Ge => (BinOp::Ge, 3),
        T::KwIn => (BinOp::Contains, 3),
        T::Plus => (BinOp::Add, 5),
        T::Minus => (BinOp::Sub, 5),
        T::Star => (BinOp::Mul, 6),
        T::Slash => (BinOp::Div, 6),
        T::Percent => (BinOp::Rem, 6),
        _ => return None,
    })
}
