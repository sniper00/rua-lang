//! Lua 5.5 source backend.
//!
//! Expression-to-statement lowering uses a "destination" model (docs §4.1):
//! every expression is generated *to* a `Dest` (discard / assign / return).
//! Control-flow expressions push the same `Dest` into their branches; only
//! value-producing control-flow in operand position hoists a temporary.
//!
//! Data mapping (docs §4.2 / §4.3):
//!   struct        -> table + metatable (methods in `__index`)
//!   enum variant  -> { tag = "Name", ... } + metatable
//!   Option        -> pure nil: Some(v) => v, None => nil
//!   Result        -> Ok(v) => { ok = v }, Err(e) => { err = e }

use crate::ast::*;
use crate::typeck::{
    IterAdapterKind, IterConsumerKind, IterPlan, IterSourceKind, TypeInfo,
};
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
enum Dest {
    Discard,
    Var(String),
    Return,
}

#[derive(Clone, Copy)]
enum VarShape {
    Unit,
    Tuple,
    Struct,
}

struct IterCall<'a> {
    kind: IterAdapterKind,
    args: &'a [Expr],
}

struct IterChain<'a> {
    source: &'a Expr,
    adapters: Vec<IterCall<'a>>,
    consumer_args: &'a [Expr],
}

#[derive(Default)]
struct IterAdapterState {
    counter: Option<String>,
    limit: Option<String>,
}

enum IterLoopSource {
    Range {
        start: String,
        end: String,
        inclusive: bool,
    },
    Vec {
        holder: String,
    },
}

/// Type/variant information gathered before codegen so paths like
/// `Shape::Circle` and bare variants can be resolved without a type checker.
struct Ctx {
    structs: HashSet<String>,
    /// enum name -> (variant name -> shape)
    enums: HashMap<String, HashMap<String, VarShape>>,
    /// variant name -> enum name (only for variants with a unique name)
    variant_enum: HashMap<String, String>,
}

impl Ctx {
    fn collect(prog: &Program) -> Ctx {
        let mut structs = HashSet::new();
        let mut enums: HashMap<String, HashMap<String, VarShape>> = HashMap::new();
        let mut variant_enum: HashMap<String, String> = HashMap::new();
        let mut ambiguous: HashSet<String> = HashSet::new();

        // Types are keyed by simple name (root + all modules); cross-module
        // references resolve via qualified `::` paths at the emit sites.
        Self::collect_items(
            &prog.items,
            &mut structs,
            &mut enums,
            &mut variant_enum,
            &mut ambiguous,
        );
        for a in ambiguous {
            variant_enum.remove(&a);
        }
        Ctx {
            structs,
            enums,
            variant_enum,
        }
    }

    fn collect_items(
        items: &[Item],
        structs: &mut HashSet<String>,
        enums: &mut HashMap<String, HashMap<String, VarShape>>,
        variant_enum: &mut HashMap<String, String>,
        ambiguous: &mut HashSet<String>,
    ) {
        for item in items {
            match item {
                Item::Struct(s) => {
                    structs.insert(s.name.clone());
                }
                Item::Enum(e) => {
                    let mut vs = HashMap::new();
                    for v in &e.variants {
                        let shape = match &v.kind {
                            VariantKind::Unit => VarShape::Unit,
                            VariantKind::Tuple(_) => VarShape::Tuple,
                            VariantKind::Struct(_) => VarShape::Struct,
                        };
                        vs.insert(v.name.clone(), shape);
                        if variant_enum.contains_key(&v.name) {
                            ambiguous.insert(v.name.clone());
                        } else {
                            variant_enum.insert(v.name.clone(), e.name.clone());
                        }
                    }
                    enums.insert(e.name.clone(), vs);
                }
                Item::Mod(m) => {
                    Self::collect_items(&m.items, structs, enums, variant_enum, ambiguous);
                }
                _ => {}
            }
        }
    }

    /// Resolve a path to a user enum variant (not Option/Result built-ins).
    fn resolve_variant(&self, segs: &[String]) -> Option<(String, String, VarShape)> {
        if segs.len() >= 2 {
            let en = &segs[segs.len() - 2];
            let var = &segs[segs.len() - 1];
            let shape = self.enums.get(en)?.get(var)?;
            Some((en.clone(), var.clone(), *shape))
        } else if segs.len() == 1 {
            let var = &segs[0];
            let en = self.variant_enum.get(var)?;
            let shape = self.enums.get(en)?.get(var)?;
            Some((en.clone(), var.clone(), *shape))
        } else {
            None
        }
    }
}

pub fn generate(
    prog: &Program,
    info: &TypeInfo,
    rules: &crate::builtins::CodegenRules,
) -> String {
    let mut cg = Codegen {
        out: String::new(),
        indent: 0,
        tmp: 0,
        ctx: Ctx::collect(prog),
        uses_rt: false,
        closure_return_targets: Vec::new(),
        info,
        builtin_rules: rules,
    };
    cg.gen_program(prog);

    let mut result = String::from("-- Generated by ruac (Rua -> Lua 5.5). Do not edit by hand.\n");
    if cg.uses_rt {
        result.push_str("local rt = require(\"rua_rt\")\n");
    }
    result.push_str(&cg.out);
    result
}

struct Codegen<'a> {
    out: String,
    indent: usize,
    tmp: usize,
    ctx: Ctx,
    /// Set when any generated code references the `rua_rt` runtime shim.
    uses_rt: bool,
    /// Inlined iterator closure returns assign a local and jump out of the
    /// closure body instead of returning from the enclosing Rua function.
    closure_return_targets: Vec<(String, String)>,
    info: &'a TypeInfo,
    builtin_rules: &'a crate::builtins::CodegenRules,
}

// ---------------------------------------------------------------------------
// EmmyLua helpers — convert rua types to LuaLS annotations
// ---------------------------------------------------------------------------

fn type_to_emmylua(ty: &Type) -> String {
    match ty {
        Type::Path { name, args } => {
            let base = match name.as_str() {
                "i64" | "i8" | "i16" | "i32" | "isize" | "u8" | "u16" | "u32" | "u64" | "usize" => "integer",
                "f64" | "f32" => "number",
                "bool" => "boolean",
                "String" | "str" => "string",
                "Vec" => {
                    if let Some(a) = args.first() {
                        return format!("{}[]", type_to_emmylua(a));
                    }
                    "table"
                }
                "Option" => {
                    if let Some(a) = args.first() {
                        return format!("{}|nil", type_to_emmylua(a));
                    }
                    "any|nil"
                }
                "Result" => {
                    let ok = args.first().map(type_to_emmylua).unwrap_or_else(|| "any".into());
                    let err = args.get(1).map(type_to_emmylua).unwrap_or_else(|| "any".into());
                    return format!("{{ ok: {ok} }}|{{ err: {err} }}");
                }
                "HashMap" => "table",
                _ => name.as_str(),
            };
            base.to_string()
        }
        Type::Ref { inner, .. } => type_to_emmylua(inner),
        Type::Unit => "nil".into(),
    }
}

fn emit_param_annotations(out: &mut String, params: &[Param]) {
    for p in params {
        out.push_str(&format!(
            "---@param {} {}\n",
            p.name,
            type_to_emmylua(&p.ty)
        ));
    }
}

