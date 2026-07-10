//! Recursive-descent parser with precedence climbing for expressions.
//!
//! Structure follows lua-rs (`compiler/statement.rs` + `compiler/expr_parser.rs`):
//! item/statement parsing calls into `parse_expr`, which uses a `subexpr`-style
//! precedence loop (`parse_bin`). Adapted to the Rust-subset grammar.

use crate::ast::*;
use crate::lexer::RuaLexer;
use crate::token::{RuaTokenKind as T, SourceRange};

/// A parsed function signature: `(name, name_span, generics, has_self, params, return type)`.
type FnSig = (
    String,
    SourceRange,
    Vec<GenericParam>,
    bool,
    Vec<Param>,
    Option<Type>,
);

pub fn parse(src: &str) -> Result<Program, String> {
    let lexer = RuaLexer::new(src)?;
    let mut p = Parser {
        lexer,
        no_struct: false,
    };
    p.parse_program()
}

struct Parser<'a> {
    lexer: RuaLexer<'a>,
    /// When true, an `Ident {` is NOT parsed as a struct literal (used while
    /// parsing `if`/`while` conditions and `match` scrutinees, where `{` starts
    /// a block). Reset to false inside delimited sub-expressions.
    no_struct: bool,
}

impl<'a> Parser<'a> {
    // --- token helpers -----------------------------------------------------

    fn cur(&self) -> T {
        self.lexer.current()
    }

    fn line(&self) -> usize {
        self.lexer.current_line()
    }

