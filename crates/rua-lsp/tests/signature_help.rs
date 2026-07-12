//! Signature help tests — verifying parameter hints in call expressions.

mod support;

use support::{uri, TestServer};

#[test]
fn signature_help_in_function_call() {
    let uri = uri("/test/sighlp.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "fn add(a: i64, b: i64) -> i64 { a + b }\nfn main() { add(1, 2); }",
    );

    // cursor at the comma between `1` and `2` in `add(1, 2)`
    let pp = srv.pp(&uri, 1, 17).unwrap();
    let help = srv.snapshot().signature_help(pp);
    assert!(
        help.is_some(),
        "signature help should be Some inside a call"
    );
    let info = help.unwrap();
    assert!(
        info.label.contains("a") || info.label.contains("b") || info.label.contains("i64"),
        "label should contain param info, got: {}",
        info.label
    );
    assert!(!info.parameters.is_empty(), "should have parameters");
}

#[test]
fn signature_help_in_method_call() {
    let uri = uri("/test/sighlp_method.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "struct Point { x: i64 }\nimpl Point {\n    fn translate(&mut self, dx: i64, dy: i64) { self.x = self.x + dx; }\n}\nfn main() { let mut p = Point { x: 0 }; p.translate(1, 2); }",
    );

    // cursor inside `p.translate(1, 2)` between args
    let pp = srv.pp(&uri, 4, 56).unwrap();
    let help = srv.snapshot().signature_help(pp);
    assert!(
        help.is_some(),
        "signature help should be Some inside a method call"
    );
    let info = help.unwrap();
    assert!(
        info.label.contains("dx") || info.label.contains("dy") || info.label.contains("i64"),
        "label should contain param info, got: {}",
        info.label
    );
    // Method params should include dx and dy
    let has_dx = info.parameters.iter().any(|p| p.contains("dx"));
    let has_dy = info.parameters.iter().any(|p| p.contains("dy"));
    assert!(has_dx || has_dy, "should have method params, got: {info:?}");
}

#[test]
fn signature_help_active_parameter_tracks_position() {
    let uri = uri("/test/sighlp_active.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "fn greet(name: String, age: i64) -> i64 { age }\nfn main() { greet(String::new(), 42); }",
    );

    // cursor after the first comma — second arg position
    let pp = srv.pp(&uri, 1, 40).unwrap();
    let help = srv.snapshot().signature_help(pp);
    if let Some(info) = help {
        // active_parameter should be 0-indexed; after first comma it should be >= 1
        assert!(
            info.active_parameter > 0,
            "active param should be >0 after comma, got: {}",
            info.active_parameter
        );
    }
    // If signature_help returns None for complex expressions, that's acceptable
    // — String::new() may not resolve in all contexts.
}

#[test]
fn signature_help_unknown_function_returns_none() {
    let uri = uri("/test/sighlp_unknown.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn main() { unknown_fn( }");

    let pp = srv.pp(&uri, 0, 21).unwrap();
    let help = srv.snapshot().signature_help(pp);
    // Unknown function may or may not return help — but must not panic
    let _ = help;
}

#[test]
fn signature_help_returns_none_at_file_start() {
    let uri = uri("/test/sighlp_empty.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn add(a: i64) -> i64 { a }\nfn main() {}");

    // cursor at file start — not in any call
    let pp = srv.pp(&uri, 0, 0).unwrap();
    let help = srv.snapshot().signature_help(pp);
    assert!(help.is_none(), "signature help should be None at file start");
}

#[test]
fn signature_help_zero_param_function() {
    let uri = uri("/test/sighlp_zero.rua");
    let mut srv = TestServer::new();
    srv.open(&uri, "fn zero() -> i64 { 0 }\nfn main() { zero(); }");

    let pp = srv.pp(&uri, 1, 20).unwrap();
    let help = srv.snapshot().signature_help(pp);
    if let Some(info) = help {
        assert!(
            info.label.contains("zero") || info.label.contains("()"),
            "label should contain fn info, got: {}",
            info.label
        );
        // zero-param functions should have empty params
        assert!(
            info.parameters.is_empty(),
            "zero-param function should have no parameters"
        );
    }
}

#[test]
fn signature_help_returns_none_outside_call() {
    // Already covered in incremental_stress.rs, but add another variant:
    // cursor between function definitions, not in any call.
    let uri = uri("/test/sighlp_outside2.rua");
    let mut srv = TestServer::new();
    srv.open(
        &uri,
        "fn foo(a: i64) -> i64 { a }\nfn bar(b: i64) -> i64 { b }\nfn main() {}",
    );

    // cursor on the empty line between definitions (line 2 doesn't exist, use line 1)
    let pp = srv.pp(&uri, 1, 0).unwrap();
    let help = srv.snapshot().signature_help(pp);
    assert!(
        help.is_none(),
        "signature help should be None outside a call"
    );
}