fn emit_return_annotation(out: &mut String, ret: &Option<Type>) {
    if let Some(r) = ret {
        out.push_str(&format!("---@return {}\n", type_to_emmylua(r)));
    }
}

impl Codegen<'_> {
    fn emit_struct_annotation(&mut self, s: &StructDecl) {
        self.out.push_str(&format!("---@class {}\n", s.name));
        if !s.generics.is_empty() {
            let gens: Vec<&str> = s.generics.iter().map(|g| g.name.as_str()).collect();
            self.out.push_str(&format!("---@generic {}\n", gens.join(", ")));
        }
    }

    fn emit_fn_annotation(&mut self, f: &FnDecl) {
        if !f.generics.is_empty() {
            let gens: Vec<&str> = f.generics.iter().map(|g| g.name.as_str()).collect();
            self.out.push_str(&format!("---@generic {}\n", gens.join(", ")));
        }
        emit_param_annotations(&mut self.out, &f.params);
        emit_return_annotation(&mut self.out, &f.ret);
    }

    fn block_has_continue(block: &Block) -> bool {
        for s in &block.stmts {
            if matches!(s, Stmt::Continue) { return true; }
            match s {
                Stmt::While { body, .. } | Stmt::Loop { body } | Stmt::WhileLet { body, .. } | Stmt::For { body, .. } => {
                    if Self::block_has_continue(body) { return true; }
                }
                Stmt::Expr(e)
                    if Self::expr_has_continue(e) => { return true; }
                _ => {}
            }
        }
        false
    }

    fn expr_has_continue(expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Block(b) => Self::block_has_continue(b),
            ExprKind::If { then_block, else_block, .. } => {
                Self::block_has_continue(then_block)
                    || else_block.as_ref().is_some_and(|eb| match eb.as_ref() {
                        ElseBranch::Block(b) => Self::block_has_continue(b),
                        ElseBranch::If(e) => Self::expr_has_continue(e),
                    })
            }
            ExprKind::Match { arms, .. } => arms.iter().any(|a| Self::expr_has_continue(&a.body)),
            _ => false,
        }
    }

    fn line(&mut self, s: &str) {
        for _ in 0..self.indent {
            self.out.push_str("    ");
        }
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn blank(&mut self) {
        self.out.push('\n');
    }

    fn fresh_tmp(&mut self) -> String {
        self.tmp += 1;
        format!("__t{}", self.tmp)
    }

    // --- program ----------------------------------------------------------

    fn gen_program(&mut self, prog: &Program) {
        let _struct_names: Vec<&str> = prog
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Struct(s) => Some(s.name.as_str()),
                _ => None,
            })
            .collect();
        let _enum_names: Vec<&str> = prog
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Enum(e) => Some(e.name.as_str()),
                _ => None,
            })
            .collect();
        let fn_names: Vec<&str> = prog
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Fn(f) => Some(f.name.as_str()),
                _ => None,
            })
            .collect();
        // Declaration-only (`.ruai`) modules emit nothing and are not locals:
        // references to them resolve to host-provided globals (e.g. `moon`).
        let mod_names: Vec<&str> = prog
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Mod(m) if !m.is_decl => Some(m.name.as_str()),
                _ => None,
            })
            .collect();
        // Hoist function and module names so they're file-scoped (not global).
        // Structs/enums get their own `local` at the class definition site.
        let mut all: Vec<&str> = Vec::new();
        all.extend(&fn_names);
        all.extend(&mod_names);
        if !all.is_empty() {
            self.line(&format!("local {}", all.join(", ")));
        }

        // Class tables (with `__index`) for structs and enums so their values
        // can carry methods. Each is self-contained: @class annotation + local table.
        for item in &prog.items {
            match item {
                Item::Struct(s) => {
                    self.emit_struct_annotation(s);
                    for field in &s.fields {
                        self.line(&format!(
                            "---@field {} {}",
                            field.name,
                            type_to_emmylua(&field.ty)
                        ));
                    }
                    self.line(&format!("local {0} = {{}}", s.name));
                    self.line(&format!("{0}.__index = {0}", s.name));
                }
                Item::Enum(e) => {
                    self.line(&format!("---@class {0}", e.name));
                    self.line(&format!("local {0} = {{}}", e.name));
                    self.line(&format!("{0}.__index = {0}", e.name));
                }
                _ => {}
            }
        }

        // Extern stubs: for each `extern "lua" { fn name(...) }`, emit a
        // local fallback so the generated code runs standalone without the
        // host providing these functions.
        for item in &prog.items {
            if let Item::Extern(eb) = item {
                for f in &eb.fns {
                    self.line(&format!(
                        "local {0} = {0} or function(...) end",
                        f.name
                    ));
                }
            }
        }

        // Trait table across all scopes (root + modules), keyed by simple name,
        // for resolving inherited default methods in `impl Trait for Type`.
        let mut traits: HashMap<&str, &TraitDecl> = HashMap::new();
        collect_traits(&prog.items, &mut traits);

        for item in &prog.items {
            if let Item::Fn(f) = item {
                self.gen_free_fn(f);
            }
        }
        for item in &prog.items {
            if let Item::Mod(m) = item
                && !m.is_decl {
                    self.gen_mod(m, &traits);
                }
        }
        self.gen_impls(&prog.items, &traits);

        if prog
            .items
            .iter()
            .any(|i| matches!(i, Item::Fn(f) if f.name == "main" && f.params.is_empty()))
        {
            self.blank();
            self.line("main()");
        }
    }

    /// Emit an inline module as a Lua table populated inside a `do` block, so
    /// sibling items see each other as block locals while cross-module access
    /// goes through the table (`mod::item` -> `mod.item`). Supports `fn`,
    /// `struct`/`enum`/`impl`/`trait`, and nested `mod` (co-located impls).
    fn gen_mod(&mut self, m: &ModDecl, traits: &HashMap<&str, &TraitDecl>) {
        self.blank();
        self.line(&format!("{} = {{}}", m.name));
        self.line("do");
        self.indent += 1;

        // All named items become block locals (types, fns, nested mods) so the
        // module body can refer to them without qualification.
        let locals: Vec<&str> = m
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Fn(f) => Some(f.name.as_str()),
                Item::Struct(s) => Some(s.name.as_str()),
                Item::Enum(e) => Some(e.name.as_str()),
                Item::Mod(md) if !md.is_decl => Some(md.name.as_str()),
                _ => None,
            })
            .collect();
        if !locals.is_empty() {
            self.line(&format!("local {}", locals.join(", ")));
        }

        // Class tables for module-local structs/enums.
        for item in &m.items {
            match item {
                Item::Struct(s) => {
                    self.line(&format!("{0} = {{}}; {0}.__index = {0}", s.name));
                }
                Item::Enum(e) => {
                    self.line(&format!("{0} = {{}}; {0}.__index = {0}", e.name));
                }
                _ => {}
            }
        }

        for item in &m.items {
            match item {
                Item::Fn(f) => self.gen_free_fn(f),
                Item::Mod(md) if !md.is_decl => self.gen_mod(md, traits),
                _ => {}
            }
        }
        self.gen_impls(&m.items, traits);

        // Publish members onto the module table.
        for item in &m.items {
            let name = match item {
                Item::Fn(f) => Some(f.name.as_str()),
                Item::Struct(s) => Some(s.name.as_str()),
                Item::Enum(e) => Some(e.name.as_str()),
                Item::Mod(md) if !md.is_decl => Some(md.name.as_str()),
                _ => None,
            };
            if let Some(n) = name {
                self.line(&format!("{}.{} = {}", m.name, n, n));
            }
        }

        self.indent -= 1;
        self.line("end");
    }

    /// Emit all `impl` blocks in `items` (methods, operator aliases, inherited
    /// trait defaults). The type's class table is a same-scope local, so
    /// `function Type.method(...)` binds correctly at root or inside a module.
    fn gen_impls(&mut self, items: &[Item], traits: &HashMap<&str, &TraitDecl>) {
        for item in items {
            if let Item::Impl(im) = item {
                let mut overridden: HashSet<&str> = HashSet::new();
                for m in &im.methods {
                    overridden.insert(m.name.as_str());
                    self.gen_method(&im.type_name, m);
                    if let Some(tr) = &im.trait_name
                        && let Some(meta) = op_alias(tr, &m.name) {
                            self.line(&format!("{0}.{1} = {0}.{2}", im.type_name, meta, m.name));
                        }
                }
                if let Some(tr) = &im.trait_name
                    && let Some(td) = traits.get(tr.as_str()) {
                        for tm in &td.methods {
                            if tm.default.is_some() && !overridden.contains(tm.name.as_str()) {
                                self.gen_trait_default(&im.type_name, tm);
                                if let Some(meta) = op_alias(tr, &tm.name) {
                                    self.line(&format!(
                                        "{0}.{1} = {0}.{2}",
                                        im.type_name, meta, tm.name
                                    ));
                                }
                            }
                        }
                    }
            }
        }
    }

    fn gen_free_fn(&mut self, f: &FnDecl) {
        self.blank();
        self.emit_fn_annotation(f);
        let params: Vec<&str> = f.params.iter().map(|p| p.name.as_str()).collect();
        self.line(&format!("function {}({})", f.name, params.join(", ")));
        self.indent += 1;
        self.gen_block_to(&f.body, &Dest::Return);
        self.indent -= 1;
        self.line("end");
    }

    fn gen_method(&mut self, type_name: &str, m: &FnDecl) {
        self.blank();
        // Emit param/return annotations (skip `self` param if present).
        let skip_self = if m.has_self { 1 } else { 0 };
        for p in m.params.iter().skip(skip_self) {
            self.line(&format!(
                "---@param {} {}",
                p.name,
                type_to_emmylua(&p.ty)
            ));
        }
        if let Some(ret) = &m.ret {
            self.line(&format!("---@return {}", type_to_emmylua(ret)));
        }
        let params: Vec<&str> = m.params.iter().map(|p| p.name.as_str()).collect();
        // `:` gives an implicit `self`; `.` for associated functions.
        let sep = if m.has_self { ":" } else { "." };
        self.line(&format!(
            "function {}{}{}({})",
            type_name,
            sep,
            m.name,
            params.join(", ")
        ));
        self.indent += 1;
        self.gen_block_to(&m.body, &Dest::Return);
        self.indent -= 1;
        self.line("end");
    }

    fn gen_trait_default(&mut self, type_name: &str, tm: &TraitMethod) {
        self.blank();
        // Emit param/return annotations
        let skip_self = if tm.has_self { 1 } else { 0 };
        for p in tm.params.iter().skip(skip_self) {
            self.line(&format!(
                "---@param {} {}",
                p.name,
                type_to_emmylua(&p.ty)
            ));
        }
        if let Some(ret) = &tm.ret {
            self.line(&format!("---@return {}", type_to_emmylua(ret)));
        }
        let params: Vec<&str> = tm.params.iter().map(|p| p.name.as_str()).collect();
        let sep = if tm.has_self { ":" } else { "." };
        self.line(&format!(
            "function {}{}{}({})",
            type_name,
            sep,
            tm.name,
            params.join(", ")
        ));
        self.indent += 1;
        // `default` is guaranteed Some by the caller.
        if let Some(body) = &tm.default {
            self.gen_block_to(body, &Dest::Return);
        }
        self.indent -= 1;
        self.line("end");
    }

    // --- statements --------------------------------------------------------

    fn gen_stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let { name, init, ty, .. } => {
                // EmmyLua type annotation for explicitly typed bindings
                if let Some(ty_ann) = ty {
                    self.line(&format!("---@type {}", type_to_emmylua(ty_ann)));
                }
                if let ExprKind::Closure { params, body, .. } = &init.kind {
                    self.gen_closure_local(name, params, body);
                } else if self
                    .info
                    .iter_plan(init.span.file, init.span.start, init.span.len)
                    .is_some()
                    || needs_hoist(init)
                {
                    self.line(&format!("local {}", name));
                    self.gen_expr_to(init, &Dest::Var(name.clone()));
                } else {
                    let v = self.gen_inline(init);
                    self.line(&format!("local {} = {}", name, v));
                }
            }
            Stmt::Expr(e) => self.gen_expr_to(e, &Dest::Discard),
            Stmt::Return(opt) => {
                if let Some((target, label)) = self.closure_return_targets.last().cloned() {
                    match opt {
                        Some(e) => self.gen_expr_to(e, &Dest::Var(target)),
                        None => self.line(&format!("{} = nil", target)),
                    }
                    self.line(&format!("goto {}", label));
                    return;
                }
                self.line("do");
                self.indent += 1;
                match opt {
                    Some(e) => self.gen_expr_to(e, &Dest::Return),
                    None => self.line("return"),
                }
                self.indent -= 1;
                self.line("end");
            }
            Stmt::While { cond, body } => {
                let c = self.gen_inline(cond);
                self.line(&format!("while {} do", c));
                self.indent += 1;
                self.gen_block_to(body, &Dest::Discard);
                if Self::block_has_continue(body) { self.line("::continue::"); }
                self.indent -= 1;
                self.line("end");
            }
            Stmt::Loop { body } => {
                self.line("while true do");
                self.indent += 1;
                self.gen_block_to(body, &Dest::Discard);
                if Self::block_has_continue(body) { self.line("::continue::"); }
                self.indent -= 1;
                self.line("end");
            }
            Stmt::For { var, iter, body, .. } => self.gen_for(var, iter, body),
            Stmt::WhileLet { pat, expr, body } => {
                self.line("while true do");
                self.indent += 1;
                let m = self.fresh_tmp();
                let s = self.gen_inline(expr);
                self.line(&format!("local {} = {}", m, s));
                let mut tests = Vec::new();
                let mut binds = Vec::new();
                self.pat_test(pat, &m, &mut tests, &mut binds);
                let cond = if tests.is_empty() {
                    "true".to_string()
                } else {
                    tests.join(" and ")
                };
                self.line(&format!("if {} then", cond));
                self.indent += 1;
                for (name, subj) in &binds {
                    self.line(&format!("local {} = {}", name, subj));
                }
                self.gen_block_to(body, &Dest::Discard);
                self.indent -= 1;
                self.line("else");
                self.indent += 1;
                self.line("break");
                self.indent -= 1;
                self.line("end");
                if Self::block_has_continue(body) { self.line("::continue::"); }
                self.indent -= 1;
                self.line("end");
            }
            Stmt::Break => self.line("break"),
            Stmt::Continue => self.line("goto continue"),
        }
    }

    fn gen_for(&mut self, var: &str, iter: &Expr, body: &Block) {
        if let Some(plan) = self
            .info
            .iter_plan(iter.span.file, iter.span.start, iter.span.len)
            && (!plan.adapters.is_empty()
                || matches!(
                    plan.source.kind,
                    IterSourceKind::VecIter | IterSourceKind::VecIntoIter
                ))
            && let Some(chain) = extract_iter_chain(iter, plan, false)
        {
            self.gen_iter_loop(&chain, plan, Some((var, body)), &Dest::Discard);
            return;
        }
        if let ExprKind::Range {
            start,
            end,
            inclusive,
        } = &iter.kind
        {
            let s = self.gen_inline(start);
            let e = self.gen_inline(end);
            let stop = if *inclusive {
                e
            } else if let ExprKind::Int(n) = &end.kind {
                // Compile-time constant: `0..5` → `0, 4`
                n.parse::<i64>().ok().map(|v| (v - 1).to_string()).unwrap_or_else(|| format!("{} - 1", e))
            } else {
                format!("{} - 1", e)
            };
            self.line(&format!("for {} = {}, {} do", var, s, stop));
            self.indent += 1;
            self.gen_block_to(body, &Dest::Discard);
            if Self::block_has_continue(body) { self.line("::continue::"); }
            self.indent -= 1;
            self.line("end");
        } else {
            // General iterable: a Vec `{ [0..n-1], n = len }`.
            let it = self.gen_inline(iter);
            let holder = self.fresh_tmp();
            self.line(&format!("local {} = {}", holder, it));
            let idx = self.fresh_tmp();
            self.line(&format!("for {} = 0, {}.n - 1 do", idx, holder));
            self.indent += 1;
            self.line(&format!("local {} = {}[{}]", var, holder, idx));
            self.gen_block_to(body, &Dest::Discard);
            if Self::block_has_continue(body) { self.line("::continue::"); }
            self.indent -= 1;
            self.line("end");
        }
    }

    fn gen_closure_local(&mut self, name: &str, params: &[ClosureParam], body: &ClosureBody) {
        let params = params
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        self.line(&format!("local function {name}({params})"));
        self.indent += 1;
        match body {
            ClosureBody::Expr(expr) => self.gen_expr_to(expr, &Dest::Return),
            ClosureBody::Block(block) => self.gen_block_to(block, &Dest::Return),
        }
        self.indent -= 1;
        self.line("end");
    }

    fn gen_closure_value(&mut self, params: &[ClosureParam], body: &ClosureBody) -> String {
        let local = self.fresh_tmp();
        self.gen_closure_local(&local, params, body);
        local
    }

    fn gen_inlined_closure(&mut self, closure: &Expr, inputs: &[String]) -> String {
        let ExprKind::Closure { params, body, .. } = &closure.kind else {
            return "nil".to_string();
        };
        let result = self.fresh_tmp();
        let done = matches!(body, ClosureBody::Block(_))
            .then(|| format!("{}_done", self.fresh_tmp()));
        self.line(&format!("local {}", result));
        self.line("do");
        self.indent += 1;
        for (param, input) in params.iter().zip(inputs) {
            self.line(&format!("local {} = {}", param.name, input));
        }
        match body {
            ClosureBody::Expr(expr) => self.gen_expr_to(expr, &Dest::Var(result.clone())),
            ClosureBody::Block(block) => {
                let done = done.as_ref().unwrap();
                self.closure_return_targets
                    .push((result.clone(), done.clone()));
                self.gen_block_to(block, &Dest::Var(result.clone()));
                self.closure_return_targets.pop();
            }
        }
        self.indent -= 1;
        self.line("end");
        if let Some(done) = done {
            self.line(&format!("::{}::", done));
        }
        result
    }

    fn gen_iter_loop(
        &mut self,
        chain: &IterChain<'_>,
        plan: &IterPlan,
        for_body: Option<(&str, &Block)>,
        dest: &Dest,
    ) {
        let source = match plan.source.kind {
            IterSourceKind::ExclusiveRange | IterSourceKind::InclusiveRange => {
                let ExprKind::Range { start, end, .. } = &chain.source.kind else {
                    return;
                };
                let start_value = self.gen_inline(start);
                let start_local = self.fresh_tmp();
                self.line(&format!("local {} = {}", start_local, start_value));
                let end_value = self.gen_inline(end);
                let end_local = self.fresh_tmp();
                self.line(&format!("local {} = {}", end_local, end_value));
                IterLoopSource::Range {
                    start: start_local,
                    end: end_local,
                    inclusive: plan.source.kind == IterSourceKind::InclusiveRange,
                }
            }
            IterSourceKind::Vec
            | IterSourceKind::VecIter
            | IterSourceKind::VecIntoIter => {
                let value = self.gen_inline(chain.source);
                let holder = self.fresh_tmp();
                self.line(&format!("local {} = {}", holder, value));
                IterLoopSource::Vec { holder }
            }
        };

        let mut states = Vec::with_capacity(chain.adapters.len());
        for adapter in &chain.adapters {
            let mut state = IterAdapterState::default();
            match adapter.kind {
                IterAdapterKind::Enumerate => {
                    let counter = self.fresh_tmp();
                    self.line(&format!("local {} = 0", counter));
                    state.counter = Some(counter);
                }
                IterAdapterKind::Skip | IterAdapterKind::Take => {
                    let limit_value = adapter
                        .args
                        .first()
                        .map(|arg| self.gen_inline(arg))
                        .unwrap_or_else(|| "0".to_string());
                    let limit = self.fresh_tmp();
                    self.line(&format!("local {} = {}", limit, limit_value));
                    let counter = self.fresh_tmp();
                    self.line(&format!("local {} = 0", counter));
                    state.limit = Some(limit);
                    state.counter = Some(counter);
                }
                _ => {}
            }
            states.push(state);
        }

        let result = match plan.consumer {
            IterConsumerKind::For => None,
            IterConsumerKind::CollectVec => {
                self.uses_rt = true;
                let result = self.fresh_tmp();
                self.line(&format!("local {} = rt.vec({{ n = 0 }})", result));
                Some(result)
            }
            IterConsumerKind::Fold => {
                let init = chain
                    .consumer_args
                    .first()
                    .map(|arg| self.gen_inline(arg))
                    .unwrap_or_else(|| "nil".to_string());
                let result = self.fresh_tmp();
                self.line(&format!("local {} = {}", result, init));
                Some(result)
            }
            IterConsumerKind::Count => {
                let result = self.fresh_tmp();
                self.line(&format!("local {} = 0", result));
                Some(result)
            }
            IterConsumerKind::Any | IterConsumerKind::All => {
                let result = self.fresh_tmp();
                let initial = if plan.consumer == IterConsumerKind::All {
                    "true"
                } else {
                    "false"
                };
                self.line(&format!("local {} = {}", result, initial));
                Some(result)
            }
            IterConsumerKind::Find => {
                let result = self.fresh_tmp();
                self.line(&format!("local {} = nil", result));
                Some(result)
            }
        };

        let item = self.fresh_tmp();
        match &source {
            IterLoopSource::Range {
                start,
                end,
                inclusive,
            } => {
                let stop = if *inclusive {
                    end.clone()
                } else {
                    format!("({}) - 1", end)
                };
                let index = self.fresh_tmp();
                self.line(&format!("for {} = {}, {} do", index, start, stop));
                self.indent += 1;
                self.line(&format!("local {} = {}", item, index));
                self.indent -= 1;
            }
            IterLoopSource::Vec { holder } => {
                let index = self.fresh_tmp();
                self.line(&format!("for {} = 0, {}.n - 1 do", index, holder));
                self.indent += 1;
                self.line(&format!("local {} = {}[{}]", item, holder, index));
                self.indent -= 1;
            }
        }
        self.indent += 1;
        let active = self.fresh_tmp();
        self.line(&format!("local {} = true", active));

        for (adapter, state) in chain.adapters.iter().zip(&states) {
            self.line(&format!("if {} then", active));
            self.indent += 1;
            match adapter.kind {
                IterAdapterKind::Map => {
                    if let Some(closure) = adapter.args.first() {
                        let mapped = self.gen_inlined_closure(closure, std::slice::from_ref(&item));
                        self.line(&format!("{} = {}", item, mapped));
                    }
                }
                IterAdapterKind::Filter => {
                    if let Some(closure) = adapter.args.first() {
                        let keep = self.gen_inlined_closure(closure, std::slice::from_ref(&item));
                        self.line(&format!("if not {} then {} = false end", keep, active));
                    }
                }
                IterAdapterKind::FilterMap => {
                    if let Some(closure) = adapter.args.first() {
                        let mapped = self.gen_inlined_closure(closure, std::slice::from_ref(&item));
                        self.line(&format!("if {} == nil then", mapped));
                        self.indent += 1;
                        self.line(&format!("{} = false", active));
                        self.indent -= 1;
                        self.line("else");
                        self.indent += 1;
                        self.line(&format!("{} = {}", item, mapped));
                        self.indent -= 1;
                        self.line("end");
                    }
                }
                IterAdapterKind::Enumerate => {
                    let counter = state.counter.as_deref().unwrap_or("0");
                    self.line(&format!(
                        "{} = {{ [0] = {}, [1] = {}, n = 2 }}",
                        item, counter, item
                    ));
                    self.line(&format!("{} = {} + 1", counter, counter));
                }
                IterAdapterKind::Skip => {
                    let counter = state.counter.as_deref().unwrap_or("0");
                    let limit = state.limit.as_deref().unwrap_or("0");
                    self.line(&format!("if {} < {} then", counter, limit));
                    self.indent += 1;
                    self.line(&format!("{} = {} + 1", counter, counter));
                    self.line(&format!("{} = false", active));
                    self.indent -= 1;
                    self.line("end");
                }
                IterAdapterKind::Take => {
                    let counter = state.counter.as_deref().unwrap_or("0");
                    let limit = state.limit.as_deref().unwrap_or("0");
                    self.line(&format!("if {} >= {} then break end", counter, limit));
                    self.line(&format!("{} = {} + 1", counter, counter));
                }
            }
            self.indent -= 1;
            self.line("end");
        }

        self.line(&format!("if {} then", active));
        self.indent += 1;
        match plan.consumer {
            IterConsumerKind::For => {
                if let Some((var, body)) = for_body {
                    self.line(&format!("local {} = {}", var, item));
                    self.gen_block_to(body, &Dest::Discard);
                }
            }
            IterConsumerKind::CollectVec => {
                let result = result.as_deref().unwrap();
                self.line(&format!("{}[{}.n] = {}", result, result, item));
                self.line(&format!("{}.n = {}.n + 1", result, result));
            }
            IterConsumerKind::Fold => {
                if let (Some(result), Some(closure)) =
                    (result.as_deref(), chain.consumer_args.get(1))
                {
                    let inputs = [result.to_string(), item.clone()];
                    let next = self.gen_inlined_closure(closure, &inputs);
                    self.line(&format!("{} = {}", result, next));
                }
            }
            IterConsumerKind::Count => {
                let result = result.as_deref().unwrap();
                self.line(&format!("{} = {} + 1", result, result));
            }
            IterConsumerKind::Any | IterConsumerKind::All | IterConsumerKind::Find => {
                if let (Some(result), Some(predicate)) =
                    (result.as_deref(), chain.consumer_args.first())
                {
                    let matches =
                        self.gen_inlined_closure(predicate, std::slice::from_ref(&item));
                    match plan.consumer {
                        IterConsumerKind::Any => {
                            self.line(&format!(
                                "if {} then {} = true; break end",
                                matches, result
                            ));
                        }
                        IterConsumerKind::All => {
                            self.line(&format!(
                                "if not {} then {} = false; break end",
                                matches, result
                            ));
                        }
                        IterConsumerKind::Find => {
                            self.line(&format!(
                                "if {} then {} = {}; break end",
                                matches, result, item
                            ));
                        }
                        _ => {}
                    }
                }
            }
        }
        self.indent -= 1;
        self.line("end");

        if plan.consumer == IterConsumerKind::For
            && let Some((_, body)) = for_body
                && Self::block_has_continue(body) { self.line("::continue::"); }
        for (adapter, state) in chain.adapters.iter().zip(&states) {
            if adapter.kind == IterAdapterKind::Take {
                let counter = state.counter.as_deref().unwrap_or("0");
                let limit = state.limit.as_deref().unwrap_or("0");
                self.line(&format!("if {} >= {} then break end", counter, limit));
            }
        }
        self.indent -= 1;
        self.line("end");

        if let Some(result) = result {
            match dest {
                Dest::Var(target) => self.line(&format!("{} = {}", target, result)),
                Dest::Return => self.line(&format!("return {}", result)),
                Dest::Discard => {}
            }
        }
    }

    // --- expression to destination ----------------------------------------

    fn gen_block_to(&mut self, block: &Block, dest: &Dest) {
        for s in &block.stmts {
            self.gen_stmt(s);
        }
        match &block.tail {
            Some(e) => self.gen_expr_to(e, dest),
            None => {
                if let Dest::Var(d) = dest {
                    self.line(&format!("{} = nil", d));
                }
            }
        }
    }

    fn gen_expr_to(&mut self, e: &Expr, dest: &Dest) {
        if let Some(plan) = self
            .info
            .iter_plan(e.span.file, e.span.start, e.span.len)
            && let Some(chain) = extract_iter_chain(e, plan, true)
        {
            self.gen_iter_loop(&chain, plan, None, dest);
            return;
        }
        match &e.kind {
            ExprKind::If {
                cond,
                then_block,
                else_block,
            } => {
                let c = self.gen_inline(cond);
                self.line(&format!("if {} then", c));
                self.indent += 1;
                self.gen_block_to(then_block, dest);
                self.indent -= 1;
                match else_block.as_deref() {
                    Some(ElseBranch::Block(b)) => {
                        self.line("else");
                        self.indent += 1;
                        self.gen_block_to(b, dest);
                        self.indent -= 1;
                    }
                    Some(ElseBranch::If(inner)) => {
                        self.line("else");
                        self.indent += 1;
                        self.gen_expr_to(inner, dest);
                        self.indent -= 1;
                    }
                    None => {
                        if let Dest::Var(d) = dest {
                            self.line("else");
                            self.indent += 1;
                            self.line(&format!("{} = nil", d));
                            self.indent -= 1;
                        }
                    }
                }
                self.line("end");
            }
            ExprKind::IfLet {
                pat,
                expr,
                then_block,
                else_block,
            } => {
                let m = self.fresh_tmp();
                let s = self.gen_inline(expr);
                self.line(&format!("local {} = {}", m, s));
                let mut tests = Vec::new();
                let mut binds = Vec::new();
                self.pat_test(pat, &m, &mut tests, &mut binds);
                let cond = if tests.is_empty() {
                    "true".to_string()
                } else {
                    tests.join(" and ")
                };
                self.line(&format!("if {} then", cond));
                self.indent += 1;
                for (name, subj) in &binds {
                    self.line(&format!("local {} = {}", name, subj));
                }
                self.gen_block_to(then_block, dest);
                self.indent -= 1;
                match else_block.as_deref() {
                    Some(ElseBranch::Block(b)) => {
                        self.line("else");
                        self.indent += 1;
                        self.gen_block_to(b, dest);
                        self.indent -= 1;
                    }
                    Some(ElseBranch::If(inner)) => {
                        self.line("else");
                        self.indent += 1;
                        self.gen_expr_to(inner, dest);
                        self.indent -= 1;
                    }
                    None => {
                        if let Dest::Var(d) = dest {
                            self.line("else");
                            self.indent += 1;
                            self.line(&format!("{} = nil", d));
                            self.indent -= 1;
                        }
                    }
                }
                self.line("end");
            }
            ExprKind::Block(b) => self.gen_block_to(b, dest),
            ExprKind::Match { scrut, arms } => self.gen_match(scrut, arms, dest),
            ExprKind::Assign { target, value } => {
                let t = self.gen_inline(target);
                self.gen_expr_to(value, &Dest::Var(t));
                match dest {
                    Dest::Var(d) => self.line(&format!("{} = nil", d)),
                    Dest::Return => self.line("return nil"),
                    Dest::Discard => {}
                }
            }
            _ => {
                let v = self.gen_inline(e);
                match dest {
                    Dest::Discard => {
                        if matches!(
                            e.kind,
                            ExprKind::Call { .. } | ExprKind::MethodCall { .. } | ExprKind::MacroCall { .. }
                        ) {
                            self.line(&v);
                        }
                    }
                    Dest::Var(d) => self.line(&format!("{} = {}", d, v)),
                    Dest::Return => self.line(&format!("return {}", v)),
                }
            }
        }
    }

    // --- match -------------------------------------------------------------

    fn gen_match(&mut self, scrut: &Expr, arms: &[MatchArm], dest: &Dest) {
        let m = self.fresh_tmp();
        let s = self.gen_inline(scrut);
        self.line(&format!("local {} = {}", m, s));

        let mut last_is_wildcard = false;
        for (i, arm) in arms.iter().enumerate() {
            let (tests, binds) = self.arm_tests(arm, &m);
            let prefix = if i == 0 { "if " } else { "elseif " };
            let has_test = !tests.is_empty();

            if i == arms.len() - 1 && !has_test {
                last_is_wildcard = true;
                self.line("else");
            } else if has_test {
                self.line(&format!("{}{} then", prefix, tests.join(" and ")));
            } else {
                self.line(&format!("{}true then", prefix));
            }
            self.indent += 1;

            for (name, subj) in &binds {
                self.line(&format!("local {} = {}", name, subj));
            }
            let guard = arm.guard.as_ref().map(|g| self.gen_inline(g));
            if let Some(g) = &guard {
                self.line(&format!("if {} then", g));
                self.indent += 1;
            }

            self.gen_expr_to(&arm.body, dest);

            if guard.is_some() {
                self.indent -= 1;
                self.line("end");
            }
            self.indent -= 1;
        }

        if !last_is_wildcard {
            self.line("else");
            self.indent += 1;
            self.line("error(\"non-exhaustive match\")");
            self.indent -= 1;
        }
        self.line("end");
    }

    /// Structural tests + bindings for a match arm against subject variable `m`.
    fn arm_tests(&mut self, arm: &MatchArm, m: &str) -> (Vec<String>, Vec<(String, String)>) {
        if arm.pats.len() == 1 {
            let mut tests = Vec::new();
            let mut binds = Vec::new();
            self.pat_test(&arm.pats[0], m, &mut tests, &mut binds);
            (tests, binds)
        } else {
            // or-patterns: combine alternatives; bindings are not supported here.
            let mut alts = Vec::new();
            for p in &arm.pats {
                let mut tests = Vec::new();
                let mut binds = Vec::new();
                self.pat_test(p, m, &mut tests, &mut binds);
                let cond = if tests.is_empty() {
                    "true".to_string()
                } else {
                    format!("({})", tests.join(" and "))
                };
                alts.push(cond);
            }
            (vec![alts.join(" or ")], Vec::new())
        }
    }

    fn pat_test(
        &mut self,
        pat: &Pattern,
        subject: &str,
        tests: &mut Vec<String>,
        binds: &mut Vec<(String, String)>,
    ) {
        match pat {
            Pattern::Wildcard => {}
            Pattern::Binding(name, _) => binds.push((name.clone(), subject.to_string())),
            Pattern::Literal(lit) => {
                let v = self.gen_inline(lit);
                tests.push(format!("{} == {}", subject, v));
            }
            Pattern::Range {
                lo,
                hi,
                inclusive,
            } => {
                let l = self.gen_inline(lo);
                let h = self.gen_inline(hi);
                let op = if *inclusive { "<=" } else { "<" };
                tests.push(format!("({0} >= {1} and {0} {2} {3})", subject, l, op, h));
            }
            Pattern::Path(segs) => {
                if segs.len() == 1 && segs[0] == "None" {
                    tests.push(format!("{} == nil", subject));
                } else if let Some((_, var, _)) = self.ctx.resolve_variant(segs) {
                    tests.push(format!("{}.tag == \"{}\"", subject, var));
                } else {
                    // best-effort: treat last segment as a tag
                    tests.push(format!("{}.tag == \"{}\"", subject, segs.last().unwrap()));
                }
            }
            Pattern::TupleVariant { path, elems } => {
                let head = path.last().map(String::as_str).unwrap_or("");
                match head {
                    "Some" => {
                        tests.push(format!("{} ~= nil", subject));
                        if let Some(inner) = elems.first() {
                            self.pat_test(inner, subject, tests, binds);
                        }
                    }
                    "Ok" => {
                        tests.push(format!("({0} ~= nil and {0}.err == nil)", subject));
                        if let Some(inner) = elems.first() {
                            let sub = format!("{}.ok", subject);
                            self.pat_test(inner, &sub, tests, binds);
                        }
                    }
                    "Err" => {
                        tests.push(format!("({0} ~= nil and {0}.err ~= nil)", subject));
                        if let Some(inner) = elems.first() {
                            let sub = format!("{}.err", subject);
                            self.pat_test(inner, &sub, tests, binds);
                        }
                    }
                    _ => {
                        if let Some((_, var, _)) = self.ctx.resolve_variant(path) {
                            tests.push(format!("{}.tag == \"{}\"", subject, var));
                        } else {
                            tests.push(format!("{}.tag == \"{}\"", subject, head));
                        }
                        for (i, elem) in elems.iter().enumerate() {
                            let sub = format!("{}[{}]", subject, i + 1);
                            self.pat_test(elem, &sub, tests, binds);
                        }
                    }
                }
            }
            Pattern::StructVariant { path, fields, .. } => {
                // If it resolves to an enum variant, test the tag; a plain struct
                // pattern needs no tag test.
                if let Some((_, var, _)) = self.ctx.resolve_variant(path) {
                    tests.push(format!("{}.tag == \"{}\"", subject, var));
                }
                for (fname, fpat) in fields {
                    let sub = format!("{}.{}", subject, fname);
                    self.pat_test(fpat, &sub, tests, binds);
                }
            }
        }
    }

    // --- inline (pure Lua expression, may hoist) --------------------------

    fn gen_inline(&mut self, e: &Expr) -> String {
        if let Some(plan) = self
            .info
            .iter_plan(e.span.file, e.span.start, e.span.len)
            && let Some(chain) = extract_iter_chain(e, plan, true)
        {
            let tmp = self.fresh_tmp();
            self.line(&format!("local {}", tmp));
            self.gen_iter_loop(&chain, plan, None, &Dest::Var(tmp.clone()));
            return tmp;
        }
        match &e.kind {
            ExprKind::Int(s) => lua_int_literal(s),
            ExprKind::Float(s) => s.replace('_', ""),
            ExprKind::Str(s) => s.clone(),
            ExprKind::Bool(b) => if *b { "true" } else { "false" }.to_string(),
            ExprKind::Closure { params, body, .. } => self.gen_closure_value(params, body),
            ExprKind::Path(segs) => self.gen_path(segs),
            ExprKind::Unary { op, expr } => {
                let inner = self.gen_inline(expr);
                match op {
                    UnOp::Neg => format!("-{}", paren_if_needed(&inner)),
                    UnOp::Not => format!("not {}", paren_if_needed(&inner)),
                }
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let l = self.gen_inline(lhs);
                let r = self.gen_inline(rhs);
                // `i64 / i64` and `i64 % i64` lower to `rt.idiv`/`rt.irem`, which
                // truncate toward zero to match Rust (Lua `//`/`%` floor, differing
                // when exactly one operand is negative: Rust `-7/2 == -3`,
                // `-7%2 == -1` vs Lua `-7//2 == -4`, `-7%2 == 1`).
                if *op == BinOp::Div && self.info.is_int_div(e.span.start, e.span.len) {
                    self.uses_rt = true;
                    return format!("rt.idiv({}, {})", l, r);
                }
                if *op == BinOp::Rem && self.info.is_int_rem(e.span.start, e.span.len) {
                    self.uses_rt = true;
                    return format!("rt.irem({}, {})", l, r);
                }
                // `String + String` is Lua concatenation, not arithmetic.
                if *op == BinOp::Add && self.info.is_str_concat(e.span.start, e.span.len) {
                    return format!("({} .. {})", l, r);
                }
                format!("{} {} {}", l, binop_lua(*op), r)
            }
            ExprKind::Call { callee, args } => self.gen_call(callee, args),
            ExprKind::MethodCall { recv, method, args, .. } => {
                let r = self.gen_inline(recv);
                let a: Vec<String> = args.iter().map(|x| self.gen_inline(x)).collect();
                // Recognized `String` methods route through `rt.str` (bracket
                // form avoids clashing with Lua keywords like `repeat`); the
                // receiver is passed as the first argument.
                if self.info.is_str_method(e.span.start, e.span.len) {
                    self.uses_rt = true;
                    let mut all = vec![r];
                    all.extend(a);
                    return format!("rt.str[\"{}\"]({})", method, all.join(", "));
                }
                // Option::map(f) — Some(v) compiles to v, None to nil.
                // Inline the closure application directly on the raw value.
                if self.info.is_option_map(e.span.start, e.span.len) && args.len() == 1 {
                    let val = self.fresh_tmp();
                    self.line(&format!("local {} = {}", val, r));
                    self.line(&format!("if {} ~= nil then", val));
                    self.indent += 1;
                    // Identity closure `|v| v` → just return the value
                    if let ExprKind::Closure { params, body, .. } = &args[0].kind {
                        if params.len() == 1
                            && let ClosureBody::Expr(inner) = body
                                && let ExprKind::Path(segs) = &inner.kind
                                    && segs.len() == 1 && segs[0] == params[0].name
                                {
                                    // Identity: |v| v → value unchanged
                                    self.indent -= 1;
                                    self.line("end");
                                    return val;
                                }
                        let applied = self.gen_inlined_closure(&args[0], std::slice::from_ref(val));
                        self.line(&format!("{} = {}", val, applied));
                    }
                    self.indent -= 1;
                    self.line("end");
                    return val;
                }
                format!("{}:{}({})", r, method, a.join(", "))
            }
            ExprKind::Field { base, name, .. } => {
                let b = self.gen_inline(base);
                format!("{}.{}", b, name)
            }
            ExprKind::Index { base, index } => {
                let b = self.gen_inline(base);
                let i = self.gen_inline(index);
                format!("{}[{}]", b, i)
            }
            ExprKind::MacroCall { name, args } => self.gen_macro(name, args),
            ExprKind::Range { start, end, .. } => {
                // Ranges are only meaningful as a `for` iterator; elsewhere they
                // have no first-class value.
                let _ = (start, end);
                "nil --[[ range only valid as a for-iterator ]]".to_string()
            }
            ExprKind::StructLit { path, fields } => self.gen_struct_lit(path, fields),
            ExprKind::Try { expr } => {
                let inner = self.gen_inline(expr);
                let r = self.fresh_tmp();
                self.line(&format!("local {} = {}", r, inner));
                // Propagate Err (docs §4.8). Option `?` needs the type checker.
                self.line(&format!("if {0} ~= nil and {0}.err ~= nil then return {0} end", r));
                format!("{}.ok", r)
            }
            // Control-flow in operand position: hoist into a temp.
            ExprKind::If { .. }
            | ExprKind::IfLet { .. }
            | ExprKind::Block(_)
            | ExprKind::Match { .. }
            | ExprKind::Assign { .. } => {
                let tmp = self.fresh_tmp();
                self.line(&format!("local {}", tmp));
                self.gen_expr_to(e, &Dest::Var(tmp.clone()));
                tmp
            }
        }
    }

    fn gen_path(&mut self, segs: &[String]) -> String {
        // Check codegen rules first (e.g. `None` → `nil`).
        let key = segs.join("::");
        if let Some(rule) = self.builtin_rules.get(&key)
            && let crate::builtins::CodegenRule::Literal(lua) = rule {
                return (*lua).to_string();
            }
        if let Some((en, var, shape)) = self.ctx.resolve_variant(segs)
            && let VarShape::Unit = shape {
                let meta = enum_ref(segs, &en);
                return format!("setmetatable({{ tag = \"{}\" }}, {})", var, meta);
            }
        segs.join(".")
    }

    fn gen_call(&mut self, callee: &Expr, args: &[Expr]) -> String {
        if let ExprKind::Path(segs) = &callee.kind {
            // Check codegen rules for builtin constructors.
            let key = segs.join("::");
            if let Some(rule) = self.builtin_rules.get(&key) {
                use crate::builtins::CodegenRule::*;
                match rule {
                    InlineArg if !args.is_empty() => return self.gen_inline(&args[0]),
                    TableCtor { tag: Some("ok") } if !args.is_empty() => {
                        let v = self.gen_inline(&args[0]);
                        return format!("{{ ok = {} }}", v);
                    }
                    TableCtor { tag: Some("err") } if !args.is_empty() => {
                        let v = self.gen_inline(&args[0]);
                        return format!("{{ err = {} }}", v);
                    }
                    RtCall(lua) => {
                        self.uses_rt = true;
                        return (*lua).to_string();
                    }
                    _ => {}
                }
            }
            // Enum tuple-variant construction.
            if let Some((en, var, VarShape::Tuple)) = self.ctx.resolve_variant(segs) {
                let a: Vec<String> = args.iter().map(|x| self.gen_inline(x)).collect();
                let meta = enum_ref(segs, &en);
                let mut parts = vec![format!("tag = \"{}\"", var)];
                parts.extend(a);
                return format!("setmetatable({{ {} }}, {})", parts.join(", "), meta);
            }
            // Associated function `Type::func(..)` or plain call.
            let a: Vec<String> = args.iter().map(|x| self.gen_inline(x)).collect();
            return format!("{}({})", segs.join("."), a.join(", "));
        }
        let c = self.gen_inline(callee);
        let a: Vec<String> = args.iter().map(|x| self.gen_inline(x)).collect();
        format!("{}({})", c, a.join(", "))
    }

    fn gen_macro(&mut self, name: &str, args: &[Expr]) -> String {
        let a: Vec<String> = args.iter().map(|x| self.gen_inline(x)).collect();
        match name {
            "vec" => {
                self.uses_rt = true;
                // 0-based storage + length field, matching Rust indexing.
                let mut parts: Vec<String> = a
                    .iter()
                    .enumerate()
                    .map(|(i, v)| format!("[{}] = {}", i, v))
                    .collect();
                parts.push(format!("n = {}", a.len()));
                format!("rt.vec({{ {} }})", parts.join(", "))
            }
            "format" => {
                self.uses_rt = true;
                format!("rt.format({})", a.join(", "))
            }
            "println" => {
                self.uses_rt = true;
                format!("rt.println({})", a.join(", "))
            }
            "print" => {
                self.uses_rt = true;
                format!("rt.print({})", a.join(", "))
            }
            "panic" => {
                self.uses_rt = true;
                format!("rt.panic(rt.format({}))", a.join(", "))
            }
            other => {
                // Unknown macro: best-effort passthrough as a call.
                format!("{}({})", other, a.join(", "))
            }
        }
    }

    fn gen_struct_lit(&mut self, path: &[String], fields: &[(String, Expr)]) -> String {
        let field_parts: Vec<String> = fields
            .iter()
            .map(|(n, e)| {
                let v = self.gen_inline(e);
                format!("{} = {}", n, v)
            })
            .collect();

        // Struct variant of an enum?
        if let Some((en, var, VarShape::Struct)) = self.ctx.resolve_variant(path) {
            let meta = enum_ref(path, &en);
            let mut parts = vec![format!("tag = \"{}\"", var)];
            parts.extend(field_parts);
            return format!("setmetatable({{ {} }}, {})", parts.join(", "), meta);
        }

        let name = path.last().cloned().unwrap_or_default();
        if self.ctx.structs.contains(&name) {
            // Cross-module `mod::Type { .. }` uses the qualified class table.
            let meta = if path.len() > 1 {
                path.join(".")
            } else {
                name.clone()
            };
            format!("setmetatable({{ {} }}, {})", field_parts.join(", "), meta)
        } else {
            format!("{{ {} }}", field_parts.join(", "))
        }
    }
}

