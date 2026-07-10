use crate::compile_str;

fn compile(src: &str) -> String {
    compile_str(src).unwrap_or_else(|e| panic!("compile error: {}", e))
}

#[test]
fn empty_fn() {
    let lua = compile("fn main() {}");
    assert!(lua.contains("function main()"));
    assert!(lua.contains("main()"));
}

#[test]
fn let_and_arithmetic() {
    let lua = compile(
        r#"
        fn f(a: i64, b: i64) -> i64 {
            let x = a + b * 2;
            x
        }
    "#,
    );
    assert!(lua.contains("function f(a, b)"));
    assert!(lua.contains("local x = (a + (b * 2))"));
    assert!(lua.contains("return x"));
}

#[test]
fn precedence() {
    // `1 + 2 * 3 == 7 && true` -> ((1 + (2*3)) == 7) and true
    let lua = compile("fn f() -> bool { 1 + 2 * 3 == 7 && true }");
    assert!(lua.contains("(((1 + (2 * 3)) == 7) and true)"), "got: {lua}");
}

#[test]
fn if_expression_hoists_temp() {
    let lua = compile(
        r#"
        fn abs(x: i64) -> i64 {
            let y = if x < 0 { -x } else { x };
            y
        }
    "#,
    );
    assert!(lua.contains("local y"));
    assert!(lua.contains("if (x < 0) then"));
    assert!(lua.contains("y = -x") || lua.contains("y = (-x)"));
    assert!(lua.contains("y = x"));
}

#[test]
fn if_as_return_tail() {
    let lua = compile("fn m(a: i64, b: i64) -> i64 { if a > b { a } else { b } }");
    assert!(lua.contains("if (a > b) then"));
    assert!(lua.contains("return a"));
    assert!(lua.contains("return b"));
}

#[test]
fn while_loop_break_continue() {
    let lua = compile(
        r#"
        fn f() {
            let mut i = 0;
            while i < 10 {
                if i == 5 { break; }
                i = i + 1;
                continue;
            }
        }
    "#,
    );
    assert!(lua.contains("while (i < 10) do"));
    assert!(lua.contains("break"));
    assert!(lua.contains("goto continue"));
    assert!(lua.contains("::continue::"));
}

#[test]
fn boolean_and_comparison_ops() {
    let lua = compile("fn f(a: i64, b: i64) -> bool { a != b || !(a < b) }");
    assert!(lua.contains("~="));
    assert!(lua.contains(" or "));
    assert!(lua.contains("not "));
}

#[test]
fn mutual_recursion_predeclared() {
    let lua = compile(
        r#"
        fn is_even(n: i64) -> bool { if n == 0 { true } else { is_odd(n - 1) } }
        fn is_odd(n: i64) -> bool { if n == 0 { false } else { is_even(n - 1) } }
    "#,
    );
    // both names predeclared as locals before either body
    assert!(lua.contains("local is_even, is_odd"));
}

#[test]
fn parse_error_reports_line() {
    let err = compile_str("fn f( {}").unwrap_err();
    assert!(err.contains(':'), "expected line-prefixed error, got: {err}");
}

// --- P2: struct / enum / match / Option / Result / ? / impl ---------------

#[test]
fn struct_decl_and_literal() {
    let lua = compile(
        r#"
        struct Point { x: f64, y: f64 }
        fn origin() -> Point { Point { x: 0.0, y: 0.0 } }
    "#,
    );
    assert!(lua.contains("local Point"), "got: {lua}");
    assert!(lua.contains("Point.__index = Point"));
    assert!(lua.contains("setmetatable({ x = 0.0, y = 0.0 }, Point)"), "got: {lua}");
}

#[test]
fn impl_methods_use_colon() {
    let lua = compile(
        r#"
        struct Point { x: f64, y: f64 }
        impl Point {
            fn new(x: f64, y: f64) -> Point { Point { x: x, y: y } }
            fn sum(&self) -> f64 { self.x + self.y }
        }
    "#,
    );
    assert!(lua.contains("function Point.new(x, y)"), "got: {lua}");
    assert!(lua.contains("function Point:sum()"), "got: {lua}");
    assert!(lua.contains("return (self.x + self.y)"));
}

#[test]
fn method_call_uses_colon() {
    let lua = compile(
        r#"
        struct P { x: f64 }
        impl P { fn get(&self) -> f64 { self.x } }
        fn f(p: P) -> f64 { p.get() }
    "#,
    );
    assert!(lua.contains("return p:get()"), "got: {lua}");
}

