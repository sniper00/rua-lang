//! Rua LSP server binary entry point.
//!
//! The `required-features = ["lsp"]` in `Cargo.toml` ensures this binary is
//! only compiled when the `lsp` feature is active, keeping the `rua-fmt`
//! binary dependency-free.

fn main() {
    rua_ide::lsp::main_loop();
}