fn extract_iter_chain<'a>(expr: &'a Expr, plan: &IterPlan, has_consumer: bool) -> Option<IterChain<'a>> {
    let (mut cursor, consumer_args): (&Expr, &[Expr]) = if has_consumer {
        let ExprKind::MethodCall { recv, args, .. } = &expr.kind else {
            return None;
        };
        (recv, args)
    } else {
        (expr, &[])
    };

    let mut adapters = Vec::with_capacity(plan.adapters.len());
    for adapter in plan.adapters.iter().rev() {
        let ExprKind::MethodCall { recv, args, .. } = &cursor.kind else {
            return None;
        };
        adapters.push(IterCall {
            kind: adapter.kind,
            args,
        });
        cursor = recv;
    }
    adapters.reverse();

    if matches!(
        plan.source.kind,
        IterSourceKind::VecIter | IterSourceKind::VecIntoIter
    ) {
        let ExprKind::MethodCall { recv, .. } = &cursor.kind else {
            return None;
        };
        cursor = recv;
    }

    Some(IterChain {
        source: cursor,
        adapters,
        consumer_args,
    })
}

/// Collect trait declarations across all scopes (root + nested modules), keyed
/// by simple name.
fn collect_traits<'p>(items: &'p [Item], out: &mut HashMap<&'p str, &'p TraitDecl>) {
    for it in items {
        match it {
            Item::Trait(t) => {
                out.insert(t.name.as_str(), t);
            }
            Item::Mod(m) => collect_traits(&m.items, out),
            _ => {}
        }
    }
}