#[test]
fn enum_construction_tagged() {
    let lua = compile(
        r#"
        enum Shape { Circle(f64), Rect { w: f64, h: f64 }, Unit }
        fn a() -> Shape { Shape::Circle(2.0) }
        fn b() -> Shape { Shape::Rect { w: 3.0, h: 4.0 } }
        fn c() -> Shape { Shape::Unit }
    "#,
    );
    assert!(lua.contains(r#"setmetatable({ tag = "Circle", 2.0 }, Shape)"#), "got: {lua}");
    assert!(lua.contains(r#"setmetatable({ tag = "Rect", w = 3.0, h = 4.0 }, Shape)"#), "got: {lua}");
    assert!(lua.contains(r#"setmetatable({ tag = "Unit" }, Shape)"#), "got: {lua}");
}

#[test]
fn option_pure_nil() {
    let lua = compile(
        r#"
        fn some_v() -> Option<i64> { let x = Some(5); x }
        fn none_v() -> Option<i64> { let y = None; y }
    "#,
    );
    assert!(lua.contains("local x = 5"), "Some(5) should be bare 5; got: {lua}");
    assert!(lua.contains("local y = nil"), "None should be nil; got: {lua}");
}

#[test]
fn result_and_try() {
    let lua = compile(
        r#"
        fn parse() -> Result<i64, String> { Ok(3) }
        fn use_it() -> Result<i64, String> {
            let v = parse()?;
            Ok(v + 1)
        }
    "#,
    );
    assert!(lua.contains("{ ok = 3 }"), "got: {lua}");
    // `?` desugars to an Err-propagating early return
    assert!(lua.contains(".err ~= nil then return"), "got: {lua}");
}

#[test]
fn match_on_enum() {
    let lua = compile(
        r#"
        enum Shape { Circle(f64), Rect { w: f64, h: f64 } }
        fn area(s: Shape) -> f64 {
            match s {
                Shape::Circle(r) => 3.14159 * r * r,
                Shape::Rect { w, h } => w * h,
            }
        }
    "#,
    );
    assert!(lua.contains(r#".tag == "Circle""#), "got: {lua}");
    assert!(lua.contains("local r ="), "binds tuple field; got: {lua}");
    assert!(lua.contains(r#".tag == "Rect""#));
    assert!(lua.contains("local w ="));
    assert!(lua.contains("local h ="));
}

#[test]
fn match_option_and_literals() {
    let lua = compile(
        r#"
        fn classify(n: i64) -> i64 {
            match n {
                0 => 10,
                1 | 2 => 20,
                _ => 30,
            }
        }
    "#,
    );
    assert!(lua.contains("== 0"), "got: {lua}");
    assert!(lua.contains(" or "), "or-pattern combines; got: {lua}");
}

// --- P3: traits, default methods, operator overloading, checker -----------

#[test]
fn operator_overload_add() {
    let lua = compile(
        r#"
        struct V2 { x: f64, y: f64 }
        impl Add for V2 {
            fn add(self, o: V2) -> V2 { V2 { x: self.x + o.x, y: self.y + o.y } }
        }
    "#,
    );
    assert!(lua.contains("function V2:add(o)"), "got: {lua}");
    assert!(lua.contains("V2.__add = V2.add"), "operator alias; got: {lua}");
}

#[test]
fn trait_default_method_inherited() {
    let lua = compile(
        r#"
        trait Named {
            fn name(&self) -> String { "thing" }
            fn describe(&self) -> String;
        }
        struct Cat {}
        impl Named for Cat {
            fn describe(&self) -> String { "a cat" }
        }
    "#,
    );
    // `describe` provided by impl, `name` inherited as default
    assert!(lua.contains("function Cat:describe()"), "got: {lua}");
    assert!(lua.contains("function Cat:name()"), "default inherited; got: {lua}");
    assert!(lua.contains("\"thing\""));
}

#[test]
fn trait_default_not_emitted_when_overridden() {
    let lua = compile(
        r#"
        trait Named { fn name(&self) -> String { "thing" } }
        struct Dog {}
        impl Named for Dog { fn name(&self) -> String { "dog" } }
    "#,
    );
    assert!(lua.contains("\"dog\""));
    assert!(!lua.contains("\"thing\""), "overridden default must not be emitted; got: {lua}");
}

#[test]
fn checker_unknown_struct_field() {
    let err = compile_str(
        r#"
        struct Point { x: f64, y: f64 }
        fn f() -> Point { Point { x: 1.0, z: 2.0 } }
    "#,
    )
    .unwrap_err();
    assert!(err.contains("no field `z`"), "got: {err}");
}

#[test]
fn checker_missing_struct_field() {
    let err = compile_str(
        r#"
        struct Point { x: f64, y: f64 }
        fn f() -> Point { Point { x: 1.0 } }
    "#,
    )
    .unwrap_err();
    assert!(err.contains("missing field `y`"), "got: {err}");
}

#[test]
fn checker_variant_arity() {
    let err = compile_str(
        r#"
        enum E { Pair(i64, i64) }
        fn f() -> E { E::Pair(1) }
    "#,
    )
    .unwrap_err();
    assert!(err.contains("expects 2 argument"), "got: {err}");
}

#[test]
fn checker_unknown_variant() {
    let err = compile_str(
        r#"
        enum E { A, B }
        fn f() -> E { E::C }
    "#,
    )
    .unwrap_err();
    assert!(err.contains("no variant `C`"), "got: {err}");
}

#[test]
fn checker_duplicate_definition() {
    let err = compile_str("fn f() {} fn f() {}").unwrap_err();
    assert!(err.contains("duplicate top-level definition `f`"), "got: {err}");
}

#[test]
fn checker_allows_unknown_external_names() {
    // `print` and other unknown call targets must NOT be rejected (extern/P4).
    let lua = compile("fn main() { print(\"hi\"); }");
    assert!(lua.contains("print(\"hi\")"));
}

#[test]
fn match_guard() {
    let lua = compile(
        r#"
        fn f(n: i64) -> i64 {
            match n {
                x if x > 0 => 1,
                _ => 0,
            }
        }
    "#,
    );
    assert!(lua.contains("local x ="), "binds x for guard; got: {lua}");
    assert!(lua.contains("if (x > 0) then"), "guard uses binding; got: {lua}");
}

// --- P4: for / range / index / macros / Vec -------------------------------

#[test]
fn for_range_exclusive() {
    let lua = compile("fn f() { for i in 0..10 { print(i); } }");
    assert!(lua.contains("for i = 0, (10) - 1 do"), "got: {lua}");
    assert!(lua.contains("::continue::"), "got: {lua}");
}

#[test]
fn for_range_inclusive() {
    let lua = compile("fn f() { for i in 1..=5 { print(i); } }");
    assert!(lua.contains("for i = 1, 5 do"), "got: {lua}");
}

#[test]
fn for_over_vec() {
    let lua = compile(
        r#"
        fn f() {
            let v = vec![1, 2, 3];
            for x in v { print(x); }
        }
    "#,
    );
    assert!(lua.contains(".n - 1 do"), "iterates by length; got: {lua}");
}

#[test]
fn vec_macro_zero_based() {
    let lua = compile("fn f() { let v = vec![10, 20]; }");
    assert!(lua.contains("rt.vec({ [0] = 10, [1] = 20, n = 2 })"), "got: {lua}");
    assert!(lua.contains("local rt = require(\"rua_rt\")"), "emits require; got: {lua}");
}

#[test]
fn index_expr() {
    let lua = compile("fn f() { let v = vec![1, 2]; let a = v[0]; }");
    assert!(lua.contains("local a = v[0]"), "0-based index; got: {lua}");
}

#[test]
fn macro_println_format() {
    let lua = compile(r#"fn main() { println!("x = {}", 42); }"#);
    assert!(lua.contains(r#"rt.println("x = {}", 42)"#), "got: {lua}");
}

#[test]
fn macro_panic() {
    let lua = compile(r#"fn f() { panic!("boom"); }"#);
    assert!(lua.contains("rt.panic(rt.format(\"boom\"))"), "got: {lua}");
}

#[test]
fn no_require_when_rt_unused() {
    let lua = compile("fn main() { let x = 1; }");
    assert!(!lua.contains("require(\"rua_rt\")"), "no require without rt; got: {lua}");
}

#[test]
fn vec_method_call() {
    // `.push()` / `.len()` lower to method calls resolved by the Vec metatable.
    let lua = compile("fn f() { let v = vec![1]; v.push(2); let n = v.len(); }");
    assert!(lua.contains("v:push(2)"), "got: {lua}");
    assert!(lua.contains("v:len()"), "got: {lua}");
}

// --- P4b: extern "lua" + HashMap ------------------------------------------

#[test]
fn extern_block_emits_no_code() {
    let lua = compile(
        r#"
        extern "lua" {
            fn print(msg: String);
            fn tostring(v: i64) -> String;
        }
        fn main() { print("hi"); }
    "#,
    );
    // Externs are ambient Lua globals: no declaration, no local, just used.
    assert!(!lua.contains("function print"), "got: {lua}");
    assert!(!lua.contains("local print"), "got: {lua}");
    assert!(lua.contains("print(\"hi\")"), "got: {lua}");
}

#[test]
fn extern_variadic_parses() {
    // `...` in an extern signature is accepted.
    let lua = compile(
        r#"
        extern "lua" {
            fn format(fmt: String, ...) -> String;
        }
        fn main() { let s = format("{} {}", 1, 2); }
    "#,
    );
    assert!(lua.contains("format(\"{} {}\", 1, 2)"), "got: {lua}");
}

#[test]
fn extern_duplicate_of_fn_is_error() {
    let err = compile_str(
        r#"
        extern "lua" { fn foo(); }
        fn foo() {}
    "#,
    )
    .unwrap_err();
    assert!(err.contains("duplicate top-level definition `foo`"), "got: {err}");
}

#[test]
fn hashmap_new_and_methods() {
    let lua = compile(
        r#"
        fn main() {
            let mut m = HashMap::new();
            m.insert("a", 1);
            let v = m.get("a");
            let has = m.contains_key("a");
            let n = m.len();
        }
    "#,
    );
    assert!(lua.contains("local m = rt.map()"), "got: {lua}");
    assert!(lua.contains("m:insert(\"a\", 1)"), "got: {lua}");
    assert!(lua.contains("m:get(\"a\")"), "got: {lua}");
    assert!(lua.contains("m:contains_key(\"a\")"), "got: {lua}");
    assert!(lua.contains("local rt = require(\"rua_rt\")"), "got: {lua}");
}

#[test]
fn vec_new_intrinsic() {
    let lua = compile("fn f() { let v = Vec::new(); v.push(1); }");
    assert!(lua.contains("local v = rt.vec({ n = 0 })"), "got: {lua}");
}

// --- P5: AST spans -> line-accurate diagnostics ---------------------------

#[test]
fn diagnostic_reports_line_number() {
    // The bad struct literal is on line 4 (1-based, counting the leading \n).
    let err = compile_str(
        "\nstruct Point { x: f64, y: f64 }\nfn f() -> Point {\n    Point { x: 1.0, z: 2.0 }\n}\n",
    )
    .unwrap_err();
    assert!(err.contains("no field `z`"), "got: {err}");
    assert!(err.starts_with("4:"), "error should be prefixed with line 4; got: {err}");
}

#[test]
fn diagnostic_line_for_variant_arity() {
    let err = compile_str("enum E { Pair(i64, i64) }\nfn f() -> E {\n    E::Pair(1)\n}\n")
        .unwrap_err();
    assert!(err.contains("expects 2 argument"), "got: {err}");
    assert!(err.starts_with("3:"), "arity error on line 3; got: {err}");
}

// --- P5b: conservative type checker ---------------------------------------

#[test]
fn typeck_non_bool_if_condition() {
    let err = compile_str("fn f() { if 1 { } }").unwrap_err();
    assert!(err.contains("`if` condition must be `bool`"), "got: {err}");
}

#[test]
fn typeck_non_bool_while_condition() {
    let err = compile_str("fn f() { while 3 { } }").unwrap_err();
    assert!(err.contains("`while` condition must be `bool`"), "got: {err}");
}

#[test]
fn typeck_return_type_mismatch() {
    let err = compile_str("fn f() -> bool { 1 }").unwrap_err();
    assert!(err.contains("expected return type `bool`"), "got: {err}");
}

#[test]
fn typeck_let_annotation_mismatch() {
    let err = compile_str("fn f() { let x: bool = 1; }").unwrap_err();
    assert!(err.contains("annotated as `bool`"), "got: {err}");
}

#[test]
fn typeck_fn_arity() {
    let err = compile_str("fn g(a: i64, b: i64) -> i64 { a + b }\nfn f() { g(1); }").unwrap_err();
    assert!(err.contains("function `g` expects 2 argument"), "got: {err}");
}

#[test]
fn typeck_fn_arg_type() {
    let err =
        compile_str("fn g(a: bool) {}\nfn f() { g(5); }").unwrap_err();
    assert!(err.contains("argument 1 of `g` expects `bool`"), "got: {err}");
}

#[test]
fn typeck_arithmetic_on_bool() {
    let err = compile_str("fn f() { let x = true + 1; }").unwrap_err();
    assert!(err.contains("arithmetic operator applied to `bool`"), "got: {err}");
}

#[test]
fn typeck_field_access_unknown() {
    let err = compile_str(
        "struct P { x: i64 }\nfn f(p: P) -> i64 { p.y }",
    )
    .unwrap_err();
    assert!(err.contains("struct `P` has no field `y`"), "got: {err}");
}

#[test]
fn typeck_numeric_mixing_is_allowed() {
    // i64 + f64 is intentionally lenient (Lua unifies numbers) -> no error.
    let lua = compile("fn f() -> f64 { let a = 1; let b = 2.0; a + b }");
    assert!(lua.contains("(a + b)"), "got: {lua}");
}

#[test]
fn typeck_extern_calls_not_flagged() {
    // Unknown/extern names never trigger type errors.
    let lua = compile(
        r#"
        extern "lua" { fn thing(x: i64) -> bool; }
        fn f() { if thing(1) { } let y = other(); }
    "#,
    );
    assert!(lua.contains("thing(1)"), "got: {lua}");
}

#[test]
fn typeck_method_arity() {
    let err = compile_str(
        "struct P { x: i64 }\nimpl P { fn get(self) -> i64 { self.x } }\nfn f(p: P) -> i64 { p.get(5) }",
    )
    .unwrap_err();
    assert!(err.contains("method `P::get` expects 0 argument"), "got: {err}");
}

#[test]
fn typeck_method_arg_type() {
    let err = compile_str(
        "struct P { x: i64 }\nimpl P { fn set(self, v: bool) {} }\nfn f(p: P) { p.set(3); }",
    )
    .unwrap_err();
    assert!(err.contains("argument 1 of `P::set` expects `bool`"), "got: {err}");
}

#[test]
fn typeck_method_return_feeds_inference() {
    let err = compile_str(
        "struct P { x: i64 }\nimpl P { fn flag(self) -> bool { true } }\nfn f(p: P) -> i64 { p.flag() }",
    )
    .unwrap_err();
    assert!(err.contains("expected return type `i64`"), "got: {err}");
}

#[test]
fn typeck_inherited_default_method_ok() {
    // `twice` is a trait default inherited by S; calling it must type-check.
    let lua = compile(
        r#"
        trait T {
            fn base(&self) -> i64;
            fn twice(&self) -> i64 { self.base() * 2 }
        }
        struct S { v: i64 }
        impl T for S { fn base(&self) -> i64 { self.v } }
        fn f(s: S) -> i64 { s.twice() }
    "#,
    );
    assert!(lua.contains("S.twice") || lua.contains("S:twice"), "got: {lua}");
}

#[test]
fn typeck_inherited_default_method_arity_checked() {
    let err = compile_str(
        r#"
        trait T {
            fn base(&self) -> i64;
            fn twice(&self) -> i64 { self.base() * 2 }
        }
        struct S { v: i64 }
        impl T for S { fn base(&self) -> i64 { self.v } }
        fn f(s: S) -> i64 { s.twice(1) }
    "#,
    )
    .unwrap_err();
    assert!(err.contains("method `S::twice` expects 0 argument"), "got: {err}");
}

#[test]
fn typeck_vec_and_map_methods_not_flagged() {
    // Receiver types for Vec/HashMap are Unknown -> method calls never flagged.
    let lua = compile(
        "fn f() { let v = vec![1]; v.push(2); let m = HashMap::new(); m.insert(\"a\", 1); }",
    );
    assert!(lua.contains("v:push(2)"), "got: {lua}");
    assert!(lua.contains("m:insert"), "got: {lua}");
}

#[test]
fn typeck_vec_element_index_type() {
    // v[0] : i64, used as an `if` condition -> non-bool error (element tracked).
    let err = compile_str("fn f() { let v = vec![1, 2]; if v[0] { } }").unwrap_err();
    assert!(err.contains("`if` condition must be `bool`"), "got: {err}");
}

#[test]
fn typeck_vec_annotation_mismatch() {
    let err = compile_str("fn f() { let x: i64 = vec![1]; }").unwrap_err();
    assert!(err.contains("annotated as `i64`"), "got: {err}");
    assert!(err.contains("Vec<i64>"), "shows element type; got: {err}");
}

#[test]
fn typeck_for_over_vec_binds_element() {
    // x : bool (from Vec<bool>), annotated as i64 -> mismatch.
    let err =
        compile_str("fn f() { let v = vec![true]; for x in v { let y: i64 = x; } }").unwrap_err();
    assert!(err.contains("annotated as `i64`"), "got: {err}");
}

#[test]
fn typeck_try_unwraps_result_element() {
    let err = compile_str(
        r#"
        fn p() -> Result<i64, String> { Ok(1) }
        fn f() -> Result<i64, String> { let v = p()?; if v { } Ok(v) }
    "#,
    )
    .unwrap_err();
    assert!(err.contains("`if` condition must be `bool`"), "got: {err}");
}

#[test]
fn typeck_map_contains_key_is_bool_ok() {
    let lua = compile(
        r#"fn f() { let m = HashMap::new(); if m.contains_key("a") { } }"#,
    );
    assert!(lua.contains("m:contains_key"), "got: {lua}");
}

#[test]
fn typeck_option_result_return_ok() {
    // Correct Option/Result returns must type-check cleanly.
    let lua = compile(
        r#"
        fn a() -> Option<i64> { Some(1) }
        fn b() -> Result<i64, String> { Ok(2) }
        fn c() -> Result<i64, String> { Err("x") }
    "#,
    );
    assert!(lua.contains("{ ok = 2 }"), "got: {lua}");
}

#[test]
fn codegen_integer_division() {
    // i64 / i64 -> truncating helper `rt.idiv` (matches Rust for negatives).
    let lua = compile("fn f() -> i64 { 7 / 2 }");
    assert!(lua.contains("rt.idiv(7, 2)"), "got: {lua}");
    assert!(!lua.contains("//"), "got: {lua}");
}

#[test]
fn codegen_float_division_stays_slash() {
    let lua = compile("fn f() -> f64 { 7.0 / 2.0 }");
    assert!(lua.contains("(7.0 / 2.0)"), "got: {lua}");
    assert!(!lua.contains("//"), "should not use integer division; got: {lua}");
}

#[test]
fn codegen_mixed_division_stays_slash() {
    // i64 / f64 is not integer division.
    let lua = compile("fn f(a: i64, b: f64) -> f64 { a / b }");
    assert!(lua.contains("(a / b)"), "got: {lua}");
    assert!(!lua.contains("//"), "got: {lua}");
}

#[test]
fn codegen_unknown_division_stays_slash() {
    // Generic/unknown operand types default to float division (safe).
    let lua = compile("fn f(a: T, b: T) { let x = a / b; }");
    assert!(lua.contains("(a / b)"), "got: {lua}");
    assert!(!lua.contains("//"), "got: {lua}");
}

#[test]
fn codegen_int_division_via_params() {
    // `i64 / i64` lowers to the truncating helper (matches Rust for negatives),
    // not Lua's floored `//`.
    let lua = compile("fn f(a: i64, b: i64) -> i64 { a / b }");
    assert!(lua.contains("rt.idiv(a, b)"), "got: {lua}");
    assert!(!lua.contains("//"), "got: {lua}");
}

#[test]
fn codegen_int_remainder_via_params() {
    // `i64 % i64` lowers to the truncating remainder helper.
    let lua = compile("fn f(a: i64, b: i64) -> i64 { a % b }");
    assert!(lua.contains("rt.irem(a, b)"), "got: {lua}");
}

#[test]
fn codegen_mixed_remainder_stays_percent() {
    // A non-integer remainder keeps the plain Lua `%`.
    let lua = compile("fn f(a: f64, b: f64) -> f64 { a % b }");
    assert!(lua.contains("(a % b)"), "got: {lua}");
    assert!(!lua.contains("rt.irem"), "got: {lua}");
}

// --- std shims: String methods + concatenation ------------------------------

#[test]
fn codegen_string_method_routes_through_rt_str() {
    let lua = compile(r#"fn f(s: String) -> String { s.to_uppercase() }"#);
    assert!(lua.contains("rt.str[\"to_uppercase\"](s)"), "got: {lua}");
}

#[test]
fn codegen_string_method_on_literal_uses_call_form() {
    // Method on a string literal must not use the `literal:method()` form.
    let lua = compile(r#"fn f() -> bool { "abc".contains("b") }"#);
    assert!(
        lua.contains("rt.str[\"contains\"](\"abc\", \"b\")"),
        "got: {lua}"
    );
}

#[test]
fn codegen_string_concat_uses_dotdot() {
    let lua = compile(r#"fn f(a: String, b: String) -> String { a + b }"#);
    assert!(lua.contains("(a .. b)"), "got: {lua}");
}

#[test]
fn typeck_string_len_is_i64() {
    // `s.len()` yields `i64`, so binding it to `bool` is a type error.
    let err = compile_str(r#"fn f(s: String) { let _n: bool = s.len(); }"#).unwrap_err();
    assert!(
        err.contains("annotated as `bool` but initialized with `i64`"),
        "got: {err}"
    );
}

#[test]
fn typeck_string_split_returns_vec_of_string() {
    // `split` yields `Vec<String>`; indexing gives `String`, so `if el { }` fails.
    let err = compile_str(
        r#"fn f(s: String) { let v = s.split(","); if v.get(0) { } }"#,
    )
    .unwrap_err();
    assert!(err.contains("must be `bool`"), "got: {err}");
}

#[test]
fn codegen_unknown_string_method_stays_colon_form() {
    // An unrecognized method on a String is left as a plain method call.
    let lua = compile(r#"fn f(s: String) { s.frobnicate(); }"#);
    assert!(lua.contains("s:frobnicate()"), "got: {lua}");
    assert!(!lua.contains("rt.str"), "got: {lua}");
}

#[test]
fn typeck_operator_overload_not_flagged() {
    // `a + b` on a user struct with `impl Add` must not be flagged as bad arithmetic.
    let lua = compile(
        r#"
        struct V { x: f64 }
        impl Add for V {
            fn add(self, o: V) -> V { V { x: self.x + o.x } }
        }
        fn f(a: V, b: V) -> V { a + b }
    "#,
    );
    assert!(lua.contains("V.__add"), "got: {lua}");
}

// --- P4c: if let / while let ------------------------------------------------

#[test]
fn if_let_some_binds_and_tests_nil() {
    // Pure-nil Option: `if let Some(x) = opt` tests `~= nil` and binds x = subject.
    let lua = compile(
        r#"
        fn f(opt: Option<i64>) {
            if let Some(x) = opt {
                println!("{}", x);
            }
        }
    "#,
    );
    assert!(lua.contains("~= nil then"), "got: {lua}");
    assert!(lua.contains("local x ="), "got: {lua}");
}

#[test]
fn if_let_with_else_branch() {
    let lua = compile(
        r#"
        fn f(opt: Option<i64>) -> i64 {
            if let Some(x) = opt { x } else { 0 }
        }
    "#,
    );
    assert!(lua.contains("else"), "got: {lua}");
    // Used in tail position, so both branches feed the return.
    assert!(lua.contains("return"), "got: {lua}");
}

#[test]
fn if_let_ok_unwraps_result() {
    let lua = compile(
        r#"
        fn f(r: Result<i64, String>) {
            if let Ok(v) = r {
                println!("{}", v);
            }
        }
    "#,
    );
    // Ok arm: not nil and no err; binds from `.ok`.
    assert!(lua.contains(".err == nil"), "got: {lua}");
    assert!(lua.contains(".ok"), "got: {lua}");
}

#[test]
fn if_let_as_statement_needs_no_semicolon() {
    // Two consecutive if-let statements must parse (block-like, no `;`).
    let lua = compile(
        r#"
        fn f(a: Option<i64>, b: Option<i64>) {
            if let Some(x) = a {
                println!("{}", x);
            }
            if let Some(y) = b {
                println!("{}", y);
            }
        }
    "#,
    );
    assert!(lua.contains("function f(a, b)"), "got: {lua}");
}

#[test]
fn while_let_drains_with_break() {
    let lua = compile(
        r#"
        fn f(stack: Vec<i64>) {
            while let Some(x) = stack.pop() {
                println!("{}", x);
            }
        }
    "#,
    );
    assert!(lua.contains("while true do"), "got: {lua}");
    assert!(lua.contains("stack:pop()"), "got: {lua}");
    assert!(lua.contains("else"), "got: {lua}");
    assert!(lua.contains("break"), "got: {lua}");
}

// --- P4c: module system (slice 1: inline mod / use / pub) -------------------

#[test]
fn module_emits_do_block_and_table() {
    let lua = compile(
        r#"
        mod math {
            pub fn add(a: i64, b: i64) -> i64 { a + b }
        }
        fn main() { println!("{}", math::add(1, 2)); }
    "#,
    );
    assert!(lua.contains("math = {}"), "got: {lua}");
    assert!(lua.contains("do"), "got: {lua}");
    assert!(lua.contains("math.add = add"), "got: {lua}");
    // Qualified call `math::add` lowers to `math.add`.
    assert!(lua.contains("math.add(1, 2)"), "got: {lua}");
}

#[test]
fn module_sibling_call_is_block_local() {
    // `double` calls `add` unqualified; both are locals inside the do-block.
    let lua = compile(
        r#"
        mod m {
            pub fn add(a: i64, b: i64) -> i64 { a + b }
            pub fn double(x: i64) -> i64 { add(x, x) }
        }
        fn main() {}
    "#,
    );
    assert!(lua.contains("local add, double"), "got: {lua}");
    assert!(lua.contains("return add(x, x)"), "got: {lua}");
}

#[test]
fn nested_module_qualified_access() {
    let lua = compile(
        r#"
        mod outer {
            pub mod inner {
                pub fn f() -> i64 { 1 }
            }
        }
        fn main() { println!("{}", outer::inner::f()); }
    "#,
    );
    assert!(lua.contains("outer.inner = inner"), "got: {lua}");
    assert!(lua.contains("inner.f = f"), "got: {lua}");
    assert!(lua.contains("outer.inner.f()"), "got: {lua}");
}

#[test]
fn use_import_desugars_to_qualified_path() {
    // `use` introduces no runtime local; the call site is fully qualified.
    let lua = compile(
        r#"
        mod m { pub fn f() -> i64 { 1 } }
        use m::f;
        fn main() { println!("{}", f()); }
    "#,
    );
    assert!(lua.contains("m.f()"), "got: {lua}");
    assert!(!lua.contains("f = m.f"), "got: {lua}");
}

#[test]
fn use_alias_and_group_desugar() {
    let lua = compile(
        r#"
        mod m {
            pub fn a() -> i64 { 1 }
            pub fn b() -> i64 { 2 }
        }
        use m::a as first;
        use m::{b};
        fn main() {
            let _x = first();
            let _y = b();
        }
    "#,
    );
    assert!(lua.contains("m.a()"), "got: {lua}");
    assert!(lua.contains("m.b()"), "got: {lua}");
    // No alias locals are emitted.
    assert!(!lua.contains("first ="), "got: {lua}");
}

#[test]
fn same_fn_name_across_modules_no_dup_error() {
    // Two modules may each define `f` without a duplicate-definition error.
    let lua = compile(
        r#"
        mod a { pub fn f() -> i64 { 1 } }
        mod b { pub fn f() -> i64 { 2 } }
        fn main() {}
    "#,
    );
    assert!(lua.contains("a = {}"), "got: {lua}");
    assert!(lua.contains("b = {}"), "got: {lua}");
}

#[test]
fn module_struct_qualified_literal_and_assoc_fn() {
    let lua = compile(
        r#"
        mod geo {
            pub struct Point { x: f64, y: f64 }
            impl Point {
                pub fn new(x: f64, y: f64) -> Point { Point { x: x, y: y } }
            }
        }
        fn main() {
            let p = geo::Point::new(1.0, 2.0);
            let q = geo::Point { x: 3.0, y: 4.0 };
            println!("{}", q.x);
        }
    "#,
    );
    // Class table lives inside the module's do-block.
    assert!(lua.contains("Point = {}; Point.__index = Point"), "got: {lua}");
    assert!(lua.contains("function Point.new(x, y)"), "got: {lua}");
    assert!(lua.contains("geo.Point = Point"), "got: {lua}");
    // Cross-module associated fn + struct literal use the qualified table.
    assert!(lua.contains("geo.Point.new(1.0, 2.0)"), "got: {lua}");
    assert!(
        lua.contains("setmetatable({ x = 3.0, y = 4.0 }, geo.Point)"),
        "got: {lua}"
    );
}

#[test]
fn module_enum_variants_qualified() {
    let lua = compile(
        r#"
        mod geo {
            pub enum Shape { Circle(f64), Rect { w: f64, h: f64 }, Dot }
        }
        fn main() {
            let a = geo::Shape::Circle(5.0);
            let b = geo::Shape::Rect { w: 2.0, h: 3.0 };
            let c = geo::Shape::Dot;
        }
    "#,
    );
    assert!(
        lua.contains("setmetatable({ tag = \"Circle\", 5.0 }, geo.Shape)"),
        "got: {lua}"
    );
    assert!(
        lua.contains("setmetatable({ tag = \"Rect\", w = 2.0, h = 3.0 }, geo.Shape)"),
        "got: {lua}"
    );
    assert!(
        lua.contains("setmetatable({ tag = \"Dot\" }, geo.Shape)"),
        "got: {lua}"
    );
}

#[test]
fn module_method_and_self_call() {
    let lua = compile(
        r#"
        mod geo {
            pub struct P { x: f64 }
            impl P {
                pub fn get(&self) -> f64 { self.x }
            }
        }
        fn main() {
            let p = geo::P { x: 9.0 };
            println!("{}", p.get());
        }
    "#,
    );
    assert!(lua.contains("function P:get()"), "got: {lua}");
    // Method call resolves at runtime via metatable (colon call).
    assert!(lua.contains("p:get()"), "got: {lua}");
}

#[test]
fn module_trait_default_method() {
    let lua = compile(
        r#"
        mod shapes {
            pub trait Named {
                fn tag(&self) -> i64 { 0 }
            }
            pub struct Sq { s: f64 }
            impl Named for Sq {}
        }
        fn main() {}
    "#,
    );
    // Inherited default method is emitted onto the module-local class table.
    assert!(lua.contains("function Sq:tag()"), "got: {lua}");
    assert!(lua.contains("shapes.Sq = Sq"), "got: {lua}");
}

#[test]
fn same_type_name_across_modules_no_dup_error() {
    // Distinct modules may each define a `Point` without a duplicate error.
    let lua = compile(
        r#"
        mod a { pub struct Point { x: i64 } }
        mod b { pub struct Point { y: i64 } }
        fn main() {}
    "#,
    );
    assert!(lua.contains("a.Point = Point"), "got: {lua}");
    assert!(lua.contains("b.Point = Point"), "got: {lua}");
}

#[test]
fn private_fn_cross_module_rejected() {
    let err = compile_str(
        r#"
        mod m { fn secret() -> i64 { 42 } }
        fn main() { println!("{}", m::secret()); }
    "#,
    )
    .unwrap_err();
    assert!(err.contains("`secret` is private"), "got: {err}");
}

#[test]
fn private_submodule_cross_access_rejected() {
    let err = compile_str(
        r#"
        mod a {
            mod b { pub fn f() -> i64 { 1 } }
        }
        fn main() { println!("{}", a::b::f()); }
    "#,
    )
    .unwrap_err();
    assert!(err.contains("`b` is private"), "got: {err}");
}

#[test]
fn private_struct_cross_module_rejected() {
    let err = compile_str(
        r#"
        mod geo { struct Point { x: i64 } }
        fn main() { let p = geo::Point { x: 1 }; }
    "#,
    )
    .unwrap_err();
    assert!(err.contains("`Point` is private"), "got: {err}");
}

#[test]
fn use_private_item_rejected() {
    // Importing a private item via `use` is itself a visibility error.
    let err = compile_str(
        r#"
        mod m { fn secret() -> i64 { 42 } }
        use m::secret;
        fn main() {}
    "#,
    )
    .unwrap_err();
    assert!(err.contains("`secret` is private"), "got: {err}");
}

#[test]
fn use_pub_item_ok() {
    let lua = compile(
        r#"
        mod m { pub fn ok() -> i64 { 1 } }
        use m::ok;
        fn main() { let _x = ok(); }
    "#,
    );
    assert!(lua.contains("m.ok()"), "got: {lua}");
}

#[test]
fn pub_item_cross_module_ok() {
    // A fully public path must not be flagged.
    let lua = compile(
        r#"
        mod a {
            pub mod b { pub fn f() -> i64 { 1 } }
        }
        fn main() { println!("{}", a::b::f()); }
    "#,
    );
    assert!(lua.contains("a.b.f()"), "got: {lua}");
}

#[test]
fn root_private_item_visible_everywhere() {
    // Root-level private items are visible crate-wide (including from modules).
    let lua = compile(
        r#"
        fn helper() -> i64 { 7 }
        mod m {
            pub fn use_root() -> i64 { helper() }
        }
        fn main() { println!("{}", m::use_root()); }
    "#,
    );
    assert!(lua.contains("m.use_root()"), "got: {lua}");
}

#[test]
fn private_item_same_module_ok() {
    // A module may use its own private items internally (bare, block-local).
    let lua = compile(
        r#"
        mod m {
            fn secret() -> i64 { 42 }
            pub fn reveal() -> i64 { secret() }
        }
        fn main() { println!("{}", m::reveal()); }
    "#,
    );
    assert!(lua.contains("return secret()"), "got: {lua}");
}

#[test]
fn use_inside_module_desugars_within_scope() {
    // A `use` inside a module rewrites references in that module's functions,
    // and its scope does not leak to sibling modules or the root.
    let lua = compile(
        r#"
        mod util { pub fn helper() -> i64 { 7 } }
        mod m {
            use util::helper as h;
            pub fn go() -> i64 { h() }
        }
        fn main() { println!("{}", m::go()); }
    "#,
    );
    assert!(lua.contains("return util.helper()"), "got: {lua}");
    // No runtime alias local for `h`.
    assert!(!lua.contains(" h ="), "got: {lua}");
}

#[test]
fn use_scope_does_not_leak_to_root() {
    // An alias declared inside a module must not affect a bare name at the root.
    let lua = compile(
        r#"
        mod m {
            use m::inner::deep as helper;
            pub mod inner { pub fn deep() -> i64 { 1 } }
        }
        fn helper() -> i64 { 42 }
        fn main() { println!("{}", helper()); }
    "#,
    );
    // Root `helper()` stays bare (not rewritten to the module alias target).
    assert!(lua.contains("helper()"), "got: {lua}");
    assert!(!lua.contains("m.inner.deep()"), "got: {lua}");
}

#[test]
fn use_alias_shadowed_by_local_not_rewritten() {
    // A local binding that shadows a `use` alias suppresses rewriting.
    let lua = compile(
        r#"
        mod m { pub fn f() -> i64 { 1 } }
        use m::f;
        fn main() {
            let f = 99;
            println!("{}", f);
        }
    "#,
    );
    // The shadowed reference stays bare rather than becoming `m.f`.
    assert!(lua.contains("rt.println(\"{}\", f)"), "got: {lua}");
}

#[test]
fn duplicate_fn_in_module_is_rejected() {
    let err = compile_str(
        r#"
        mod m {
            pub fn f() -> i64 { 1 }
            pub fn f() -> i64 { 2 }
        }
        fn main() {}
    "#,
    )
    .unwrap_err();
    assert!(err.contains("duplicate definition `f` in module `m`"), "got: {err}");
}

#[test]
fn file_module_via_string_errors() {
    // `mod m;` parses, but resolving it needs a base directory (a file compile).
    let err = compile_str("mod m;\nfn main() {}").unwrap_err();
    assert!(err.contains("requires compiling from a file"), "got: {err}");
}

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut d = std::env::temp_dir();
    d.push(format!("ruac_test_{}_{}_{}", tag, std::process::id(), nanos));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn file_module_resolution() {
    let dir = tmp_dir("filemod");
    std::fs::write(
        dir.join("main.rua"),
        "mod util;\nfn main() { println!(\"{}\", util::f()); }\n",
    )
    .unwrap();
    std::fs::write(dir.join("util.rua"), "pub fn f() -> i64 { 7 }\n").unwrap();
    let lua = crate::compile_path(&dir.join("main.rua")).unwrap();
    assert!(lua.contains("util = {}"), "got: {lua}");
    assert!(lua.contains("util.f = f"), "got: {lua}");
    assert!(lua.contains("util.f()"), "got: {lua}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn nested_file_module_resolution() {
    let dir = tmp_dir("nested");
    std::fs::write(
        dir.join("main.rua"),
        "mod math;\nfn main() { println!(\"{}\", math::trig::triple(2)); }\n",
    )
    .unwrap();
    std::fs::write(dir.join("math.rua"), "pub mod trig;\n").unwrap();
    std::fs::create_dir_all(dir.join("math")).unwrap();
    std::fs::write(
        dir.join("math").join("trig.rua"),
        "pub fn triple(x: i64) -> i64 { x * 3 }\n",
    )
    .unwrap();
    let lua = crate::compile_path(&dir.join("main.rua")).unwrap();
    assert!(lua.contains("math.trig.triple(2)"), "got: {lua}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mod_dir_style_resolution() {
    // `mod foo;` may also resolve to `foo/mod.rua`.
    let dir = tmp_dir("moddir");
    std::fs::write(
        dir.join("main.rua"),
        "mod foo;\nfn main() { println!(\"{}\", foo::g()); }\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("foo")).unwrap();
    std::fs::write(dir.join("foo").join("mod.rua"), "pub fn g() -> i64 { 1 }\n").unwrap();
    let lua = crate::compile_path(&dir.join("main.rua")).unwrap();
    assert!(lua.contains("foo.g()"), "got: {lua}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_file_module_errors() {
    let dir = tmp_dir("missing");
    std::fs::write(dir.join("main.rua"), "mod nope;\nfn main() {}\n").unwrap();
    let err = crate::compile_path(&dir.join("main.rua")).unwrap_err();
    assert!(
        err.contains("cannot find file for module `nope`"),
        "got: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn file_module_visibility_enforced() {
    // Privacy still applies across file modules.
    let dir = tmp_dir("filevis");
    std::fs::write(
        dir.join("main.rua"),
        "mod m;\nfn main() { println!(\"{}\", m::secret()); }\n",
    )
    .unwrap();
    std::fs::write(dir.join("m.rua"), "fn secret() -> i64 { 42 }\n").unwrap();
    let err = crate::compile_path(&dir.join("main.rua")).unwrap_err();
    assert!(err.contains("`secret` is private"), "got: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn type_error_in_child_file_attributes_file_and_line() {
    // A type error in a loaded child file must report that file's path + line,
    // not a bare line number relative to the merged root.
    let dir = tmp_dir("attrib");
    std::fs::write(
        dir.join("main.rua"),
        "mod util;\nfn main() { let _ = util::f(); }\n",
    )
    .unwrap();
    // Return type mismatch on line 2 of util.rua.
    std::fs::write(
        dir.join("util.rua"),
        "pub fn f() -> bool {\n    1 + 2\n}\n",
    )
    .unwrap();
    let err = crate::compile_path(&dir.join("main.rua")).unwrap_err();
    assert!(err.contains("util.rua:2:"), "expected file:line attribution; got: {err}");
    assert!(
        err.contains("expected return type `bool`, found `i64`"),
        "got: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn type_error_in_root_file_attributes_root() {
    // Errors in the root file are attributed to the root path.
    let dir = tmp_dir("rootattrib");
    std::fs::write(
        dir.join("main.rua"),
        "fn main() {\n    let _x: bool = 1 + 2;\n}\n",
    )
    .unwrap();
    let err = crate::compile_path(&dir.join("main.rua")).unwrap_err();
    assert!(err.contains("main.rua:2:"), "got: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn if_let_binding_is_scoped_no_false_positive() {
    // The binding introduced by if-let is usable in the then-block and must
    // not trigger a type error.
    let lua = compile(
        r#"
        fn f(opt: Option<i64>) -> i64 {
            if let Some(x) = opt {
                x + 1
            } else {
                0
            }
        }
    "#,
    );
    assert!(lua.contains("(x + 1)"), "got: {lua}");
}

// --- P5c-4 generics + bounds ------------------------------------------------

#[test]
fn generic_identity_fn_erases_to_plain_lua() {
    let lua = compile("fn id<T>(x: T) -> T { x }\nfn main() {}");
    assert!(lua.contains("function id(x)"), "got: {lua}");
    assert!(lua.contains("return x"), "got: {lua}");
}

#[test]
fn generic_struct_and_enum_parse_and_erase() {
    let lua = compile(
        r#"
        struct Wrapper<T> { value: T }
        enum Either<L, R> { Left(L), Right(R) }
        fn main() {}
    "#,
    );
    assert!(lua.contains("Wrapper = {}"), "got: {lua}");
    assert!(lua.contains("Either = {}"), "got: {lua}");
}

#[test]
fn generic_bound_method_call_ok() {
    // A method provided by the trait bound resolves and type-checks.
    let lua = compile(
        r#"
        trait Animal { fn speak(&self) -> String; }
        fn describe<T: Animal>(a: T) -> String { a.speak() }
        fn main() {}
    "#,
    );
    assert!(lua.contains("function describe(a)"), "got: {lua}");
    assert!(lua.contains("a:speak()"), "got: {lua}");
}

#[test]
fn generic_bound_method_wrong_arity_rejected() {
    let err = compile_str(
        r#"
        trait Animal { fn speak(&self) -> String; }
        fn describe<T: Animal>(a: T) -> String { a.speak(1) }
        fn main() {}
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("`Animal::speak` expects 0 argument(s), got 1"),
        "got: {err}"
    );
}

#[test]
fn generic_bound_method_wrong_arg_type_rejected() {
    let err = compile_str(
        r#"
        trait Adder { fn add_i(&self, x: i64) -> i64; }
        fn use_it<T: Adder>(a: T) -> i64 { a.add_i(true) }
        fn main() {}
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("argument 1 of `Adder::add_i` expects `i64`, found `bool`"),
        "got: {err}"
    );
}

#[test]
fn method_not_in_bound_is_silent() {
    // A method the bound does not declare stays Unknown (no false positive).
    let lua = compile(
        r#"
        trait Animal { fn speak(&self) -> String; }
        fn describe<T: Animal>(a: T) { a.wander(1, 2, 3); }
        fn main() {}
    "#,
    );
    assert!(lua.contains("a:wander(1, 2, 3)"), "got: {lua}");
}

#[test]
fn unknown_trait_bound_rejected() {
    let err = compile_str("fn f<T: Bogus>(x: T) {}\nfn main() {}").unwrap_err();
    assert!(err.contains("unknown trait `Bogus`"), "got: {err}");
}

#[test]
fn builtin_trait_bounds_ok() {
    let lua = compile("fn f<T: Clone + Debug>(x: T) {}\nfn main() {}");
    assert!(lua.contains("function f(x)"), "got: {lua}");
}

#[test]
fn generic_return_type_no_false_positive() {
    // `T` return with a `T` tail must not raise a return-type mismatch.
    let lua = compile(
        r#"
        fn first<T>(a: T, b: T) -> T { a }
        fn main() {}
    "#,
    );
    assert!(lua.contains("function first(a, b)"), "got: {lua}");
    assert!(lua.contains("return a"), "got: {lua}");
}

#[test]
fn impl_generic_bound_in_module_ok() {
    // Generics + bounds resolve inside modules too.
    let lua = compile(
        r#"
        mod z {
            pub trait Named { fn name(&self) -> String; }
            pub fn label<T: Named>(x: T) -> String { x.name() }
        }
        fn main() {}
    "#,
    );
    assert!(lua.contains("function label(x)"), "got: {lua}");
    assert!(lua.contains("x:name()"), "got: {lua}");
}

// --- P5c-5: `where` clauses + call-site constraint satisfaction -------------

#[test]
fn where_clause_bounds_merge_and_resolve() {
    // A `where`-declared bound behaves exactly like an inline `<T: Animal>`.
    let lua = compile(
        r#"
        trait Animal { fn speak(&self) -> String; }
        fn describe<T>(a: T) -> String where T: Animal { a.speak() }
        fn main() {}
    "#,
    );
    assert!(lua.contains("function describe(a)"), "got: {lua}");
    assert!(lua.contains("a:speak()"), "got: {lua}");
}

#[test]
fn where_clause_unknown_trait_rejected() {
    let err = compile_str("fn f<T>(x: T) where T: Bogus {}\nfn main() {}").unwrap_err();
    assert!(err.contains("unknown trait `Bogus`"), "got: {err}");
}

#[test]
fn where_clause_on_impl_ok() {
    let lua = compile(
        r#"
        trait Named { fn name(&self) -> String; }
        struct Pair<T> { a: T, b: T }
        impl<T> Pair<T> where T: Named {
            fn describe(&self) -> String { self.a.name() }
        }
        fn main() {}
    "#,
    );
    assert!(lua.contains("function Pair:describe()"), "got: {lua}");
}

#[test]
fn call_site_bound_satisfied_ok() {
    let lua = compile(
        r#"
        trait Animal { fn speak(&self) -> String; }
        struct Dog { n: i64 }
        impl Animal for Dog { fn speak(&self) -> String { format!("woof") } }
        fn describe<T: Animal>(a: T) -> String { a.speak() }
        fn main() { let _ = describe(Dog { n: 1 }); }
    "#,
    );
    assert!(lua.contains("describe("), "got: {lua}");
}

#[test]
fn call_site_bound_unsatisfied_rejected() {
    let err = compile_str(
        r#"
        trait Animal { fn speak(&self) -> String; }
        struct Cat { n: i64 }
        fn describe<T: Animal>(a: T) {}
        fn main() { describe(Cat { n: 1 }); }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("type `Cat` does not implement trait `Animal`"),
        "got: {err}"
    );
}

#[test]
fn call_site_where_bound_unsatisfied_rejected() {
    // The `where` form is enforced at call sites just like the inline form.
    let err = compile_str(
        r#"
        trait Animal { fn speak(&self) -> String; }
        struct Cat { n: i64 }
        fn describe<T>(a: T) where T: Animal {}
        fn main() { describe(Cat { n: 1 }); }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("type `Cat` does not implement trait `Animal`"),
        "got: {err}"
    );
}

#[test]
fn call_site_builtin_bound_not_checked() {
    // Builtin traits cannot be verified, so no false positive even without impl.
    let lua = compile(
        r#"
        struct Thing { n: i64 }
        fn f<T: Clone>(x: T) {}
        fn main() { f(Thing { n: 1 }); }
    "#,
    );
    assert!(lua.contains("function f(x)"), "got: {lua}");
}

#[test]
fn call_site_scalar_arg_not_checked() {
    // A non-user-type argument (scalar) is never flagged (conservative).
    let lua = compile(
        r#"
        trait Animal { fn speak(&self) -> String; }
        fn describe<T: Animal>(a: T) {}
        fn main() { describe(5); }
    "#,
    );
    assert!(lua.contains("describe(5)"), "got: {lua}");
}

// --- method-level generics + call-site checking -----------------------------

#[test]
fn generic_method_bound_satisfied_ok() {
    let lua = compile(
        r#"
        trait Shape { fn area(&self) -> f64; }
        struct Circle { r: f64 }
        impl Shape for Circle { fn area(&self) -> f64 { 1.0 } }
        struct Registry { n: i64 }
        impl Registry {
            fn add<T: Shape>(&self, s: T) {}
        }
        fn main() {
            let reg = Registry { n: 0 };
            reg.add(Circle { r: 1.0 });
        }
    "#,
    );
    assert!(lua.contains("function Registry:add(s)"), "got: {lua}");
}

#[test]
fn generic_method_bound_unsatisfied_rejected() {
    let err = compile_str(
        r#"
        trait Shape { fn area(&self) -> f64; }
        struct Square { s: f64 }
        struct Registry { n: i64 }
        impl Registry {
            fn add<T: Shape>(&self, s: T) {}
        }
        fn main() {
            let reg = Registry { n: 0 };
            reg.add(Square { s: 1.0 });
        }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("type `Square` does not implement trait `Shape`"),
        "got: {err}"
    );
}

#[test]
fn generic_method_return_substituted() {
    let err = compile_str(
        r#"
        struct Boxx { n: i64 }
        impl Boxx {
            fn wrap<U>(&self, x: U) -> U { x }
        }
        fn main() {
            let b = Boxx { n: 0 };
            let _y: bool = b.wrap(1);
        }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("annotated as `bool` but initialized with `i64`"),
        "got: {err}"
    );
}

#[test]
fn generic_method_builtin_bound_not_checked() {
    let lua = compile(
        r#"
        struct Thing { n: i64 }
        struct Reg { n: i64 }
        impl Reg {
            fn add<T: Clone>(&self, x: T) {}
        }
        fn main() {
            let r = Reg { n: 0 };
            r.add(Thing { n: 1 });
        }
    "#,
    );
    assert!(lua.contains("function Reg:add(x)"), "got: {lua}");
}

// --- cross-module type-name dedup -------------------------------------------

#[test]
fn same_struct_name_in_two_modules_no_false_positive() {
    // Two modules each declaring `Point` with different fields must compile;
    // the checker degrades an ambiguous simple name to Unknown (no field error).
    let lua = compile(
        r#"
        mod a { pub struct Point { pub x: i64 } }
        mod b { pub struct Point { pub y: i64 } }
        fn main() {
            let _p = a::Point { x: 1 };
            let _q = b::Point { y: 2 };
        }
    "#,
    );
    // Runtime uses distinct qualified class tables.
    assert!(lua.contains("a.Point"), "got: {lua}");
    assert!(lua.contains("b.Point"), "got: {lua}");
}

#[test]
fn unique_struct_field_still_checked() {
    // Regression: a uniquely-named struct still gets field validation.
    let err = compile_str(
        r#"
        struct Solo { x: i64 }
        fn main() { let _ = Solo { y: 1 }; }
    "#,
    )
    .unwrap_err();
    assert!(err.contains("has no field `y`"), "got: {err}");
}

#[test]
fn same_fn_name_in_two_modules_no_false_positive() {
    // Same free-fn name in two modules with different arity must not be flagged
    // when called through qualified paths.
    let lua = compile(
        r#"
        mod a { pub fn f(x: i64) -> i64 { x } }
        mod b { pub fn f(x: i64, y: i64) -> i64 { x + y } }
        fn main() {
            let _ = a::f(1);
            let _ = b::f(1, 2);
        }
    "#,
    );
    assert!(lua.contains("a.f"), "got: {lua}");
    assert!(lua.contains("b.f"), "got: {lua}");
}

#[test]
fn generic_return_type_substituted_at_call_site() {
    // `id(1)` returns `i64`, so binding it to a `bool` must be a mismatch.
    let err = compile_str(
        r#"
        fn id<T>(x: T) -> T { x }
        fn main() { let _y: bool = id(1); }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("annotated as `bool` but initialized with `i64`"),
        "got: {err}"
    );
}

// --- P4c-7: `.ruai` declaration files + qualified function-call checking ---

#[test]
fn qualified_fn_call_arity_checked() {
    // A module-qualified call is now checked against the callee's signature.
    let err = compile_str(
        r#"
        mod m { pub fn add(a: i64, b: i64) -> i64 { a + b } }
        fn main() { let _x = m::add(1); }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("function `m::add` expects 2 argument"),
        "got: {err}"
    );
}

#[test]
fn qualified_extern_call_arity_checked() {
    let err = compile_str(
        r#"
        mod host { extern "lua" { fn log(msg: String); } }
        fn main() { host::log("a", "b"); }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("function `host::log` expects 1 argument"),
        "got: {err}"
    );
}

#[test]
fn qualified_extern_arg_type_checked() {
    let err = compile_str(
        r#"
        mod host { extern "lua" { fn log(msg: String); } }
        fn main() { host::log(42); }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("argument 1 of `host::log` expects `String`"),
        "got: {err}"
    );
}

#[test]
fn qualified_extern_ok_and_return_type_flows() {
    // Correct calls compile; the declared `-> i64` return type flows into `let`.
    let lua = compile(
        r#"
        mod host { extern "lua" { fn log(msg: String); fn time() -> i64; } }
        fn main() { host::log("hi"); let _t: i64 = host::time(); }
    "#,
    );
    assert!(lua.contains("host.log(\"hi\")"), "got: {lua}");
    // A return-type mismatch on the same signature must be rejected.
    let err = compile_str(
        r#"
        mod host { extern "lua" { fn time() -> i64; } }
        fn main() { let _t: bool = host::time(); }
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("annotated as `bool` but initialized with `i64`"),
        "got: {err}"
    );
}

#[test]
fn variadic_extern_not_arity_checked() {
    // Variadic extern fns are left unchecked (any arity accepted).
    let lua = compile(
        r#"
        mod host { extern "lua" { fn printf(fmt: String, ...); } }
        fn main() { host::printf("%d %d", 1, 2); }
    "#,
    );
    assert!(lua.contains("host.printf(\"%d %d\", 1, 2)"), "got: {lua}");
}

#[test]
fn ruai_module_omitted_from_codegen() {
    // A `.ruai` file is a declaration-only module: the checker knows its
    // signatures, but codegen emits no Lua and references hit the host global.
    let dir = tmp_dir("ruai_codegen");
    std::fs::write(
        dir.join("main.rua"),
        "mod moon;\nfn main() { moon::log(\"hi\"); }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("moon.ruai"),
        "extern \"lua\" { fn log(msg: String); }\n",
    )
    .unwrap();
    let lua = crate::compile_path(&dir.join("main.rua")).unwrap();
    assert!(lua.contains("moon.log(\"hi\")"), "got: {lua}");
    // The module must NOT be declared as a local nor defined as a table.
    assert!(!lua.contains("moon = {}"), "got: {lua}");
    assert!(!lua.contains("local main, moon"), "got: {lua}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ruai_declaration_type_checked() {
    // Calls against a `.ruai` declaration are arity/type checked.
    let dir = tmp_dir("ruai_check");
    std::fs::write(
        dir.join("main.rua"),
        "mod moon;\nfn main() { moon::log(1, 2); }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("moon.ruai"),
        "extern \"lua\" { fn log(msg: String); }\n",
    )
    .unwrap();
    let err = crate::compile_path(&dir.join("main.rua")).unwrap_err();
    assert!(
        err.contains("function `moon::log` expects 1 argument"),
        "got: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// --- P5c-7: trait-method-level generics ---

#[test]
fn trait_method_generic_bound_satisfied_ok() {
    // `store<U: Show>` called with a type that DOES implement `Show`.
    let lua = compile(
        r#"
        trait Show { fn show(&self) -> String; }
        trait Container { fn store<U: Show>(&self, x: U); }
        struct Plain {}
        impl Show for Plain { fn show(&self) -> String { "p" } }
        fn use_it<T: Container>(c: T, p: Plain) { c.store(p); }
        fn main() {}
    "#,
    );
    assert!(lua.contains("function use_it(c, p)"), "got: {lua}");
}

#[test]
fn trait_method_generic_bound_unsatisfied_rejected() {
    // `store<U: Show>` called with `Plain`, which does NOT implement `Show`.
    let err = compile_str(
        r#"
        trait Show { fn show(&self) -> String; }
        trait Container { fn store<U: Show>(&self, x: U); }
        struct Plain {}
        fn use_it<T: Container>(c: T, p: Plain) { c.store(p); }
        fn main() {}
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("type `Plain` does not implement trait `Show`")
            && err.contains("Container::store"),
        "got: {err}"
    );
}

#[test]
fn trait_method_generic_return_substituted() {
    // `wrap<U>(x: U) -> U` — passing `1` yields `i64`, so a `bool` binding fails.
    let err = compile_str(
        r#"
        trait Wrapper { fn wrap<U>(&self, x: U) -> U; }
        fn use_it<T: Wrapper>(w: T) { let _y: bool = w.wrap(1); }
        fn main() {}
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("annotated as `bool` but initialized with `i64`"),
        "got: {err}"
    );
}

#[test]
fn trait_method_generic_builtin_bound_not_checked() {
    // A builtin bound (`Clone`) on a trait-method generic is never verified,
    // so no false positive even though `Plain` has no explicit impl.
    let lua = compile(
        r#"
        trait Container { fn store<U: Clone>(&self, x: U); }
        struct Plain {}
        fn use_it<T: Container>(c: T, p: Plain) { c.store(p); }
        fn main() {}
    "#,
    );
    assert!(lua.contains("function use_it(c, p)"), "got: {lua}");
}

#[test]
fn trait_method_generic_unknown_bound_rejected() {
    // An unknown trait in a trait-method generic bound is now caught (previously
    // the method generics were dropped at parse time).
    let err = compile_str(
        r#"
        trait Foo { fn bar<U: Bogus>(&self, x: U); }
        fn main() {}
    "#,
    )
    .unwrap_err();
    assert!(
        err.contains("unknown trait `Bogus` in bound `U: Bogus`"),
        "got: {err}"
    );
}

// --- check_diags: byte-offset spans for LSP -------------------------------

#[test]
fn check_diags_reports_located_span_for_struct_field() {
    // A struct literal with an unknown field has an expression span, so the
    // diagnostic must carry a precise byte range that points at the source.
    let src = "struct P { x: i64 }\nfn main() { let _ = P { y: 1 }; }";
    let (diags, _files) = crate::check_diags(src);
    let d = diags
        .iter()
        .find(|d| d.msg.contains("has no field `y`"))
        .expect("expected a `has no field` diagnostic");
    assert!(d.len > 0, "located diag should carry a non-empty span: {d:?}");
    // The span must fall inside the source and cover part of the literal.
    assert!(d.start + d.len <= src.len(), "span out of bounds: {d:?}");
    assert!(d.line >= 1, "line should be 1-based: {d:?}");
    // The byte range should intersect the struct-literal text on line 2.
    let snippet = &src[d.start..d.start + d.len];
    assert!(
        snippet.contains('P') || snippet.contains('y') || snippet.contains('{'),
        "span {snippet:?} should point at the struct literal"
    );
}

#[test]
fn check_diags_bare_for_duplicate_definition() {
    // Duplicate top-level definitions have no AST span, so the diagnostic is
    // bare: start == len == 0 (rendered whole-file / at file top by the LSP).
    let src = "fn dup() {}\nfn dup() {}\nfn main() {}";
    let (diags, _files) = crate::check_diags(src);
    let d = diags
        .iter()
        .find(|d| d.msg.contains("duplicate top-level definition `dup`"))
        .expect("expected a duplicate-definition diagnostic");
    assert_eq!(d.start, 0, "bare diag has no start: {d:?}");
    assert_eq!(d.len, 0, "bare diag has no length: {d:?}");
}

#[test]
fn check_diags_locates_match_pattern_error() {
    // Pattern diagnostics carry the enclosing match arm's span as a fallback,
    // so they no longer degrade to a bare (file-top) diagnostic.
    let src = "enum E { A, B(i64) }\nfn main() { let e = E::A; match e { A(x) => x, _ => 0 }; }";
    let (diags, _files) = crate::check_diags(src);
    let d = diags
        .iter()
        .find(|d| d.msg.contains("has no tuple payload"))
        .expect("expected a unit-variant payload diagnostic");
    assert!(d.len > 0, "pattern diag should now be located: {d:?}");
    assert!(d.start + d.len <= src.len(), "span out of bounds: {d:?}");
}

#[test]
fn check_diags_clean_program_has_no_diags() {
    let src = "fn add(a: i64, b: i64) -> i64 { a + b }\nfn main() { let _ = add(1, 2); }";
    let (diags, _files) = crate::check_diags(src);
    assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
}

// --- B1: member_index (LSP member resolution) --------------------------------

use crate::typeck::MemberKind;

/// Byte offset of the `n`-th (0-based) whole-word occurrence of `needle`.
fn nth_word(src: &str, needle: &str, n: usize) -> usize {
    let b = src.as_bytes();
    let is_id = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut rem = n;
    let mut pos = 0;
    loop {
        let off = pos + src[pos..].find(needle).expect("occurrence not found");
        let after = off + needle.len();
        let left = off == 0 || !is_id(b[off - 1]);
        let right = after >= b.len() || !is_id(b[after]);
        if left && right {
            if rem == 0 {
                return off;
            }
            rem -= 1;
        }
        pos = off + 1;
    }
}

#[test]
fn member_index_resolves_struct_field() {
    let src = "struct Point { x: f64, y: f64 }\nfn main() { let p = Point { x: 1.0, y: 2.0 }; let _ = p.x; }";
    let mi = crate::member_index(src);
    // Cursor on the `x` in `p.x` (the use site — last occurrence of `x`).
    let use_off = nth_word(src, "x", 2);
    let hit = mi.at(0, use_off).expect("p.x should resolve");
    assert_eq!(hit.kind, MemberKind::Field);
    // Target points at the field definition `x` (first occurrence).
    let def = nth_word(src, "x", 0);
    assert_eq!(hit.target_start, def);
    assert_eq!(&src[hit.target_start..hit.target_start + hit.target_len], "x");
    assert_eq!(hit.detail, "x: f64");
}

#[test]
fn member_index_resolves_method_call() {
    let src = "struct P { v: i64 }\nimpl P {\n    fn get(&self) -> i64 { self.v }\n}\nfn main() { let p = P { v: 1 }; let _ = p.get(); }";
    let mi = crate::member_index(src);
    // Cursor on `get` in `p.get()` (second `get`; first is the definition).
    let use_off = nth_word(src, "get", 1);
    let hit = mi.at(0, use_off).expect("p.get() should resolve");
    assert_eq!(hit.kind, MemberKind::Method);
    let def = nth_word(src, "get", 0);
    assert_eq!(hit.target_start, def);
    assert_eq!(hit.detail, "fn get(&self) -> i64");
}

#[test]
fn member_index_resolves_self_field() {
    let src = "struct P { v: i64 }\nimpl P {\n    fn read(&self) -> i64 { self.v }\n}\n";
    let mi = crate::member_index(src);
    // `self.v` — cursor on the `v` use site (second `v`; first is field def).
    let use_off = nth_word(src, "v", 1);
    let hit = mi.at(0, use_off).expect("self.v should resolve");
    assert_eq!(hit.kind, MemberKind::Field);
    assert_eq!(hit.detail, "v: i64");
}

#[test]
fn member_index_records_builtin_vec_and_string_methods() {
    // Vec::push, String::to_string, and String::len are now recorded with
    // zero-length sentinel target spans so the LSP can show hover detail
    // (there is no source definition to jump to; go-to-def is suppressed).
    let src = "fn main() { let v = vec![1, 2]; v.push(3); let s = \"hi\".to_string(); let _ = s.len(); }";
    let mi = crate::member_index(src);
    assert_eq!(mi.len(), 3, "builtin/std member calls must be recorded: {:?}", mi.hits());
    // Each hit should have a zero-length target span (sentinel: no jump target).
    for hit in mi.hits() {
        assert_eq!(hit.target_len, 0, "builtin method '{}' should have zero target_len", hit.detail);
        assert!(!hit.detail.is_empty(), "builtin method should have hover detail");
    }
    // Verify specific details.
    let push = mi.hits().iter().find(|h| h.detail.contains("push")).unwrap();
    assert!(push.detail.contains("Vec") || push.detail.contains("i64"), "push detail: {}", push.detail);
    let to_string = mi.hits().iter().find(|h| h.detail.contains("to_string")).unwrap();
    assert!(to_string.detail.contains("String"), "to_string detail: {}", to_string.detail);
    let len = mi.hits().iter().find(|h| h.detail.contains("len(&self)")).unwrap();
    assert!(len.detail.contains("i64"), "len detail: {}", len.detail);
}

#[test]
fn member_index_skips_unknown_receiver() {
    // Field access on an unknown-typed receiver is not recorded.
    let src = "fn main() { let _ = foo().bar; }";
    let mi = crate::member_index(src);
    assert!(mi.is_empty(), "unknown receiver must not be recorded: {:?}", mi.hits());
}

#[test]
fn member_index_parse_error_is_empty() {
    let mi = crate::member_index("fn (((");
    assert!(mi.is_empty());
}

// --- C0: type_members (member-completion catalog) ----------------------------

#[test]
fn type_members_lists_struct_fields_and_methods() {
    let src = "struct Point { x: f64, y: f64 }\nimpl Point {\n    fn dist(&self) -> f64 { self.x }\n    fn scale(&self, k: f64) -> f64 { self.x }\n}";
    let tm = crate::type_members(src);
    let got: Vec<(&str, MemberKind)> = tm
        .get("Point")
        .iter()
        .map(|m| (m.name.as_str(), m.kind))
        .collect();
    assert_eq!(
        got,
        vec![
            ("x", MemberKind::Field),
            ("y", MemberKind::Field),
            ("dist", MemberKind::Method),
            ("scale", MemberKind::Method),
        ]
    );
    let f = tm.get("Point").iter().find(|m| m.name == "x").unwrap();
    assert_eq!(f.detail, "x: f64");
    let d = tm.get("Point").iter().find(|m| m.name == "dist").unwrap();
    assert_eq!(d.detail, "fn dist(&self) -> f64");
}

#[test]
fn type_members_includes_trait_default_and_impl_methods() {
    let src = "trait Area {\n    fn area(&self) -> f64;\n    fn name(&self) -> String { \"s\".to_string() }\n}\nstruct C { r: f64 }\nimpl Area for C {\n    fn area(&self) -> f64 { self.r }\n}";
    let tm = crate::type_members(src);
    let names: Vec<&str> = tm.get("C").iter().map(|m| m.name.as_str()).collect();
    assert!(names.contains(&"r")); // field
    assert!(names.contains(&"area")); // impl method
    assert!(names.contains(&"name")); // inherited default
}

#[test]
fn type_members_enum_methods_only() {
    let src = "enum Dir { N, S }\nimpl Dir {\n    fn flip(&self) -> i64 { 0 }\n}";
    let tm = crate::type_members(src);
    let ms = tm.get("Dir");
    assert_eq!(ms.len(), 1);
    assert_eq!(ms[0].name, "flip");
    assert_eq!(ms[0].kind, MemberKind::Method);
}

#[test]
fn type_members_unknown_and_builtins_are_empty() {
    let tm = crate::type_members("fn f() { let v = vec![1]; }");
    assert!(tm.get("Nope").is_empty());
    assert!(tm.get("Vec").is_empty()); // builtin container never in catalog
}

#[test]
fn type_members_parse_error_is_empty() {
    assert!(crate::type_members("struct {{{").is_empty());
}

#[test]
fn type_members_drops_ambiguous_cross_module_type() {
    // Same simple name in two modules → dropped (zero false positives).
    let src = "mod a { pub struct P { pub x: i64 } }\nmod b { pub struct P { pub y: i64 } }";
    assert!(crate::type_members(src).get("P").is_empty());
}

// --- C1: member_completion (ReceiverIndex + TypeMembers) --------------------

#[test]
fn member_completion_records_field_receiver() {
    let src =
        "struct Point { x: f64 }\nfn main() { let p = Point { x: 1.0 }; let _ = p.x; }";
    let (tm, ri) = crate::member_completion(src);
    let p = nth_word(src, "p", 1); // `p` in `p.x`
    let r = ri.at_end(0, p + 1).expect("receiver p recorded"); // len("p")==1
    assert_eq!(r.type_name, "Point");
    assert!(!tm.get("Point").is_empty());
}

#[test]
fn member_completion_records_self_receiver() {
    let src = "struct P { v: i64 }\nimpl P {\n    fn get(&self) -> i64 { self.v }\n}";
    let (_, ri) = crate::member_completion(src);
    let s = nth_word(src, "self", 1); // `self` in `self.v` (0 is `&self`)
    let r = ri.at_end(0, s + "self".len()).expect("self receiver recorded");
    assert_eq!(r.type_name, "P");
}

#[test]
fn member_completion_records_method_call_receiver() {
    let src = "struct P { v: i64 }\nimpl P {\n    fn get(&self) -> i64 { self.v }\n}\nfn main() { let p = P { v: 1 }; let _ = p.get(); }";
    let (_, ri) = crate::member_completion(src);
    let p = nth_word(src, "p", 1); // `p` in `p.get()`
    let r = ri
        .at_end(0, p + 1)
        .expect("method-call receiver recorded");
    assert_eq!(r.type_name, "P");
}

#[test]
fn member_completion_nested_chain_by_end() {
    let src = "struct Inner { z: i64 }\nstruct Outer { inner: Inner }\nfn main() { let o = Outer { inner: Inner { z: 1 } }; let _ = o.inner.z; }";
    let (_, ri) = crate::member_completion(src);
    let pos = src.find("o.inner.z").unwrap();
    // receiver of `.inner` is `o` (Outer); end at end of `o`.
    assert_eq!(ri.at_end(0, pos + 1).unwrap().type_name, "Outer");
    // receiver of `.z` is `o.inner` (Inner); end at end of `inner`.
    assert_eq!(
        ri.at_end(0, pos + "o.inner".len()).unwrap().type_name,
        "Inner"
    );
}

#[test]
fn member_completion_skips_unknown_receiver() {
    let (_, ri1) = crate::member_completion("fn main() { let q = bogus(); let _ = q.z; }");
    assert!(ri1.is_empty()); // Unknown receiver -> not recorded
}

#[test]
fn member_completion_records_builtin_vec_receiver() {
    // Vec receivers are now recorded (with a built-in member catalog) so `v.`
    // completion lists Vec methods.
    let src = "fn main() { let v = vec![1, 2]; v.push(3); }";
    let p = nth_word(src, "v", 1); // `v` in `v.push`
    let (tm, ri) = crate::member_completion(src);
    let r = ri.at_end(0, p + 1).expect("Vec receiver recorded"); // len("v")==1
    assert_eq!(r.type_name, "Vec<i64>");
    let members = tm.get("Vec<i64>");
    let names: Vec<&str> = members.iter().map(|m| m.name.as_str()).collect();
    assert!(names.contains(&"push"), "Vec methods: {names:?}");
    assert!(names.contains(&"len"));
    assert!(names.contains(&"get"));
}

#[test]
fn member_completion_records_builtin_string_receiver() {
    let src = "fn f(s: String) { let _ = s.len(); }";
    let p = nth_word(src, "s", 1); // `s` in `s.len`
    let (tm, ri) = crate::member_completion(src);
    let r = ri.at_end(0, p + 1).expect("String receiver recorded");
    assert_eq!(r.type_name, "String");
    let names: Vec<&str> = tm.get("String").iter().map(|m| m.name.as_str()).collect();
    assert!(names.contains(&"trim"), "String methods: {names:?}");
    assert!(names.contains(&"to_uppercase"));
    assert!(names.contains(&"split"));
}

#[test]
fn member_completion_parse_error_is_empty() {
    let (tm, ri) = crate::member_completion("struct {{{");
    assert!(tm.is_empty() && ri.is_empty());
}

/// The recorded hover text for the binding whose display starts with `prefix`.
fn binding_display(src: &str, prefix: &str) -> String {
    let bt = crate::binding_types(src);
    bt.hits()
        .iter()
        .find(|b| b.display.starts_with(prefix))
        .unwrap_or_else(|| panic!("no binding starting with {prefix:?} in {:?}", bt.hits()))
        .display
        .clone()
}

#[test]
fn empty_hashmap_infers_kv_from_insert() {
    let src = "fn main() { let mut scores = HashMap::new(); scores.insert(\"alice\", 10); }";
    assert_eq!(
        binding_display(src, "let mut scores"),
        "let mut scores: HashMap<String, i64>"
    );
}

#[test]
fn empty_vec_infers_element_from_push() {
    let src = "fn main() { let mut xs = Vec::new(); xs.push(1); }";
    assert_eq!(binding_display(src, "let mut xs"), "let mut xs: Vec<i64>");
}

#[test]
fn empty_hashmap_infers_value_from_local_var() {
    // Value comes from an already-bound local (side-effect-free quick type).
    let src = "fn main() { let n = 3; let mut m = HashMap::new(); m.insert(\"k\", n); }";
    assert_eq!(
        binding_display(src, "let mut m"),
        "let mut m: HashMap<String, i64>"
    );
}

#[test]
fn empty_hashmap_without_insert_stays_unknown() {
    // No inserts: key/value remain `?` (no invented types).
    let src = "fn main() { let mut m = HashMap::new(); }";
    assert_eq!(binding_display(src, "let mut m"), "let mut m: HashMap<?, ?>");
}

#[test]
fn annotated_hashmap_is_not_overridden_by_usage() {
    // An explicit annotation wins; usage-based refinement only applies to
    // inferred bindings.
    let src = "fn main() { let mut m: HashMap<String, bool> = HashMap::new(); m.insert(\"k\", 1); }";
    assert_eq!(
        binding_display(src, "let mut m"),
        "let mut m: HashMap<String, bool>"
    );
}

#[test]
fn hashmap_insert_value_type_mismatch_is_reported() {
    // scores inferred as HashMap<String, i64>; inserting a String value errors.
    let src = "fn main() {\n    let mut scores = HashMap::new();\n    scores.insert(\"alice\", 10);\n    scores.insert(\"bob\", \"world\");\n}";
    let (diags, _) = crate::check_diags(src);
    assert!(
        diags.iter().any(|d| d.msg.contains("HashMap value type mismatch")
            && d.msg.contains("expected `i64`")
            && d.msg.contains("found `String`")),
        "expected a HashMap value mismatch, got {:?}",
        diags.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
}

#[test]
fn hashmap_insert_matching_types_ok() {
    let src = "fn main() {\n    let mut scores = HashMap::new();\n    scores.insert(\"alice\", 10);\n    scores.insert(\"bob\", 20);\n}";
    let (diags, _) = crate::check_diags(src);
    assert!(
        diags.iter().all(|d| !d.msg.contains("mismatch")),
        "matching inserts must not error, got {:?}",
        diags.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
}

#[test]
fn hashmap_key_type_mismatch_is_reported() {
    let src = "fn main() {\n    let mut m = HashMap::new();\n    m.insert(\"a\", 1);\n    m.insert(2, 3);\n}";
    let (diags, _) = crate::check_diags(src);
    assert!(
        diags.iter().any(|d| d.msg.contains("HashMap key type mismatch")),
        "expected a HashMap key mismatch, got {:?}",
        diags.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
}

#[test]
fn vec_push_type_mismatch_is_reported() {
    let src = "fn main() {\n    let mut xs = Vec::new();\n    xs.push(1);\n    xs.push(\"nope\");\n}";
    let (diags, _) = crate::check_diags(src);
    assert!(
        diags.iter().any(|d| d.msg.contains("Vec element type mismatch")
            && d.msg.contains("expected `i64`")
            && d.msg.contains("found `String`")),
        "expected a Vec element mismatch, got {:?}",
        diags.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
}

#[test]
fn uninferred_hashmap_does_not_flag_inserts() {
    // No first insert to fix the value type: stays HashMap<String, ?>, so mixed
    // value types are not flagged (no false positives on `?`).
    let src = "fn f(m: HashMap<String, i64>) {}\nfn main() {\n    let mut m = HashMap::new();\n    m.insert(\"a\", true);\n}";
    // The single insert fixes value = bool; only later conflicting inserts would
    // error. A lone insert never mismatches itself.
    let (diags, _) = crate::check_diags(src);
    assert!(
        diags.iter().all(|d| !d.msg.contains("mismatch")),
        "a single insert cannot mismatch itself, got {:?}",
        diags.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
}