    fn text(&self) -> &'a str {
        self.lexer.current_text()
    }

    fn bump(&mut self) -> Result<(), String> {
        self.lexer.bump()
    }

    fn accept(&mut self, kind: T) -> Result<bool, String> {
        if self.cur() == kind {
            self.bump()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn expect(&mut self, kind: T) -> Result<(), String> {
        if self.cur() == kind {
            self.bump()
        } else {
            Err(self.err(&format!(
                "expected `{}`, found `{}`",
                kind.to_user_string(),
                self.cur().to_user_string()
            )))
        }
    }

    fn expect_ident(&mut self) -> Result<String, String> {
        if self.cur() == T::Ident || self.cur() == T::KwSelf {
            let s = self.text().to_string();
            self.bump()?;
            Ok(s)
        } else {
            Err(self.err(&format!(
                "expected identifier, found `{}`",
                self.cur().to_user_string()
            )))
        }
    }

    fn err(&self, msg: &str) -> String {
        format!("{}: {}", self.line(), msg)
    }

    /// Wrap an expression kind with the span running from `start` (captured
    /// before parsing began) through the last consumed token.
    fn mk(&self, kind: ExprKind, start: SourceRange) -> Expr {
        let end = self.lexer.previous_range().end();
        let len = end.saturating_sub(start.start);
        Expr::new(kind, SourceRange::new(start.start, len, start.line))
    }

    /// Skip an optional generic parameter/argument list `<...>` (angle-balanced).
    fn skip_generics(&mut self) -> Result<(), String> {
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
    fn parse_generics(&mut self) -> Result<Vec<GenericParam>, String> {
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
                    // A bound is a (possibly qualified) trait path; keep the leaf.
                    let mut leaf = self.expect_ident()?;
                    while self.accept(T::ColonColon)? {
                        leaf = self.expect_ident()?;
                    }
                    // Tolerate associated-type args, e.g. `Iterator<Item = U>`.
                    self.skip_generics()?;
                    bounds.push(leaf);
                    if !self.accept(T::Plus)? {
                        break;
                    }
                }
            }
            params.push(GenericParam { name, bounds });
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
    fn parse_where(&mut self, generics: &mut [GenericParam]) -> Result<(), String> {
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
                    let mut leaf = self.expect_ident()?;
                    while self.accept(T::ColonColon)? {
                        leaf = self.expect_ident()?;
                    }
                    self.skip_generics()?; // tolerate `Iterator<Item = U>`
                    bounds.push(leaf);
                    if !self.accept(T::Plus)? {
                        break;
                    }
                }
            }
            if simple {
                if let Some(g) = generics.iter_mut().find(|g| g.name == name) {
                    g.bounds.extend(bounds);
                }
            }
            if !self.accept(T::Comma)? {
                break;
            }
        }
        Ok(())
    }

    // --- items -------------------------------------------------------------

    fn parse_program(&mut self) -> Result<Program, String> {
        let mut items = Vec::new();
        while self.cur() != T::Eof {
            items.push(self.parse_item()?);
        }
        Ok(Program { items })
    }

    fn parse_item(&mut self) -> Result<Item, String> {
        let is_pub = self.accept(T::KwPub)?;
        match self.cur() {
            T::KwFn => {
                let mut f = self.parse_fn()?;
                f.is_pub = is_pub;
                Ok(Item::Fn(f))
            }
            T::KwStruct => {
                let mut s = self.parse_struct()?;
                s.is_pub = is_pub;
                Ok(Item::Struct(s))
            }
            T::KwEnum => {
                let mut e = self.parse_enum()?;
                e.is_pub = is_pub;
                Ok(Item::Enum(e))
            }
            T::KwImpl => Ok(Item::Impl(self.parse_impl()?)),
            T::KwTrait => {
                let mut t = self.parse_trait()?;
                t.is_pub = is_pub;
                Ok(Item::Trait(t))
            }
            T::KwExtern => Ok(Item::Extern(self.parse_extern()?)),
            T::KwMod => Ok(Item::Mod(self.parse_mod(is_pub)?)),
            T::KwUse => Ok(Item::Use(self.parse_use()?)),
            other => Err(self.err(&format!(
                "expected item (`fn`/`struct`/`enum`/`impl`/`trait`/`extern`/`mod`/`use`), found `{}`",
                other.to_user_string()
            ))),
        }
    }

    /// `mod name { <items> }` (inline) or `mod name;` (file module — its items
    /// are loaded from a sibling `.rua` file during resolution).
    fn parse_mod(&mut self, is_pub: bool) -> Result<ModDecl, String> {
        self.expect(T::KwMod)?;
        let name = self.expect_ident()?;
        if self.accept(T::Semi)? {
            return Ok(ModDecl {
                name,
                items: Vec::new(),
                is_pub,
                is_file: true,
                is_decl: false,
            });
        }
        self.expect(T::LBrace)?;
        let mut items = Vec::new();
        while self.cur() != T::RBrace {
            items.push(self.parse_item()?);
        }
        self.expect(T::RBrace)?;
        Ok(ModDecl {
            name,
            items,
            is_pub,
            is_file: false,
            is_decl: false,
        })
    }

    /// `use a::b::c;` / `use a::b as c;` / `use a::b::{c, d as e};`.
    fn parse_use(&mut self) -> Result<UseDecl, String> {
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
    fn parse_fn_sig(&mut self) -> Result<FnSig, String> {
        self.expect(T::KwFn)?;
        let name_span = self.lexer.current_range();
        let name = self.expect_ident()?;
        let mut generics = self.parse_generics()?;
        self.expect(T::LParen)?;

        // Optional `self` / `&self` / `&mut self` receiver.
        let mut has_self = false;
        if self.cur() == T::Amp {
            // `&self` or `&mut self` (a `&Type` param would need a name first).
            self.bump()?;
            let _ = self.accept(T::KwMut)?;
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
            params.push(Param { name: pname, name_span: pspan, ty });
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
        Ok((name, name_span, generics, has_self, params, ret))
    }

    fn parse_fn(&mut self) -> Result<FnDecl, String> {
        let (name, name_span, generics, has_self, params, ret) = self.parse_fn_sig()?;
        let body = self.parse_block()?;
        Ok(FnDecl {
            name,
            name_span,
            generics,
            is_pub: false,
            has_self,
            params,
            ret,
            body,
        })
    }

    /// `extern "lua" { fn name(params) -> R; ... }`.
    fn parse_extern(&mut self) -> Result<ExternBlock, String> {
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
            let _ = self.accept(T::KwPub)?;
            self.expect(T::KwFn)?;
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
                params.push(Param { name: pname, name_span: pspan, ty });
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
                params,
                ret,
                variadic,
            });
        }
        self.expect(T::RBrace)?;
        Ok(ExternBlock { abi, fns })
    }

    fn parse_trait(&mut self) -> Result<TraitDecl, String> {
        self.expect(T::KwTrait)?;
        let name = self.expect_ident()?;
        let mut generics = self.parse_generics()?;
        self.parse_where(&mut generics)?;
        self.expect(T::LBrace)?;
        let mut methods = Vec::new();
        while self.cur() != T::RBrace && self.cur() != T::Eof {
            let (mname, name_span, mgen, has_self, params, ret) = self.parse_fn_sig()?;
            let default = if self.cur() == T::LBrace {
                Some(self.parse_block()?)
            } else {
                self.expect(T::Semi)?;
                None
            };
            methods.push(TraitMethod {
                name: mname,
                name_span,
                generics: mgen,
                has_self,
                params,
                ret,
                default,
            });
        }
        self.expect(T::RBrace)?;
        Ok(TraitDecl {
            name,
            generics,
            methods,
            is_pub: false,
        })
    }

    fn parse_struct(&mut self) -> Result<StructDecl, String> {
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
            generics,
            fields,
            is_pub: false,
        })
    }

    fn parse_field_list(&mut self) -> Result<Vec<Field>, String> {
        self.expect(T::LBrace)?;
        let mut fields = Vec::new();
        while self.cur() != T::RBrace {
            let _ = self.accept(T::KwPub)?;
            let name_span = self.lexer.current_range();
            let fname = self.expect_ident()?;
            self.expect(T::Colon)?;
            let ty = self.parse_type()?;
            fields.push(Field { name: fname, ty, name_span });
            if !self.accept(T::Comma)? {
                break;
            }
        }
        self.expect(T::RBrace)?;
        Ok(fields)
    }

    fn parse_enum(&mut self) -> Result<EnumDecl, String> {
        self.expect(T::KwEnum)?;
        let name = self.expect_ident()?;
        let mut generics = self.parse_generics()?;
        self.parse_where(&mut generics)?;
        self.expect(T::LBrace)?;
        let mut variants = Vec::new();
        while self.cur() != T::RBrace {
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
            variants.push(Variant { name: vname, kind });
            if !self.accept(T::Comma)? {
                break;
            }
        }
        self.expect(T::RBrace)?;
        Ok(EnumDecl {
            name,
            generics,
            variants,
            is_pub: false,
        })
    }

    fn parse_impl(&mut self) -> Result<ImplDecl, String> {
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
            let _ = self.accept(T::KwPub)?;
            methods.push(self.parse_fn()?);
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

    fn parse_type(&mut self) -> Result<Type, String> {
        if self.accept(T::Amp)? {
            let mutable = self.accept(T::KwMut)?;
            let inner = Box::new(self.parse_type()?);
            return Ok(Type::Ref { mutable, inner });
        }
        if self.cur() == T::LParen {
            self.bump()?;
            self.expect(T::RParen)?;
            return Ok(Type::Unit);
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
        Ok(Type::Path { name, args })
    }

    // --- blocks & statements ----------------------------------------------

    fn parse_block(&mut self) -> Result<Block, String> {
        self.expect(T::LBrace)?;
        // Inside a block, struct literals are allowed again.
        let saved = self.no_struct;
        self.no_struct = false;
        let mut stmts = Vec::new();
        let mut tail = None;
        while self.cur() != T::RBrace && self.cur() != T::Eof {
            match self.cur() {
                T::KwLet => stmts.push(self.parse_let()?),
                T::KwReturn => stmts.push(self.parse_return()?),
                T::KwWhile => stmts.push(self.parse_while()?),
                T::KwLoop => stmts.push(self.parse_loop()?),
                T::KwFor => stmts.push(self.parse_for()?),
                T::KwBreak => {
                    self.bump()?;
                    self.expect(T::Semi)?;
                    stmts.push(Stmt::Break);
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
                    } else if is_block_like(&e) {
                        stmts.push(Stmt::Expr(e));
                    } else {
                        self.no_struct = saved;
                        return Err(self.err(&format!(
                            "expected `;` or `}}`, found `{}`",
                            self.cur().to_user_string()
                        )));
                    }
                }
            }
        }
        self.expect(T::RBrace)?;
        self.no_struct = saved;
        Ok(Block { stmts, tail })
    }

    fn parse_let(&mut self) -> Result<Stmt, String> {
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

    fn parse_return(&mut self) -> Result<Stmt, String> {
        self.expect(T::KwReturn)?;
        if self.accept(T::Semi)? {
            return Ok(Stmt::Return(None));
        }
        let e = self.parse_expr()?;
        self.expect(T::Semi)?;
        Ok(Stmt::Return(Some(e)))
    }

    fn parse_while(&mut self) -> Result<Stmt, String> {
        self.expect(T::KwWhile)?;
        if self.cur() == T::KwLet {
            self.expect(T::KwLet)?;
            let pat = self.parse_pattern()?;
            self.expect(T::Eq)?;
            let expr = self.parse_cond()?;
            let body = self.parse_block()?;
            return Ok(Stmt::WhileLet { pat: Box::new(pat), expr, body });
        }
        let cond = self.parse_cond()?;
        let body = self.parse_block()?;
        Ok(Stmt::While { cond, body })
    }

    fn parse_loop(&mut self) -> Result<Stmt, String> {
        self.expect(T::KwLoop)?;
        let body = self.parse_block()?;
        Ok(Stmt::Loop { body })
    }

    fn parse_for(&mut self) -> Result<Stmt, String> {
        self.expect(T::KwFor)?;
        let var_span = self.lexer.current_range();
        let var = self.expect_ident()?;
        self.expect(T::KwIn)?;
        let iter = self.parse_cond()?; // no struct literal before the `{` body
        let body = self.parse_block()?;
        Ok(Stmt::For { var, var_span, iter, body })
    }

    // --- expressions -------------------------------------------------------

    /// Parse an expression with struct literals suppressed (for conditions and
    /// match scrutinees).
    fn parse_cond(&mut self) -> Result<Expr, String> {
        let saved = self.no_struct;
        self.no_struct = true;
        let e = self.parse_expr();
        self.no_struct = saved;
        e
    }

    /// Parse a sub-expression where struct literals are allowed again (call
    /// args, grouping, struct field values, match bodies).
    fn parse_expr_allow_struct(&mut self) -> Result<Expr, String> {
        let saved = self.no_struct;
        self.no_struct = false;
        let e = self.parse_expr();
        self.no_struct = saved;
        e
    }

    fn parse_expr(&mut self) -> Result<Expr, String> {
        let start = self.lexer.current_range();
        let lhs = self.parse_bin(0)?;
        // range `a..b` / `a..=b` (low precedence, above assignment)
        if self.cur() == T::DotDot || self.cur() == T::DotDotEq {
            let inclusive = self.cur() == T::DotDotEq;
            self.bump()?;
            let end = self.parse_bin(0)?;
            return Ok(self.mk(
                ExprKind::Range {
                    start: Box::new(lhs),
                    end: Box::new(end),
                    inclusive,
                },
                start,
            ));
        }
        if self.cur() == T::Eq {
            self.bump()?;
            let value = self.parse_expr()?;
            return Ok(self.mk(
                ExprKind::Assign {
                    target: Box::new(lhs),
                    value: Box::new(value),
                },
                start,
            ));
        }
        Ok(lhs)
    }

    fn parse_bin(&mut self, min_bp: u8) -> Result<Expr, String> {
        let start = self.lexer.current_range();
        let mut lhs = self.parse_unary()?;
        while let Some((op, lbp)) = binop(self.cur()) {
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

    fn parse_unary(&mut self) -> Result<Expr, String> {
        let start = self.lexer.current_range();
        match self.cur() {
            T::Minus => {
                self.bump()?;
                let expr = Box::new(self.parse_unary()?);
                Ok(self.mk(ExprKind::Unary { op: UnOp::Neg, expr }, start))
            }
            T::Not => {
                self.bump()?;
                let expr = Box::new(self.parse_unary()?);
                Ok(self.mk(ExprKind::Unary { op: UnOp::Not, expr }, start))
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr, String> {
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
                T::Dot => {
                    self.bump()?;
                    let member_span = self.lexer.current_range();
                    let name = self.expect_ident()?;
                    if self.cur() == T::LParen {
                        let args = self.parse_call_args()?;
                        e = self.mk(
                            ExprKind::MethodCall {
                                recv: Box::new(e),
                                method: name,
                                args,
                                method_span: member_span,
                            },
                            start,
                        );
                    } else {
                        e = self.mk(
                            ExprKind::Field {
                                base: Box::new(e),
                                name,
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

    fn parse_call_args(&mut self) -> Result<Vec<Expr>, String> {
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

    fn parse_primary(&mut self) -> Result<Expr, String> {
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
                other.to_user_string()
            ))),
        }
    }

    fn parse_closure(&mut self) -> Result<Expr, String> {
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

    fn parse_macro(&mut self) -> Result<Expr, String> {
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
                    other.to_user_string()
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

    fn parse_struct_lit_fields(&mut self) -> Result<Vec<(String, Expr)>, String> {
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

    fn parse_if(&mut self) -> Result<Expr, String> {
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

    fn parse_if_let(&mut self, start: SourceRange) -> Result<Expr, String> {
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

    fn parse_else(&mut self) -> Result<Option<Box<ElseBranch>>, String> {
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

    fn parse_match(&mut self) -> Result<Expr, String> {
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

    fn parse_pattern(&mut self) -> Result<Pattern, String> {
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
                    Ok(Pattern::TupleVariant { path: segs, elems })
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
                        Ok(Pattern::Path(segs))
                    }
                }
            }
        } else {
            Err(self.err(&format!(
                "expected pattern, found `{}`",
                self.cur().to_user_string()
            )))
        }
    }

    fn parse_pattern_literal(&mut self) -> Result<Expr, String> {
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
                other.to_user_string()
            ))),
        }
    }
}

fn starts_lowercase(s: &str) -> bool {
    s.chars().next().map(|c| c.is_lowercase() || c == '_').unwrap_or(false)
}

fn is_block_like(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::If { .. } | ExprKind::IfLet { .. } | ExprKind::Block(_) | ExprKind::Match { .. }
    )
}

/// Binary operator + left binding power for the current token.
fn binop(t: T) -> Option<(BinOp, u8)> {
    Some(match t {
        T::OrOr => (BinOp::Or, 1),
        T::AndAnd => (BinOp::And, 2),
        T::EqEq => (BinOp::Eq, 3),
        T::Ne => (BinOp::Ne, 3),
        T::Lt => (BinOp::Lt, 3),
        T::Le => (BinOp::Le, 3),
        T::Gt => (BinOp::Gt, 3),
        T::Ge => (BinOp::Ge, 3),
        T::Plus => (BinOp::Add, 4),
        T::Minus => (BinOp::Sub, 4),
        T::Star => (BinOp::Mul, 5),
        T::Slash => (BinOp::Div, 5),
        T::Percent => (BinOp::Rem, 5),
        _ => return None,
    })
}