/// Lua expression for the enum class table referenced by a variant path.
/// `["math","Shape","Circle"]` -> `math.Shape`; a bare `["Circle"]` uses the
/// resolved owning enum's (simple) name, which is a same-scope local.
fn enum_ref(segs: &[String], en_simple: &str) -> String {
    if segs.len() >= 2 {
        segs[..segs.len() - 1].join(".")
    } else {
        en_simple.to_string()
    }
}

/// If `impl <trait> for T` defines the operator method `method`, return the Lua
/// metamethod name to alias it to (enabling `a + b`, `a == b`, etc.).
fn op_alias(trait_name: &str, method: &str) -> Option<&'static str> {
    Some(match (trait_name, method) {
        ("Add", "add") => "__add",
        ("Sub", "sub") => "__sub",
        ("Mul", "mul") => "__mul",
        ("Div", "div") => "__div",
        ("Rem", "rem") => "__mod",
        ("Neg", "neg") => "__unm",
        ("PartialEq", "eq") | ("Eq", "eq") => "__eq",
        _ => return None,
    })
}

fn needs_hoist(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::If { .. } | ExprKind::IfLet { .. } | ExprKind::Block(_) | ExprKind::Match { .. }
    )
}

fn binop_lua(op: BinOp) -> &'static str {
    // `/` maps to Lua float division; integer-vs-float selection needs the type
    // checker (docs §9 open question 6). This MVP always emits `/`.
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        BinOp::Eq => "==",
        BinOp::Ne => "~=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::And => "and",
        BinOp::Or => "or",
    }
}

fn lua_int_literal(s: &str) -> String {
    let clean = s.replace('_', "");
    if let Some(bits) = clean.strip_prefix("0b").or_else(|| clean.strip_prefix("0B"))
        && let Ok(v) = i64::from_str_radix(bits, 2) {
            return v.to_string();
        }
    clean
}

fn paren_if_needed(s: &str) -> String {
    let is_atom = !s.contains(' ')
        || (s.starts_with('(') && s.ends_with(')'))
        || s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.');
    if is_atom {
        s.to_string()
    } else {
        format!("({})", s)
    }
}
