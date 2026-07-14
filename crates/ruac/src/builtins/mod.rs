//! Builtin type declarations loaded from `.ruai` sources.
//!
//! The default sysroot is embedded in the compiler so library use never depends
//! on the process working directory. Hosts and the CLI may explicitly replace it
//! with a directory; explicit directories are validated strictly.
//!
//! Code generation rules that can't be expressed in .ruai syntax (e.g. `None`
//! compiles to `nil`) live separately in `CodegenRules`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Code generation rules
// ---------------------------------------------------------------------------

/// Describes how a builtin path or method generates Lua code.
#[derive(Clone, Debug)]
pub enum CodegenRule {
    /// Replace the path with Lua `nil` (`Option::None`).
    Nil,
    /// Inline the first argument as a raw expression (e.g. `Some(v)` → `v`).
    InlineArg,
    /// Wrap the arguments in a Lua table constructor.
    TableCtor { tag: Option<&'static str> },
    /// Construct Rua's first-class tagged Result value.
    TaggedResult { ok: bool },
    /// Construct an empty runtime Vec.
    EmptyVec,
    /// Construct an empty runtime HashMap.
    EmptyMap,
}

/// Resolved builtin identity to codegen rule.
#[derive(Clone, Debug)]
pub struct CodegenRules {
    rules: HashMap<rua_core::BuiltinId, CodegenRule>,
}

impl CodegenRules {
    pub fn get(&self, builtin: rua_core::BuiltinId) -> Option<&CodegenRule> {
        self.rules.get(&builtin)
    }
}

impl Default for CodegenRules {
    fn default() -> Self {
        let mut m = HashMap::new();

        // --- Option<T> ---
        // Lua-idiomatic: Some(v) is the bare value, None is nil.
        m.insert(rua_core::BuiltinId::VariantOptionNone, CodegenRule::Nil);
        m.insert(
            rua_core::BuiltinId::VariantOptionSome,
            CodegenRule::InlineArg,
        );

        // --- Result<T, E> ---
        m.insert(
            rua_core::BuiltinId::VariantResultOk,
            CodegenRule::TaggedResult { ok: true },
        );
        m.insert(
            rua_core::BuiltinId::VariantResultErr,
            CodegenRule::TaggedResult { ok: false },
        );

        // --- Vec<T> ---
        m.insert(rua_core::BuiltinId::AssociatedVecNew, CodegenRule::EmptyVec);

        // --- HashMap<K, V> ---
        m.insert(
            rua_core::BuiltinId::AssociatedHashMapNew,
            CodegenRule::EmptyMap,
        );

        Self { rules: m }
    }
}

// ---------------------------------------------------------------------------
// .ruai source loading
// ---------------------------------------------------------------------------

fn parse_builtin_sources<'a>(
    sources: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<Vec<crate::ast::Item>, String> {
    let mut items = Vec::new();
    for (index, (name, source)) in sources.into_iter().enumerate() {
        let semantic_file = u32::MAX
            .checked_sub(index as u32)
            .expect("builtin source count exceeds semantic file namespace");
        let mut program = crate::parser::parse_with_semantic_file(source, semantic_file)
            .map_err(|error| format!("builtin {name}: {error}"))?;
        crate::resolve::validate_declaration_program(&program, semantic_file)
            .map_err(|error| format!("builtin {name}: {}", error.msg))?;
        items.append(&mut program.items);
    }
    Ok(items)
}

/// Parse the compiler's versioned, embedded builtin declarations.
pub fn load_embedded_builtins() -> Result<Vec<crate::ast::Item>, String> {
    parse_builtin_sources(
        rua_core::BUILTIN_SOURCES
            .iter()
            .map(|source| (source.name, source.text)),
    )
}

/// Load all `.ruai` files from `dir`, parse them, and return their items.
/// Files are loaded in lexicographic order for deterministic behaviour.
pub fn load_builtins_dir(dir: &Path) -> Result<Vec<crate::ast::Item>, String> {
    let entries = fs::read_dir(dir)
        .map_err(|error| format!("reading builtins dir {}: {error}", dir.display()))?;
    let mut paths = entries
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|error| format!("reading builtins dir {}: {error}", dir.display()))
        })
        .collect::<Result<Vec<PathBuf>, String>>()?;
    paths.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "ruai")
    });
    paths.sort();

    if paths.is_empty() {
        return Err(format!(
            "builtins dir {} contains no .ruai declarations",
            dir.display()
        ));
    }

    let sources = paths
        .iter()
        .map(|path| {
            fs::read_to_string(path)
                .map(|source| (path.display().to_string(), source))
                .map_err(|error| format!("reading {}: {error}", path.display()))
        })
        .collect::<Result<Vec<_>, String>>()?;
    parse_builtin_sources(
        sources
            .iter()
            .map(|(name, source)| (name.as_str(), source.as_str())),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_sysroot_parses() {
        assert!(!load_embedded_builtins().unwrap().is_empty());
    }
}
